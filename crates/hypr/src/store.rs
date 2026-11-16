//! Mirrors Hyprland's state from a snapshot plus the event stream.

use std::future::Future;
use std::ops::{BitOr, BitOrAssign};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until};
use tracing::{debug, warn};

use crate::model::ActiveWindow;
use crate::{Address, Client, Devices, Error, Event, Monitor, Window, Workspace, parse_line};

const DEBOUNCE: Duration = Duration::from_millis(30);
const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
const NOTICE_CAPACITY: usize = 64;

const SNAPSHOT: [&str; 6] =
    ["j/monitors", "j/workspaces", "j/clients", "j/activeworkspace", "j/activewindow", "j/devices"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub connected: bool,
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    /// `None` for named and special workspaces, which have no numeric id in Hyprland 0.57.
    pub active_workspace: Option<i64>,
    pub focused_monitor: Option<String>,
    pub active_window: Option<Address>,
    pub keyboard_layout: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Urgent(Address),
    OpenLayer(String),
    CloseLayer(String),
    Custom(String),
    ConfigReloaded,
}

/// Dropping it stops the task.
pub struct Store {
    refresh: Arc<Notify>,
    refresh_monitors: Arc<Notify>,
    notices: broadcast::Sender<Notice>,
    task: Option<JoinHandle<()>>,
}

impl Store {
    pub fn spawn(
        client: Client,
    ) -> (
        Self,
        watch::Receiver<State>,
        broadcast::Receiver<Notice>,
    ) {
        let (state_tx, state_rx) = watch::channel(State::default());
        let (notice_tx, notice_rx) = broadcast::channel(NOTICE_CAPACITY);
        let refresh = Arc::new(Notify::new());
        let refresh_monitors = Arc::new(Notify::new());
        let task = tokio::spawn(run(
            client,
            state_tx,
            notice_tx.clone(),
            Arc::clone(&refresh),
            Arc::clone(&refresh_monitors),
        ));
        (
            Self { refresh, refresh_monitors, notices: notice_tx, task: Some(task) },
            state_rx,
            notice_rx,
        )
    }

    /// Hyprland sends no event for swaps, resizes and drag ends.
    pub fn refresh(&self) {
        self.refresh.notify_one();
    }

