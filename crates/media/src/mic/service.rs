//! The mic service task and its pulse connection.

use std::path::PathBuf;
use std::time::Duration;

use pulseaudio::protocol::{
    Command as Pulse, GetSourceInfo, SubscriptionEvent, SubscriptionEventFacility,
    SubscriptionEventType, SubscriptionMask,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::hold::Holds;
use super::model::{Mic, MicUser};
use super::rules::{self, Client, Output, Readers, Source};
use crate::pulse::{Conn, Event, Message, PulseError, device, info};

#[derive(Debug, Clone)]
pub struct MicConfig {
    pub socket: PathBuf,
    pub client_name: String,
    pub hold: Duration,
    pub own_pid: u32,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl MicConfig {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            client_name: "deskshell".to_owned(),
            hold: Duration::from_secs(1),
            own_pid: std::process::id(),
            reconnect_min: Duration::from_millis(200),
            reconnect_max: Duration::from_secs(5),
        }
    }
}

pub struct MicService {
    task: JoinHandle<()>,
}

impl MicService {
    pub fn start(config: MicConfig) -> (Self, watch::Receiver<Mic>) {
        let (state, state_rx) = watch::channel(Mic::default());
        let task = tokio::spawn(run(config, state));
        (Self { task }, state_rx)
    }
}

impl Drop for MicService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(config: MicConfig, state: watch::Sender<Mic>) {
    let mut backoff = config.reconnect_min;
    loop {
        match Conn::connect(&config.socket, &config.client_name).await {
            Ok(conn) => {
                info!(
                    version = conn.version(),
                    "mic: connected to the pulse server"
                );
                backoff = config.reconnect_min;
                let mut session = Session::new(conn, &config);
                match session.run(&state).await {
                    Ok(()) => return,
                    Err(error) => warn!(%error, "mic: lost the pulse server"),
                }
            }
            Err(error) => debug!(%error, "mic: cannot connect to the pulse server"),
        }
        state.send_if_modified(|mic| {
            let was = !mic.users.is_empty();
            mic.users.clear();
            was
        });
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = state.closed() => return,
        }
        backoff = (backoff * 2).min(config.reconnect_max);
    }
}

#[derive(Debug, Clone, Copy)]
enum Pending {
    Ack,
    Sources,
    Outputs,
    Clients,
    Source(u32),
    Output(u32),
    Client(u32),
}

struct Session {
    conn: Conn,
    model: Model,
}

struct Model {
    pending: Vec<(u32, Pending)>,
    sources: Vec<Source>,
    outputs: Vec<Output>,
    clients: Vec<Client>,
    own_pid: u32,
    holds: Holds,
    kinds: Vec<(u32, Readers)>,
    users: Vec<MicUser>,
    wake: Option<Instant>,
    retry: Retry,
    retry_at: Option<Instant>,
    retry_after: Duration,
}

#[derive(Debug, Default, Clone, Copy)]
struct Retry {
    sources: bool,
    outputs: bool,
    clients: bool,
}

impl Session {
    fn new(conn: Conn, config: &MicConfig) -> Self {
        let model = Model {
            pending: Vec::new(),
            sources: Vec::new(),
            outputs: Vec::new(),
            clients: Vec::new(),
            own_pid: config.own_pid,
            holds: Holds::new(config.hold),
            kinds: Vec::new(),
            users: Vec::new(),
            wake: None,
            retry: Retry::default(),
            retry_at: None,
            retry_after: config.reconnect_max,
        };
        Self { conn, model }
    }

    async fn request(&mut self, command: &Pulse, pending: Pending) -> Result<(), PulseError> {
        let seq = self.conn.send(command).await?;
        self.model.pending.push((seq, pending));
        Ok(())
    }

