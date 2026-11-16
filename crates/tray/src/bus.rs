//! D-Bus plumbing for the host: match rules, the message forwarder and the calls in flight.

use std::collections::HashMap;
use std::future::poll_fn;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, warn};
use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::{Connection, MatchRule, Message, MessageStream, OwnedMatchRule};

pub(crate) const DBUS_NAME: &str = "org.freedesktop.DBus";
pub(crate) const DBUS_PATH: &str = "/org/freedesktop/DBus";
pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub(crate) const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
pub(crate) const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
pub(crate) const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

// The host checks every sender against the owner it tracks.
pub(crate) fn host_rules() -> zbus::Result<Vec<OwnedMatchRule>> {
    let signal = || MatchRule::builder().msg_type(MessageType::Signal);
    let changed = |interface| {
        signal()
            .interface(PROPERTIES)
            .member("PropertiesChanged")
            .arg(0, interface)
            .build()
    };
    let rules = [
        signal()
            .sender(DBUS_NAME)
            .interface(DBUS_NAME)
            .member("NameOwnerChanged")
            .build()?,
        signal()
            .interface(WATCHER_NAME)
            .build()?,
        signal()
            .interface(ITEM_INTERFACE)
            .build()?,
        changed(ITEM_INTERFACE)?,
        signal()
            .interface(MENU_INTERFACE)
            .build()?,
        changed(MENU_INTERFACE)?,
    ];
    Ok(rules
        .into_iter()
        .map(OwnedMatchRule::from)
        .collect())
}

fn wanted(message: &Message) -> bool {
    match message.message_type() {
        MessageType::MethodReturn | MessageType::Error => true,
        MessageType::Signal => {
            let header = message.header();
            matches!(
                header.interface().map(|i| i.as_str()),
                Some(DBUS_NAME | WATCHER_NAME | ITEM_INTERFACE | MENU_INTERFACE | PROPERTIES)
            )
        }
        MessageType::MethodCall => false,
    }
}

// zbus stops reading the socket while any stream's queue is full, which would hold up every
// user of the connection. So this never waits on the host.
pub(crate) fn forward(conn: &Connection) -> (mpsc::UnboundedReceiver<Message>, ForwardTask) {
    let mut stream = MessageStream::from(conn);
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Some(item) = poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await {
            match item {
                Ok(message) => {
                    if wanted(&message) && tx.send(message).is_err() {
                        break;
                    }
                }
                Err(error) => debug!(%error, "dropping an unreadable message"),
            }
        }
    });
    (rx, ForwardTask(task))
}

pub(crate) struct ForwardTask(tokio::task::JoinHandle<()>);

impl Drop for ForwardTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Dest {
    Bus,
    Peer(Arc<str>),
}

impl Dest {
    fn name(&self) -> &str {
        match self {
            Self::Bus => DBUS_NAME,
            Self::Peer(name) => name,
        }
    }
}

struct Pending<T> {
    dest: Dest,
    sent: Instant,
    then: T,
}

// Calls go out without waiting, and replies come through the same channel as the signals.
// So both are handled in bus order, and a peer that never answers holds up nothing.
pub(crate) struct Calls<T> {
    conn: Connection,
    pending: HashMap<NonZeroU32, Pending<T>>,
    timeout: Duration,
}

impl<T> Calls<T> {
    pub(crate) fn new(conn: Connection, timeout: Duration) -> Self {
        Self { conn, pending: HashMap::new(), timeout }
    }

    pub(crate) async fn call<B>(
        &mut self,
        dest: Dest,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
        then: T,
    ) where
        B: serde::Serialize + zbus::DynamicType,
    {
        let message = Message::method_call(path, method)
            .destination(dest.name())
            .interface(interface)
            .build(body);
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, method, "cannot build a call");
                return;
            }
        };
        let serial = message.primary_header().serial_num();
        if let Err(error) = self.conn.send(&message).await {
            warn!(%error, method, "cannot send a call");
            return;
        }
        self.pending.insert(
            serial,
            Pending { dest, sent: Instant::now(), then },
        );
    }

    // A reply from anyone but the callee is ignored. The bus answers for a callee that is gone.
    pub(crate) fn reply(&mut self, message: &Message) -> Option<(T, zbus::Result<Message>)> {
        let header = message.header();
        let serial = header.reply_serial()?;
        let sender = header.sender()?.as_str();
        let call = self.pending.get(&serial)?;
        let is_error = message.message_type() == MessageType::Error;
        let from_callee = sender == call.dest.name() || (is_error && sender == DBUS_NAME);
        if !from_callee {
            debug!(sender, "ignoring a reply from someone else");
            return None;
        }
        let call = self.pending.remove(&serial)?;
        let result =
            if is_error { Err(zbus::Error::from(message.clone())) } else { Ok(message.clone()) };
        Some((call.then, result))
    }

    pub(crate) fn expired(&mut self, now: Instant) -> Vec<T> {
        let timeout = self.timeout;
        self.pending
            .extract_if(|_, call| now.duration_since(call.sent) >= timeout)
            .map(|(_, call)| call.then)
            .collect()
    }

    pub(crate) fn forget(&mut self, mut matches: impl FnMut(&T) -> bool) {
        self.pending
            .retain(|_, call| !matches(&call.then));
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

pub(crate) fn owner_change<'b>(
    message: &Message,
    body: &'b zbus::message::Body,
) -> Option<(&'b str, Option<&'b str>)> {
    let header = message.header();
    if header.sender().map(|s| s.as_str()) != Some(DBUS_NAME)
        || header.interface().map(|i| i.as_str()) != Some(DBUS_NAME)
        || header.member().map(|m| m.as_str()) != Some("NameOwnerChanged")
    {
        return None;
    }
    let (name, _, new): (&str, &str, &str) = body.deserialize().ok()?;
    Some((name, (!new.is_empty()).then_some(new)))
}

pub(crate) fn error_text(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, description, _) => match description {
            Some(description) => format!("{name}: {description}"),
            None => name.to_string(),
        },
        other => other.to_string(),
    }
}

pub(crate) fn unknown_method(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
        }
        zbus::Error::FDO(error) => matches!(**error, zbus::fdo::Error::UnknownMethod(_)),
        _ => false,
    }
}
