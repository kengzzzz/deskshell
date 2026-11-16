//! Auth surfaces: the polkit and updates sheets, the keyring prompt and the touch prompt.

pub mod faillock;
pub mod inert;
pub mod keyring;
pub mod polkit;
pub mod secret;
pub mod sheet;
mod shown;
mod style;
pub mod touch;

use std::time::{Duration, Instant};

use deskshell_auth::touch::TouchState;
use deskshell_notify::Post;
use deskshell_ui::Theme;
use iced::Element;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use deskshell_polkit::{Agent, Command as PolkitCommand, Event as PolkitEvent, Flow};
use faillock::Faillock;
use keyring::service::{self as keyring_service, Command as KeyringCommand};
use keyring::view::KeyringPrompt;
use polkit::Polkit;
pub use secret::Secret;
pub use sheet::{Outcome, Sheet, SheetKind, Source, Stage, Text};
use touch::{TouchPrompt, WaitLock};

pub const FAILLOCK_POLL: Duration = Duration::from_secs(2);
pub const FAILLOCK_CLOCK: Duration = Duration::from_secs(1);
pub const POLKIT_START: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Sheet(SheetKind),
    Keyring,
    Touch,
}

impl Surface {
    pub const ALL: [Self; 4] = [
        Self::Sheet(SheetKind::Polkit),
        Self::Sheet(SheetKind::Updates),
        Self::Keyring,
        Self::Touch,
    ];

    pub fn index(self) -> usize {
        match self {
            Self::Sheet(SheetKind::Polkit) => 0,
            Self::Sheet(SheetKind::Updates) => 1,
            Self::Keyring => 2,
            Self::Touch => 3,
        }
    }

