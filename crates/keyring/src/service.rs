//! The prompter service task: the connection, the name, the clients and the YubiKey path.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use zbus::export::futures_core::Stream;
use zbus::fdo::{self, DBusProxy, NameAcquiredStream, NameLostStream, NameOwnerChangedStream};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::names::BusName;
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::{Connection, Variant};

use crate::exchange::Exchange;
use crate::prompter::{Incoming, Key, Prompter, Prop};
use crate::yubikey::{self, Outcome};
use crate::{
    Answer, Command, Config, Event, FAILED_WARNING, Kind, NO_CARD_WARNING, OBJECT_PATH, Prompt,
    Token, YubiKey, strip_mnemonic,
};

const CALLBACK_INTERFACE: &str = "org.gnome.keyring.internal.Prompter.Callback";
const NOT_BEGUN: &str = "Not begun prompting for this prompt callback";
const ALREADY_BEGUN: &str = "Already begun prompting for this prompt callback";
const ALREADY_PERFORMING: &str = "Already performing a prompt for this prompt callback";
const BAD_EXCHANGE: &str = "Invalid secret exchange received";
const BAD_TYPE: &str = "Invalid type argument";
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// Longer caller texts are cut to this many bytes.
pub const MAX_PROMPT_TEXT: usize = 4096;
/// A connection that lasted this long starts the backoff over.
const STEADY: Duration = Duration::from_secs(60);

/// Dropping it stops the service in the background.
pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start(config: Config) -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (commands_tx, commands) = mpsc::unbounded_channel();
        let (stop_tx, stop) = oneshot::channel();
        let task = tokio::spawn(run(config, events_tx, commands, stop));
        let service = Self { commands: commands_tx, stop: Some(stop_tx), task: Some(task) };
        (service, events)
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

enum Ended {
    Stopped,
    Lost,
}

async fn run(
    config: Config,
    events: mpsc::UnboundedSender<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut retry = FIRST_RETRY;
    // Kept across connections, so no two tokens ever share a serial.
    let mut serial = 0;
    loop {
        let started = tokio::time::Instant::now();
        match connect(&config).await {
            Ok(conn) => {
                let ended = serve(
                    &config,
                    conn,
                    &events,
                    &mut commands,
                    &mut stop,
                    &mut serial,
                )
                .await;
                if matches!(ended, Ended::Stopped) {
                    return;
                }
                warn!("the keyring prompter lost its bus connection");
                if started.elapsed() >= STEADY {
                    retry = FIRST_RETRY;
                }
            }
            Err(error) => warn!(%error, "the keyring prompter cannot connect to the bus"),
        }
        let wait = tokio::time::sleep(retry);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                () = &mut wait => break,
                command = commands.recv() => match command {
                    Some(command) => debug!(?command, "no bus, the keyring command is dropped"),
                    None => return,
                },
            }
        }
        retry = (retry * 2).min(LAST_RETRY);
    }
}

async fn connect(config: &Config) -> zbus::Result<Connection> {
    match &config.address {
        Some(address) => {
            zbus::connection::Builder::address(address.as_str())
                .build()
                .await
        }
        None => Connection::session().await,
    }
}

struct Signals {
    acquired: NameAcquiredStream,
    lost: NameLostStream,
    owners: NameOwnerChangedStream,
}

