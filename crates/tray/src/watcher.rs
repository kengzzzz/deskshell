//! The `org.kde.StatusNotifierWatcher` service.

use std::future::poll_fn;
use std::pin::Pin;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use zbus::export::futures_core::Stream;
use zbus::fdo::RequestNameFlags;
use zbus::message::{Header, Type as MessageType};
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, MatchRule, Message, MessageStream, interface};

use crate::bus::{
    DBUS_NAME, DBUS_PATH, DEFAULT_ITEM_PATH, WATCHER_NAME, WATCHER_PATH, owner_change,
};
use crate::service::Error;

enum Request {
    Item { sender: String, service: String },
    Host { service: String },
}

// Both methods only queue the request and return, because checking the name takes a round
// trip to the bus and calls on this connection wait meanwhile.
struct Server {
    requests: mpsc::UnboundedSender<Request>,
    items: watch::Receiver<Vec<String>>,
}

#[interface(name = "org.kde.StatusNotifierWatcher", spawn = false)]
impl Server {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus passes the header by value"
    )]
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let sender = header
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let _ = self
            .requests
            .send(Request::Item { sender, service: service.to_owned() });
    }

    fn register_status_notifier_host(&self, service: &str) {
        let _ = self
            .requests
            .send(Request::Host { service: service.to_owned() });
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.borrow().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

// A registration with only a path belongs to the sender, and one without a path gets
// `/StatusNotifierItem`.
pub(crate) fn qualify(sender: &str, item: &str) -> String {
    let mut qualified =
        if item.starts_with('/') { format!("{sender}{item}") } else { item.to_owned() };
    if !qualified.contains('/') {
        qualified.push_str(DEFAULT_ITEM_PATH);
    }
    qualified
}

pub(crate) fn service_of(address: &str) -> &str {
    address
        .split_once('/')
        .map_or(address, |(service, _)| service)
}

pub struct Watcher {
    owned: watch::Receiver<bool>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Watcher {
    // The name is requested without replacing an owner and without allowing replacement. While
    // another connection owns it, the watcher tries again when that owner leaves.
    pub async fn start(conn: &Connection) -> Result<Self, Error> {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(DBUS_NAME)
            .interface(DBUS_NAME)
            .member("NameOwnerChanged")
            .build()?;
        let stream = MessageStream::for_match_rule(rule, conn, None).await?;
        let (owners_tx, owners) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(stream, owners_tx));

        let (requests_tx, requests) = mpsc::unbounded_channel();
        let (items_tx, items_rx) = watch::channel(Vec::new());
        let server = Server { requests: requests_tx, items: items_rx };
        let added = conn
            .object_server()
            .at(WATCHER_PATH, server)
            .await;
        let failed = match added {
            Ok(None) => None,
            Ok(Some(_)) => Some(Error::PathTaken),
            Err(error) => Some(Error::from(error)),
        };
        if let Some(error) = failed {
            forwarder.abort();
            return Err(error);
        }

        let (owned_tx, owned) = watch::channel(false);
        let emitter = SignalEmitter::new(conn, WATCHER_PATH)?.into_owned();
        let task = Task {
            conn: conn.clone(),
            emitter,
            items: items_tx,
            hosts: Vec::new(),
            owned: owned_tx,
        };
        task.try_take().await;
        let (stop_tx, stop) = oneshot::channel();
        let task = tokio::spawn(task.run(requests, owners, forwarder, stop));
        Ok(Self { owned, stop: Some(stop_tx), task: Some(task) })
    }

    pub fn owns_name(&self) -> bool {
        *self.owned.borrow()
    }

    pub fn ownership(&self) -> watch::Receiver<bool> {
        self.owned.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
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

struct Task {
    conn: Connection,
    emitter: SignalEmitter<'static>,
    items: watch::Sender<Vec<String>>,
    hosts: Vec<String>,
    owned: watch::Sender<bool>,
}

impl Task {
    async fn run(
        mut self,
        mut requests: mpsc::UnboundedReceiver<Request>,
        mut owners: mpsc::UnboundedReceiver<Message>,
        forwarder: JoinHandle<()>,
        mut stop: oneshot::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                Some(request) = requests.recv() => self.request(request).await,
                Some(message) = owners.recv() => self.owner_changed(&message).await,
                else => break,
            }
        }
        forwarder.abort();
        if *self.owned.borrow()
            && let Err(error) = self
                .conn
                .release_name(WATCHER_NAME)
                .await
        {
            warn!(%error, "cannot release {WATCHER_NAME}");
        }
        let _ = self
            .conn
            .object_server()
            .remove::<Server, _>(WATCHER_PATH)
            .await;
    }

    async fn try_take(&self) {
        let owned = match self
            .conn
            .request_name_with_flags(WATCHER_NAME, RequestNameFlags::DO_NOT_QUEUE)
            .await
        {
            Ok(_) => true,
            Err(zbus::Error::NameTaken) => {
                debug!("another watcher owns {WATCHER_NAME}, waiting for it to leave");
                false
            }
            Err(error) => {
                warn!(%error, "cannot request {WATCHER_NAME}");
                false
            }
        };
        self.owned.send_replace(owned);
    }

    /// Whether `name` is a valid bus name with an owner. Never starts a service.
    async fn has_owner(&self, name: &str) -> bool {
        if BusName::try_from(name).is_err() {
            return false;
        }
        self.conn
            .call_method(
                Some(DBUS_NAME),
                DBUS_PATH,
                Some(DBUS_NAME),
                "GetNameOwner",
                &(name,),
            )
            .await
            .is_ok()
    }

    async fn request(&mut self, request: Request) {
        match request {
            Request::Item { sender, service } => {
                let qualified = qualify(&sender, &service);
                if self.items.borrow().contains(&qualified) {
                    debug!(
                        item = qualified,
                        "skipping a duplicate item registration"
                    );
                    return;
                }
                if !self
                    .has_owner(service_of(&qualified))
                    .await
                {
                    warn!(
                        item = qualified,
                        "ignoring an item registration without an owner"
                    );
                    return;
                }
                self.items
                    .send_modify(|items| items.push(qualified.clone()));
                debug!(item = qualified, "item registered");
                if let Err(error) =
                    Server::status_notifier_item_registered(&self.emitter, &qualified).await
                {
                    warn!(%error, "cannot emit StatusNotifierItemRegistered");
                }
            }
            Request::Host { service } => {
                if self.hosts.contains(&service) {
                    debug!(
                        host = service,
                        "skipping a duplicate host registration"
                    );
                    return;
                }
                if !self.has_owner(&service).await {
                    warn!(
                        host = service,
                        "ignoring a host registration without an owner"
                    );
                    return;
                }
                debug!(host = service, "host registered");
                self.hosts.push(service);
                if let Err(error) = Server::status_notifier_host_registered(&self.emitter).await {
                    warn!(%error, "cannot emit StatusNotifierHostRegistered");
                }
            }
        }
    }

    // Names match exactly, so `:1.1` leaving never takes `:1.10`'s items, and every item that left
    // together is signalled.
    async fn owner_changed(&mut self, message: &Message) {
        let body = message.body();
        let Some((name, None)) = owner_change(message, &body) else {
            return;
        };
        if name == WATCHER_NAME {
            if !*self.owned.borrow() {
                debug!("the active watcher left, taking {WATCHER_NAME}");
                self.try_take().await;
            }
            return;
        }
        let mut gone = Vec::new();
        self.items.send_if_modified(|items| {
            gone.extend(items.extract_if(.., |item| service_of(item) == name));
            !gone.is_empty()
        });
        for item in &gone {
            debug!(item, "item unregistered");
            if let Err(error) = Server::status_notifier_item_unregistered(&self.emitter, item).await
            {
                warn!(%error, "cannot emit StatusNotifierItemUnregistered");
            }
        }
        let hosts = self.hosts.len();
        self.hosts.retain(|host| host != name);
        if self.hosts.len() != hosts {
            debug!(host = name, "host unregistered");
            if let Err(error) = Server::status_notifier_host_unregistered(&self.emitter).await {
                warn!(%error, "cannot emit StatusNotifierHostUnregistered");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_qualified() {
        assert_eq!(
            qualify(":1.5", "/org/ayatana/Item"),
            ":1.5/org/ayatana/Item"
        );
        assert_eq!(
            qualify(":1.5", "org.kde.StatusNotifierItem-1-1"),
            "org.kde.StatusNotifierItem-1-1/StatusNotifierItem"
        );
        assert_eq!(
            qualify(":1.5", "org.example.App/Tray"),
            "org.example.App/Tray"
        );
        assert_eq!(qualify(":1.5", ":1.7"), ":1.7/StatusNotifierItem");
        assert_eq!(qualify(":1.5", ""), "/StatusNotifierItem");
        assert_eq!(service_of(":1.5/org/ayatana/Item"), ":1.5");
        assert_eq!(service_of("org.example.App"), "org.example.App");
        assert_eq!(service_of("/StatusNotifierItem"), "");
    }
}