    pub fn namespace(self) -> &'static str {
        match self {
            Self::Sheet(kind) => kind.namespace(),
            Self::Keyring => keyring::view::NAMESPACE,
            Self::Touch => touch::NAMESPACE,
        }
    }

    pub fn keyboard(self) -> bool {
        !matches!(self, Self::Touch)
    }

    pub fn width(self) -> u32 {
        match self {
            Self::Sheet(_) => sheet::WIDTH,
            Self::Keyring => keyring::view::WIDTH,
            Self::Touch => touch::WIDTH,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Sheet(sheet::Message),
    Keyring(keyring::view::Message),
    Escape,
    Enter,
    Tab(bool),
    Redraw,
    Shown(u64),
    Released(Key),
}

impl Message {
    pub fn redraws(&self) -> bool {
        match self {
            Self::Sheet(message) => message.redraws(),
            Self::Keyring(message) => message.redraws(),
            Self::Shown(_) | Self::Released(_) => false,
            _ => true,
        }
    }

    // A field that takes Enter sends its own message, and the key subscription drops that press.
    fn key_press(&self) -> Option<Key> {
        use keyring::view::Message as Form;
        match self {
            Self::Escape => Some(Key::Escape),
            Self::Enter
            | Self::Sheet(sheet::Message::Enter)
            | Self::Keyring(Form::PasswordEnter | Form::ConfirmEnter) => Some(Key::Enter),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
}

// iced_exwlshell sends a held key's repeats as plain presses with `repeat` false. So a press
// while the key is down is a repeat, and only the first press acts.
#[derive(Debug, Clone, Copy, Default)]
struct Held {
    escape: bool,
    enter: bool,
}

impl Held {
    fn press(&mut self, key: Key) -> bool {
        let down = self.slot(key);
        let fresh = !*down;
        *down = true;
        fresh
    }

    fn release(&mut self, key: Key) {
        *self.slot(key) = false;
    }

    fn slot(&mut self, key: Key) -> &mut bool {
        match key {
            Key::Escape => &mut self.escape,
            Key::Enter => &mut self.enter,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Touch(TouchState),
    Flow(Option<Flow>),
    Polkit(PolkitEvent),
    Keyring(keyring_service::State),
    Faillock(faillock::Read, u64),
    Clock(u64),
    PollFaillock,
    Updates,
    /// Whether the session lock held, read for the touch wait `generation`.
    SessionLocked {
        generation: u64,
        locked: bool,
    },
}

#[derive(Debug)]
pub enum Request {
    Polkit(PolkitCommand),
    Keyring(KeyringCommand),
    Notify(Post),
    Focus(&'static str),
    FocusNext(bool),
    ReadFaillock,
    ReadSessionLock(u64),
}

#[derive(Debug)]
pub struct Auth {
    polkit: Polkit,
    sheets: [Sheet; 2],
    keyring: KeyringPrompt,
    keyring_state: keyring_service::State,
    touch: TouchState,
    touch_prompt: TouchPrompt,
    wait_lock: WaitLock,
    faillock: Faillock,
    sheet_shown: bool,
    // Keys and buttons act only on what was drawn.
    polkit_drawn: Drawn,
    keyring_drawn: Drawn,
    updates_drawn: Drawn,
    updates_request: u64,
    updates_run: Option<String>,
    updates_prompt: u64,
    updates_open: Option<i64>,
    // A second Esc at the same run, prompt and failed cancel count sends nothing. A new prompt or
    // a failed cancel takes a cancel again.
    updates_cancelled: Option<(u64, u64, u32)>,
    keyboard_queue: Vec<Surface>,
    held: Held,
    pub updates: UpdatesLink,
}

/// A request that took over an open surface ignores keys and buttons this long once drawn, so a
/// second press meant for the old request never acts on one nobody had time to read.
pub const TAKEOVER_GRACE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, Default)]
struct Drawn {
    token: Option<u64>,
    at: Option<Instant>,
    took_over: bool,
}

impl Drawn {
    fn shown(&mut self, token: u64, now: Instant) {
        if self.token != Some(token) {
            self.token = Some(token);
            self.at = Some(now);
        }
    }

    fn replaced(&mut self, took_over: bool) {
        self.took_over = took_over;
    }

    fn ready(&self, token: u64, now: Instant) -> bool {
        if self.token != Some(token) {
            return false;
        }
        !self.took_over
            || self
                .at
                .is_some_and(|at| now.saturating_duration_since(at) >= TAKEOVER_GRACE)
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self::new(Faillock::default())
    }
}

impl Auth {
    pub fn new(faillock: Faillock) -> Self {
        Self {
            polkit: Polkit::default(),
            sheets: [Sheet::new(SheetKind::Polkit), Sheet::new(SheetKind::Updates)],
            keyring: KeyringPrompt::default(),
            keyring_state: keyring_service::State::default(),
            touch: TouchState::default(),
            touch_prompt: TouchPrompt::default(),
            wait_lock: WaitLock::default(),
            faillock,
            sheet_shown: false,
            polkit_drawn: Drawn::default(),
            keyring_drawn: Drawn::default(),
            updates_drawn: Drawn::default(),
            updates_request: 0,
            updates_run: None,
            updates_prompt: 0,
            updates_open: None,
            updates_cancelled: None,
            keyboard_queue: Vec::with_capacity(3),
            held: Held::default(),
            updates: UpdatesLink::default(),
        }
    }

    pub fn touch(&self) -> &TouchState {
        &self.touch
    }

    pub fn sheet(&self, kind: SheetKind) -> &Sheet {
        &self.sheets[kind as usize]
    }

    pub fn polkit(&self) -> &Polkit {
        &self.polkit
    }

    pub fn keyring(&self) -> &KeyringPrompt {
        &self.keyring
    }

    pub fn keyring_state(&self) -> &keyring_service::State {
        &self.keyring_state
    }

    pub fn touch_prompt(&self) -> &TouchPrompt {
        &self.touch_prompt
    }

    pub fn faillock(&self) -> &Faillock {
        &self.faillock
    }

    pub fn source(&self, kind: SheetKind) -> Source {
        match kind {
            SheetKind::Polkit => self.polkit.source(&self.touch),
            SheetKind::Updates => updates_sheet(&self.updates, &self.touch, None).source,
        }
    }

    pub fn visible(&self, surface: Surface) -> bool {
        if surface.keyboard() {
            return self.keyboard_queue.first() == Some(&surface) && self.wants(surface);
        }
        self.wants(surface)
    }

    pub fn wants(&self, surface: Surface) -> bool {
        match surface {
            Surface::Sheet(kind) => self.source(kind).visible,
            Surface::Keyring => self.keyring_state.active(),
            Surface::Touch => self.touch_prompt.visible(),
        }
    }

    pub fn sheet_visible(&self) -> bool {
        SheetKind::ALL
            .iter()
            .any(|kind| self.source(*kind).visible)
    }

    pub fn counting_down(&self) -> bool {
        self.sheet_visible() && self.faillock.counting_down()
    }

    pub fn height(&self, surface: Surface) -> u32 {
        match surface {
            Surface::Sheet(SheetKind::Polkit) => {
                let kind = SheetKind::Polkit;
                self.sheet(kind)
                    .surface_height(&self.source(kind), false)
            }
            Surface::Sheet(SheetKind::Updates) => {
                let kind = SheetKind::Updates;
                let has_details = updates_sheet(&self.updates, &self.touch, None).has_details;
                self.sheet(kind)
                    .surface_height(&self.source(kind), has_details)
            }
            Surface::Keyring => self.keyring.surface_height(),
            Surface::Touch => touch::HEIGHT,
        }
    }

    pub fn event(&mut self, event: Event, requests: &mut Vec<Request>) -> bool {
        let changed = match event {
            Event::Touch(touch) => {
                let changed = self.touch != touch;
                self.touch = touch;
                changed
            }
            Event::Flow(flow) => {
                let open = self.polkit.has_flow();
                let synced = self.polkit.sync(flow.as_ref());
                if synced.new_flow {
                    self.sheets[SheetKind::Polkit as usize].reset();
                    self.polkit_drawn.replaced(open);
                }
                if let Some(command) = synced.respond {
                    requests.push(Request::Polkit(command));
                }
                true
            }
            Event::Polkit(PolkitEvent::FailedBeforeResponse { action_id, error, .. }) => {
                info!(
                    action_id,
                    "polkit failed before any response, the request ends"
                );
                requests.push(Request::Notify(polkit::failure_post(
                    error.as_deref(),
                )));
                false
            }
            Event::Polkit(PolkitEvent::ResponseRejected {
                cookie,
                conversation,
                prompt,
                reason,
            }) => {
                info!(?reason, "the polkit agent refused the PIN");
                self.polkit
                    .rejected(&cookie, conversation, prompt);
                true
            }
            Event::Keyring(state) => {
                let changed = self.keyring_state != state;
                if state.serial() != self.keyring_state.serial() {
                    self.keyring_drawn
                        .replaced(self.keyring_state.active() && state.active());
                }
                self.keyring_state = state;
                changed
            }
            Event::Faillock(read, now) => self.faillock.apply(read, now),
            Event::Clock(now) => self.faillock.tick(now),
            Event::PollFaillock => {
                if self.sheet_visible() {
                    requests.push(Request::ReadFaillock);
                }
                false
            }
            Event::Updates => true,
            Event::SessionLocked { generation, locked } => {
                let applied = self
                    .wait_lock
                    .answer(generation, locked);
                if !applied {
                    debug!(
                        generation,
                        "a lock read for a touch wait that is over, dropped"
                    );
                }
                applied
            }
        };
        self.settle(requests);
        changed
    }

    pub fn message(&mut self, surface: Surface, message: Message, requests: &mut Vec<Request>) {
        self.message_at(surface, message, Instant::now(), requests);
    }

    pub fn message_at(
        &mut self,
        surface: Surface,
        message: Message,
        now: Instant,
        requests: &mut Vec<Request>,
    ) {
        if let Message::Released(key) = message {
            self.held.release(key);
            return;
        }
        if let Some(key) = message.key_press()
            && !self.held.press(key)
        {
            debug!(?key, "a held key repeats, ignored");
            return;
        }
        let locked = self.faillock.locked();
        match (surface, message) {
            (Surface::Sheet(kind), Message::Sheet(message)) => {
                self.sheet_message(kind, message, locked, now, requests)
            }
            (Surface::Sheet(kind), Message::Escape) => {
                self.sheet_message(
                    kind,
                    sheet::Message::Cancel,
                    locked,
                    now,
                    requests,
                );
            }
            (Surface::Keyring, Message::Keyring(message)) => {
                self.keyring_message(message, now, requests)
            }
            (Surface::Keyring, Message::Escape) => {
                self.keyring_message(keyring::view::Message::Cancel, now, requests);
            }
            (Surface::Keyring, Message::Enter) => {
                self.keyring_message(keyring::view::Message::Enter, now, requests);
            }
            (Surface::Keyring, Message::Tab(back)) => requests.push(Request::FocusNext(back)),
            (Surface::Sheet(SheetKind::Polkit), Message::Shown(token)) => {
                self.polkit_drawn.shown(token, now);
            }
            (Surface::Sheet(SheetKind::Updates), Message::Shown(token)) => {
                self.updates_drawn.shown(token, now);
            }
            (Surface::Keyring, Message::Shown(serial)) => {
                self.keyring_drawn.shown(serial, now);
            }
            _ => {}
        }
        self.settle(requests);
    }

    // Checked against the drawn request first, so a refused submit takes no copy of the PIN.
    fn sheet_message(
        &mut self,
        kind: SheetKind,
        message: sheet::Message,
        locked: bool,
        now: Instant,
        requests: &mut Vec<Request>,
    ) {
        if kind == SheetKind::Updates {
            self.track_updates_run();
        }
        let acts = matches!(
            message,
            sheet::Message::Submit | sheet::Message::Enter | sheet::Message::Cancel
        );
        if acts && !self.sheet_ready(kind, now) {
            debug!(
                ?kind,
                ?message,
                "the request on the sheet is not drawn or too new, dropped"
            );
            return;
        }
        let cancel_key = (
            self.updates_request,
            self.updates_prompt,
            self.updates.job().cancels_failed,
        );
        if kind == SheetKind::Updates
            && matches!(message, sheet::Message::Cancel)
            && self.updates_cancelled == Some(cancel_key)
        {
            debug!("a cancel went out at this prompt of the run already, dropped");
            return;
        }
        let source = self.source(kind);
        let Some(outcome) = self.sheets[kind as usize].update(message, &source, locked) else {
            return;
        };
        match kind {
            SheetKind::Polkit => {
                if let Some(command) = self.polkit.outcome(outcome) {
                    requests.push(Request::Polkit(command));
                }
            }
            SheetKind::Updates => {
                if matches!(outcome, Outcome::Cancel) {
                    self.updates_cancelled = Some(cancel_key);
                }
                updates_outcome(&mut self.updates, outcome);
            }
        }
    }

    fn sheet_ready(&self, kind: SheetKind, now: Instant) -> bool {
        match kind {
            SheetKind::Polkit => self
                .polkit_drawn
                .ready(self.polkit.request(), now),
            SheetKind::Updates => self
                .updates_drawn
                .ready(self.updates_request, now),
        }
    }

    // Checked before the form sees it, so a refused submit keeps the fields.
    fn keyring_message(
        &mut self,
        message: keyring::view::Message,
        now: Instant,
        requests: &mut Vec<Request>,
    ) {
        use keyring::view::Message as Form;
        let edits = matches!(
            message,
            Form::Password(..)
                | Form::Confirm(..)
                | Form::Choice(_)
                | Form::FormHeight(_)
                | Form::TextsHeight(_)
        );
        if !edits
            && !self
                .keyring_drawn
                .ready(self.keyring_state.serial(), now)
        {
            debug!(
                ?message,
                "the keyring prompt is not drawn or too new, dropped"
            );
            return;
        }
        let effects = self
            .keyring
            .update(message, &self.keyring_state);
        if let Some(command) = effects.command {
            requests.push(Request::Keyring(command));
        }
        if let Some(field) = effects.focus {
            requests.push(Request::Focus(field.id()));
        }
    }

    fn settle(&mut self, requests: &mut Vec<Request>) {
        self.track_updates_run();
        let locked = self.faillock.locked();
        for kind in SheetKind::ALL {
            let source = self.source(kind);
            self.sheets[kind as usize].update(sheet::Message::Sync, &source, locked);
        }
        let shown = self.sheet_visible();
        if shown && !self.sheet_shown {
            requests.push(Request::ReadFaillock);
        }
        self.sheet_shown = shown;
        if let Some(field) = self.keyring.sync(&self.keyring_state) {
            requests.push(Request::Focus(field.id()));
        }
        let waiting = self.touch.any();
        if let Some(generation) = self.wait_lock.follow(waiting) {
            requests.push(Request::ReadSessionLock(generation));
        }
        let suppressed = shown || self.keyring_state.yubikey_waiting() || self.wait_lock.locked();
        self.touch_prompt
            .set(&self.touch, suppressed, self.wait_lock.pending());
        self.queue_keyboard_surfaces();
    }

    // The held keys are forgotten when another surface maps. The old surface's release may never
    // come, and iced_exwlshell stops a key's repeats when the keyboard enters a new surface.
    fn queue_keyboard_surfaces(&mut self) {
        let head = self.keyboard_queue.first().copied();
        let wants: [bool; 3] = [
            self.wants(Surface::Sheet(SheetKind::Polkit)),
            self.wants(Surface::Sheet(SheetKind::Updates)),
            self.wants(Surface::Keyring),
        ];
        let surfaces = [
            Surface::Sheet(SheetKind::Polkit),
            Surface::Sheet(SheetKind::Updates),
            Surface::Keyring,
        ];
        self.keyboard_queue.retain(|surface| {
            surfaces
                .iter()
                .position(|known| known == surface)
                .is_some_and(|index| wants[index])
        });
        for (surface, wants) in surfaces.into_iter().zip(wants) {
            if wants && !self.keyboard_queue.contains(&surface) {
                self.keyboard_queue.push(surface);
            }
        }
        if self.keyboard_queue.first().copied() != head {
            self.held = Held::default();
        }
    }

    // A new install run is a new request, so a PIN typed for the last run never answers this one.
    fn track_updates_run(&mut self) {
        let visible = self.source(SheetKind::Updates).visible;
        let job = self.updates.job();
        let open = if visible { job.open_prompt() } else { None };
        if open.is_some() && open != self.updates_open {
            self.updates_prompt += 1;
        }
        self.updates_open = open;
        let run = visible.then_some(job.run_id.as_str());
        if run == self.updates_run.as_deref() {
            return;
        }
        if run.is_some() {
            self.updates_request += 1;
            self.updates_drawn
                .replaced(self.updates_run.is_some());
            self.sheets[SheetKind::Updates as usize].reset();
        }
        self.updates_run = run.map(str::to_owned);
    }

    pub fn view<'a>(&'a self, surface: Surface, theme: &'a Theme) -> Element<'a, Message> {
        match surface {
            Surface::Sheet(SheetKind::Polkit) => {
                let kind = SheetKind::Polkit;
                let token = self.polkit.request();
                let view = self
                    .sheet(kind)
                    .view(
                        theme,
                        &self.source(kind),
                        self.polkit.text(),
                        Some(self.faillock.lockout()),
                        None,
                    )
                    .map(Message::Sheet);
                shown::shown(view, token, Message::Shown(token)).into()
            }
            Surface::Sheet(SheetKind::Updates) => {
                let kind = SheetKind::Updates;
                let token = self.updates_request;
                let updates = updates_sheet(&self.updates, &self.touch, Some(theme));
                let view = self
                    .sheet(kind)
                    .view(
                        theme,
                        &updates.source,
                        updates.text,
                        Some(self.faillock.lockout()),
                        updates.details,
                    )
                    .map(Message::Sheet);
                shown::shown(view, token, Message::Shown(token)).into()
            }
            Surface::Keyring => {
                let serial = self.keyring_state.serial();
                let view = self
                    .keyring
                    .view(theme, &self.keyring_state, &self.touch)
                    .map(Message::Keyring);
                shown::shown(view, serial, Message::Shown(serial)).into()
            }
            Surface::Touch => self.touch_prompt.view(theme),
        }
    }
}

pub type UpdatesLink = crate::updates::sheet::Link;

pub struct UpdatesSheet<'a> {
    pub source: Source,
    pub text: &'a Text,
    pub has_details: bool,
    pub details: Option<Element<'a, sheet::Message>>,
}

pub fn updates_sheet<'a>(
    link: &'a UpdatesLink,
    touch: &TouchState,
    view: Option<&'a Theme>,
) -> UpdatesSheet<'a> {
    link.sheet(touch, view)
}

pub fn updates_outcome(link: &mut UpdatesLink, outcome: Outcome) {
    link.outcome(outcome);
}

#[derive(Debug, Default)]
pub struct Boot {
    pub touch: Option<watch::Receiver<TouchState>>,
    pub polkit: Option<polkit::Link>,
    pub keyring: Option<keyring_service::Service>,
    pub faillock: Option<faillock::Source>,
}

#[derive(Default)]
pub struct Services {
    polkit: Option<Agent>,
    keyring: Option<keyring_service::Stop>,
}

impl Services {
    pub async fn shutdown(self) {
        if let Some(agent) = self.polkit {
            agent.shutdown().await;
            info!("the polkit agent stopped");
        }
        if let Some(keyring) = self.keyring {
            keyring.stop().await;
        }
    }
}

/// `remove_env` lists what the shell set for itself, which the prompter's `gpg` must not inherit.
pub fn start(remove_env: &[&'static str], runtime: &tokio::runtime::Runtime) -> (Boot, Services) {
    let user = std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty());
    let started = match &user {
        Some(user) => {
            let user = user.clone();
            runtime.block_on(async {
                tokio::time::timeout(POLKIT_START, polkit::start(user))
                    .await
                    .unwrap_or_else(|_| {
                        warn!("the polkit agent took too long to start, running without one");
                        None
                    })
            })
        }
        None => {
            warn!("USER is not set, no polkit agent and no faillock state");
            None
        }
    };
    let (agent, link) = started.unzip();
    let _context = runtime.enter();
    let gpg_socket = deskshell_auth::touch::gpg_socket();
    if gpg_socket.is_none() {
        warn!("XDG_RUNTIME_DIR is not set, no GPG touch waits");
    }
    let touch = Some(deskshell_auth::touch::spawn(
        deskshell_auth::touch::Sources {
            fido: Some(deskshell_auth::touch::HidPaths::system()),
            gpg_socket,
        },
    ));
    let (keyring, keyring_stop) = keyring_service::spawn(keyring_service::config(remove_env));
    let faillock = user.map(faillock::Source::new);
    (
        Boot { touch, polkit: link, keyring: Some(keyring), faillock },
        Services { polkit: agent, keyring: Some(keyring_stop) },
    )
}

#[cfg(test)]
mod tests;
