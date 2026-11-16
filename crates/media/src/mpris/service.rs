//! The MPRIS service task: follows player names, reads properties and runs commands.

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::names::BusName;
use zbus::{Connection, MatchRule, Message, MessageStream, ObjectPath, OwnedVariant, Variant};

use super::model::{Command, PlaybackStatus, Player, Players};
use super::props::{self, Applied, PLAYER_IFACE, PROP_COUNT, Prop, ROOT_IFACE};

const NAME_PREFIX: &str = "org.mpris.MediaPlayer2.";
const NAME_NAMESPACE: &str = "org.mpris.MediaPlayer2";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
/// How long a call to a player may take. A hung player only delays its own updates.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum MprisError {
    #[error("D-Bus: {0}")]
    DBus(#[from] zbus::Error),
    #[error("D-Bus: {0}")]
    Fdo(#[from] zbus::fdo::Error),
}

pub struct MprisService {
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl MprisService {
    pub async fn start(conn: &Connection) -> Result<(Self, watch::Receiver<Players>), MprisError> {
        let owners = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")
            .interface("org.freedesktop.DBus")
            .member("NameOwnerChanged")
            .arg0ns(NAME_NAMESPACE)
            .build()?;
        let changes = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(PROPERTIES_IFACE)
            .member("PropertiesChanged")
            .path(OBJECT_PATH)
            .arg0ns(NAME_NAMESPACE)
            .build()?;
        let seeks = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(PLAYER_IFACE)
            .member("Seeked")
            .path(OBJECT_PATH)
            .build()?;
        // Subscribe before listing, so a player that appears in between is not missed.
        let streams = Streams {
            owners: MessageStream::for_match_rule(owners, conn, None).await?,
            changes: MessageStream::for_match_rule(changes, conn, None).await?,
            seeks: MessageStream::for_match_rule(seeks, conn, None).await?,
        };

        let bus = zbus::fdo::DBusProxy::new(conn).await?;
        let mut existing = Vec::new();
        for name in bus.list_names().await? {
            if !name.as_str().starts_with(NAME_PREFIX) {
                continue;
            }
            match bus
                .get_name_owner(BusName::from(name.clone()))
                .await
            {
                Ok(owner) => existing.push((name.to_string(), owner.to_string())),
                Err(error) => debug!(%error, %name, "the player left while listing"),
            }
        }

        let (state, state_rx) = watch::channel(Players::default());
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (fetched, fetched_rx) = mpsc::unbounded_channel();
        let mut task = Task {
            conn: conn.clone(),
            state,
            entries: Vec::new(),
            selected: None,
            fetched,
            next_generation: 0,
        };
        for (name, owner) in existing {
            task.add(name, owner);
        }
        let task = tokio::spawn(task.run(streams, commands_rx, fetched_rx));
        Ok((Self { commands, task }, state_rx))
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }
}

impl Drop for MprisService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Streams {
    owners: MessageStream,
    changes: MessageStream,
    seeks: MessageStream,
}

async fn next(stream: &mut MessageStream) -> Option<Message> {
    loop {
        match poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await? {
            Ok(message) => return Some(message),
            Err(error) => debug!(%error, "skipping a bad D-Bus message"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Fetch {
    RootAll,
    PlayerAll,
    One(Prop),
}

struct Fetched {
    bus_name: String,
    generation: u64,
    fetch: Fetch,
    snapshot: [u32; PROP_COUNT],
    result: Result<HashMap<String, OwnedVariant>, zbus::Error>,
}

struct Entry {
    bus_name: String,
    owner: String,
    // Tells this entry apart from an earlier one with the same name, so a late reply is dropped.
    generation: u64,
    // Bumped whenever a signal sets the property, so an older reply cannot overwrite it.
    versions: [u32; PROP_COUNT],
    pending: Option<Box<Player>>,
    root_done: bool,
    player_ok: Option<bool>,
    // Some players fill in `Metadata` late, so it is read again on the first status change
    // after a track change.
    track_changed_before_state: bool,
}

struct Task {
    conn: Connection,
    state: watch::Sender<Players>,
    entries: Vec<Entry>,
    selected: Option<String>,
    fetched: mpsc::UnboundedSender<Fetched>,
    next_generation: u64,
}

impl Task {
    async fn run(
        mut self,
        mut streams: Streams,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut fetched: mpsc::UnboundedReceiver<Fetched>,
    ) {
        loop {
            tokio::select! {
                message = next(&mut streams.owners) => match message {
                    Some(message) => self.on_owner_changed(&message),
                    None => break,
                },
                message = next(&mut streams.changes) => match message {
                    Some(message) => self.on_properties_changed(&message),
                    None => break,
                },
                message = next(&mut streams.seeks) => match message {
                    Some(message) => self.on_seeked(&message),
                    None => break,
                },
                Some(reply) = fetched.recv() => self.on_fetched(&reply),
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
            }
        }
        self.state
            .send_modify(|players| *players = Players::default());
    }

    fn add(&mut self, bus_name: String, owner: String) {
        if self
            .entries
            .iter()
            .any(|e| e.bus_name == bus_name && e.owner == owner)
        {
            return;
        }
        self.remove(&bus_name);
        let generation = self.next_generation;
        self.next_generation += 1;
        let player = Box::new(Player::new(bus_name.clone()));
        self.entries.push(Entry {
            bus_name,
            owner,
            generation,
            versions: [0; PROP_COUNT],
            pending: Some(player),
            root_done: false,
            player_ok: None,
            track_changed_before_state: false,
        });
        let index = self.entries.len() - 1;
        self.fetch(index, Fetch::RootAll);
        self.fetch(index, Fetch::PlayerAll);
    }

    fn remove(&mut self, bus_name: &str) {
        let Some(i) = self
            .entries
            .iter()
            .position(|e| e.bus_name == bus_name)
        else {
            return;
        };
        let entry = self.entries.swap_remove(i);
        if entry.pending.is_none() {
            let selected = &mut self.selected;
            self.state.send_modify(|players| {
                players
                    .list
                    .retain(|p| p.bus_name != bus_name);
                reconcile(players, selected);
                derive(players, selected.as_deref());
            });
        }
    }

    fn fetch(&self, index: usize, fetch: Fetch) {
        let entry = &self.entries[index];
        let conn = self.conn.clone();
        let tx = self.fetched.clone();
        let bus_name = entry.bus_name.clone();
        let generation = entry.generation;
        let snapshot = entry.versions;
        tokio::spawn(async move {
            let result = match fetch {
                Fetch::RootAll => get_all(&conn, &bus_name, ROOT_IFACE).await,
                Fetch::PlayerAll => get_all(&conn, &bus_name, PLAYER_IFACE).await,
                Fetch::One(prop) => get_one(&conn, &bus_name, prop).await,
            };
            let _ = tx.send(Fetched { bus_name, generation, fetch, snapshot, result });
        });
    }

    fn call(&self, bus_name: &str, method: &'static str, args: Call) {
        let conn = self.conn.clone();
        let bus_name = bus_name.to_owned();
        tokio::spawn(async move {
            let call = async {
                match &args {
                    Call::None => {
                        conn.call_method(
                            Some(bus_name.as_str()),
                            OBJECT_PATH,
                            Some(PLAYER_IFACE),
                            method,
                            &(),
                        )
                        .await
                    }
                    Call::Seek(offset) => {
                        conn.call_method(
                            Some(bus_name.as_str()),
                            OBJECT_PATH,
                            Some(PLAYER_IFACE),
                            method,
                            &(*offset,),
                        )
                        .await
                    }
                    Call::SetPosition(track, position) => {
                        conn.call_method(
                            Some(bus_name.as_str()),
                            OBJECT_PATH,
                            Some(PLAYER_IFACE),
                            method,
                            &(track, *position),
                        )
                        .await
                    }
                }
            };
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => debug!(%error, %bus_name, method, "player call failed"),
                Err(_) => debug!(%bus_name, method, "player call timed out"),
            }
        });
    }

    fn with_player<R>(&mut self, index: usize, f: impl FnOnce(&mut Player) -> R) -> Option<R> {
        let entry = &mut self.entries[index];
        if let Some(player) = entry.pending.as_deref_mut() {
            return Some(f(player));
        }
        let bus_name = entry.bus_name.as_str();
        let selected = self.selected.as_deref();
        let mut result = None;
        self.state.send_if_modified(|players| {
            let Some(player) = players
                .list
                .iter_mut()
                .find(|p| p.bus_name == bus_name)
            else {
                return false;
            };
            result = Some(f(player));
            derive(players, selected);
            true
        });
        result
    }

    fn on_owner_changed(&mut self, message: &Message) {
        let body = message.body();
        let Ok((name, old, new)) = body.deserialize::<(&str, &str, &str)>() else {
            return;
        };
        if !name.starts_with(NAME_PREFIX) {
            return;
        }
        if !old.is_empty()
            && self
                .entries
                .iter()
                .any(|e| e.bus_name == name && e.owner == old)
        {
            self.remove(name);
        }
        if !new.is_empty() {
            self.add(name.to_owned(), new.to_owned());
        }
    }

    fn owned_by(&self, message: &Message) -> Vec<usize> {
        let header = message.header();
        let Some(sender) = header.sender() else {
            return Vec::new();
        };
        let sender = sender.as_str();
        (0..self.entries.len())
            .filter(|&i| self.entries[i].owner == sender)
            .collect()
    }

    fn on_properties_changed(&mut self, message: &Message) {
        let indices = self.owned_by(message);
        if indices.is_empty() {
            return;
        }
        let body = message.body();
        type Changes<'a> = (
            &'a str,
            HashMap<&'a str, Variant<'a>>,
            Vec<&'a str>,
        );
        let Ok((iface, changed, invalidated)) = body.deserialize::<Changes<'_>>() else {
            return;
        };
        for index in indices {
            let now = Instant::now();
            let mut applied = Applied::default();
            for (prop, value) in in_order(iface, &changed) {
                self.entries[index].versions[prop as usize] += 1;
                let result = self.with_player(index, |player| apply_at(player, prop, value, now));
                if let Some(Some(a)) = result {
                    applied.status_changed |= a.status_changed;
                    applied.track_changed |= a.track_changed;
                }
            }
            for name in &invalidated {
                let Some(prop) = Prop::lookup(iface, name) else { continue };
                self.entries[index].versions[prop as usize] += 1;
                self.fetch(index, Fetch::One(prop));
            }
            self.follow_up(index, applied);
        }
    }

    fn follow_up(&mut self, index: usize, applied: Applied) {
        if applied.track_changed {
            self.entries[index].track_changed_before_state = true;
            self.fetch(index, Fetch::One(Prop::Position));
        }
        if applied.status_changed {
            self.fetch(index, Fetch::One(Prop::Position));
            if std::mem::take(&mut self.entries[index].track_changed_before_state) {
                self.fetch(index, Fetch::One(Prop::Metadata));
            }
        }
    }

    fn on_seeked(&mut self, message: &Message) {
        let indices = self.owned_by(message);
        let body = message.body();
        let Ok((position,)) = body.deserialize::<(i64,)>() else {
            return;
        };
        let now = Instant::now();
        for index in indices {
            self.entries[index].versions[Prop::Position as usize] += 1;
            self.with_player(index, |player| {
                player.position.micros = position;
                player.position.read_at = now;
            });
        }
    }

    fn on_fetched(&mut self, reply: &Fetched) {
        let Some(index) = self
            .entries
            .iter()
            .position(|e| e.bus_name == reply.bus_name && e.generation == reply.generation)
        else {
            return;
        };
        let iface = match reply.fetch {
            Fetch::RootAll => ROOT_IFACE,
            Fetch::PlayerAll => PLAYER_IFACE,
            Fetch::One(prop) => prop.iface(),
        };
        let mut applied = Applied::default();
        match &reply.result {
            Ok(values) => {
                let now = Instant::now();
                let versions = self.entries[index].versions;
                for (prop, value) in in_order(iface, values) {
                    if versions[prop as usize] != reply.snapshot[prop as usize] {
                        continue;
                    }
                    let result =
                        self.with_player(index, |player| apply_at(player, prop, value, now));
                    if let Some(Some(a)) = result {
                        applied.status_changed |= a.status_changed;
                        applied.track_changed |= a.track_changed;
                    }
                }
            }
            Err(error) => debug!(%error, bus_name = reply.bus_name, ?reply.fetch, "read failed"),
        }

        let entry = &mut self.entries[index];
        match reply.fetch {
            Fetch::RootAll => entry.root_done = true,
            Fetch::PlayerAll => entry.player_ok = Some(reply.result.is_ok()),
            Fetch::One(_) => self.follow_up(index, applied),
        }
        self.publish_if_ready(index);
    }

    fn publish_if_ready(&mut self, index: usize) {
        let entry = &mut self.entries[index];
        if !entry.root_done || entry.player_ok != Some(true) {
            return;
        }
        let Some(player) = entry.pending.take() else { return };
        let selected = &mut self.selected;
        self.state.send_modify(|players| {
            players.list.push(*player);
            reconcile(players, selected);
            derive(players, selected.as_deref());
        });
    }

    fn on_command(&mut self, command: Command) {
        let state = self.state.borrow();
        let active = state
            .active()
            .map(|p| p.bus_name.as_str());
        match &command {
            Command::PlayPause => {
                if let Some(player) = state.active()
                    && player.can_toggle_playing()
                {
                    let method = if player.is_playing() { "Pause" } else { "Play" };
                    self.call(&player.bus_name, method, Call::None);
                }
            }
            Command::Pause => {
                if let Some(player) = state.active().filter(|p| p.can_pause()) {
                    self.call(&player.bus_name, "Pause", Call::None);
                }
            }
            Command::Next => {
                if let Some(player) = state
                    .active()
                    .filter(|p| p.can_go_next())
                {
                    self.call(&player.bus_name, "Next", Call::None);
                }
            }
            Command::Previous => {
                if let Some(player) = state
                    .active()
                    .filter(|p| p.can_go_previous())
                {
                    self.call(&player.bus_name, "Previous", Call::None);
                }
            }
            Command::TogglePlaying(bus_name) => {
                if let Some(player) = state
                    .get(bus_name)
                    .filter(|p| p.can_toggle_playing())
                {
                    let method = if player.is_playing() { "Pause" } else { "Play" };
                    self.call(&player.bus_name, method, Call::None);
                }
            }
            Command::Select(_) | Command::SetPosition(..) | Command::RefreshPosition => {}
        }
        let refresh =
            matches!(command, Command::RefreshPosition).then(|| active.map(str::to_owned));
        drop(state);

        match command {
            Command::Select(bus_name) => self.select(bus_name),
            Command::SetPosition(bus_name, position) => self.set_position(&bus_name, position),
            Command::RefreshPosition => {
                if let Some(index) = refresh.flatten().and_then(|name| {
                    self.entries
                        .iter()
                        .position(|e| e.bus_name == name)
                }) {
                    self.fetch(index, Fetch::One(Prop::Position));
                }
            }
            _ => {}
        }
    }

    fn select(&mut self, bus_name: String) {
        let selected = &mut self.selected;
        self.state.send_if_modified(|players| {
            if players.get(&bus_name).is_none() {
                return false;
            }
            *selected = Some(bus_name);
            let before = players.active;
            derive(players, selected.as_deref());
            players.active != before
        });
    }

    fn set_position(&mut self, bus_name: &str, position: Duration) {
        let Some(index) = self
            .entries
            .iter()
            .position(|e| e.bus_name == bus_name)
        else {
            return;
        };
        let now = Instant::now();
        let target = i64::try_from(position.as_millis()).unwrap_or(i64::MAX / 1000) * 1000;
        let request = self.with_player(index, |player| {
            if !player.position.supported || !player.can_seek() {
                return None;
            }
            let call = match ObjectPath::try_from(player.track.id.as_str()) {
                Ok(path) if !player.track.id.is_empty() => {
                    Call::SetPosition(path.to_owned(), target)
                }
                _ => {
                    let current = i64::try_from(player.position_at(now).as_micros()).unwrap_or(0);
                    Call::Seek(target - current)
                }
            };
            player.position.micros = target;
            player.position.read_at = now;
            Some(call)
        });
        if let Some(Some(call)) = request {
            self.entries[index].versions[Prop::Position as usize] += 1;
            let method = if matches!(call, Call::Seek(_)) { "Seek" } else { "SetPosition" };
            self.call(bus_name, method, call);
        }
    }
}

enum Call {
    None,
    Seek(i64),
    SetPosition(ObjectPath<'static>, i64),
}

// A status or rate change first carries the position forward under the old values, so the
// estimate does not jump before the next read.
fn apply_at(player: &mut Player, prop: Prop, value: &Variant<'_>, now: Instant) -> Option<Applied> {
    let was_stopped = player.status == PlaybackStatus::Stopped;
    let before = player.position_at(now);
    let applied = props::apply(player, prop, value)?;
    match prop {
        Prop::PlaybackStatus | Prop::Rate if player.position.supported => {
            if !was_stopped {
                player.position.micros = i64::try_from(before.as_micros()).unwrap_or(i64::MAX);
            }
            player.position.read_at = now;
        }
        Prop::Position => player.position.read_at = now,
        _ => {}
    }
    Some(applied)
}

// `Position` goes last, so a status or rate change in the same batch carries the old position
// forward first and the new position then replaces it.
fn in_order<'a, K: AsRef<str>, V>(
    iface: &'a str,
    values: &'a HashMap<K, V>,
) -> impl Iterator<Item = (Prop, &'a V)> + 'a {
    let known = move |(name, value): (&'a K, &'a V)| {
        Prop::lookup(iface, name.as_ref()).map(|prop| (prop, value))
    };
    let others = values
        .iter()
        .filter_map(known)
        .filter(|(p, _)| *p != Prop::Position);
    let position = values
        .iter()
        .filter_map(known)
        .filter(|(p, _)| *p == Prop::Position);
    others.chain(position)
}

fn reconcile(players: &Players, selected: &mut Option<String>) {
    if selected
        .as_deref()
        .is_some_and(|name| players.get(name).is_some())
    {
        return;
    }
    *selected = players
        .list
        .iter()
        .find(|p| p.is_playing())
        .or(players.list.first())
        .map(|p| p.bus_name.clone());
}

fn derive(players: &mut Players, selected: Option<&str>) {
    players.playing_count = players
        .list
        .iter()
        .filter(|p| p.is_playing())
        .count();
    players.active = selected
        .and_then(|name| {
            players
                .list
                .iter()
                .position(|p| p.bus_name == name)
        })
        .or_else(|| {
            players
                .list
                .iter()
                .position(Player::is_playing)
        })
        .or(if players.list.is_empty() { None } else { Some(0) });
}

async fn get_all(
    conn: &Connection,
    bus_name: &str,
    iface: &str,
) -> Result<HashMap<String, OwnedVariant>, zbus::Error> {
    let args = (iface,);
    let call = conn.call_method(
        Some(bus_name),
        OBJECT_PATH,
        Some(PROPERTIES_IFACE),
        "GetAll",
        &args,
    );
    let reply = timed(call).await?;
    reply
        .body()
        .deserialize::<HashMap<String, OwnedVariant>>()
}

async fn get_one(
    conn: &Connection,
    bus_name: &str,
    prop: Prop,
) -> Result<HashMap<String, OwnedVariant>, zbus::Error> {
    let args = (prop.iface(), prop.name());
    let call = conn.call_method(
        Some(bus_name),
        OBJECT_PATH,
        Some(PROPERTIES_IFACE),
        "Get",
        &args,
    );
    let reply = timed(call).await?;
    let value = reply
        .body()
        .deserialize::<OwnedVariant>()?;
    Ok(HashMap::from([(prop.name().to_owned(), value)]))
}

async fn timed(
    call: impl Future<Output = Result<Message, zbus::Error>>,
) -> Result<Message, zbus::Error> {
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .unwrap_or_else(|_| {
            warn!("an MPRIS player did not answer in time");
            Err(zbus::Error::InputOutput(
                std::io::Error::from(std::io::ErrorKind::TimedOut).into(),
            ))
        })
}