async fn serve(
    config: &Config,
    conn: Connection,
    events: &mpsc::UnboundedSender<Event>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: &mut oneshot::Receiver<()>,
    serial: &mut u64,
) -> Ended {
    let (incoming_tx, mut incoming) = mpsc::unbounded_channel();
    let setup = async {
        let bus = DBusProxy::new(&conn).await?;
        let me = conn
            .unique_name()
            .ok_or(zbus::Error::Unsupported)?
            .to_owned();
        let uid = bus
            .get_connection_credentials(BusName::Unique(me.as_ref()))
            .await?
            .unix_user_id()
            .ok_or(zbus::Error::Unsupported)?;
        let prompter = Prompter {
            incoming: incoming_tx,
            bus: bus.clone(),
            uid,
            yubikey: config.yubikey.is_some(),
        };
        conn.object_server()
            .at(OBJECT_PATH, prompter)
            .await?;
        // Subscribed before the request, so a quick handover is not missed.
        let name = config.bus_name.as_str();
        let signals = Signals {
            acquired: bus
                .receive_name_acquired_with_args(&[(0, name)])
                .await?,
            lost: bus
                .receive_name_lost_with_args(&[(0, name)])
                .await?,
            owners: bus.receive_name_owner_changed().await?,
        };
        let reply = conn
            .request_name_with_flags(name, RequestNameFlags::empty())
            .await?;
        Ok::<_, zbus::Error>((signals, reply))
    };
    let (mut signals, reply) = match setup.await {
        Ok(setup) => setup,
        Err(error) => {
            warn!(%error, "the keyring prompter could not set up");
            let _ = conn
                .object_server()
                .remove::<Prompter, _>(OBJECT_PATH)
                .await;
            return Ended::Lost;
        }
    };
    let owned = match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            info!(
                name = config.bus_name,
                "serving the keyring prompter"
            );
            true
        }
        RequestNameReply::InQueue | RequestNameReply::Exists => {
            warn!(
                name = config.bus_name,
                "another keyring prompter owns the name, waiting until it leaves"
            );
            false
        }
    };
    let (done_tx, mut done) = mpsc::unbounded_channel();
    let mut task = Task {
        conn: conn.clone(),
        config,
        events,
        done: done_tx,
        owned,
        clients: HashMap::new(),
        closing: HashMap::new(),
        deferred: HashMap::new(),
        tails: HashMap::new(),
        waiting: VecDeque::new(),
        active: None,
        suspended: None,
        decrypt: None,
        result: None,
        shown: None,
        serial,
        next_id: 0,
    };
    let ended = loop {
        tokio::select! {
            _ = &mut *stop => break Ended::Stopped,
            () = conn.closed() => break Ended::Lost,
            Some(call) = incoming.recv() => task.incoming(call),
            command = commands.recv() => match command {
                Some(command) => task.command(command),
                None => break Ended::Stopped,
            },
            Some(done) = done.recv() => task.done(done),
            Some(_) = next(&mut signals.acquired) => task.acquired(),
            Some(_) = next(&mut signals.lost) => task.lost(),
            Some(change) = next(&mut signals.owners) => task.owner_changed(&change),
        }
    };
    task.end_all();
    if matches!(ended, Ended::Stopped) {
        if let Err(error) = conn
            .release_name(config.bus_name.as_str())
            .await
        {
            warn!(%error, "cannot release the keyring prompter name");
        }
        let _ = conn
            .object_server()
            .remove::<Prompter, _>(OBJECT_PATH)
            .await;
        info!("the keyring prompter stopped");
    }
    ended
}

async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}

#[derive(Debug, Default, Clone)]
struct Props {
    title: String,
    message: String,
    description: String,
    warning: String,
    choice_label: String,
    continue_label: String,
    cancel_label: String,
    password_new: bool,
    choice_chosen: bool,
}

impl Props {
    fn apply(&mut self, name: &str, value: Prop) {
        let text = match (name, value) {
            ("password-new", Prop::Bool(value)) => {
                self.password_new = value;
                return;
            }
            ("choice-chosen", Prop::Bool(value)) => {
                self.choice_chosen = value;
                return;
            }
            (_, Prop::Str(text)) => text,
            (_, Prop::Bool(_)) => return,
        };
        let slot = match name {
            "title" => &mut self.title,
            "message" => &mut self.message,
            "description" => &mut self.description,
            "warning" => &mut self.warning,
            "choice-label" => &mut self.choice_label,
            "continue-label" => &mut self.continue_label,
            "cancel-label" => &mut self.cancel_label,
            _ => return,
        };
        *slot = capped(text);
    }
}

/// Cut at a character boundary, so no caller can make the shell lay out or decode an endless
/// string.
fn capped(mut text: String) -> String {
    if text.len() > MAX_PROMPT_TEXT {
        let end = text.floor_char_boundary(MAX_PROMPT_TEXT);
        text.truncate(end);
    }
    text
}

struct Active {
    key: Key,
    exchange: Exchange,
    received: bool,
    rounds: u64,
    open: Option<Kind>,
    props: Props,
    yubikey: bool,
    auto_tried: bool,
    call: Option<u64>,
}

enum Done {
    Begun { key: Key, id: u64 },
    Closed { key: Key, id: u64 },
    Call { id: u64, key: Key, error: Option<String> },
    Decrypt { id: u64, outcome: Outcome },
}

