//! The `org.freedesktop.Notifications` D-Bus server.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};
use zbus::export::futures_core::Stream;
use zbus::fdo::{DBusProxy, NameAcquiredStream, RequestNameFlags, RequestNameReply};
use zbus::object_server::{ResponseDispatchNotifier, SignalEmitter};
use zbus::{Connection, fdo, interface};

use crate::focus::Effect;
use crate::hints::Hints;
use crate::model::{Command, State};
use crate::persist::{self, Source, Writer};
use crate::store::{Incoming, Outcome, Signal, Store, parse_actions};

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

pub const CAPABILITIES: [&str; 7] = [
    "persistence",
    "body",
    "body-markup",
    "body-hyperlinks",
    "actions",
    "icon-static",
    "inline-reply",
];

pub const SERVER_NAME: &str = "deskshell";
pub const SERVER_VENDOR: &str = "deskshell";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SPEC_VERSION: &str = "1.3";

#[derive(Debug, Clone)]
pub struct Config {
    pub state_path: Option<PathBuf>,
    pub save_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { state_path: None, save_delay: Duration::from_millis(500) }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this connection already serves an object at {OBJECT_PATH}")]
    PathTaken,
    #[error("D-Bus: {0}")]
    DBus(#[from] zbus::Error),
}

enum Request {
    Notify { incoming: Box<Incoming>, reply: oneshot::Sender<ResponseDispatchNotifier<u32>> },
    Close { id: u32, done: oneshot::Sender<()> },
}

/// `spawn = false` keeps calls in the order they arrive.
struct Server {
    requests: mpsc::UnboundedSender<Request>,
}

fn stopped<T>(_: T) -> fdo::Error {
    fdo::Error::Failed("the notification service stopped".to_owned())
}

#[interface(name = "org.freedesktop.Notifications", spawn = false)]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        CAPABILITIES.to_vec()
    }

    /// `expire_timeout` is ignored. A notification stays live until it is closed, and the
    /// toast lifetime is UI policy.
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method takes this many arguments"
    )]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<&str>,
        hints: Hints,
        _expire_timeout: i32,
    ) -> fdo::Result<ResponseDispatchNotifier<u32>> {
        let incoming = Incoming {
            app_name: app_name.to_owned(),
            replaces_id,
            app_icon: app_icon.to_owned(),
            summary: summary.to_owned(),
            body: body.to_owned(),
            actions: parse_actions(&actions),
            hints,
        };
        let (reply, id) = oneshot::channel();
        self.requests
            .send(Request::Notify { incoming: Box::new(incoming), reply })
            .map_err(stopped)?;
        id.await.map_err(stopped)
    }

    async fn close_notification(&self, id: u32) -> fdo::Result<()> {
        let (done, closed) = oneshot::channel();
        self.requests
            .send(Request::Close { id, done })
            .map_err(stopped)?;
        closed.await.map_err(stopped)
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(
        &self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    ) {
        (
            SERVER_NAME,
            SERVER_VENDOR,
            SERVER_VERSION,
            SPEC_VERSION,
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    /// Not in the freedesktop spec. Senders that offer an inline reply listen for it.
    #[zbus(signal)]
    async fn notification_replied(
        emitter: &SignalEmitter<'_>,
        id: u32,
        text: &str,
    ) -> zbus::Result<()>;
}

/// Dropping it stops the server after a last save.
pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Service {
    pub async fn start(
        conn: &Connection,
        config: Config,
    ) -> Result<
        (
            Self,
            watch::Receiver<State>,
            mpsc::UnboundedReceiver<Effect>,
        ),
        Error,
    > {
        let loaded = match &config.state_path {
            Some(path) => Some(persist::load(path).await),
            None => None,
        };

        let (requests_tx, requests) = mpsc::unbounded_channel();
        let server = Server { requests: requests_tx };
        if conn
            .object_server()
            .at(OBJECT_PATH, server)
            .await?
            .is_some()
        {
            return Err(Error::PathTaken);
        }
        let acquired = match request_name(conn).await {
            Ok(acquired) => acquired,
            Err(error) => {
                let _ = conn
                    .object_server()
                    .remove::<Server, _>(OBJECT_PATH)
                    .await;
                return Err(error.into());
            }
        };

        let mut initial = State::default();
        if let (Some(path), Some(loaded)) = (&config.state_path, loaded) {
            initial.dnd = loaded.dnd;
            initial.history = loaded.history;
            if loaded.source == Source::Corrupt {
                persist::quarantine(path).await;
            }
        }
        let (state, state_rx) = watch::channel(initial);
        let writer = config
            .state_path
            .map(|path| Writer::spawn(path, config.save_delay, state_rx.clone()));

        let (commands_tx, commands) = mpsc::unbounded_channel();
        let (effects_tx, effects) = mpsc::unbounded_channel();
        let (stop_tx, stop) = oneshot::channel();
        let task = Task {
            conn: conn.clone(),
            store: Store::new(),
            state,
            effects: effects_tx,
            writer,
            acquired,
        };
        let task = tokio::spawn(task.run(requests, commands, stop));
        let service = Self { commands: commands_tx, stop: Some(stop_tx), task: Some(task) };
        Ok((service, state_rx, effects))
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
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

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Task {
    conn: Connection,
    store: Store,
    state: watch::Sender<State>,
    effects: mpsc::UnboundedSender<Effect>,
    writer: Option<Writer>,
    acquired: Option<NameAcquiredStream>,
}

impl Task {
    async fn run(
        mut self,
        mut requests: mpsc::UnboundedReceiver<Request>,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut stop: oneshot::Receiver<()>,
    ) {
        let emitter = SignalEmitter::new(&self.conn, OBJECT_PATH).expect("a valid object path");
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                Some(request) = requests.recv() => match request {
                    Request::Notify { incoming, reply } => {
                        let (id, mut outcome) = self.store.notify(&self.state, *incoming, now_ms());
                        let after_reply = std::mem::take(&mut outcome.after_reply);
                        self.apply(&emitter, outcome).await;
                        let (response, sent) = ResponseDispatchNotifier::new(id);
                        let _ = reply.send(response);
                        if !after_reply.is_empty() {
                            let emitter = emitter.clone();
                            tokio::spawn(async move {
                                sent.await;
                                emit(&emitter, after_reply).await;
                            });
                        }
                    }
                    Request::Close { id, done } => {
                        let outcome = self.store.close_notification(&self.state, id);
                        self.apply(&emitter, outcome).await;
                        let _ = done.send(());
                    }
                },
                Some(command) = commands.recv() => {
                    let outcome = self.store.command(&self.state, command, now_ms());
                    self.apply(&emitter, outcome).await;
                }
                acquired = next_acquired(&mut self.acquired) => {
                    if acquired.is_some() {
                        info!("the other notification server left, {BUS_NAME} is ours now");
                    }
                    self.acquired = None;
                }
            }
        }

        // Calls still queued get an error now instead of waiting for the shutdown.
        drop(requests);
        drop(commands);
        if let Some(writer) = self.writer.take() {
            writer.finish().await;
        }
        if let Err(error) = self.conn.release_name(BUS_NAME).await {
            warn!(%error, "cannot release {BUS_NAME}");
        }
        let _ = self
            .conn
            .object_server()
            .remove::<Server, _>(OBJECT_PATH)
            .await;
    }

    async fn apply(&self, emitter: &SignalEmitter<'_>, outcome: Outcome) {
        if outcome.persist
            && let Some(writer) = &self.writer
        {
            writer.mark();
        }
        emit(emitter, outcome.signals).await;
        for effect in outcome.effects {
            let _ = self.effects.send(effect);
        }
    }
}

async fn emit(emitter: &SignalEmitter<'_>, signals: Vec<Signal>) {
    for signal in signals {
        let result = match &signal {
            Signal::Closed { id, reason } => {
                Server::notification_closed(emitter, *id, *reason).await
            }
            Signal::ActionInvoked { id, key } => Server::action_invoked(emitter, *id, key).await,
            Signal::Replied { id, text } => Server::notification_replied(emitter, *id, text).await,
        };
        if let Err(error) = result {
            warn!(%error, ?signal, "cannot emit a notification signal");
        }
    }
}

/// Requests the name without `DO_NOT_QUEUE`, so the bus hands it over when the current
/// owner leaves.
async fn request_name(conn: &Connection) -> zbus::Result<Option<NameAcquiredStream>> {
    // Subscribed before the request, so a quick handover is not missed.
    let acquired = DBusProxy::new(conn)
        .await?
        .receive_name_acquired_with_args(&[(0, BUS_NAME)])
        .await?;
    match conn
        .request_name_with_flags(BUS_NAME, RequestNameFlags::empty())
        .await?
    {
        RequestNameReply::InQueue => {
            warn!("another notification server owns {BUS_NAME}, waiting until it leaves");
            Ok(Some(acquired))
        }
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(None),
        RequestNameReply::Exists => Err(zbus::Error::NameTaken),
    }
}

async fn next_acquired(acquired: &mut Option<NameAcquiredStream>) -> Option<()> {
    let Some(stream) = acquired.as_mut() else {
        return std::future::pending().await;
    };
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx))
        .await
        .map(drop)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