    /// Hyprland sends no event when a script changes a monitor's color management preset.
    pub fn refresh_monitors(&self) {
        self.refresh_monitors.notify_one();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Dirty(u8);

impl Dirty {
    const NONE: Self = Self(0);
    const WINDOWS: Self = Self(1);
    const WORKSPACES: Self = Self(2);
    const MONITORS: Self = Self(4);
    const ALL: Self = Self(7);

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Dirty {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Dirty {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self { next: BACKOFF_MIN }
    }

    fn reset(&mut self) {
        self.next = BACKOFF_MIN;
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(BACKOFF_MAX);
        delay
    }
}

async fn run(
    client: Client,
    state: watch::Sender<State>,
    notices: broadcast::Sender<Notice>,
    refresh: Arc<Notify>,
    refresh_monitors: Arc<Notify>,
) {
    let mut backoff = Backoff::new();
    let mut line = Vec::with_capacity(1024);
    loop {
        match connect_and_sync(&client, &state).await {
            Ok(stream) => {
                backoff.reset();
                match listen(
                    &client,
                    stream,
                    &state,
                    &notices,
                    [&refresh, &refresh_monitors],
                    &mut line,
                )
                .await
                {
                    Ok(()) => warn!("Hyprland event socket closed"),
                    Err(err) => warn!("Hyprland event socket lost: {err}"),
                }
            }
            Err(err) => debug!("Hyprland resync failed: {err}"),
        }
        state.send_if_modified(|s| std::mem::replace(&mut s.connected, false));
        sleep(backoff.next_delay()).await;
    }
}

/// Connects the event socket before the snapshot, so events that race the snapshot wait
/// in the socket and get applied on top of it.
async fn connect_and_sync(
    client: &Client,
    state: &watch::Sender<State>,
) -> Result<UnixStream, Error> {
    let stream = client.connect_events().await?;
    let snapshot = Snapshot::fetch(client).await?;
    state.send_modify(|s| snapshot.install(s));
    Ok(stream)
}

struct Snapshot {
    monitors: Vec<Monitor>,
    workspaces: Vec<Workspace>,
    windows: Vec<Window>,
    active_workspace: Option<i64>,
    active_window: Option<Address>,
    keyboard_layout: String,
}

impl Snapshot {
    async fn fetch(client: &Client) -> Result<Self, Error> {
        let batch = client.batch(&SNAPSHOT).await?;
        let mut parts = batch.parts();
        let mut next = || parts.next().unwrap_or_default();
        let monitors: Vec<Monitor> = serde_json::from_str(next())?;
        let workspaces = serde_json::from_str(next())?;
        let windows = serde_json::from_str(next())?;
        // Both "active" queries answer with plain text or `{}` when nothing is active.
        let active_workspace = serde_json::from_str::<Workspace>(next())
            .ok()
            .map(|w| w.id)
            .unwrap_or_else(|| {
                monitors
                    .iter()
                    .find(|m| m.focused)
                    .and_then(|m| m.active_workspace.as_ref())
                    .and_then(|w| w.id)
            });
        let active_window = serde_json::from_str::<ActiveWindow>(next())
            .ok()
            .and_then(|w| w.address);
        let devices: Devices = serde_json::from_str(next())?;
        let keyboard_layout = devices
            .main_keyboard()
            .map(|k| k.active_keymap.clone())
            .unwrap_or_default();
        Ok(
            Self {
                monitors,
                workspaces,
                windows,
                active_workspace,
                active_window,
                keyboard_layout,
            },
        )
    }

    fn install(self, s: &mut State) {
        s.connected = true;
        s.focused_monitor = self
            .monitors
            .iter()
            .find(|m| m.focused)
            .map(|m| m.name.clone());
        s.monitors = self.monitors;
        s.workspaces = self.workspaces;
        s.windows = self.windows;
        s.active_workspace = self.active_workspace;
        s.active_window = self.active_window;
        s.keyboard_layout = self.keyboard_layout;
    }
}

#[derive(Default)]
struct Fetched {
    monitors: Option<Vec<Monitor>>,
    workspaces: Option<Vec<Workspace>>,
    windows: Option<Vec<Window>>,
}

impl Fetched {
    fn apply(self, s: &mut State) {
        if let Some(monitors) = self.monitors {
            if let Some(focused) = monitors.iter().find(|m| m.focused) {
                set_name(&mut s.focused_monitor, &focused.name);
            }
            s.monitors = monitors;
        }
        if let Some(workspaces) = self.workspaces {
            s.workspaces = workspaces;
        }
        if let Some(windows) = self.windows {
            s.windows = windows;
        }
    }
}

async fn fetch_dirty(client: &Client, dirty: Dirty) -> Result<Fetched, Error> {
    let mut commands = [""; 3];
    let mut len = 0;
    for (part, command) in [
        (Dirty::MONITORS, "j/monitors"),
        (Dirty::WORKSPACES, "j/workspaces"),
        (Dirty::WINDOWS, "j/clients"),
    ] {
        if dirty.contains(part) {
            commands[len] = command;
            len += 1;
        }
    }
    let commands = &commands[..len];

    let mut fetched = Fetched::default();
    let mut fill = |command: &str, reply: &str| -> Result<(), Error> {
        match command {
            "j/monitors" => fetched.monitors = Some(serde_json::from_str(reply)?),
            "j/workspaces" => fetched.workspaces = Some(serde_json::from_str(reply)?),
            _ => fetched.windows = Some(serde_json::from_str(reply)?),
        }
        Ok(())
    };
    if let [command] = commands {
        fill(command, &client.request(command).await?)?;
    } else if !commands.is_empty() {
        let batch = client.batch(commands).await?;
        for (command, reply) in commands.iter().zip(batch.parts()) {
            fill(command, reply)?;
        }
    }
    Ok(fetched)
}

type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Fetched, Error>> + Send + 'a>>;

async fn listen(
    client: &Client,
    stream: UnixStream,
    state: &watch::Sender<State>,
    notices: &broadcast::Sender<Notice>,
    [refresh, refresh_monitors]: [&Notify; 2],
    line: &mut Vec<u8>,
) -> Result<(), Error> {
    let mut reader = BufReader::new(stream);
    let mut dirty = Dirty::NONE;
    let mut deadline: Option<Instant> = None;
    let mut fetch: Option<FetchFuture<'_>> = None;
    line.clear();

    loop {
        tokio::select! {
            read = reader.read_until(b'\n', line) => {
                read?;
                if line.last() != Some(&b'\n') {
                    return Ok(());
                }
                {
                    // socket2 cuts event data at 1024 bytes, which can split a UTF-8 sequence.
                    let text = String::from_utf8_lossy(line);
                    match parse_line(&text) {
                        Ok(event) => {
                            let mut parts = apply(event, state, notices);
                            if fetch.is_some() {
                                parts |= patched_lists(&event);
                            }
                            mark(&mut dirty, &mut deadline, parts);
                        }
                        Err(err) => debug!("ignoring Hyprland event {text:?}: {err}"),
                    }
                }
                line.clear();
            }
            () = refresh.notified() => mark(&mut dirty, &mut deadline, Dirty::WINDOWS),
            () = refresh_monitors.notified() => mark(&mut dirty, &mut deadline, Dirty::MONITORS),
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() && fetch.is_none() => {
                deadline = None;
                let parts = std::mem::take(&mut dirty);
                fetch = Some(Box::pin(fetch_dirty(client, parts)));
            }
            fetched = async { fetch.as_mut().expect("guarded by is_some").await }, if fetch.is_some() => {
                fetch = None;
                let fetched = fetched?;
                state.send_modify(|s| fetched.apply(s));
            }
        }
    }
}

/// Arms the debounce timer on the clean-to-dirty transition only, so a steady stream of
/// events cannot postpone the fetch forever.
fn mark(dirty: &mut Dirty, deadline: &mut Option<Instant>, parts: Dirty) {
    if parts.is_empty() {
        return;
    }
    if dirty.is_empty() {
        *deadline = Some(Instant::now() + DEBOUNCE);
    }
    *dirty |= parts;
}

/// Lists that [`apply`] patches in place. A fetch already in flight carries older data and
/// would undo the patch, so these lists get fetched again after it.
fn patched_lists(event: &Event<'_>) -> Dirty {
    match event {
        Event::FocusedMonV2 { .. } => Dirty::MONITORS,
        Event::WindowTitleV2 { .. } => Dirty::WINDOWS,
        _ => Dirty::NONE,
    }
}

/// Events only set fields or mark lists dirty, so replaying them after a resync is harmless.
fn apply(
    event: Event<'_>,
    state: &watch::Sender<State>,
    notices: &broadcast::Sender<Notice>,
) -> Dirty {
    let notice = |notice: Notice| {
        let _ = notices.send(notice);
    };
    match event {
        Event::WorkspaceV2 { address, .. } => {
            let id = numbered_id(address);
            state.send_if_modified(|s| std::mem::replace(&mut s.active_workspace, id) != id);
            Dirty::MONITORS
        }
        Event::FocusedMonV2 { monitor, workspace } => {
            let id = numbered_id(workspace);
            state.send_if_modified(|s| {
                let mut changed = std::mem::replace(&mut s.active_workspace, id) != id;
                changed |= set_name(&mut s.focused_monitor, monitor);
                for m in &mut s.monitors {
                    let focused = m.name == monitor;
                    changed |= std::mem::replace(&mut m.focused, focused) != focused;
                }
                changed
            });
            Dirty::NONE
        }
        Event::ActiveWindowV2(address) => {
            state.send_if_modified(|s| std::mem::replace(&mut s.active_window, address) != address);
            Dirty::WINDOWS
        }
        Event::ActiveLayout { layout, .. } => {
            state.send_if_modified(|s| {
                if s.keyboard_layout == layout {
                    return false;
                }
                s.keyboard_layout.clear();
                s.keyboard_layout.push_str(layout);
                true
            });
            Dirty::NONE
        }
        Event::WindowTitleV2 { address, title } => {
            state.send_if_modified(|s| {
                match s
                    .windows
                    .iter_mut()
                    .find(|w| w.address == address)
                {
                    Some(w) if w.title != title => {
                        w.title.clear();
                        w.title.push_str(title);
                        true
                    }
                    _ => false,
                }
            });
            Dirty::NONE
        }
        Event::ChangeWorkspaceId { old, new } => {
            state.send_if_modified(|s| {
                let remap = s.active_workspace == Some(old);
                if remap {
                    s.active_workspace = Some(new);
                }
                remap
            });
            Dirty::ALL
        }
        Event::OpenWindow { .. }
        | Event::CloseWindow(_)
        | Event::MoveWindowV2 { .. }
        | Event::Fullscreen(_) => Dirty::WINDOWS | Dirty::WORKSPACES,
        Event::ChangeFloatingMode { .. } => Dirty::WINDOWS,
        Event::CreateWorkspaceV2 { .. } | Event::DestroyWorkspaceV2 { .. } => Dirty::WORKSPACES,
        Event::ActiveSpecialV2 { .. } => Dirty::MONITORS | Dirty::WORKSPACES,
        Event::MoveWorkspaceV2 { .. }
        | Event::RenameWorkspace { .. }
        | Event::MonitorAddedV2 { .. }
        | Event::MonitorRemovedV2 { .. } => Dirty::ALL,
        Event::Urgent(address) => {
            notice(Notice::Urgent(address));
            Dirty::NONE
        }
        Event::OpenLayer(namespace) => {
            notice(Notice::OpenLayer(namespace.to_owned()));
            Dirty::NONE
        }
        Event::CloseLayer(namespace) => {
            notice(Notice::CloseLayer(namespace.to_owned()));
            Dirty::NONE
        }
        Event::Custom(data) => {
            notice(Notice::Custom(data.to_owned()));
            Dirty::NONE
        }
        Event::ConfigReloaded => {
            notice(Notice::ConfigReloaded);
            Dirty::ALL
        }
        Event::Other { name: "pin", .. } => Dirty::WINDOWS,
        Event::ActiveWindow { .. } | Event::Other { .. } => Dirty::NONE,
    }
}

fn numbered_id(address: &str) -> Option<i64> {
    if address.is_empty()
        || !address
            .bytes()
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    address
        .parse::<u32>()
        .ok()
        .filter(|&id| id > 0)
        .map(i64::from)
}

fn set_name(slot: &mut Option<String>, name: &str) -> bool {
    match slot {
        Some(current) if current == name => false,
        Some(current) => {
            current.clear();
            current.push_str(name);
            true
        }
        None => {
            *slot = Some(name.to_owned());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps_at_five_seconds() {
        let mut backoff = Backoff::new();
        let delays: Vec<u64> = (0..10)
            .map(|_| backoff.next_delay().as_millis() as u64)
            .collect();
        assert_eq!(
            delays,
            [100, 200, 400, 800, 1600, 3200, 5000, 5000, 5000, 5000]
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(), BACKOFF_MIN);
    }

    #[test]
    fn numbered_ids_come_from_addresses() {
        assert_eq!(numbered_id("3"), Some(3));
        assert_eq!(numbered_id("4294967295"), Some(4294967295));
        assert_eq!(numbered_id("0"), None);
        assert_eq!(numbered_id("+3"), None);
        assert_eq!(numbered_id("web"), None);
        assert_eq!(numbered_id("special:magic"), None);
        assert_eq!(numbered_id(""), None);
    }

    #[test]
    fn debounce_arms_only_on_first_mark() {
        let mut dirty = Dirty::NONE;
        let mut deadline = None;
        mark(&mut dirty, &mut deadline, Dirty::NONE);
        assert!(deadline.is_none());
        mark(&mut dirty, &mut deadline, Dirty::WINDOWS);
        let first = deadline.expect("armed");
        mark(&mut dirty, &mut deadline, Dirty::MONITORS);
        assert_eq!(deadline, Some(first));
        assert_eq!(dirty, Dirty::WINDOWS | Dirty::MONITORS);
    }

    #[test]
    fn pin_refetches_windows() {
        let (state, _rx) = watch::channel(State::default());
        let (notices, _nrx) = broadcast::channel(8);
        let parts = apply(
            parse_line("pin>>55b0cd60fa30,1\n").unwrap(),
            &state,
            &notices,
        );
        assert_eq!(parts, Dirty::WINDOWS);
    }

    #[test]
    fn replayed_events_are_idempotent() {
        let (state, _rx) = watch::channel(State::default());
        let (notices, _nrx) = broadcast::channel(8);
        let events = [
            "focusedmonv2>>DP-1,3\n",
            "workspacev2>>4,4\n",
            "activewindowv2>>abc\n",
            "activelayout>>kbd,English (US)\n",
            "changeworkspaceid>>4,9\n",
        ];
        for line in events {
            apply(parse_line(line).unwrap(), &state, &notices);
        }
        let once = state.borrow().clone();
        for line in events {
            apply(parse_line(line).unwrap(), &state, &notices);
        }
        assert_eq!(*state.borrow(), once);
        assert_eq!(once.active_workspace, Some(9));
        assert_eq!(once.focused_monitor.as_deref(), Some("DP-1"));
        assert_eq!(once.active_window, Some(Address(0xabc)));
        assert_eq!(once.keyboard_layout, "English (US)");
    }
}