enum Tried {
    Password(zeroize::Zeroizing<Vec<u8>>),
    Form(&'static str),
}

/// `lifetime` ends when the client stops, so a `PromptReady` still queued behind an earlier call
/// never goes out.
struct Begin {
    id: u64,
    lifetime: Arc<AtomicBool>,
}

struct Task<'a> {
    conn: Connection,
    config: &'a Config,
    events: &'a mpsc::UnboundedSender<Event>,
    done: mpsc::UnboundedSender<Done>,
    owned: bool,
    clients: HashMap<Key, Begin>,
    closing: HashMap<Key, u64>,
    deferred: HashMap<Key, u64>,
    /// Each new call and each close goes out after the last one to its callback, so calls to
    /// one callback never cross, across every life on that sender and path.
    tails: HashMap<Key, JoinHandle<()>>,
    waiting: VecDeque<Key>,
    active: Option<Active>,
    suspended: Option<Active>,
    decrypt: Option<(u64, JoinHandle<()>)>,
    result: Option<Tried>,
    shown: Option<(Token, Kind)>,
    serial: &'a mut u64,
    next_id: u64,
}

impl Task<'_> {
    fn incoming(&mut self, call: Incoming) {
        match call {
            Incoming::Begin { key, reply, sent } => {
                let begun = self.begin(&key);
                let id = begun.as_ref().ok().copied();
                let _ = reply.send(begun.map(drop));
                if let Some(id) = id {
                    let done = self.done.clone();
                    tokio::spawn(async move {
                        sent.await;
                        let _ = done.send(Done::Begun { key, id });
                    });
                }
            }
            Incoming::Perform { key, kind, props, exchange, secrets_owner, reply } => {
                self.perform(
                    &key,
                    &kind,
                    props,
                    &exchange,
                    secrets_owner,
                    reply,
                );
            }
            Incoming::Stop { key, reply } => {
                let _ = reply.send(Ok(()));
                self.stop(&key, true);
                self.next_ready();
            }
        }
    }

    fn begin(&mut self, key: &Key) -> fdo::Result<u64> {
        if !self.owned {
            return Err(not_serving());
        }
        if self.clients.contains_key(key) {
            return Err(fdo::Error::Failed(ALREADY_BEGUN.to_owned()));
        }
        self.next_id += 1;
        let begin = Begin { id: self.next_id, lifetime: Arc::new(AtomicBool::new(true)) };
        self.clients.insert(key.clone(), begin);
        debug!(sender = %key.sender, queued = self.active.is_some(), "a keyring client began");
        Ok(self.next_id)
    }

    /// A client that stopped and began again waits for its own begin reply and for its last
    /// life's calls to close, so an old answer or `PromptDone` never reaches the new one.
    fn begun(&mut self, key: Key, id: u64) {
        if self
            .clients
            .get(&key)
            .is_none_or(|begin| begin.id != id)
        {
            return;
        }
        if self.closing.contains_key(&key) {
            self.deferred.insert(key, id);
            return;
        }
        self.waiting.push_back(key);
        self.next_ready();
    }

    fn closed(&mut self, key: Key, id: u64) {
        if self.closing.get(&key) != Some(&id) {
            return;
        }
        self.closing.remove(&key);
        if !self.clients.contains_key(&key) {
            self.tails.remove(&key);
        }
        if let Some(begin) = self.deferred.remove(&key) {
            self.begun(key, begin);
        }
    }

    fn perform(
        &mut self,
        key: &Key,
        kind: &str,
        props: Vec<(String, Prop)>,
        exchange: &str,
        secrets_owner: bool,
        reply: crate::prompter::Reply,
    ) {
        if !self.owned {
            let _ = reply.send(Err(not_serving()));
            return;
        }
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.key == *key)
        else {
            let _ = reply.send(Err(fdo::Error::Failed(NOT_BEGUN.to_owned())));
            return;
        };
        if active.open.is_some() {
            let _ = reply.send(Err(fdo::Error::Failed(
                ALREADY_PERFORMING.to_owned(),
            )));
            return;
        }
        let warned = props
            .iter()
            .any(|(name, value)| name == "warning" && *value != Prop::Str(String::new()));
        for (name, value) in props {
            active.props.apply(&name, value);
        }
        // The client never sends a secret. One that does is wiped here.
        if active
            .exchange
            .receive(exchange)
            .is_err()
        {
            let _ = reply.send(Err(fdo::Error::InvalidArgs(
                BAD_EXCHANGE.to_owned(),
            )));
            return;
        }
        active.received = true;
        let kind = match kind {
            "password" => Kind::Password,
            "confirm" => Kind::Confirm,
            _ => {
                let _ = reply.send(Err(fdo::Error::InvalidArgs(BAD_TYPE.to_owned())));
                return;
            }
        };
        active.rounds += 1;
        active.open = Some(kind);
        let _ = reply.send(Ok(()));
        let yubikey = kind == Kind::Password
            && !active.props.password_new
            && self.suspended.is_none()
            && secrets_owner
            && self
                .config
                .yubikey
                .as_ref()
                .and_then(yubikey::settings)
                .is_some_and(|settings| {
                    !settings.label.is_empty()
                        && active
                            .props
                            .description
                            .contains(&settings.label)
                });
        active.yubikey = yubikey;
        // gnome-keyring sends a warning only after a wrong password, so a stale envelope
        // falls back to typing instead of trying again and again.
        if yubikey && !active.auto_tried && !warned {
            active.auto_tried = true;
            self.use_yubikey();
            return;
        }
        self.show_active(kind);
    }

    /// Ends a client. `done` sends `PromptDone`, as `StopPrompting` does.
    fn stop(&mut self, key: &Key, done: bool) {
        let Some(begin) = self.clients.remove(key) else {
            return;
        };
        begin
            .lifetime
            .store(false, Ordering::SeqCst);
        self.deferred.remove(key);
        self.waiting
            .retain(|waiting| waiting != key);
        if self
            .suspended
            .as_ref()
            .is_some_and(|suspended| suspended.key == *key)
        {
            self.cancel_decrypt();
            self.result = None;
            self.suspended = None;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.key == *key)
        {
            self.active = None;
        }
        self.close_shown(key);
        // The close goes after every earlier call to this callback, earlier lives included.
        // A later begin becomes ready only once the latest close is done.
        let tail = self.tails.remove(key);
        let last = if done {
            Some(
                self.spawn_call(key, tail, "PromptDone", None, None)
                    .1,
            )
        } else {
            tail
        };
        self.next_id += 1;
        let id = self.next_id;
        self.closing.insert(key.clone(), id);
        let done = self.done.clone();
        let owner = key.clone();
        let closer = tokio::spawn(async move {
            if let Some(last) = last {
                let _ = last.await;
            }
            let _ = done.send(Done::Closed { key: owner, id });
        });
        self.tails.insert(key.clone(), closer);
    }

    fn vanished(&mut self, name: &str) {
        let gone: Vec<Key> = self
            .clients
            .keys()
            .filter(|key| &*key.sender == name)
            .cloned()
            .collect();
        if gone.is_empty() {
            return;
        }
        debug!(sender = name, "a keyring client left the bus");
        for key in &gone {
            self.stop(key, false);
        }
        self.next_ready();
    }

    fn next_ready(&mut self) {
        if self.active.is_some() {
            return;
        }
        if self.suspended.is_some() && self.result.is_some() {
            self.active = self.suspended.take();
            match self.result.take() {
                Some(Tried::Password(password)) => {
                    if let Some(active) = &mut self.active {
                        active.open = None;
                    }
                    self.send_ready("yes", Some(&password));
                }
                Some(Tried::Form(warning)) => {
                    if let Some(active) = &mut self.active {
                        warning.clone_into(&mut active.props.warning);
                    }
                    self.show_active(Kind::Password);
                }

                None => {}
            }
            return;
        }
        while let Some(key) = self.waiting.pop_front() {
            let exchange = match Exchange::new() {
                Ok(exchange) => exchange,
                Err(error) => {
                    warn!(%error, "no secret exchange for a keyring client, it is stopped");
                    self.clients.remove(&key);
                    continue;
                }
            };
            self.active = Some(Active {
                key,
                exchange,
                received: false,
                rounds: 0,
                open: None,
                props: Props::default(),
                yubikey: false,
                auto_tried: false,
                call: None,
            });
            self.send_ready("", None);
            return;
        }
        let waiting_shows = matches!(
            (&self.shown, &self.suspended),
            (Some((token, Kind::Yubikey)), Some(suspended))
                if *token.sender == *suspended.key.sender && *token.callback == *suspended.key.callback
        );
        if self.suspended.is_some() && !waiting_shows {
            self.show_suspended();
        }
    }

    fn command(&mut self, command: Command) {
        let Some((token, kind)) = self.shown.clone() else {
            debug!(
                ?command,
                "no keyring prompt shows, the command is dropped"
            );
            return;
        };
        if *command.token() != token {
            debug!(?command, showing = ?token, "a keyring command for another prompt, dropped");
            return;
        }
        let key = Key { sender: Arc::clone(&token.sender), callback: Arc::clone(&token.callback) };
        let is_active = self
            .active
            .as_ref()
            .is_some_and(|active| active.key == key && active.open == Some(kind));
        // Another client, such as pinentry's probe, may hold the slot meanwhile.
        let waits = kind == Kind::Yubikey
            && self
                .suspended
                .as_ref()
                .is_some_and(|suspended| suspended.key == key);
        match command {
            Command::Reply { answer, .. } if is_active => {
                let Some(active) = &mut self.active else {
                    return;
                };
                match (kind, answer) {
                    (Kind::Password, Answer::Password { password, choice }) => {
                        active.props.choice_chosen = choice;
                        active.open = None;
                        self.send_ready("yes", Some(password.as_bytes()));
                    }
                    (Kind::Confirm, Answer::Confirm { choice }) => {
                        active.props.choice_chosen = choice;
                        active.open = None;
                        self.send_ready("yes", None);
                    }
                    _ => debug!("an answer of the wrong kind for the keyring prompt, dropped"),
                }
            }
            Command::Cancel { .. } if is_active => {
                if let Some(active) = &mut self.active {
                    active.open = None;
                }
                self.send_ready("no", None);
            }
            Command::Cancel { .. } if waits => self.cancel_wait(&key),
            Command::UseYubikey { .. }
                if is_active
                    && kind == Kind::Password
                    && self.suspended.is_none()
                    && self
                        .active
                        .as_ref()
                        .is_some_and(|active| active.yubikey) =>
            {
                self.use_yubikey();
            }
            Command::UsePassword { .. } if waits => self.password_from_wait(),
            Command::UsePassword { .. }
                if is_active && kind == Kind::Password && self.suspended.is_some() =>
            {
                self.abandon_yubikey();
            }
            command => debug!(
                ?command,
                "the keyring prompt does not take this now, dropped"
            ),
        }
    }

    /// Frees the slot, because pinentry asks for the card PIN through this same prompter and
    /// would wait behind the suspended prompt for ever.
    fn use_yubikey(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.props.warning.clear();
        self.suspended = Some(active);
        self.start_decrypt();
        self.show_suspended();
        self.next_ready();
    }

    fn start_decrypt(&mut self) {
        let Some(yubikey) = self.config.yubikey.clone() else {
            self.result = Some(Tried::Form(FAILED_WARNING));
            return;
        };
        let Some(settings) = yubikey::settings(&yubikey) else {
            self.result = Some(Tried::Form(FAILED_WARNING));
            return;
        };
        self.next_id += 1;
        let id = self.next_id;
        let done = self.done.clone();
        let task = tokio::spawn(async move {
            let outcome = decrypt(&yubikey, &settings).await;
            let _ = done.send(Done::Decrypt { id, outcome });
        });
        self.decrypt = Some((id, task));
    }

    fn cancel_decrypt(&mut self) {
        if let Some((_, task)) = self.decrypt.take() {
            // Dropping the child kills gpg.
            task.abort();
        }
    }

    fn cancel_wait(&mut self, key: &Key) {
        if self.active.is_none() {
            self.resume(false);
            return;
        }
        // Another client holds the slot. gcr answers a cancelled prompt with `PromptReady("no")`
        // and sends `PromptDone` only on `StopPrompting`, so the request gets `no` now.
        self.cancel_decrypt();
        self.result = None;
        if let Some(mut suspended) = self.suspended.take() {
            suspended.open = None;
            if !self.ready_to(&mut suspended, "no", None) {
                let key = suspended.key.clone();
                self.stop(&key, false);
            }
        }
        self.close_shown(key);
    }

    fn password_from_wait(&mut self) {
        if self.active.is_none() {
            self.resume(true);
            return;
        }
        self.cancel_decrypt();
        self.result = Some(Tried::Form(""));
    }

    fn resume(&mut self, use_password: bool) {
        self.cancel_decrypt();
        self.result = None;
        self.active = self.suspended.take();
        if use_password {
            self.show_active(Kind::Password);
        } else {
            if let Some(active) = &mut self.active {
                active.open = None;
            }
            self.send_ready("no", None);
        }
    }

    fn abandon_yubikey(&mut self) {
        self.cancel_decrypt();
        self.result = Some(Tried::Form(""));
        if let Some(active) = &mut self.active {
            active.open = None;
        }
        self.send_ready("no", None);
    }

    fn done(&mut self, done: Done) {
        match done {
            Done::Begun { key, id } => self.begun(key, id),
            Done::Closed { key, id } => self.closed(key, id),
            Done::Call { id, key, error } => {
                let mine = [self.active.as_mut(), self.suspended.as_mut()]
                    .into_iter()
                    .flatten()
                    .find(|client| client.key == key && client.call == Some(id));
                let Some(client) = mine else {
                    return;
                };
                client.call = None;
                if let Some(error) = error {
                    info!(sender = %key.sender, error, "a keyring client did not take PromptReady, it is stopped");
                    self.stop(&key, false);
                    self.next_ready();
                }
            }
            Done::Decrypt { id, outcome } => {
                if self
                    .decrypt
                    .as_ref()
                    .map(|(current, _)| *current)
                    != Some(id)
                {
                    return;
                }
                self.decrypt = None;
                if self.suspended.is_none() {
                    return;
                }
                self.result = Some(match outcome {
                    Outcome::Password(password) => Tried::Password(password),
                    Outcome::NoCard => Tried::Form(NO_CARD_WARNING),
                    Outcome::Failed => Tried::Form(FAILED_WARNING),
                });
                self.next_ready();
            }
        }
    }

    fn acquired(&mut self) {
        if !self.owned {
            info!(
                name = self.config.bus_name,
                "the keyring prompter owns its name now"
            );
        }
        self.owned = true;
    }

    fn lost(&mut self) {
        warn!(
            name = self.config.bus_name,
            "the keyring prompter lost its name"
        );
        self.owned = false;
        self.end_all();
    }

    fn owner_changed(&mut self, change: &fdo::NameOwnerChanged) {
        let Ok(args) = change.args() else {
            return;
        };
        if args.new_owner().is_none() && args.name().as_str().starts_with(':') {
            self.vanished(args.name().as_str());
        }
    }

    fn end_all(&mut self) {
        self.cancel_decrypt();
        self.result = None;
        for begin in self.clients.values() {
            begin
                .lifetime
                .store(false, Ordering::SeqCst);
        }
        self.clients.clear();
        self.closing.clear();
        self.deferred.clear();
        self.tails.clear();
        self.waiting.clear();
        self.active = None;
        self.suspended = None;
        if let Some((token, _)) = self.shown.take() {
            let _ = self
                .events
                .send(Event::Closed { token });
        }
    }

    fn show_active(&mut self, kind: Kind) {
        let fallback = self.suspended.is_some();
        let Some(active) = &self.active else {
            return;
        };
        *self.serial += 1;
        let prompt = prompt(active, kind, *self.serial, fallback);
        self.publish(prompt);
    }

    fn show_suspended(&mut self) {
        let Some(suspended) = &self.suspended else {
            return;
        };
        *self.serial += 1;
        let prompt = prompt(suspended, Kind::Yubikey, *self.serial, false);
        self.publish(prompt);
    }

    fn publish(&mut self, prompt: Prompt) {
        debug!(
            serial = prompt.token.serial,
            round = prompt.token.round,
            kind = ?prompt.kind,
            warned = !prompt.warning.is_empty(),
            fallback = prompt.password_fallback,
            "a keyring prompt shows"
        );
        self.shown = Some((prompt.token.clone(), prompt.kind));
        let _ = self.events.send(Event::Prompt(prompt));
    }

    fn close_shown(&mut self, key: &Key) {
        let mine = self
            .shown
            .as_ref()
            .is_some_and(|(token, _)| {
                *token.sender == *key.sender && *token.callback == *key.callback
            });
        if mine && let Some((token, _)) = self.shown.take() {
            debug!(serial = token.serial, "the keyring prompt closed");
            let _ = self
                .events
                .send(Event::Closed { token });
        }
    }

    fn send_ready(&mut self, reply: &'static str, secret: Option<&[u8]>) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        let sent = self.ready_to(&mut active, reply, secret);
        let key = active.key.clone();
        self.active = Some(active);
        if !sent {
            self.stop(&key, false);
            self.next_ready();
        }
    }

    fn ready_to(
        &mut self,
        client: &mut Active,
        reply: &'static str,
        secret: Option<&[u8]>,
    ) -> bool {
        let exchange = if client.received {
            client.exchange.send(secret)
        } else {
            Ok(client.exchange.begin())
        };
        let exchange = match exchange {
            Ok(exchange) => exchange,
            Err(error) => {
                warn!(%error, "the keyring exchange failed, the client is stopped");
                return false;
            }
        };
        // gcr's client unwraps each value with g_variant_get_variant, so each is double-wrapped.
        let strength = i32::from(secret.is_some_and(|secret| !secret.is_empty()));
        let props: HashMap<&'static str, Variant<'static>> = HashMap::from([
            (
                "choice-chosen",
                Variant::new(Variant::new(client.props.choice_chosen)),
            ),
            (
                "password-strength",
                Variant::new(Variant::new(strength)),
            ),
        ]);
        let tail = self.tails.remove(&client.key);
        let lifetime = self
            .clients
            .get(&client.key)
            .map(|begin| Arc::clone(&begin.lifetime));
        let (id, task) = self.spawn_call(
            &client.key,
            tail,
            "PromptReady",
            Some((reply, props, exchange)),
            lifetime,
        );
        self.tails
            .insert(client.key.clone(), task);
        client.call = Some(id);
        true
    }

    fn spawn_call(
        &mut self,
        key: &Key,
        previous: Option<JoinHandle<()>>,
        method: &'static str,
        ready: Option<ReadyBody>,
        lifetime: Option<Arc<AtomicBool>>,
    ) -> (u64, JoinHandle<()>) {
        self.next_id += 1;
        let id = self.next_id;
        let conn = self.conn.clone();
        let done = self.done.clone();
        let limit = self.config.call_timeout;
        let target = key.clone();
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            if lifetime.is_some_and(|lifetime| !lifetime.load(Ordering::SeqCst)) {
                debug!(
                    method,
                    "the keyring client stopped, the queued call is dropped"
                );
                return;
            }
            let call = async {
                let proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&conn)
                    .destination(target.sender.as_ref())
                    .path(target.callback.as_ref())
                    .interface(CALLBACK_INTERFACE)
                    .cache_properties(CacheProperties::No)
                    .build()
                    .await?;
                match &ready {
                    Some(body) => {
                        proxy
                            .call_with_flags::<_, _, ()>(method, MethodFlags::NO_AUTO_START, body)
                            .await
                    }
                    None => {
                        proxy
                            .call_with_flags::<_, _, ()>(method, MethodFlags::NO_AUTO_START, &())
                            .await
                    }
                }
            };
            let error = match tokio::time::timeout(limit, call).await {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some(String::from("timed out")),
            };
            let _ = done.send(Done::Call { id, key: target, error });
        });
        (id, task)
    }
}

type ReadyBody = (
    &'static str,
    HashMap<&'static str, Variant<'static>>,
    String,
);

fn not_serving() -> fdo::Error {
    fdo::Error::Failed("This prompter does not own its name.".to_owned())
}

fn prompt(client: &Active, kind: Kind, serial: u64, password_fallback: bool) -> Prompt {
    let props = &client.props;
    Prompt {
        token: Token {
            sender: Arc::clone(&client.key.sender),
            callback: Arc::clone(&client.key.callback),
            round: client.rounds,
            serial,
        },
        kind,
        title: props.title.clone(),
        message: props.message.clone(),
        description: props.description.clone(),
        warning: props.warning.clone(),
        password_new: props.password_new,
        choice_label: strip_mnemonic(&props.choice_label),
        choice_chosen: props.choice_chosen,
        continue_label: strip_mnemonic(&props.continue_label),
        cancel_label: strip_mnemonic(&props.cancel_label),
        yubikey_available: client.yubikey,
        password_fallback,
    }
}

async fn decrypt(yubikey: &YubiKey, settings: &crate::Settings) -> Outcome {
    yubikey::decrypt(yubikey, settings).await
}
