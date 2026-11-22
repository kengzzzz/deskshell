//! The greeter program: startup checks, one Overlay surface per output, and the flow's driver.

use std::env;
use std::ffi::OsString;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, IsTerminal, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deskshell_auth::faillock;
use deskshell_auth::key;
use deskshell_ui::{Theme, fonts};
use iced::futures::{Stream, StreamExt, stream};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::image::Handle;
use iced::{Element, Event as IcedEvent, Subscription, Task, mouse, touch, window};
use iced_exwlshell::build_pattern::daemon;
use iced_exwlshell::redraw::Scope;
use iced_exwlshell::reexport::{
    Anchor, BlurOption, KeyboardInteractivity, Layer, LayerSize, NewLayerShellSettings,
    OutputOption,
};
use iced_exwlshell::settings::{ExWlSettings, LayerShellSettings, StartMode};
use iced_exwlshell::shell::{self as wl_shell, ShellEvent, ShellReceiver};
use iced_exwlshell::to_exwlshell_message;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::uptime;

use crate::clock::{self, Clock};
use crate::config::Config;
use crate::flow::{Action, Event, Flow, Step};
use crate::greetd::{self, Client, Outgoing, Reply};
use crate::idle::{self, Idle};
use crate::inputs::{self, Paths};
use crate::pictures::{self, Guard};
use crate::runtime;
use crate::view::{self, Login, Scene, Texts, Ui};

pub const NAMESPACE: &str = "deskshell-greeter";
pub const BLANK: &str = "hl.dsp.dpms(\"off\")";
pub const WAKE: &str = "hl.dsp.dpms(\"on\")";
pub const IDLE_ENV: &str = "DESKSHELL_GREETER_IDLE_SECONDS";
const LOG_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";
const CANCEL_NOTICE: Duration = Duration::from_secs(3);
const CANCEL_BOUND: Duration = Duration::from_secs(600);
const CANCEL_SLOW: Duration = Duration::from_millis(100);
const CANCEL_SETTLE: Duration = Duration::from_millis(500);
const NO_USERS: &str =
    "No users to sign in: list them in users, or add them to /etc/Yubico/u2f_mappings";

const USAGE: &str = "usage: deskshell-greeter
       deskshell-greeter cancel-stale

Run by greetd as its greeter, inside the greeter's own Hyprland. cancel-stale cancels
a login a previous greeter left behind, for the wrapper script.";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;

