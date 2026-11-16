//! D-Bus plumbing shared by the NetworkManager and BlueZ clients.

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;

use futures_core::Stream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;
use zbus::message::Type as MessageType;
use zbus::{Connection, DBusError, MatchRule, Message, MessageStream, Variant};

pub(crate) const DBUS_NAME: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub(crate) const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";

pub(crate) type Props<'a> = HashMap<&'a str, Variant<'a>>;

// zbus blocks its socket reader while a stream's queue is full, which also holds back our
// call replies. A task per stream drains it into an unbounded channel.
pub(crate) struct Signals {
    rx: mpsc::UnboundedReceiver<Message>,
    tasks: Vec<JoinHandle<()>>,
}

impl Signals {
    // zbus cannot check a well-known sender, so anyone may send these. The caller compares the
    // sender with the owner it tracks.
    pub(crate) async fn subscribe(
        conn: &Connection,
        name: &'static str,
        path_namespace: Option<&'static str>,
    ) -> zbus::Result<Self> {
        let owner = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(DBUS_NAME)
            .interface(DBUS_NAME)
            .member("NameOwnerChanged")
            .arg(0, name)
            .build()?;
        let mut service = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(name);
        if let Some(namespace) = path_namespace {
            service = service.path_namespace(namespace);
        }
        let service = service.build()?;

        let (tx, rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::with_capacity(2);
        for rule in [owner, service] {
            let stream = MessageStream::for_match_rule(rule, conn, None).await?;
            tasks.push(tokio::spawn(forward(stream, tx.clone())));
        }
        Ok(Self { rx, tasks })
    }

    pub(crate) async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn forward(mut stream: MessageStream, tx: mpsc::UnboundedSender<Message>) {
    while let Some(item) = poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await {
        match item {
            Ok(message) => {
                if tx.send(message).is_err() {
                    break;
                }
            }
            Err(error) => debug!(%error, "dropping an unreadable signal"),
        }
    }
}

pub(crate) fn owner_change(message: &Message, name: &str) -> Option<Option<String>> {
    let header = message.header();
    if header.sender().map(|s| s.as_str()) != Some(DBUS_NAME)
        || header.interface().map(|i| i.as_str()) != Some(DBUS_NAME)
        || header.member().map(|m| m.as_str()) != Some("NameOwnerChanged")
    {
        return None;
    }
    let body = message.body();
    let (changed, _, new): (&str, &str, &str) = body.deserialize().ok()?;
    if changed != name {
        return None;
    }
    Some((!new.is_empty()).then(|| new.to_owned()))
}

pub(crate) async fn name_owner(conn: &Connection, name: &str) -> zbus::Result<Option<String>> {
    match conn
        .call_method(
            Some(DBUS_NAME),
            DBUS_PATH,
            Some(DBUS_NAME),
            "GetNameOwner",
            &(name,),
        )
        .await
    {
        Ok(reply) => Ok(Some(reply.body().deserialize::<String>()?)),
        Err(zbus::Error::FDO(error)) if matches!(*error, zbus::fdo::Error::NameHasNoOwner(_)) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

// Calls go to the owner's unique name. That never starts a service through D-Bus activation,
// and a call for an owner that left fails instead of reaching its successor.
pub(crate) async fn set_property(
    conn: &Connection,
    owner: &str,
    path: &str,
    interface: &str,
    name: &str,
    value: Variant<'_>,
) -> zbus::Result<()> {
    conn.call_method(
        Some(owner),
        path,
        Some(PROPERTIES),
        "Set",
        &(interface, name, value),
    )
    .await?;
    Ok(())
}

pub(crate) async fn call_unit(
    conn: &Connection,
    owner: &str,
    path: &str,
    interface: &str,
    method: &str,
) -> zbus::Result<()> {
    conn.call_method(Some(owner), path, Some(interface), method, &())
        .await?;
    Ok(())
}

pub(crate) fn error_text(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, description, _) => description
            .clone()
            .unwrap_or_else(|| name.to_string()),
        zbus::Error::FDO(error) => error
            .description()
            .map_or_else(|| error.name().to_string(), str::to_owned),
        other => other.to_string(),
    }
}

fn inner<'v, 'a>(value: &'v Variant<'a>) -> &'v Variant<'a> {
    match value {
        Variant::Variant(boxed) => inner(boxed),
        other => other,
    }
}

pub(crate) fn as_bool(value: &Variant<'_>) -> Option<bool> {
    match inner(value) {
        Variant::Bool(b) => Some(*b),
        _ => None,
    }
}

pub(crate) fn as_u8(value: &Variant<'_>) -> Option<u8> {
    match inner(value) {
        Variant::U8(n) => Some(*n),
        _ => None,
    }
}

pub(crate) fn as_u32(value: &Variant<'_>) -> Option<u32> {
    match inner(value) {
        Variant::U32(n) => Some(*n),
        _ => None,
    }
}

pub(crate) fn as_i64(value: &Variant<'_>) -> Option<i64> {
    match inner(value) {
        Variant::I64(n) => Some(*n),
        _ => None,
    }
}

pub(crate) fn as_u64(value: &Variant<'_>) -> Option<u64> {
    match inner(value) {
        Variant::U64(n) => Some(*n),
        _ => None,
    }
}

pub(crate) fn as_str<'v>(value: &'v Variant<'_>) -> Option<&'v str> {
    match inner(value) {
        Variant::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

pub(crate) fn as_path<'v>(value: &'v Variant<'_>) -> Option<&'v str> {
    match inner(value) {
        Variant::ObjectPath(path) => Some(path.as_str()),
        _ => None,
    }
}

fn elements<'v, 'a>(value: &'v Variant<'a>) -> &'v [Variant<'a>] {
    match inner(value) {
        Variant::Array(array) => array.inner(),
        _ => &[],
    }
}

pub(crate) fn paths<'v>(value: &'v Variant<'_>) -> impl Iterator<Item = &'v str> {
    elements(value)
        .iter()
        .filter_map(as_path)
}

pub(crate) fn strings<'v>(value: &'v Variant<'_>) -> impl Iterator<Item = &'v str> {
    elements(value)
        .iter()
        .filter_map(as_str)
}

pub(crate) fn bytes_lossy(value: &Variant<'_>) -> Option<String> {
    let Variant::Array(array) = inner(value) else {
        return None;
    };
    let bytes: Vec<u8> = array
        .inner()
        .iter()
        .filter_map(as_u8)
        .collect();
    Some(String::from_utf8_lossy_owned(bytes))
}

pub(crate) fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

// `items` must come in the same order as `out`.
pub(crate) fn sync_list<T, S>(
    out: &mut Vec<T>,
    items: impl IntoIterator<Item = S>,
    same: impl Fn(&T, &S) -> bool,
    mut update: impl FnMut(&mut T, &S) -> bool,
    mut make: impl FnMut(&S) -> T,
) -> bool {
    let mut changed = false;
    let mut i = 0;
    for item in items {
        match out[i..]
            .iter()
            .position(|entry| same(entry, &item))
        {
            Some(offset) => {
                if offset > 0 {
                    out.drain(i..i + offset);
                    changed = true;
                }
                changed |= update(&mut out[i], &item);
            }
            None => {
                let mut entry = make(&item);
                update(&mut entry, &item);
                out.insert(i, entry);
                changed = true;
            }
        }
        i += 1;
    }
    if out.len() > i {
        out.truncate(i);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(out: &mut Vec<(u32, u32)>, items: &[(u32, u32)]) -> bool {
        sync_list(
            out,
            items.iter(),
            |entry, item| entry.0 == item.0,
            |entry, item| set(&mut entry.1, item.1),
            |item| (item.0, 0),
        )
    }

    #[test]
    fn sync_list_updates_inserts_and_removes() {
        let mut out = Vec::new();
        assert!(sync(&mut out, &[(1, 10), (2, 20), (3, 30)]));
        assert_eq!(out, [(1, 10), (2, 20), (3, 30)]);
        assert!(!sync(&mut out, &[(1, 10), (2, 20), (3, 30)]));
        assert!(sync(&mut out, &[(1, 10), (2, 21), (3, 30)]));
        assert_eq!(out, [(1, 10), (2, 21), (3, 30)]);
        assert!(sync(&mut out, &[(1, 10), (3, 30)]));
        assert_eq!(out, [(1, 10), (3, 30)]);
        assert!(sync(
            &mut out,
            &[(0, 5), (1, 10), (2, 20), (3, 30), (4, 40)]
        ));
        assert_eq!(out, [(0, 5), (1, 10), (2, 20), (3, 30), (4, 40)]);
        assert!(sync(&mut out, &[(4, 40)]));
        assert_eq!(out, [(4, 40)]);
        assert!(sync(&mut out, &[]));
        assert!(out.is_empty());
    }

    #[test]
    fn variant_readers() {
        assert_eq!(as_bool(&Variant::Bool(true)), Some(true));
        assert_eq!(
            as_bool(&Variant::new(Variant::Bool(true))),
            Some(true)
        );
        assert_eq!(as_u32(&Variant::U32(7)), Some(7));
        assert_eq!(as_u32(&Variant::U8(7)), None);
        assert_eq!(as_str(&Variant::from("wlan0")), Some("wlan0"));
        let ssid = Variant::from(vec![b'C', b'a', 0xff, b'e']);
        assert_eq!(bytes_lossy(&ssid).as_deref(), Some("Ca\u{fffd}e"));
        let list = Variant::from(vec!["wpa", "rsn"]);
        assert_eq!(strings(&list).collect::<Vec<_>>(), ["wpa", "rsn"]);
    }
}