    async fn run(&mut self, state: &watch::Sender<Mic>) -> Result<(), PulseError> {
        let mask =
            SubscriptionMask::SOURCE | SubscriptionMask::SOURCE_OUTPUT | SubscriptionMask::CLIENT;
        self.request(&Pulse::Subscribe(mask), Pending::Ack)
            .await?;
        // Outputs last, so their reply finds the sources and clients they name.
        self.request(&Pulse::GetClientInfoList, Pending::Clients)
            .await?;
        self.request(&Pulse::GetSourceInfoList, Pending::Sources)
            .await?;
        self.request(&Pulse::GetSourceOutputInfoList, Pending::Outputs)
            .await?;

        let mut dirty = false;
        loop {
            let version = self.conn.version();
            let wake = self.model.wake;
            let retry_at = self.model.retry_at;
            tokio::select! {
                biased;
                message = self.conn.next() => {
                    dirty = true;
                    let mut event = None;
                    let mut relist = false;
                    let mut changed = false;
                    match message? {
                        Message::Reply { seq, body } => {
                            if let Some(pending) = take_pending(&mut self.model.pending, seq) {
                                relist = self.model.on_reply(pending, body, version);
                                changed = !matches!(pending, Pending::Ack);
                            }
                        }
                        Message::Error { seq, code } => {
                            if let Some(pending) = take_pending(&mut self.model.pending, seq) {
                                debug!(?pending, code, "mic: pulse request failed");
                                self.model.on_error(pending);
                                changed = true;
                            }
                        }
                        Message::Event(Event::Subscription(e)) => {
                            changed = e.event_type == SubscriptionEventType::Removed;
                            event = Some(e);
                        }
                        Message::Event(_) | Message::Data { .. } | Message::Unknown => {}
                    }
                    if relist {
                        self.request(&Pulse::GetSourceOutputInfoList, Pending::Outputs)
                            .await?;
                    }
                    if let Some(event) = event {
                        self.on_event(event).await?;
                    }
                    if changed {
                        self.model.note_readers();
                    }
                }
                () = state.closed() => return Ok(()),
                () = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)),
                    if wake.is_some() => {
                    self.model.wake = None;
                    dirty = true;
                }
                () = tokio::time::sleep_until(retry_at.unwrap_or_else(Instant::now)),
                    if retry_at.is_some() => self.retry().await?,
                () = std::future::ready(()), if dirty => {
                    dirty = false;
                    self.model.publish(state);
                }
            }
        }
    }

    async fn retry(&mut self) -> Result<(), PulseError> {
        let retry = std::mem::take(&mut self.model.retry);
        self.model.retry_at = None;
        if retry.clients {
            self.request(&Pulse::GetClientInfoList, Pending::Clients)
                .await?;
        }
        if retry.sources {
            self.request(&Pulse::GetSourceInfoList, Pending::Sources)
                .await?;
        }
        if retry.outputs {
            self.request(&Pulse::GetSourceOutputInfoList, Pending::Outputs)
                .await?;
        }
        Ok(())
    }

    async fn on_event(&mut self, event: SubscriptionEvent) -> Result<(), PulseError> {
        use SubscriptionEventFacility as Facility;
        let Some(index) = event.index else {
            return Ok(());
        };
        let removed = event.event_type == SubscriptionEventType::Removed;
        match event.event_facility {
            Facility::Source if removed => remove(&mut self.model.sources, index),
            Facility::SourceOutput if removed => remove(&mut self.model.outputs, index),
            Facility::Client if removed => remove(&mut self.model.clients, index),
            Facility::Source => {
                let query = GetSourceInfo { index: Some(index), name: None };
                self.request(
                    &Pulse::GetSourceInfo(query),
                    Pending::Source(index),
                )
                .await?;
            }
            Facility::SourceOutput => {
                self.request(
                    &Pulse::GetSourceOutputInfo(index),
                    Pending::Output(index),
                )
                .await?;
            }
            Facility::Client => {
                self.request(
                    &Pulse::GetClientInfo(index),
                    Pending::Client(index),
                )
                .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl Model {
    // pipewire-pulse sends no source output event for a new link, so a source state change
    // lists the outputs again to refresh a stale source index.
    fn on_reply(&mut self, pending: Pending, body: &[u8], version: u16) -> bool {
        match pending {
            Pending::Ack => {}
            Pending::Sources => {
                let (list, error) = device::read_full_list(body, version);
                self.sources = list.iter().map(Source::from).collect();
                if let Some(error) = error {
                    warn!(%error, "mic: cannot read all pulse sources");
                    self.retry_later(|r| r.sources = true);
                }
            }
            Pending::Outputs => {
                let (list, error) = info::read_outputs(body, version);
                self.outputs = list
                    .into_iter()
                    .map(Output::from)
                    .collect();
                if let Some(error) = error {
                    warn!(%error, "mic: cannot read all pulse source outputs");
                    self.retry_later(|r| r.outputs = true);
                }
            }
            Pending::Clients => {
                let (list, error) = info::read_clients(body, version);
                self.clients = list
                    .into_iter()
                    .map(Client::from)
                    .collect();
                if let Some(error) = error {
                    warn!(%error, "mic: cannot read all pulse clients");
                    self.retry_later(|r| r.clients = true);
                }
            }
            Pending::Source(_) => match device::read_full_one(body, version) {
                Ok(full) => {
                    let source = Source::from(&full);
                    let was = self
                        .sources
                        .iter()
                        .find(|s| s.index == source.index)
                        .map(|s| s.running);
                    let relist = was != Some(source.running);
                    upsert(&mut self.sources, source);
                    return relist;
                }
                Err(error) => {
                    warn!(%error, "mic: cannot read a pulse source");
                    self.retry_later(|r| r.sources = true);
                }
            },
            Pending::Output(_) => match info::read_output(body, version) {
                Ok(info) => upsert(&mut self.outputs, Output::from(info)),
                Err(error) => {
                    warn!(%error, "mic: cannot read a pulse source output");
                    self.retry_later(|r| r.outputs = true);
                }
            },
            Pending::Client(_) => match info::read_client(body, version) {
                Ok(info) => upsert(&mut self.clients, Client::from(info)),
                Err(error) => {
                    warn!(%error, "mic: cannot read a pulse client");
                    self.retry_later(|r| r.clients = true);
                }
            },
        }
        false
    }

    fn retry_later(&mut self, mark: impl FnOnce(&mut Retry)) {
        mark(&mut self.retry);
        self.retry_at
            .get_or_insert_with(|| Instant::now() + self.retry_after);
    }

    // pipewire-pulse leaves a stream out of every reply until it has a format, so "no such
    // entity" for a fresh index is normal. The next change event brings the entry back.
    fn on_error(&mut self, pending: Pending) {
        match pending {
            Pending::Output(index) => remove(&mut self.outputs, index),
            Pending::Client(index) => remove(&mut self.clients, index),
            Pending::Source(index) => remove(&mut self.sources, index),
            Pending::Ack | Pending::Sources | Pending::Outputs | Pending::Clients => {}
        }
    }

    // Runs after every change, not once per batch, so a meter that leaves in the same batch as
    // an app is still seen as the last reader.
    fn note_readers(&mut self) {
        rules::reader_kinds(
            &self.sources,
            &self.outputs,
            &self.clients,
            self.own_pid,
            &mut self.kinds,
        );
        self.holds
            .note(Instant::now(), &self.kinds);
    }

    fn publish(&mut self, state: &watch::Sender<Mic>) {
        let live = rules::recordings(
            &self.sources,
            &self.outputs,
            &self.clients,
            self.own_pid,
        );
        self.wake = self
            .holds
            .update(Instant::now(), &live, &mut self.users);
        let users = &self.users;
        state.send_if_modified(|mic| {
            if mic.users == *users {
                return false;
            }
            mic.users.clone_from(users);
            true
        });
    }
}

trait Indexed {
    fn index(&self) -> u32;
}

impl Indexed for Source {
    fn index(&self) -> u32 {
        self.index
    }
}

impl Indexed for Output {
    fn index(&self) -> u32 {
        self.index
    }
}

impl Indexed for Client {
    fn index(&self) -> u32 {
        self.index
    }
}

fn upsert<T: Indexed>(list: &mut Vec<T>, item: T) {
    match list
        .iter_mut()
        .find(|old| old.index() == item.index())
    {
        Some(old) => *old = item,
        None => list.push(item),
    }
}

fn remove<T: Indexed>(list: &mut Vec<T>, index: u32) {
    list.retain(|item| item.index() != index);
}

fn take_pending(pending: &mut Vec<(u32, Pending)>, seq: u32) -> Option<Pending> {
    let i = pending
        .iter()
        .position(|(s, _)| *s == seq)?;
    Some(pending.remove(i).1)
}