pub fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => run(),
        [command] if command == "cancel-stale" => cancel_stale(),
        [flag] if flag == "-h" || flag == "--help" => {
            let _ = writeln!(io::stdout(), "{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            let _ = writeln!(io::stderr(), "{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn greetd_socket() -> Option<PathBuf> {
    env::var_os(greetd::SOCKET_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn cancel_stale() -> ExitCode {
    let Some(socket) = greetd_socket() else {
        let _ = writeln!(io::stderr(), "{} is not set", greetd::SOCKET_ENV);
        return ExitCode::from(EXIT_FAILED);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(
                io::stderr(),
                "cannot start the async runtime: {err}"
            );
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let notice = || {
        let _ = writeln!(
            io::stderr(),
            "greetd is still finishing an earlier login. The greeter starts when it is done."
        );
    };
    let patience = greetd::Patience {
        notice_after: CANCEL_NOTICE,
        bound: CANCEL_BOUND,
        slow: CANCEL_SLOW,
        settle: CANCEL_SETTLE,
    };
    match runtime.block_on(greetd::cancel_stale(&socket, patience, notice)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

fn run() -> ExitCode {
    // SAFETY: this is the first thing the run path does, before any thread exists.
    unsafe { force_vulkan() };
    install_panic_hook();
    init_logging();
    if let Err(err) = harden() {
        let _ = writeln!(
            io::stderr(),
            "cannot make the greeter non-dumpable: {err}"
        );
        return ExitCode::from(EXIT_FAILED);
    }

    let Some(socket) = greetd_socket() else {
        let _ = writeln!(
            io::stderr(),
            "{} is not set: deskshell-greeter runs as greetd's greeter",
            greetd::SOCKET_ENV
        );
        return ExitCode::from(EXIT_FAILED);
    };
    let stream = match StdUnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = writeln!(
                io::stderr(),
                "cannot connect to greetd at {}: {err}",
                socket.display()
            );
            return ExitCode::from(EXIT_FAILED);
        }
    };
    if !has_wayland() {
        let _ = writeln!(
            io::stderr(),
            "No Wayland display: WAYLAND_DISPLAY and WAYLAND_SOCKET are not set."
        );
        return ExitCode::from(EXIT_FAILED);
    }
    let runtime = match runtime::get() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(
                io::stderr(),
                "cannot start the async runtime: {err}"
            );
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let (requests, replies) = {
        let _context = runtime.enter();
        match Client::from_std(stream) {
            Ok(client) => greetd::spawn(Some(client), socket),
            Err(err) => {
                let _ = writeln!(
                    io::stderr(),
                    "cannot use the greetd connection: {err}"
                );
                return ExitCode::from(EXIT_FAILED);
            }
        }
    };
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let paths = Paths::system(runtime_dir);
    let hypr = match deskshell_hypr::Paths::from_env() {
        Ok(hypr_paths) => Some(deskshell_hypr::Client::new(hypr_paths)),
        Err(err) => {
            warn!(%err, "no Hyprland: the idle blank cannot turn the screen off");
            None
        }
    };
    let boot = Boot::load(paths, idle_timeout(), requests, replies, hypr);
    match &boot.login {
        Ok(flow) => {
            info!(users = flow.users().len(), session = %flow.session().name, "greeter starts")
        }
        Err(reason) => warn!(%reason, "greeter starts without a usable config"),
    }

    let (shell_broadcast, shell_events) = wl_shell::channel();
    let boot = std::cell::Cell::new(Some((boot, shell_events)));
    let mut app = daemon(
        move || {
            let (boot, shell_events) = boot
                .take()
                .expect("iced boots the greeter once");
            Greeter::new(boot, shell_events)
        },
        NAMESPACE,
        Greeter::update,
        Greeter::view,
    )
    .wl_settings(ExWlSettings {
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..LayerShellSettings::default()
        },
        shell_broadcast,
        ..ExWlSettings::default()
    })
    .subscription(Greeter::subscription)
    .style(Greeter::style)
    .theme(|_: &Greeter, _| iced::Theme::Dark)
    .executor::<runtime::Executor>()
    .redraw_scope(redraw_scope)
    .default_font(fonts::PLEX);
    for font in fonts::all() {
        app = app.font(*font);
    }
    match app.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "event loop failed");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// # Safety
///
/// No other thread may exist yet, because it writes the environment.
unsafe fn force_vulkan() {
    for (key, value) in [("WGPU_BACKEND", "vulkan"), ("ICED_BACKEND", "wgpu")] {
        if env::var_os(key).is_none() {
            // SAFETY: the caller guarantees that no other thread runs yet.
            unsafe { env::set_var(key, value) };
        }
    }
}

/// The greeter has no catch zones, so every panic aborts.
fn install_panic_hook() {
    let print = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        print(info);
        process::abort();
    }));
}

/// Non-dumpable before any input, so neither a core dump (the panic hook aborts without
/// zeroizing) nor ptrace by another greeter process can read a PIN.
pub fn harden() -> io::Result<()> {
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .map_err(io::Error::from)
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(LOG_FILTER));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(uptime())
        .with_ansi(io::stderr().is_terminal())
        .with_writer(io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn has_wayland() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()))
}

fn idle_timeout() -> Duration {
    parse_idle(env::var(IDLE_ENV).ok().as_deref())
}

pub fn parse_idle(value: Option<&str>) -> Duration {
    value
        .and_then(|text| text.trim().parse::<u64>().ok())
        .filter(|&seconds| seconds > 0)
        .map_or(idle::TIMEOUT, Duration::from_secs)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn login(paths: &Paths) -> Result<(Flow, Option<PathBuf>), String> {
    let config = Config::load(&paths.config).map_err(|err| err.to_string())?;
    let mappings = inputs::read_mappings(&paths.mappings);
    let users = config.users(mappings.as_deref());
    if users.is_empty() {
        return Err(String::from(NO_USERS));
    }
    let last = inputs::read_last_user(&paths.cache);
    let flow = Flow::new(users, mappings, config.session, last.as_deref());
    Ok((flow, config.wallpaper))
}

struct Boot {
    login: Result<Flow, String>,
    wallpaper: Option<PathBuf>,
    host: String,
    faillock: Arc<faillock::Config>,
    paths: Paths,
    idle: Duration,
    requests: mpsc::UnboundedSender<Outgoing>,
    replies: mpsc::UnboundedReceiver<Reply>,
    hypr: Option<deskshell_hypr::Client>,
}

impl Boot {
    fn load(
        paths: Paths,
        idle: Duration,
        requests: mpsc::UnboundedSender<Outgoing>,
        replies: mpsc::UnboundedReceiver<Reply>,
        hypr: Option<deskshell_hypr::Client>,
    ) -> Self {
        let (login, wallpaper) = match login(&paths) {
            Ok((flow, wallpaper)) => (Ok(flow), wallpaper),
            Err(reason) => (Err(reason), None),
        };
        Self {
            login,
            wallpaper,
            host: inputs::read_hostname(&paths.hostname),
            faillock: Arc::new(faillock::Config::load(&paths.faillock_config)),
            paths,
            idle,
            requests,
            replies,
            hypr,
        }
    }
}

#[to_exwlshell_message]
#[derive(Debug, Clone)]
enum Message {
    Shell(ShellEvent),
    Opened(window::Id),
    Input(Input),
    Ui(Ui),
    Flow(Event),
    KeyPoll,
    FaillockPoll,
    IdleFire,
    BlankDone(bool),
    WakeDone(bool),
    Minute,
    Avatars(Vec<Option<Handle>>),
    Wallpaper(Option<Handle>),
    Redraw,
    Done,
}

#[derive(Debug, Clone)]
enum Input {
    Key(keyboard::Event),
    Moved(f32, f32),
    Pressed,
}

fn redraw_scope(message: &Message) -> Scope {
    match message {
        Message::Redraw => Scope::All,
        _ => Scope::None,
    }
}

struct Greeter {
    theme: Theme,
    login: Result<Flow, String>,
    host: String,
    faillock: Arc<faillock::Config>,
    paths: Paths,
    surfaces: Vec<(window::Id, u32)>,
    shell_events: ShellReceiver,
    requests: mpsc::UnboundedSender<Outgoing>,
    replies: Feed<mpsc::UnboundedReceiver<Reply>>,
    hypr: Option<deskshell_hypr::Client>,
    idle: Idle,
    idle_pending: bool,
    hovered: Option<usize>,
    unix_now: u64,
    ready: bool,
    clock: Clock,
    texts: Texts,
    texts_from: u64,
    avatars: Vec<Option<Handle>>,
    wallpaper: Option<Handle>,
    wallpaper_path: Option<PathBuf>,
    guard: Arc<Guard>,
}

impl Greeter {
    fn new(boot: Boot, shell_events: ShellReceiver) -> (Self, Task<Message>) {
        let now = Instant::now();
        let guard = Arc::new(Guard::open(Some(&boot.paths.cache)));
        let can_blank = boot
            .login
            .as_ref()
            .map_or(true, Flow::can_blank);
        let mut greeter = Self {
            theme: Theme::default(),
            login: boot.login,
            host: boot.host,
            faillock: boot.faillock,
            paths: boot.paths,
            surfaces: Vec::with_capacity(2),
            shell_events,
            requests: boot.requests,
            replies: Feed::new(boot.replies),
            hypr: boot.hypr,
            idle: Idle::new(boot.idle, now, can_blank),
            idle_pending: false,
            hovered: None,
            unix_now: unix_now(),
            ready: false,
            clock: Clock::now(),
            texts: Texts::default(),
            texts_from: 0,
            avatars: Vec::new(),
            wallpaper: None,
            wallpaper_path: boot.wallpaper,
            guard,
        };
        greeter.sync_texts();
        let poll = greeter.feed(Event::PollKey);
        let start = Task::batch([poll, greeter.run(vec![Action::RefreshFaillock])]);
        let idle = greeter.schedule_idle();
        let avatars = greeter.load_avatars();
        (greeter, Task::batch([start, idle, avatars]))
    }

    fn load_avatars(&self) -> Task<Message> {
        let Some(flow) = self.flow() else {
            return Task::none();
        };
        let paths: Vec<PathBuf> = flow
            .users()
            .iter()
            .map(|user| self.paths.avatar(user))
            .collect();
        let guard = Arc::clone(&self.guard);
        blocking(move || {
            let avatars = paths
                .iter()
                .map(|path| guard.load(path, pictures::AVATAR_MAX, pictures::avatar))
                .collect();
            Message::Avatars(avatars)
        })
    }

    fn load_wallpaper(&mut self, size: (i32, i32)) -> Task<Message> {
        let Some(path) = self.wallpaper_path.take() else {
            return Task::none();
        };
        let (width, height) = (
            size.0.max(1).unsigned_abs(),
            size.1.max(1).unsigned_abs(),
        );
        let guard = Arc::clone(&self.guard);
        blocking(move || {
            let decode = |_: &Path, bytes: &[u8]| pictures::wallpaper(bytes, width, height);
            Message::Wallpaper(guard.load(&path, pictures::WALLPAPER_MAX, decode))
        })
    }

    fn sync_texts(&mut self) {
        let Ok(flow) = &self.login else {
            return;
        };
        let mut hasher = DefaultHasher::new();
        flow.faillock().hash(&mut hasher);
        flow.index().hash(&mut hasher);
        flow.message().hash(&mut hasher);
        if flow.faillock().locked.is_some() {
            self.unix_now.hash(&mut hasher);
        }
        let from = hasher.finish();
        if from != self.texts_from || self.texts == Texts::default() {
            self.texts = Texts::new(flow, self.faillock.unlock_time, self.unix_now);
            self.texts_from = from;
        }
    }

    fn flow(&self) -> Option<&Flow> {
        self.login.as_ref().ok()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let now = Instant::now();
        let shown = self.visible_state();
        let polled = self.flow().is_some_and(Flow::polls_key);
        let task = match message {
            Message::Shell(event) => self.shell_event(&event),
            Message::Opened(id) => self.opened(id),
            Message::Input(input) => self.input(input, now),
            Message::Ui(ui) => self.ui(ui, now),
            Message::Flow(event) => self.feed(event),
            Message::KeyPoll => self.feed(Event::PollKey),
            Message::FaillockPoll => {
                self.unix_now = unix_now();
                self.run(vec![Action::RefreshFaillock])
            }
            Message::IdleFire => self.idle_fire(now),
            Message::BlankDone(sent) => self.blank_done(sent),
            Message::WakeDone(confirmed) => {
                self.idle.on_done(confirmed);
                Task::none()
            }
            Message::Minute => {
                self.clock = Clock::now();
                Task::none()
            }
            Message::Avatars(avatars) => {
                self.avatars = avatars;
                Task::none()
            }
            Message::Wallpaper(handle) => {
                self.wallpaper = handle;
                Task::none()
            }
            _ => Task::none(),
        };
        let mut tasks = vec![task];
        let flow = self.login.as_ref().ok();
        let can_blank = flow.is_none_or(Flow::can_blank);
        let polls = flow.is_some_and(Flow::polls_key);
        self.idle.allow(now, can_blank);
        if !polled && polls {
            tasks.push(self.feed(Event::PollKey));
        }
        self.sync_texts();
        tasks.push(self.schedule_idle());
        if self.visible_state() != shown {
            tasks.push(Task::done(Message::Redraw));
        }
        Task::batch(tasks)
    }

    /// A hash of everything the view draws. Widgets animate on their own once a change drew.
    fn visible_state(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hovered.hash(&mut hasher);
        self.surfaces.len().hash(&mut hasher);
        self.clock.hash(&mut hasher);
        self.texts.hash(&mut hasher);
        self.wallpaper
            .is_some()
            .hash(&mut hasher);
        for avatar in &self.avatars {
            avatar.is_some().hash(&mut hasher);
        }
        match &self.login {
            Ok(flow) => {
                flow.view().hash(&mut hasher);
                flow.pin_len().hash(&mut hasher);
                flow.index().hash(&mut hasher);
                flow.failed().hash(&mut hasher);
                flow.stalled().hash(&mut hasher);
                flow.busy().hash(&mut hasher);
                flow.message().hash(&mut hasher);
                flow.faillock().hash(&mut hasher);
                flow.leaving().hash(&mut hasher);
                flow.can_power().hash(&mut hasher);
            }
            Err(reason) => reason.hash(&mut hasher),
        }
        hasher.finish()
    }

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let login = match &self.login {
            Ok(flow) => Login::Ready { flow, texts: &self.texts, avatars: &self.avatars },
            Err(reason) => Login::Broken(reason),
        };
        let scene = Scene {
            theme: &self.theme,
            login,
            host: &self.host,
            clock: &self.clock,
            wallpaper: self.wallpaper.as_ref(),
            hovered: self.hovered,
        };
        deskshell_ui::primed(view::view(scene).map(Message::Ui))
    }

    fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style { background_color: view::BACKGROUND, text_color: theme.palette().text }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            self.shell_events
                .listen()
                .map(Message::Shell),
            window::open_events().map(Message::Opened),
            Subscription::run_with(self.replies.clone(), replies)
                .map(|reply| Message::Flow(Event::Greetd(reply))),
            iced::event::listen_with(input),
            Subscription::run(clock::minutes).map(|()| Message::Minute),
        ];
        if let Some(flow) = self.flow() {
            if flow.polls_key() {
                let every = if flow.key_present() { 2000 } else { 500 };
                subscriptions.push(
                    iced::time::every(Duration::from_millis(every)).map(|_| Message::KeyPoll),
                );
            }
            if flow.polls_faillock() {
                let every = if flow.faillock().locked.is_some() { 1 } else { 2 };
                subscriptions.push(
                    iced::time::every(Duration::from_secs(every)).map(|_| Message::FaillockPoll),
                );
            }
        }
        Subscription::batch(subscriptions)
    }

    fn shell_event(&mut self, event: &ShellEvent) -> Task<Message> {
        match event {
            ShellEvent::OutputAdded(output) => {
                if self
                    .surfaces
                    .iter()
                    .any(|(_, known)| *known == output.id)
                {
                    return Task::none();
                }
                info!(name = ?output.name, id = output.id, "output added, opening a greeter surface");
                let id = window::Id::unique();
                self.surfaces.push((id, output.id));
                let wallpaper = match output.logical_size {
                    Some(size) => self.load_wallpaper(size),
                    None => Task::none(),
                };
                let open = Task::done(Message::NewLayerShell {
                    settings: surface_settings(output.id),
                    id,
                });
                Task::batch([open, wallpaper])
            }
            // The toolkit closes the surfaces of a removed output itself.
            ShellEvent::OutputRemoved(output) => {
                info!(name = ?output.name, id = output.id, "output removed");
                self.surfaces
                    .retain(|(_, known)| *known != output.id);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn opened(&mut self, id: window::Id) -> Task<Message> {
        if !self
            .surfaces
            .iter()
            .any(|(known, _)| *known == id)
        {
            return Task::none();
        }
        if !self.ready {
            self.ready = true;
            if let Some(dir) = &self.paths.markers
                && let Err(err) = inputs::write_marker(dir, inputs::READY_MARKER)
            {
                warn!(%err, "cannot write the ready marker");
            }
        }
        Task::none()
    }

    fn input(&mut self, input: Input, now: Instant) -> Task<Message> {
        match input {
            Input::Moved(x, y) => {
                self.idle.pointer(now, (x, y));
                Task::none()
            }
            Input::Pressed => {
                self.idle.press(now);
                Task::none()
            }
            Input::Key(keyboard::Event::KeyPressed { key, text, modifiers, .. }) => {
                if self.idle.key(now) {
                    return Task::none();
                }
                let event = match key {
                    Key::Named(Named::Enter) => Event::Submit,
                    Key::Named(Named::Escape) => Event::Clear,
                    Key::Named(Named::Backspace) => Event::Backspace,
                    Key::Named(Named::Tab) if modifiers.shift() => Event::CycleUser(Step::Previous),
                    Key::Named(Named::Tab) => Event::CycleUser(Step::Next),
                    _ => {
                        if modifiers.control() || modifiers.alt() || modifiers.logo() {
                            return Task::none();
                        }
                        let Some(text) = text else {
                            return Task::none();
                        };
                        let mut actions = Vec::new();
                        if let Ok(flow) = &mut self.login {
                            for c in text.chars() {
                                actions.extend(flow.handle(Event::Type(c)));
                            }
                        }
                        return self.run(actions);
                    }
                };
                self.feed(event)
            }
            Input::Key(_) => Task::none(),
        }
    }

    fn ui(&mut self, ui: Ui, now: Instant) -> Task<Message> {
        self.idle.press(now);
        match ui {
            Ui::SelectUser(index) => self.feed(Event::SelectUser(index)),
            Ui::Hover(index) => {
                self.hovered = index;
                Task::none()
            }
            Ui::Submit => self.feed(Event::Submit),
            Ui::Power(power) => match &mut self.login {
                Ok(_) => self.feed(Event::Power(power)),
                Err(_) => self.run(vec![Action::Power(power)]),
            },
        }
    }

    fn feed(&mut self, event: Event) -> Task<Message> {
        let actions = match &mut self.login {
            Ok(flow) => flow.handle(event),
            Err(_) => Vec::new(),
        };
        self.run(actions)
    }

    fn run(&self, actions: Vec<Action>) -> Task<Message> {
        let mut tasks = Vec::with_capacity(actions.len());
        for action in actions {
            tasks.push(match action {
                Action::CheckKey(check) => {
                    let hidraw = self.paths.hidraw.clone();
                    blocking(move || {
                        let present = key::key_present(&hidraw);
                        Message::Flow(Event::KeyChecked { check, present })
                    })
                }
                Action::Greetd(outgoing) => {
                    if self.requests.send(outgoing).is_err() {
                        warn!("the greetd connection task is gone");
                    }
                    Task::none()
                }
                Action::RefreshFaillock => self.refresh_faillock(),
                Action::Arm(timer) => Task::perform(
                    async move { tokio::time::sleep(timer.delay()).await },
                    move |()| Message::Flow(Event::Timer(timer)),
                ),
                Action::SaveLastUser(user) => {
                    let cache = self.paths.cache.clone();
                    blocking(move || {
                        if let Err(err) = inputs::write_last_user(&cache, &user) {
                            warn!(%err, "cannot save the last user");
                        }
                        Message::Done
                    })
                }
                Action::Launched => {
                    if let Some(dir) = &self.paths.markers
                        && let Err(err) = inputs::write_marker(dir, inputs::LAUNCHED_MARKER)
                    {
                        warn!(%err, "cannot write the launched marker");
                    }
                    info!("greetd accepted the session, exiting");
                    iced::exit()
                }
                Action::Power(power) => {
                    let [program, verb] = power.argv();
                    info!(verb, "power button");
                    blocking(move || {
                        if let Err(err) = Command::new(program).arg(verb).status() {
                            warn!(%err, program, verb, "cannot run the power command");
                        }
                        Message::Done
                    })
                }
            });
        }
        Task::batch(tasks)
    }

    fn refresh_faillock(&self) -> Task<Message> {
        let Some(flow) = self.flow() else {
            return Task::none();
        };
        let user = flow.user().to_owned();
        let config = Arc::clone(&self.faillock);
        blocking(move || {
            let tally = faillock::read_tally(&config.dir, &user);
            let status = faillock::evaluate(&config, &tally, unix_now());
            Message::Flow(Event::Faillock { user, status })
        })
    }

    fn schedule_idle(&mut self) -> Task<Message> {
        let Some(deadline) = self
            .idle
            .deadline()
            .filter(|_| !self.idle_pending)
        else {
            return Task::none();
        };
        self.idle_pending = true;
        Task::perform(
            async move { tokio::time::sleep_until(deadline.into()).await },
            |()| Message::IdleFire,
        )
    }

    fn idle_fire(&mut self, now: Instant) -> Task<Message> {
        self.idle_pending = false;
        if !self.idle.fire(now) {
            return Task::none();
        }
        info!("idle, blanking the screen");
        if let Ok(flow) = &mut self.login {
            flow.handle(Event::Blank);
        }
        match self.hypr.clone() {
            Some(hypr) => Task::perform(
                async move { hypr.dispatch(BLANK).await },
                |result| {
                    if let Err(err) = &result {
                        warn!(%err, "cannot blank the screen");
                    }
                    Message::BlankDone(result.is_ok())
                },
            ),
            // No Hyprland to blank, but the next key is still swallowed.
            None => Task::done(Message::BlankDone(true)),
        }
    }

    fn blank_done(&mut self, confirmed: bool) -> Task<Message> {
        if !self.idle.off_done(confirmed) {
            return Task::none();
        }
        info!(
            confirmed,
            "turning the screen back on after the blank"
        );
        match self.hypr.clone() {
            Some(hypr) => Task::perform(
                async move { hypr.dispatch(WAKE).await },
                |result| {
                    if let Err(err) = &result {
                        warn!(%err, "cannot turn the screen back on");
                    }
                    Message::WakeDone(result.is_ok())
                },
            ),
            None => Task::done(Message::WakeDone(true)),
        }
    }
}

fn surface_settings(output: u32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: LayerSize::FILL,
        layer: Layer::Overlay,
        anchor: Anchor::all(),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        output_option: OutputOption::GlobalName(output),
        namespace: Some(String::from(NAMESPACE)),
        blur_option: BlurOption::None,
        ..NewLayerShellSettings::default()
    }
}

fn input(event: IcedEvent, _status: iced::event::Status, _window: window::Id) -> Option<Message> {
    let input = match event {
        IcedEvent::Keyboard(event @ keyboard::Event::KeyPressed { .. }) => Input::Key(event),
        IcedEvent::Mouse(mouse::Event::CursorMoved { position }) => {
            Input::Moved(position.x, position.y)
        }
        IcedEvent::Mouse(mouse::Event::ButtonPressed(_))
        | IcedEvent::Touch(touch::Event::FingerPressed { .. }) => Input::Pressed,
        _ => return None,
    };
    Some(Message::Input(input))
}

fn blocking(f: impl FnOnce() -> Message + Send + 'static) -> Task<Message> {
    Task::perform(
        async move { tokio::task::spawn_blocking(f).await },
        |result| result.unwrap_or(Message::Done),
    )
}

fn replies(feed: &Feed<mpsc::UnboundedReceiver<Reply>>) -> impl Stream<Item = Reply> + use<> {
    stream::iter(feed.take()).flat_map(|replies| {
        stream::unfold(replies, |mut replies| async move {
            let reply = replies.recv().await?;
            Some((reply, replies))
        })
    })
}

/// `Subscription::run_with` needs `Hash` data, so this hashes a receiver by identity and
/// the stream takes it out once.
#[derive(Debug)]
struct Feed<T>(Arc<std::sync::Mutex<Option<T>>>);

impl<T> Feed<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(value))))
    }

    fn take(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl<T> Clone for Feed<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Hash for Feed<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0)
            .cast::<()>()
            .hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn idle_override() {
        assert_eq!(parse_idle(None), idle::TIMEOUT);
        assert_eq!(parse_idle(Some("20")), Duration::from_secs(20));
        assert_eq!(parse_idle(Some(" 7 ")), Duration::from_secs(7));
        assert_eq!(parse_idle(Some("0")), idle::TIMEOUT);
        assert_eq!(parse_idle(Some("soon")), idle::TIMEOUT);
    }

    #[test]
    fn effects() {
        assert_eq!(BLANK, r#"hl.dsp.dpms("off")"#);
        assert_eq!(WAKE, r#"hl.dsp.dpms("on")"#);
        let settings = surface_settings(7);
        assert_eq!(
            settings.namespace.as_deref(),
            Some("deskshell-greeter")
        );
        assert_matches!(settings.layer, Layer::Overlay);
        assert_matches!(
            settings.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        );
        assert_matches!(
            settings.output_option,
            OutputOption::GlobalName(7)
        );
    }

    #[test]
    fn a_broken_or_empty_config_is_a_reason() {
        let dir = env::temp_dir().join(format!("deskshell-greeter-app-{}", process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Paths::system(None);
        paths.config = dir.join("greeter.toml");
        paths.mappings = dir.join("u2f_mappings");
        paths.cache = dir.clone();
        assert_eq!(
            login(&paths).unwrap_err(),
            "/etc/deskshell/greeter.toml is missing"
        );
        std::fs::write(
            &paths.config,
            "[session]\ncommand = [\"Hyprland\"]\n",
        )
        .unwrap();
        assert_eq!(login(&paths).unwrap_err(), NO_USERS);
        std::fs::write(&paths.mappings, "alice:kh,pk\nbob:kh,pk\n").unwrap();
        inputs::write_last_user(&dir, "bob").unwrap();
        let (flow, wallpaper) = login(&paths).unwrap();
        assert_eq!(wallpaper, None);
        assert_eq!(flow.users(), ["alice", "bob"]);
        assert_eq!(flow.user(), "bob");
        assert!(flow.registered());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
