//! The running shell: startup, the surfaces and the wiring between the services and iced.

use std::cell::Cell;
use std::env;
use std::hash::{Hash, Hasher};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell_apps::{Dirs, Locale, Snapshot};
use deskshell_hypr::Notice;
use deskshell_hypr::{Client, Paths, State, Store};
use deskshell_ipc::{self as ipc, BindError, Reply, Request, Server};
use deskshell_notify::{
    Command as NotifyCommand, Effect as NotifyEffect, Service as NotifyService,
    State as NotifyState, Timer,
};
use deskshell_system::systemd::Systemd;
use deskshell_ui::focus_grab::{self, Grab, Grabber};
use deskshell_ui::fonts;
use deskshell_ui::theme::{self, Theme};
use iced::futures::{Stream, StreamExt, stream};
use iced::keyboard::{self, key};
use iced::widget::{Space, stack};
use iced::{Color, Element, Event, Subscription, Task, window};
use iced_exwlshell::build_pattern::daemon;
use iced_exwlshell::redraw::Scope;
use iced_exwlshell::reexport::{
    Anchor, BlurOption, KeyboardInteractivity, Layer, LayerSize, Margin, NewLayerShellSettings,
    OutputOption,
};
use iced_exwlshell::settings::{ExWlSettings, LayerShellSettings, StartMode};
use iced_exwlshell::shell::{self as wl_shell, ExWlOutputInfo, ShellEvent, ShellReceiver};
use iced_exwlshell::to_exwlshell_message;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};
use tracing_log::LogTracer;
use tracing_log::log::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::uptime;

use crate::capture::Captures;
use crate::feed::Feed;
use crate::panel::calendar;
use crate::panel::clipboard;
use crate::panel::launcher::{self, Bangs, bangs, usage};
use crate::panel::share;
use crate::panel::{self, Contents, Effect, Env};
use crate::panels::{self, Manager, Op, Open, Panel, Route};
use crate::screenshot::clearance::{Clearance, Fade};
use crate::toasts::wiring;
use crate::toasts::{self, Toasts};
use crate::updates;
use crate::watchdog::Watchdog;
use crate::{EXIT_FAILED, clock, panic, probe, runtime, view};
use crate::{bar, services};
use crate::{pickers, screenshot};

mod auth_link;
mod bars;
mod clearance_link;
pub(crate) mod clipboard_link;
mod hub;
mod install;
mod media;
mod screenshot_link;
mod share_link;

const LOG_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,usvg=error";
// Popovers sit 6 px under the bar.
const POPOVER_TOP: i32 = 60;
const UPDATES_HEIGHT: u32 = panel::updates::HEIGHT;
const GPU_POLL: Duration = Duration::from_millis(100);
const NOTIFY_SHUTDOWN: Duration = Duration::from_secs(3);
const AUTH_SHUTDOWN: Duration = Duration::from_secs(3);

pub fn run() -> ExitCode {
    // SAFETY: the shell path calls this first, before anything has started a thread.
    let forced_env = unsafe { force_vulkan() };
    // SAFETY: as above.
    let watchdog = unsafe { Watchdog::take() };
    panic::install();
    init_logging();
    let watchdog = match watchdog {
        Some(Ok(watchdog)) => {
            info!(
                interval_ms = watchdog.interval().as_millis(),
                "systemd watchdog on"
            );
            Some(watchdog)
        }
        Some(Err(err)) => {
            warn!(%err, "cannot open the notify socket, so systemd's watchdog will stop the shell");
            None
        }
        None => None,
    };
    remember_binary();

    if !has_wayland() {
        eprintln!("No Wayland display: WAYLAND_DISPLAY and WAYLAND_SOCKET are not set.");
        return ExitCode::from(EXIT_FAILED);
    }
    let socket = match ipc::socket_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let server = match Server::bind(&socket) {
        Ok(server) => server,
        Err(BindError::InUse) => {
            eprintln!("deskshell is already running");
            return ExitCode::from(EXIT_FAILED);
        }
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let runtime = match runtime::get() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("cannot start the async runtime: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };

    let (requests, store, hypr_state, hypr_client) = {
        let _context = runtime.enter();
        let requests = server.spawn(ipc::DEFAULT_HANDLER_TIMEOUT);
        match Paths::from_env() {
            Ok(paths) => {
                let client = Client::new(paths);
                let (store, state, _notices) = Store::spawn(client.clone());
                (requests, Some(store), Some(state), Some(client))
            }
            Err(err) => {
                warn!(%err, "running without Hyprland state");
                (requests, None, None, None)
            }
        }
    };

    let theme_path = theme::default_path();
    let theme = match &theme_path {
        Some(path) => Theme::load(path).unwrap_or_else(|err| {
            warn!(path = %path.display(), "theme file rejected, using the defaults: {err}");
            Theme::default()
        }),
        None => {
            warn!("neither XDG_CONFIG_HOME nor HOME is set, theme.toml is not read");
            Theme::default()
        }
    };
    info!(
        socket = %socket.display(),
        hyprland = store.is_some(),
        "deskshell starts"
    );
    let apps = start_apps(&theme);
    let launcher = start_launcher(runtime);
    let (notify_service, notify) = match start_notify(runtime) {
        Some((service, state, effects)) => {
            let commands = service.commands();
            (
                Some(service),
                Some(NotifyLink { commands, state, effects }),
            )
        }
        None => (None, None),
    };
    let signals = {
        let _context = runtime.enter();
        Signals::install()
    };
    let (bar_links, bar_services) = services::start(runtime);
    let (auth, auth_services) = crate::auth::start(&forced_env, runtime);
    let (clipboard_link, clipboard) = {
        let _context = runtime.enter();
        clipboard_link::ClipboardLink::start()
    };
    let holidays = calendar::feed::start();

    // Before the daemon opens iced's device, so the device can import dmabuf thumbnails.
    let hooked = deskshell_wl::install_device_hook();
    info!(hooked, "device hook for dmabuf thumbnails");
    let (shell_broadcast, shell_events) = wl_shell::channel();
    let boot = Cell::new(Some(Boot {
        theme,
        theme_path: theme_path.map(Arc::from),
        shell_events,
        requests,
        store,
        hypr_state,
        hypr_client,
        forced_env,
        apps,
        launcher,
        notify,
        signals,
        bar: bar_links,
        auth,
        clipboard_link,
        clipboard,
        holidays,
        watchdog,
    }));
    let mut app = daemon(
        move || {
            let mut shell = Shell::new(
                boot.take()
                    .expect("iced boots the shell once"),
            );
            let prewarm =
                Task::batch([shell.prewarm_launcher(), shell.bar_boot(), shell.layer_fade_query()]);
            (shell, prewarm)
        },
        "deskshell",
        Shell::update,
        Shell::view,
    )
    .wl_settings(ExWlSettings {
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..LayerShellSettings::default()
        },
        shell_broadcast,
        ..ExWlSettings::default()
    })
    .subscription(Shell::subscription)
    .style(Shell::style)
    .theme(|_: &Shell, _| iced::Theme::Dark)
    .executor::<runtime::Executor>()
    .redraw_scope(redraw_scope)
    .default_font(fonts::PLEX);
    for font in fonts::all() {
        app = app.font(*font);
    }
    let result = app.run();
    // Auth goes first, so polkit unregisters before the slower stops. The timeouts are made inside
    // the blocks, where the runtime's timer is in context.
    if runtime
        .block_on(async { tokio::time::timeout(AUTH_SHUTDOWN, auth_services.shutdown()).await })
        .is_err()
    {
        warn!("the polkit agent did not stop in time");
    }
    if let Some(service) = notify_service
        && runtime
            .block_on(async { tokio::time::timeout(NOTIFY_SHUTDOWN, service.shutdown()).await })
            .is_err()
    {
        warn!("the notification service did not stop in time");
    }
    runtime.block_on(bar_services.shutdown());
    info!(frames = probe::total(), "deskshell exits");
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(%err, "event loop failed");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// Returns the variables it set, which programs the shell starts must not inherit.
///
/// # Safety
///
/// No other thread may exist yet, because it writes the environment.
unsafe fn force_vulkan() -> Vec<&'static str> {
    let mut set = Vec::with_capacity(2);
    for (key, value) in [("WGPU_BACKEND", "vulkan"), ("ICED_BACKEND", "wgpu")] {
        if env::var_os(key).is_none() {
            // SAFETY: the caller guarantees that no other thread runs yet.
            unsafe { env::set_var(key, value) };
            set.push(key);
        }
    }
    set
}

fn remember_binary() {
    match env::current_exe().map(PathBuf::into_os_string) {
        Ok(path) => match path.into_string() {
            Ok(path) => panel::remember_binary(path),
            Err(path) => warn!(
                ?path,
                "the binary path is not UTF-8, launches use PATH"
            ),
        },
        Err(err) => warn!(%err, "no path for the binary, launches use PATH"),
    }
}

// The `log` bridge stays at Warn, because iced_wgpu enumerates every adapter when the `log`
// level is Info or more.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(LOG_FILTER));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(uptime())
        .with_thread_names(true)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    let _ = LogTracer::builder()
        .with_max_level(LevelFilter::Warn)
        .init();
}

fn has_wayland() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()))
}

fn start_apps(theme: &Theme) -> Option<deskshell_apps::Service> {
    let config = deskshell_apps::Config {
        dirs: Dirs::from_env(),
        locale: Locale::from_env(),
        icon_theme: theme.icons.theme.to_owned(),
    };
    match deskshell_apps::Service::spawn(config) {
        Ok(service) => Some(service),
        Err(err) => {
            warn!(%err, "the app index did not start, the launcher lists no apps");
            None
        }
    }
}

struct LauncherBoot {
    shared: launcher::Shared,
    writer: Option<Arc<usage::Writer>>,
    bangs: Option<watch::Receiver<Arc<Bangs>>>,
}

fn start_launcher(runtime: &tokio::runtime::Runtime) -> LauncherBoot {
    let mut boot = LauncherBoot { shared: launcher::Shared::default(), writer: None, bangs: None };
    boot.shared.config = launcher::Config::from_env();
    match usage::Paths::from_env() {
        Some(paths) => {
            boot.shared.usage = usage::load(&paths);
            boot.writer = Some(Arc::new(usage::Writer::new(paths.file)));
        }
        None => warn!("neither XDG_STATE_HOME nor HOME is set, launch counts are not kept"),
    }
    match bangs::Paths::from_env() {
        Some(paths) => {
            let _context = runtime.enter();
            boot.bangs = Some(bangs::spawn(
                paths,
                String::from(bangs::SOURCE_URL),
                Arc::new(bangs::fetch_url),
            ));
        }
        None => warn!("neither XDG_CACHE_HOME nor HOME is set, no bang list"),
    }
    info!(
        qalc = boot.shared.config.qalc.is_some(),
        launches = boot.shared.usage.launches.len(),
        "launcher ready"
    );
    boot
}

fn start_notify(
    runtime: &tokio::runtime::Runtime,
) -> Option<(
    NotifyService,
    watch::Receiver<NotifyState>,
    mpsc::UnboundedReceiver<NotifyEffect>,
)> {
    let config = wiring::config(
        env::var_os("XDG_STATE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    );
    let state_path = config.state_path.clone();
    let started = runtime.block_on(async {
        let conn = zbus::Connection::session()
            .await
            .map_err(deskshell_notify::Error::from)?;
        NotifyService::start(&conn, config).await
    });
    match started {
        Ok(started) => {
            info!(
                state = ?state_path.as_deref().map(Path::display),
                "notification service started"
            );
            Some(started)
        }
        Err(err) => {
            warn!(%err, "the notification service did not start");
            None
        }
    }
}

struct NotifyLink {
    commands: mpsc::UnboundedSender<NotifyCommand>,
    state: watch::Receiver<NotifyState>,
    effects: mpsc::UnboundedReceiver<NotifyEffect>,
}

struct Signals {
    terminate: Signal,
    interrupt: Signal,
}

impl Signals {
    // Needs the runtime's context.
    fn install() -> Option<Self> {
        let installed = signal(SignalKind::terminate()).and_then(|terminate| {
            Ok(Self { terminate, interrupt: signal(SignalKind::interrupt())? })
        });
        match installed {
            Ok(signals) => Some(signals),
            Err(err) => {
                warn!(%err, "no signal handler, SIGTERM skips the exit path");
                None
            }
        }
    }
}

struct Boot {
    theme: Theme,
    theme_path: Option<Arc<Path>>,
    shell_events: ShellReceiver,
    requests: mpsc::Receiver<Request>,
    store: Option<Store>,
    hypr_state: Option<watch::Receiver<State>>,
    hypr_client: Option<Client>,
    forced_env: Vec<&'static str>,
    apps: Option<deskshell_apps::Service>,
    launcher: LauncherBoot,
    notify: Option<NotifyLink>,
    signals: Option<Signals>,
    bar: services::Links,
    auth: crate::auth::Boot,
    clipboard_link: clipboard_link::ClipboardLink,
    clipboard: clipboard::Config,
    holidays: Option<watch::Receiver<Arc<calendar::Holidays>>>,
    watchdog: Option<Watchdog>,
}

#[cfg(test)]
impl Boot {
    // No compositor and no services, so a test shell reaches nothing of the session.
    fn inert() -> Self {
        let (_, shell_events) = iced_exwlshell::shell::channel();
        let (_, requests) = mpsc::channel(1);
        let (clipboard_link, clipboard) = clipboard_link::ClipboardLink::off();
        Self {
            theme: Theme::default(),
            theme_path: None,
            shell_events,
            requests,
            store: None,
            hypr_state: None,
            hypr_client: None,
            forced_env: Vec::new(),
            apps: None,
            launcher: LauncherBoot {
                shared: launcher::Shared::default(),
                writer: None,
                bangs: None,
            },
            notify: None,
            signals: None,
            bar: services::Links::default(),
            auth: crate::auth::Boot::default(),
            clipboard_link,
            clipboard,
            holidays: None,
            watchdog: None,
        }
    }
}

#[to_exwlshell_message]
#[derive(Debug)]
pub enum Message {
    Shell(ShellEvent),
    Opened(window::Id),
    Closed(window::Id),
    Ipc(Request),
    Minute,
    Theme(theme::Update),
    Escape(window::Id),
    Grab(focus_grab::Event),
    Grabbed(window::Id, Option<Grab>),
    Panel(window::Id, panel::Message),
    Apps(Arc<Snapshot>),
    Bangs(Arc<Bangs>),
    Prewarm(panel::Message),
    Capture(deskshell_wl::Event),
    CheckGpu,
    Watchdog,
    RefreshWindows,
    NotifyChanged,
    NotifyEffects(Vec<NotifyEffect>),
    FocusTimer(Timer),
    Urgent(u64),
    Toast(window::Id, toasts::Message),
    Toasts(toasts::Message),
    ToastMonitor(Option<String>),
    Quit,
    Holidays(Arc<calendar::Holidays>),
    Shot(window::Id, screenshot::Message),
    Shots(screenshot::Message),
    Screenshot(screenshot_link::Event),
    Bar(window::Id, bar::Message),
    TrayMenu(window::Id, bar::menu::Message),
    Bars(bars::Event),
    MediaPlayers,
    HubChanged(hub::Change),
    Updates(updates::Event),
    Auth(window::Id, crate::auth::Message),
    AuthEvent(crate::auth::Event),
    Clipboard(clipboard_link::Change),
    ShareHangup(u64),
    LayerFade(Fade),
    ConfigReloaded,
    SnapshotWake(u64),
    SnapshotReady { generation: u64, locked: bool },
}

// Only messages that change what a surface shows redraw, so an idle bar draws once a minute.
fn redraw_scope(message: &Message) -> Scope {
    match message {
        Message::Theme(theme::Update::Changed(_)) => Scope::All,
        Message::Panel(id, _) => Scope::Window(*id),
        Message::Toast(id, message) if message.redraws() => Scope::Window(*id),
        Message::Shot(id, message) if message.redraws() => Scope::Window(*id),
        Message::Bar(id, message) if message.redraws() => Scope::Window(*id),
        Message::TrayMenu(id, _) => Scope::Window(*id),
        Message::Auth(id, message) if message.redraws() => Scope::Window(*id),
        _ => Scope::None,
    }
}

#[derive(Debug)]
struct ToastSurface {
    id: window::Id,
    output: Option<u32>,
    monitor: Option<String>,
    height: u32,
}

// Hashed by the toasts surface it serves, so a moved surface subscribes anew.
#[derive(Debug, Clone)]
struct MonitorSource {
    receiver: watch::Receiver<State>,
    surface: window::Id,
}

impl Hash for MonitorSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
    }
}

#[derive(Debug)]
struct Output {
    id: u32,
    name: Option<String>,
    logical: Option<(i32, i32)>,
}

impl Output {
    fn from_info(info: &ExWlOutputInfo) -> Self {
        Self { id: info.id, name: info.name.clone(), logical: info.logical_size }
    }
}

struct Shell {
    theme: Theme,
    theme_path: Option<Arc<Path>>,
    shell_events: ShellReceiver,
    outputs: Vec<Output>,
    bars: Vec<bar::Surface>,
    panels: Manager<window::Id>,
    hypr_state: Option<watch::Receiver<State>>,
    hypr_feed: Feed<watch::Receiver<State>>,
    requests: Feed<mpsc::Receiver<Request>>,
    store: Option<Store>,
    hypr_client: Option<Client>,
    forced_env: Vec<&'static str>,
    contents: Contents,
    effects: Vec<Effect>,
    grabber: Option<Grabber>,
    grab: Option<Grab>,
    grabs: u64,
    apps_service: Option<deskshell_apps::Service>,
    apps: Arc<Snapshot>,
    apps_feed: Feed<watch::Receiver<Arc<Snapshot>>>,
    icon_theme: &'static str,
    bangs: Arc<Bangs>,
    bangs_feed: Feed<watch::Receiver<Arc<Bangs>>>,
    launcher: launcher::Shared,
    usage_writer: Option<Arc<usage::Writer>>,
    usage_saves: u64,
    clipboard: clipboard::Config,
    clipboard_link: clipboard_link::ClipboardLink,
    captures: Captures,
    capture_events: Feed<deskshell_wl::Events>,
    notify: Option<mpsc::UnboundedSender<NotifyCommand>>,
    notify_state: Option<watch::Receiver<NotifyState>>,
    notify_feed: Feed<watch::Receiver<NotifyState>>,
    notify_effects: Feed<mpsc::UnboundedReceiver<NotifyEffect>>,
    urgent_feed: Feed<broadcast::Receiver<Notice>>,
    reload_feed: Feed<broadcast::Receiver<Notice>>,
    signals: Feed<Signals>,
    toasts: Toasts,
    toast_surface: Option<ToastSurface>,
    toast_requests: Vec<toasts::Request>,
    holidays: Feed<watch::Receiver<Arc<calendar::Holidays>>>,
    watchdog: Option<Watchdog>,
    share_screens: Vec<share::Screen>,
    share_reply: pickers::ShareReply,
    selector: screenshot::Selector,
    shot_requests: Vec<screenshot::Request>,
    shots: screenshot_link::Shots,
    clearance: Clearance,
    snapshot: Option<clearance_link::PendingSnapshot>,
    snapshot_generation: u64,
    bar: bars::BarState,
    media: media::MediaLink,
    hub: hub::HubState,
    systemd: Option<Systemd>,
    updates: updates::Link,
    auth: auth_link::AuthLink,
}

/// The session counts as locked while the unit runs or restarts, because the compositor keeps a
/// crashed lock's session locked. That errs toward locked, the safe side.
pub const LOCK_UNIT: &str = "deskshell-lock.service";

async fn session_locked(systemd: &Systemd) -> zbus::Result<bool> {
    systemd
        .unit_state(LOCK_UNIT)
        .await
        .map(|state| state.running_or_restarting())
}

impl Shell {
    fn new(mut boot: Boot) -> Self {
        let icon_theme = boot.theme.icons.theme;
        let notify = boot.notify.take();
        let urgent = match (&notify, &boot.store) {
            (Some(_), Some(store)) => Some(store.subscribe()),
            _ => None,
        };
        let reload = boot
            .store
            .as_ref()
            .map(Store::subscribe);
        let (notify, notify_state, notify_feed, notify_effects) = match notify {
            Some(link) => (
                Some(link.commands),
                Some(link.state.clone()),
                Some(link.state),
                Some(link.effects),
            ),
            None => (None, None, None, None),
        };
        let signals = boot.signals.take();
        let systemd = boot.bar.systemd.take();
        // Bars open from the OutputAdded events, which the receiver replays for the outputs
        // that exist at startup.
        let hypr_feed = boot.hypr_state.clone();
        let bar = bars::BarState::new(
            std::mem::take(&mut boot.bar),
            boot.store.as_ref(),
            notify_state.as_ref(),
            boot.apps.as_ref(),
        );
        let media = media::MediaLink::new(&bar);
        Self {
            theme: boot.theme,
            theme_path: boot.theme_path,
            shell_events: boot.shell_events,
            outputs: Vec::with_capacity(4),
            bars: Vec::with_capacity(4),
            panels: Manager::default(),
            hypr_state: boot.hypr_state,
            hypr_feed: Feed::new(hypr_feed),
            requests: Feed::new(Some(boot.requests)),
            store: boot.store,
            hypr_client: boot.hypr_client,
            forced_env: boot.forced_env,
            contents: Contents::default(),
            effects: Vec::with_capacity(4),
            grabber: None,
            grab: None,
            grabs: 0,
            apps: boot
                .apps
                .as_ref()
                .map_or_else(Default::default, deskshell_apps::Service::current),
            apps_feed: Feed::new(
                boot.apps
                    .as_ref()
                    .map(deskshell_apps::Service::subscribe),
            ),
            apps_service: boot.apps,
            icon_theme,
            bangs: Arc::default(),
            bangs_feed: Feed::new(boot.launcher.bangs),
            launcher: boot.launcher.shared,
            usage_writer: boot.launcher.writer,
            usage_saves: 0,
            clipboard: boot.clipboard,
            clipboard_link: boot.clipboard_link,
            captures: Captures::default(),
            capture_events: Feed::new(None),
            notify,
            notify_state,
            notify_feed: Feed::new(notify_feed),
            notify_effects: Feed::new(notify_effects),
            urgent_feed: Feed::new(urgent),
            reload_feed: Feed::new(reload),
            signals: Feed::new(signals),
            toasts: Toasts::default(),
            toast_surface: None,
            toast_requests: Vec::with_capacity(4),
            holidays: Feed::new(boot.holidays),
            watchdog: boot.watchdog,
            share_screens: Vec::with_capacity(4),
            share_reply: pickers::ShareReply::default(),
            selector: screenshot::Selector::default(),
            shot_requests: Vec::with_capacity(8),
            shots: screenshot_link::Shots::new(),
            clearance: Clearance::default(),
            snapshot: None,
            snapshot_generation: 0,
            bar,
            media,
            hub: hub::HubState::default(),
            updates: updates::Link::start(systemd.as_ref()),
            systemd,
            auth: auth_link::AuthLink::new(boot.auth),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shell(event) => {
                let screens = self.track_screens(&event);
                Task::batch([screens, self.shell_event(event)])
            }
            Message::Opened(id) => self.opened(id),
            Message::Closed(id) => {
                self.clearance
                    .closed(id, Instant::now());
                self.closed(id);
                self.screenshot_closed(id)
            }
            Message::Ipc(request) => self.request(request),
            Message::Screenshot(event) => self.screenshot_event(event),
            Message::Minute => self.bar_minute(),
            Message::Theme(update) => {
                if update.apply(&mut self.theme) {
                    info!("theme applied");
                }
                self.sync_icon_theme();
                Task::none()
            }
            Message::Escape(id) if self.selector.is_surface(id) => {
                self.shot_message(Some(id), screenshot::Message::Cancel)
            }
            Message::Escape(id) if self.bar_is_menu(id) => self.bar_dismiss_menu(),
            Message::Escape(id) => match self.panels.dismiss(id) {
                Some(id) => self.close_panel(id),
                None => Task::none(),
            },
            Message::Grab(event) => self.grab_event(event),
            Message::Grabbed(id, grab) => {
                self.grabbed(id, grab);
                Task::none()
            }
            Message::Panel(id, message) => self.panel_message(id, message),
            Message::Apps(snapshot) => {
                self.apps = snapshot;
                let toasts = self.notify_changed();
                let launcher = match self.panels.current() {
                    Some(open) if open.panel == Panel::Launcher => self.refresh_launcher(),
                    _ => self.prewarm_launcher(),
                };
                Task::batch([toasts, launcher])
            }
            Message::Prewarm(message) => self.prewarmed(message),
            Message::Bangs(bangs) => {
                self.bangs = bangs;
                self.refresh_launcher()
            }
            Message::Capture(event) => self.capture_event(event),
            Message::CheckGpu => {
                self.start_capture();
                Task::none()
            }
            Message::Watchdog => {
                if let Some(watchdog) = &mut self.watchdog {
                    watchdog.ping(Instant::now());
                }
                #[cfg(feature = "test-hooks")]
                crate::watchdog::test_stall();
                Task::none()
            }
            Message::RefreshWindows => {
                if let Some(store) = &self.store {
                    store.refresh();
                }
                Task::none()
            }
            Message::NotifyChanged => self.notify_changed(),
            Message::NotifyEffects(effects) => self.notify_effects(effects),
            Message::FocusTimer(timer) => {
                self.focus_timer(timer);
                Task::none()
            }
            Message::Urgent(address) => {
                self.urgent(address);
                Task::none()
            }
            Message::Toast(id, message) => self.toast_message(Some(id), message),
            Message::Toasts(message) => self.toast_message(None, message),
            Message::ToastMonitor(monitor) => self.toast_monitor(monitor),
            Message::Quit => {
                info!("signal received, leaving the event loop");
                self.snapshot_cancel("the shell exits");
                iced::exit()
            }
            Message::Holidays(holidays) => {
                self.contents
                    .calendar
                    .set_holidays(holidays);
                match self.panels.current() {
                    Some(open) if open.panel == Panel::Calendar => Task::done(Message::Panel(
                        open.id,
                        panel::Message::Calendar(calendar::Message::Refresh),
                    )),
                    _ => Task::none(),
                }
            }
            Message::Shot(id, message) => self.shot_message(Some(id), message),
            Message::Shots(message) => self.shot_message(None, message),
            Message::Bar(id, message) => self.bar_message(id, message),
            Message::TrayMenu(id, message) => self.bar_menu_message(id, message),
            Message::Bars(event) => self.bar_event(event),
            Message::MediaPlayers => self.media_players(),
            Message::HubChanged(change) => self.hub_changed(change),
            Message::Updates(event) => self.updates_event(event),
            Message::Auth(id, message) => self.auth_message(id, message),
            Message::AuthEvent(event) => self.updates_auth_event(event),
            Message::Clipboard(change) => self.clipboard_change(change),
            Message::ShareHangup(pick) => self.share_hangup(pick),
            Message::LayerFade(fade) => {
                self.layer_fade(fade);
                Task::none()
            }
            Message::ConfigReloaded => self.layer_fade_query(),
            Message::SnapshotWake(generation) => self.snapshot_wake(generation),
            Message::SnapshotReady { generation, locked } => {
                self.snapshot_ready(generation, locked)
            }
            _ => Task::none(),
        }
    }

    fn send_notify(&self, command: NotifyCommand) {
        match &self.notify {
            Some(commands) => {
                let _ = commands.send(command);
            }
            None => debug!(?command, "no notification service, dropped"),
        }
    }

    fn notify_changed(&mut self) -> Task<Message> {
        let Some(receiver) = &self.notify_state else {
            return Task::none();
        };
        let (changed, loads) = {
            let state = receiver.borrow();
            self.toasts.sync(
                &state.popups,
                &self.theme,
                &self.apps,
                Instant::now(),
            )
        };
        let surface = self.sync_toast_surface();
        let redraw = if changed { self.toast_redraw() } else { Task::none() };
        Task::batch([loads.map(Message::Toasts), surface, redraw])
    }

    fn toast_redraw(&self) -> Task<Message> {
        match &self.toast_surface {
            Some(surface) => Task::done(Message::Toast(
                surface.id,
                toasts::Message::Redraw,
            )),
            None => Task::none(),
        }
    }

    fn sync_toast_surface(&mut self) -> Task<Message> {
        match (&self.toast_surface, self.toasts.is_empty()) {
            (None, false) => self.open_toasts(),
            (Some(surface), true) => {
                let id = surface.id;
                info!("toasts surface closes");
                self.toast_surface = None;
                Task::done(Message::RemoveWindow(id))
            }
            _ => Task::none(),
        }
    }

    fn open_toasts(&mut self) -> Task<Message> {
        let monitor = self
            .hypr_state
            .as_ref()
            .and_then(|receiver| {
                receiver
                    .borrow()
                    .focused_monitor
                    .clone()
            });
        let output = panels::choose_output(
            monitor.as_deref(),
            self.outputs
                .iter()
                .map(|output| (output.id, output.name.as_deref())),
        );
        let id = window::Id::unique();
        let height = toast_height_guess(&self.toasts);
        info!(monitor, "toasts surface opens");
        self.toast_surface = Some(ToastSurface { id, output, monitor, height });
        Task::done(Message::NewLayerShell { settings: toast_settings(output, height), id })
    }

    fn toast_message(
        &mut self,
        from: Option<window::Id>,
        message: toasts::Message,
    ) -> Task<Message> {
        if let toasts::Message::Height(height) = message {
            return self.toast_height(from, height);
        }
        let mut requests = std::mem::take(&mut self.toast_requests);
        let changed = self.toasts.update(
            message,
            &self.theme,
            Instant::now(),
            &mut requests,
        );
        let redraw = if from.is_none() && changed { self.toast_redraw() } else { Task::none() };
        if requests.is_empty() {
            self.toast_requests = requests;
            return redraw;
        }
        let mut tasks = Vec::with_capacity(requests.len() + 1);
        tasks.push(redraw);
        for request in requests.drain(..) {
            tasks.push(self.toast_request(request));
        }
        self.toast_requests = requests;
        Task::batch(tasks)
    }

    fn toast_request(&mut self, request: toasts::Request) -> Task<Message> {
        debug!(?request, "toasts request");
        match request {
            toasts::Request::Notify(command) => {
                self.send_notify(command);
                Task::none()
            }
            toasts::Request::ShowHub => self.panel_command(Panel::Hub, Op::Show),
            toasts::Request::OpenLink(url) => {
                spawn(&panel::open_url(url), None, &self.forced_env);
                Task::none()
            }
        }
    }

    fn toast_height(&mut self, from: Option<window::Id>, height: f32) -> Task<Message> {
        let Some(surface) = &mut self.toast_surface else {
            return Task::none();
        };
        if from.is_some_and(|id| id != surface.id) {
            return Task::none();
        }
        let height = (height.ceil() as u32).max(1);
        if surface.height == height {
            return Task::none();
        }
        surface.height = height;
        Task::done(Message::LayoutChange {
            id: surface.id,
            anchor: Anchor::Top | Anchor::Right,
            size: LayerSize::px(toasts::WIDTH as u32, height),
        })
    }

    fn toast_monitor(&mut self, monitor: Option<String>) -> Task<Message> {
        let Some(surface) = &mut self.toast_surface else {
            return Task::none();
        };
        if surface.monitor == monitor {
            return Task::none();
        }
        let output = panels::choose_output(
            monitor.as_deref(),
            self.outputs
                .iter()
                .map(|output| (output.id, output.name.as_deref())),
        );
        if output.is_none() || output == surface.output {
            surface.monitor = monitor;
            return Task::none();
        }
        info!(monitor, "the toasts follow the focused monitor");
        self.toasts.surface_lost(Instant::now());
        let old = surface.id;
        let height = surface.height;
        let id = window::Id::unique();
        self.toast_surface = Some(ToastSurface { id, output, monitor, height });
        Task::batch([
            Task::done(Message::RemoveWindow(old)),
            Task::done(Message::NewLayerShell { settings: toast_settings(output, height), id }),
        ])
    }

    fn toasts_output_removed(&mut self, output: u32) -> Task<Message> {
        if self
            .toast_surface
            .as_ref()
            .is_some_and(|surface| surface.output == Some(output))
        {
            self.toast_surface = None;
            self.toasts.surface_lost(Instant::now());
            return self.sync_toast_surface();
        }
        Task::none()
    }

    // Dispatches in a row go out in one task, so they reach Hyprland in order.
    fn notify_effects(&mut self, effects: Vec<NotifyEffect>) -> Task<Message> {
        let mut tasks = Vec::with_capacity(effects.len());
        let mut lua: Vec<String> = Vec::new();
        for effect in effects {
            match effect {
                NotifyEffect::Dispatch(line) => lua.push(line),
                NotifyEffect::RefreshWindows => {
                    if lua.is_empty() {
                        if let Some(store) = &self.store {
                            store.refresh();
                        }
                    } else {
                        tasks.push(self.dispatches(std::mem::take(&mut lua), true));
                    }
                }
                NotifyEffect::Schedule { timer, after } => {
                    if !lua.is_empty() {
                        tasks.push(self.dispatches(std::mem::take(&mut lua), false));
                    }
                    tasks.push(Task::future(async move {
                        tokio::time::sleep(after).await;
                        Message::FocusTimer(timer)
                    }));
                }
                NotifyEffect::Action { owner, key } => {
                    tasks.push(self.notification_action(&owner, &key));
                }
            }
        }
        if !lua.is_empty() {
            tasks.push(self.dispatches(lua, false));
        }
        Task::batch(tasks)
    }

    fn notification_action(&mut self, owner: &str, key: &str) -> Task<Message> {
        if owner == updates::OWNER {
            return self.updates_action(key);
        }
        if owner == screenshot::notice::OWNER {
            return self.screenshot_action(key);
        }
        warn!(
            owner,
            key, "no handler for this notification action"
        );
        Task::none()
    }

    fn focus_timer(&self, timer: Timer) {
        let command = match &self.hypr_state {
            Some(receiver) => wiring::focus_timer(timer, &receiver.borrow()),
            None => NotifyCommand::FocusTimer { timer, window: None, monitors: Vec::new() },
        };
        self.send_notify(command);
    }

    fn urgent(&self, address: u64) {
        let Some(receiver) = &self.hypr_state else {
            return;
        };
        let command = wiring::urgent(&receiver.borrow(), address);
        if let Some(command) = command {
            self.send_notify(command);
        }
    }

    fn refresh_launcher(&self) -> Task<Message> {
        match self.panels.current() {
            Some(open) if open.panel == Panel::Launcher => Task::done(Message::Panel(
                open.id,
                panel::Message::Launcher(launcher::Message::Refresh),
            )),
            _ => Task::none(),
        }
    }

    fn prewarm_launcher(&mut self) -> Task<Message> {
        if self
            .panels
            .current()
            .is_some_and(|open| open.panel == Panel::Launcher)
        {
            return Task::none();
        }
        let env = Env {
            theme: &self.theme,
            apps: &self.apps,
            bangs: &self.bangs,
            launcher: &self.launcher,
            hypr: self.hypr_client.as_ref(),
            clipboard: &self.clipboard,
            hypr_state: self.hypr_state.as_ref(),
            capture: Some(&self.captures),
            screens: &self.share_screens,
            players: &self.media.players,
            hub: Some(
                self.hub
                    .inputs(self.bar.bars.keep_awake),
            ),
            updates: Some(&self.updates.view),
        };
        self.contents
            .prewarm(Panel::Launcher, &env)
            .map(Message::Prewarm)
    }

    fn prewarmed(&mut self, message: panel::Message) -> Task<Message> {
        if let Some(id) = self.media_prewarmed(&message) {
            return Task::done(Message::Panel(id, message));
        }
        if let Some(open) = self.panels.current()
            && open.panel == Panel::Launcher
        {
            return self.panel_message(open.id, message);
        }
        let env = Env {
            theme: &self.theme,
            apps: &self.apps,
            bangs: &self.bangs,
            launcher: &self.launcher,
            hypr: self.hypr_client.as_ref(),
            clipboard: &self.clipboard,
            hypr_state: self.hypr_state.as_ref(),
            capture: Some(&self.captures),
            screens: &self.share_screens,
            players: &self.media.players,
            hub: Some(
                self.hub
                    .inputs(self.bar.bars.keep_awake),
            ),
            updates: Some(&self.updates.view),
        };
        let task = self
            .contents
            .update(message, &env, &mut self.effects)
            .map(Message::Prewarm);
        if !self.effects.is_empty() {
            warn!(effects = ?self.effects, "a closed panel asked for effects, dropped");
            self.effects.clear();
        }
        task
    }

    fn sync_icon_theme(&mut self) {
        let name = self.theme.icons.theme;
        if name == self.icon_theme {
            return;
        }
        self.icon_theme = name;
        if let Some(service) = &self.apps_service {
            info!(theme = name, "icon theme changes");
            service.set_icon_theme(name);
        }
    }

    fn save_usage(&mut self) -> Task<Message> {
        let Some(writer) = &self.usage_writer else {
            return Task::none();
        };
        self.usage_saves += 1;
        let sequence = self.usage_saves;
        let json = self.launcher.usage.to_json();
        let writer = Arc::clone(writer);
        Task::future(async move {
            let saved = tokio::task::spawn_blocking(move || {
                writer
                    .save(sequence, &json)
                    .map_err(|err| (writer.path().to_path_buf(), err))
            })
            .await;
            if let Ok(Err((path, err))) = saved {
                warn!(path = %path.display(), %err, "cannot write the launcher counts");
            }
        })
        .discard()
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let content = if let Some(content) = self.bar_view(id) {
            content
        } else if let Some(panel) = self.panels.panel_of(id) {
            match self.contents.view(panel, &self.env()) {
                Some(content) => Element::from(crate::auth::inert::inert(
                    content,
                    self.auth_has_keyboard(),
                ))
                .map(move |message| Message::Panel(id, message)),
                None => view::panel(&self.theme, panel, card_size(panel)),
            }
        } else if self
            .toast_surface
            .as_ref()
            .is_some_and(|surface| surface.id == id)
        {
            toasts::view(&self.toasts, &self.theme).map(move |message| Message::Toast(id, message))
        } else if self.selector.is_surface(id) {
            screenshot::view(&self.selector, id, &self.theme)
                .map(move |message| Message::Shot(id, message))
        } else if self.selector.is_pill(id) {
            screenshot::pill(&self.selector, &self.theme)
                .map(move |message| Message::Shot(id, message))
        } else if let Some(content) = self.auth_view(id) {
            content
        } else {
            Space::new().into()
        };
        let content =
            if probe::enabled() { stack![content, probe::probe(id)].into() } else { content };
        deskshell_ui::primed(content)
    }

    fn style(&self, _theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: self.theme.colors.text_primary,
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = Vec::with_capacity(10);
        subscriptions.extend([
            self.shell_events
                .listen()
                .map(Message::Shell),
            window::open_events().map(Message::Opened),
            window::close_events().map(Message::Closed),
            escape(),
            Subscription::run_with(self.requests.clone(), requests).map(Message::Ipc),
            Subscription::run(focus_grab::events).map(Message::Grab),
            Subscription::run_with(self.apps_feed.clone(), changes::<Arc<Snapshot>>)
                .map(Message::Apps),
            Subscription::run_with(self.bangs_feed.clone(), changes::<Arc<Bangs>>)
                .map(Message::Bangs),
            Subscription::run_with(self.capture_events.clone(), capture_events)
                .map(Message::Capture),
            Subscription::run_with(
                self.clipboard_link.events.clone(),
                clipboard_link::events,
            )
            .map(|event| Message::Clipboard(clipboard_link::Change::Watcher(event))),
        ]);
        if self.captures.needs_gpu() {
            subscriptions.push(iced::time::every(GPU_POLL).map(|_| Message::CheckGpu));
        }
        if let Some(watchdog) = &self.watchdog {
            subscriptions.push(iced::time::every(watchdog.interval()).map(|_| Message::Watchdog));
        }
        subscriptions.push(Subscription::run(clock::minutes).map(|()| Message::Minute));
        subscriptions.push(
            Subscription::run_with(self.hypr_feed.clone(), bars::units::<State>)
                .map(|()| Message::Bars(bars::Event::Hypr)),
        );
        // A selector surface takes the keyboard, and panel keys must not act under it.
        // Neither may they under an auth surface.
        if let Some(open) = self
            .panels
            .current()
            .filter(|_| !self.selector.has_keyboard() && !self.auth_has_keyboard())
        {
            subscriptions.push(
                self.contents
                    .subscription(open.panel)
                    .with(open.id)
                    .map(|(id, message)| Message::Panel(id, message)),
            );
        }
        if let Some(path) = &self.theme_path {
            subscriptions.push(
                Subscription::run_with(Arc::clone(path), |path| {
                    theme::watch(path.to_path_buf())
                })
                .map(Message::Theme),
            );
        }
        subscriptions.extend([
            Subscription::run_with(self.signals.clone(), signal_stream).map(|()| Message::Quit),
            Subscription::run_with(
                self.reload_feed.clone(),
                clearance_link::reload_notices,
            )
            .map(|()| Message::ConfigReloaded),
        ]);
        if self.notify.is_some() {
            subscriptions.extend([
                Subscription::run_with(self.notify_feed.clone(), notify_changes)
                    .map(|()| Message::NotifyChanged),
                Subscription::run_with(self.notify_effects.clone(), notify_effects)
                    .map(Message::NotifyEffects),
                Subscription::run_with(self.urgent_feed.clone(), urgent_notices)
                    .map(Message::Urgent),
            ]);
        }
        if let Some(surface) = &self.toast_surface {
            if self.toasts.ticking() {
                subscriptions.push(
                    iced::time::every(toasts::TICK)
                        .with(surface.id)
                        .map(|(id, at)| Message::Toast(id, toasts::Message::Tick(at))),
                );
            }
            if self.toasts.animating() {
                subscriptions.push(
                    window::frames()
                        .with(surface.id)
                        .map(|(id, at)| Message::Toast(id, toasts::Message::Frame(at))),
                );
            }
            if let Some(receiver) = &self.hypr_state {
                let source = MonitorSource { receiver: receiver.clone(), surface: surface.id };
                subscriptions.push(
                    Subscription::run_with(source, focused_monitor_changes)
                        .map(Message::ToastMonitor),
                );
            }
        }
        subscriptions.push(
            Subscription::run_with(
                self.holidays.clone(),
                changes::<Arc<calendar::Holidays>>,
            )
            .map(Message::Holidays),
        );
        if self.selector.ticking() {
            let tick = iced::time::every(screenshot::TICK);
            subscriptions.push(match self.selector.pill_id() {
                Some(pill) => tick
                    .with(pill)
                    .map(|(id, _)| Message::Shot(id, screenshot::Message::Tick)),
                None => tick.map(|_| Message::Shots(screenshot::Message::Tick)),
            });
        }
        if self.selector.has_keyboard() {
            subscriptions.push(
                iced::event::listen_with(screenshot::key)
                    .map(|(id, message)| Message::Shot(id, message)),
            );
        }
        subscriptions.extend(self.bar_subscriptions());
        subscriptions.push(self.media_subscription());
        subscriptions.extend(self.hub_subscriptions());
        subscriptions.extend(self.updates_subscriptions());
        subscriptions.extend(self.auth_subscriptions());
        Subscription::batch(subscriptions)
    }

    fn request(&mut self, request: Request) -> Task<Message> {
        let request = match self.picker_request(request) {
            Ok(task) => return task,
            Err(request) => request,
        };
        match panels::route(request.is_cancelled(), &request.command) {
            Route::Skip => {
                debug!(command = ?request.command, "skipped a call that timed out");
                Task::none()
            }
            Route::Panel(target, op) => {
                let task = self.panel_command(target.panel(), op);
                request.reply(Reply::Unit);
                task
            }
            Route::RefreshWindows => {
                if let Some(store) = &self.store {
                    store.refresh();
                }
                request.reply(Reply::Unit);
                Task::none()
            }
            Route::Drop => {
                debug!(command = ?request.command, "dropped a call this shell does not serve");
                Task::none()
            }
            route @ (Route::ToggleDnd | Route::ClearNotifications) => {
                match (&self.notify, wiring::route_command(route)) {
                    (Some(_), Some(command)) => {
                        self.send_notify(command);
                        request.reply(Reply::Unit);
                    }
                    _ => debug!(
                        command = ?request.command,
                        "no notification service, dropped the call"
                    ),
                }
                Task::none()
            }
            Route::ToggleKeepAwake => {
                request.reply(Reply::Unit);
                self.toggle_keep_awake()
            }
            Route::MediaControl(call) => {
                self.bar_media_call(call);
                request.reply(Reply::Unit);
                Task::none()
            }
            Route::RefreshUpdates => {
                self.bar_refresh_updates();
                request.reply(Reply::Unit);
                Task::none()
            }
            Route::Lock => {
                self.snapshot_cancel("the session locks");
                let Some(systemd) = self.systemd.clone() else {
                    warn!("no systemd user manager, `lock lock` fails");
                    return Task::none();
                };
                // The reply waits for the lock unit's start job, which ends once the compositor
                // confirmed the lock. A failed start fails the call, so no hook sees a false lock.
                Task::future(async move {
                    match systemd
                        .start_unit_wait(LOCK_UNIT, "replace", ipc::LOCK_WAIT)
                        .await
                    {
                        Ok(()) => request.reply(Reply::Unit),
                        Err(err) => {
                            warn!(%err, "{LOCK_UNIT} did not come up");
                            request.fail(format!("The lock did not come up: {err}."));
                        }
                    }
                })
                .discard()
            }
            Route::IsLocked => {
                let Some(systemd) = self.systemd.clone() else {
                    warn!("no systemd user manager, `lock isLocked` fails");
                    return Task::none();
                };
                Task::future(async move {
                    match session_locked(&systemd).await {
                        Ok(locked) => request.reply(Reply::Bool(locked)),
                        Err(err) => warn!(%err, "cannot read the state of {LOCK_UNIT}"),
                    }
                })
                .discard()
            }
            Route::ClipboardCopyEpoch => {
                let epoch = *self.clipboard_link.requested.borrow();
                request.reply(Reply::Text(epoch.to_string()));
                Task::none()
            }
            Route::NotificationCount => {
                match &self.notify_state {
                    Some(state) => {
                        let count = state.borrow().history.len();
                        request.reply(Reply::Text(count.to_string()));
                    }
                    None => debug!("no notification service, dropped `notifications count`"),
                }
                Task::none()
            }
            Route::UpdatesStart => {
                let task = self.updates_request(updates::Request::Start);
                if self.updates.runs() {
                    request.reply(Reply::Unit);
                }
                task
            }
            Route::UpdatesCancel => self.updates_ipc_cancel(request),
            Route::Display(call) => {
                self.bar_display_call(call);
                request.reply(Reply::Unit);
                Task::none()
            }
            Route::CpuPower(call) => {
                self.bar_cpu_power_call(call);
                request.reply(Reply::Unit);
                Task::none()
            }
        }
    }

    fn picker_request(&mut self, request: Request) -> Result<Task<Message>, Request> {
        let Some(route) = pickers::route(request.is_cancelled(), &request.command) else {
            return Err(request);
        };
        let request = match self.auth_refuse_picker(route, request) {
            Ok(()) => return Ok(Task::none()),
            Err(request) => request,
        };
        let task = match (route, &request.command) {
            (pickers::Route::Skip, _) => {
                debug!(command = ?request.command, "skipped a picker call that timed out");
                return Ok(Task::none());
            }
            (pickers::Route::SharePick, _) => self.share_pick(request),
            (pickers::Route::Screenshot(mode), _) => self.screenshot_trigger(mode, Some(request)),
        };
        Ok(task)
    }

    fn shot_message(
        &mut self,
        from: Option<window::Id>,
        message: screenshot::Message,
    ) -> Task<Message> {
        let mut requests = std::mem::take(&mut self.shot_requests);
        if self.auth_cancels_selection(&message) {
            self.selector.cancel(&mut requests);
        } else {
            self.selector
                .update(from, message, &mut requests);
        }
        let task = self.shot_requests(&mut requests);
        self.shot_requests = requests;
        self.share_region_sweep();
        task
    }

    fn shot_requests(&mut self, requests: &mut Vec<screenshot::Request>) -> Task<Message> {
        if requests.is_empty() {
            return Task::none();
        }
        let mut tasks = Vec::with_capacity(requests.len());
        for request in requests.drain(..) {
            debug!(?request, "screenshot request");
            match request {
                screenshot::Request::Open { id, output } => {
                    tasks.push(Task::done(Message::NewLayerShell {
                        settings: selector_settings(output),
                        id,
                    }));
                }
                screenshot::Request::OpenPill { id, output } => {
                    tasks.push(Task::done(Message::NewLayerShell {
                        settings: pill_settings(output),
                        id,
                    }));
                }
                screenshot::Request::Close(id) => {
                    self.clearance
                        .closing(id, Instant::now(), false);
                    tasks.push(Task::done(Message::RemoveWindow(id)));
                }
                screenshot::Request::Convert { generation, surface, output } => {
                    tasks.push(self.screenshot_convert(generation, surface, &output));
                }
                screenshot::Request::Chosen(run, choice) => {
                    tasks.push(self.screenshot_chosen(run, choice));
                }
            }
        }
        Task::batch(tasks)
    }

    // Outputs without a name cannot be named in a reply and are left out.
    fn track_screens(&mut self, event: &ShellEvent) -> Task<Message> {
        let changed = match event {
            ShellEvent::OutputAdded(info) | ShellEvent::OutputUpdated(info) => {
                let (width, height) = info.logical_size.unwrap_or((0, 0));
                let screen = info
                    .name
                    .as_ref()
                    .map(|name| share::Screen {
                        output: info.id,
                        name: name.clone(),
                        model: info.model.clone(),
                        width,
                        height,
                    });
                let known = self
                    .share_screens
                    .iter()
                    .position(|screen| screen.output == info.id);
                match (known, screen) {
                    (Some(index), Some(screen)) if self.share_screens[index] == screen => false,
                    (Some(index), Some(screen)) => {
                        self.share_screens[index] = screen;
                        true
                    }
                    (None, Some(screen)) => {
                        self.share_screens.push(screen);
                        true
                    }
                    (Some(index), None) => {
                        self.share_screens.remove(index);
                        true
                    }
                    (None, None) => false,
                }
            }
            ShellEvent::OutputRemoved(info) => {
                let before = self.share_screens.len();
                self.share_screens
                    .retain(|screen| screen.output != info.id);
                before != self.share_screens.len()
            }
            _ => false,
        };
        match self.panels.current() {
            Some(open) if changed && open.panel == Panel::SharePicker => {
                Task::done(Message::Panel(
                    open.id,
                    panel::Message::Share(share::Message::Screens),
                ))
            }
            _ => Task::none(),
        }
    }

    fn panel_command(&mut self, panel: Panel, op: Op) -> Task<Message> {
        if self.media_refused(panel, op)
            || self.updates_refused(panel, op)
            || self.auth_refuses_panel(panel, op)
        {
            return Task::none();
        }
        let output = {
            let state = self
                .hypr_state
                .as_ref()
                .map(watch::Receiver::borrow);
            panels::choose_output(
                state
                    .as_ref()
                    .and_then(|state| state.focused_monitor.as_deref()),
                self.outputs
                    .iter()
                    .map(|output| (output.id, output.name.as_deref())),
            )
        };
        let actions = self
            .panels
            .command(panel, op, output, window::Id::unique);
        let close = actions
            .close
            .map(|id| self.close_panel(id));
        let open = actions
            .open
            .map(|open| self.open_panel(open));
        Task::batch(close.into_iter().chain(open))
    }

    fn env(&self) -> Env<'_> {
        Env {
            theme: &self.theme,
            apps: &self.apps,
            bangs: &self.bangs,
            launcher: &self.launcher,
            hypr: self.hypr_client.as_ref(),
            clipboard: &self.clipboard,
            hypr_state: self.hypr_state.as_ref(),
            capture: Some(&self.captures),
            screens: &self.share_screens,
            players: &self.media.players,
            hub: Some(
                self.hub
                    .inputs(self.bar.bars.keep_awake),
            ),
            updates: Some(&self.updates.view),
        }
    }

    fn panel_message(&mut self, id: window::Id, message: panel::Message) -> Task<Message> {
        if self.panels.panel_of(id).is_none() {
            debug!(?id, "dropped a message for a panel that closed");
            return Task::none();
        }
        let env = Env {
            theme: &self.theme,
            apps: &self.apps,
            bangs: &self.bangs,
            launcher: &self.launcher,
            hypr: self.hypr_client.as_ref(),
            clipboard: &self.clipboard,
            hypr_state: self.hypr_state.as_ref(),
            capture: Some(&self.captures),
            screens: &self.share_screens,
            players: &self.media.players,
            hub: Some(
                self.hub
                    .inputs(self.bar.bars.keep_awake),
            ),
            updates: Some(&self.updates.view),
        };
        let task = self
            .contents
            .update(message, &env, &mut self.effects)
            .map(move |message| Message::Panel(id, message));
        let mut tasks = Vec::with_capacity(1 + self.effects.len());
        tasks.push(task);
        let mut effects = std::mem::take(&mut self.effects);
        for effect in effects.drain(..) {
            tasks.push(self.run_effect(id, effect));
        }
        self.effects = effects;
        self.hub_effects();
        Task::batch(tasks)
    }

    fn run_effect(&mut self, id: window::Id, effect: Effect) -> Task<Message> {
        match effect {
            Effect::Close => match self.panels.dismiss(id) {
                Some(id) => self.close_panel(id),
                None => Task::none(),
            },
            Effect::Show(panel) => self.panel_command(panel, Op::Show),
            Effect::Spawn { argv, cwd } => {
                spawn(&argv, cwd.as_deref(), &self.forced_env);
                Task::none()
            }
            Effect::SpawnSettled(argv) => self.snapshot_queue(argv),
            Effect::Dispatch(lua) => match &self.hypr_client {
                Some(client) => {
                    let client = client.clone();
                    Task::future(async move {
                        if let Err(err) = client.dispatch(&lua).await {
                            warn!(%err, lua, "dispatch failed");
                        }
                    })
                    .discard()
                }
                None => {
                    warn!(lua, "no Hyprland, dispatch dropped");
                    Task::none()
                }
            },
            Effect::CountLaunch(id) => {
                self.launcher.usage.count_launch(&id);
                self.save_usage()
            }
            Effect::CountBang(trigger) => {
                self.launcher.usage.count_bang(&trigger);
                self.save_usage()
            }
            Effect::ToggleDnd => {
                self.send_notify(NotifyCommand::ToggleDnd);
                Task::none()
            }
            Effect::ClearNotifications => {
                self.send_notify(NotifyCommand::Clear);
                Task::none()
            }
            Effect::ToggleKeepAwake => self.toggle_keep_awake(),
            Effect::Pipe { argv, input, then } => pipe(id, &argv, input, then, &self.forced_env),
            Effect::Dispatches { lua, refresh } => self.dispatches(lua, refresh),
            Effect::RefreshWindows => {
                if let Some(store) = &self.store {
                    store.refresh();
                }
                Task::none()
            }
            Effect::Capture(addresses) => {
                self.captures.want(&addresses);
                self.start_capture();
                Task::none()
            }
            Effect::Mpris(command) => {
                self.send_mpris(command);
                Task::none()
            }
            Effect::ShareReply(line) => self.share_choice(line),
            Effect::CaptureTargets(targets) => {
                self.captures.want_targets(&targets);
                self.start_capture();
                Task::none()
            }
            Effect::Resize => self.media_resize(id),
            Effect::Updates(request) => self.updates_request(request),
            Effect::Clipboard(request) => self.clipboard_request(id, request),
            Effect::Screenshot(mode) => self.screenshot_trigger(mode, None),
        }
    }

    fn open_panel(&mut self, open: Open<window::Id>) -> Task<Message> {
        self.snapshot_cancel("a panel opened");
        if let Some(command) = wiring::panel_opened(open.panel) {
            self.send_notify(command);
        }
        let output = open.output.and_then(|id| {
            self.outputs
                .iter()
                .find(|output| output.id == id)
        });
        info!(
            panel = open.panel.title(),
            output = output.and_then(|output| output.name.as_deref()),
            "panel opens"
        );
        // When the compositor picks the output, size for the smallest one it may pick.
        let logical = match output {
            Some(output) => output.logical,
            None => self
                .outputs
                .iter()
                .filter_map(|output| output.logical)
                .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))),
        };
        self.media_opening(open.panel, open.id, logical);
        if open.panel == Panel::Hub {
            self.hub_copy_all();
        }
        let id = open.id;
        let content = self
            .contents
            .opened(
                open.panel,
                &Env {
                    theme: &self.theme,
                    apps: &self.apps,
                    bangs: &self.bangs,
                    launcher: &self.launcher,
                    hypr: self.hypr_client.as_ref(),
                    clipboard: &self.clipboard,
                    hypr_state: self.hypr_state.as_ref(),
                    capture: Some(&self.captures),
                    screens: &self.share_screens,
                    players: &self.media.players,
                    hub: Some(
                        self.hub
                            .inputs(self.bar.bars.keep_awake),
                    ),
                    updates: Some(&self.updates.view),
                },
            )
            .map(move |message| Message::Panel(id, message));
        self.hub_effects();
        Task::batch([
            Task::done(Message::NewLayerShell {
                settings: panel_settings(open.panel, open.output, logical),
                id,
            }),
            content,
            self.bar_highlights(),
            self.bar_dismiss_menu(),
        ])
    }

    fn close_panel(&mut self, id: window::Id) -> Task<Message> {
        self.clearance
            .closing(id, Instant::now(), true);
        self.release_grab();
        self.captures.stop_all();
        self.contents.release(
            self.panels
                .current()
                .map(|open| open.panel),
        );
        self.hub_effects();
        self.share_closed();
        Task::batch([Task::done(Message::RemoveWindow(id)), self.bar_highlights()])
    }

    fn shell_event(&mut self, event: ShellEvent) -> Task<Message> {
        match event {
            ShellEvent::OutputAdded(info) | ShellEvent::OutputUpdated(info) => {
                let output = Output::from_info(&info);
                info!(name = ?output.name, id = output.id, size = ?output.logical, "output");
                match self
                    .outputs
                    .iter_mut()
                    .find(|known| known.id == output.id)
                {
                    Some(known) => *known = output,
                    None => self.outputs.push(output),
                }
                let renamed = self.bar_outputs_changed();
                let has_bar = self
                    .bars
                    .iter()
                    .any(|bar| bar.output == info.id);
                if has_bar {
                    return renamed;
                }
                Task::batch([renamed, self.open_bar(info.id)])
            }
            ShellEvent::OutputRemoved(info) => {
                info!(name = ?info.name, id = info.id, "output removed");
                self.outputs
                    .retain(|output| output.id != info.id);
                // The toolkit closes the surfaces that were only on this output, so they
                // are forgotten here without a RemoveWindow.
                self.bars
                    .retain(|bar| bar.output != info.id);
                let shot = self.screenshot_output_removed(info.id);
                if let Some(id) = self.panels.output_removed(info.id) {
                    info!(?id, "the open panel's output went away");
                    self.release_grab();
                    self.captures.stop_all();
                    self.share_closed();
                }
                let bars = self.bar_output_removed(info.id);
                let auth = self.auth_output_removed(info.id);
                Task::batch([self.toasts_output_removed(info.id), bars, auth, shot])
            }
            _ => Task::none(),
        }
    }

    fn opened(&mut self, id: window::Id) -> Task<Message> {
        self.start_capture();
        match self.panels.panel_of(id) {
            Some(panel) if panel.focus_grab() => self.request_grab(id),
            _ if self.bar_wants_grab(id) => self.request_grab(id),
            _ if self.auth_owns(id) => self.auth_opened(id),
            _ => Task::none(),
        }
    }

    fn closed(&mut self, id: window::Id) {
        if self.panels.closed(id) {
            info!(?id, "the open panel was closed by the toolkit");
            self.release_grab();
            self.captures.stop_all();
            self.share_closed();
            self.contents.release(
                self.panels
                    .current()
                    .map(|open| open.panel),
            );
            self.hub_effects();
        }
        self.bars.retain(|bar| bar.id != id);
        self.bar_closed(id);
        if self
            .toast_surface
            .as_ref()
            .is_some_and(|surface| surface.id == id)
        {
            info!(
                ?id,
                "the toasts surface was closed by the toolkit"
            );
            self.toast_surface = None;
            self.toasts.surface_lost(Instant::now());
        }
        self.auth_closed(id);
    }

    fn request_grab(&mut self, id: window::Id) -> Task<Message> {
        let Some(grabber) = self.grabber.clone() else {
            return Task::none();
        };
        self.grabs += 1;
        let serial = self.grabs;
        window::run(id, move |window| {
            grabber.grab_window(window, serial)
        })
        .map(move |grab| Message::Grabbed(id, grab))
    }

    fn grabbed(&mut self, id: window::Id, grab: Option<Grab>) {
        let Some(grab) = grab else {
            warn!(
                ?id,
                "no wl_surface for the panel, focus grab skipped"
            );
            return;
        };
        let wanted = self
            .panels
            .panel_of(id)
            .is_some_and(Panel::focus_grab)
            || self.bar_wants_grab(id);
        match &self.grabber {
            Some(grabber) if !wanted || self.grab.is_some() => grabber.release(&grab),
            Some(_) => {
                debug!(?id, serial = grab.serial(), "focus grab active");
                self.grab = Some(grab);
            }
            None => {}
        }
    }

    fn grab_event(&mut self, event: focus_grab::Event) -> Task<Message> {
        match event {
            focus_grab::Event::Ready(grabber) => {
                self.grabber = Some(grabber);
                match self.panels.current() {
                    Some(open) if open.panel.focus_grab() && self.grab.is_none() => {
                        self.request_grab(open.id)
                    }
                    _ => Task::none(),
                }
            }
            focus_grab::Event::Unavailable(reason) => {
                warn!(
                    reason,
                    "hyprland_focus_grab_v1 unavailable, popovers stay open on outside clicks"
                );
                Task::none()
            }
            focus_grab::Event::Cleared(serial) => {
                if self
                    .grab
                    .as_ref()
                    .is_none_or(|grab| grab.serial() != serial)
                {
                    return Task::none();
                }
                self.release_grab();
                match self
                    .panels
                    .current()
                    .map(|open| open.id)
                    .and_then(|id| self.panels.dismiss(id))
                {
                    Some(id) => {
                        info!("outside click, closing the panel");
                        self.close_panel(id)
                    }
                    None => self.bar_grab_cleared(),
                }
            }
        }
    }

    fn start_capture(&mut self) {
        if self.captures.is_running() {
            return;
        }
        let Some(gpu) = deskshell_wl::Gpu::from_iced() else {
            return;
        };
        info!(
            dmabuf = gpu.dmabuf_possible(),
            reason = gpu.dmabuf_unavailable(),
            adapter = gpu.adapter_name(),
            hook_devices = deskshell_wl::hook_devices(),
            "iced's device for captures"
        );
        let (capture, events) = deskshell_wl::Capture::spawn(gpu);
        self.capture_events = Feed::new(Some(events));
        self.captures.attach(capture);
    }

    fn capture_event(&mut self, event: deskshell_wl::Event) -> Task<Message> {
        self.captures.event(event);
        match (event, self.panels.current()) {
            (deskshell_wl::Event::Frame(_) | deskshell_wl::Event::Gone(_), Some(open))
                if open.panel == Panel::Overview =>
            {
                Task::done(Message::Panel(
                    open.id,
                    panel::Message::Overview(panel::overview::Message::Frame),
                ))
            }
            (deskshell_wl::Event::Frame(_) | deskshell_wl::Event::Gone(_), Some(open))
                if open.panel == Panel::SharePicker =>
            {
                Task::done(Message::Panel(
                    open.id,
                    panel::Message::Share(share::Message::Frame),
                ))
            }
            _ => Task::none(),
        }
    }

    fn dispatches(&self, lua: Vec<String>, refresh: bool) -> Task<Message> {
        let Some(client) = &self.hypr_client else {
            warn!(?lua, "no Hyprland, dispatches dropped");
            return Task::none();
        };
        let client = client.clone();
        let run = Task::future(async move {
            for lua in &lua {
                if let Err(err) = client.dispatch(lua).await {
                    warn!(%err, lua, "dispatch failed");
                }
            }
        });
        if refresh { run.map(|()| Message::RefreshWindows) } else { run.discard() }
    }

    fn release_grab(&mut self) {
        if let (Some(grabber), Some(grab)) = (&self.grabber, self.grab.take()) {
            grabber.release(&grab);
        }
    }
}

// Detached in its own process group, without the variables the shell forced for itself.
fn spawn(argv: &[String], cwd: Option<&Path>, forced_env: &[&str]) {
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let runtime = match runtime::get() {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(%err, program, "no runtime, spawn dropped");
            return;
        }
    };
    let _context = runtime.enter();
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for key in forced_env {
        command.env_remove(key);
    }
    match command.spawn() {
        Ok(mut child) => {
            info!(program, "spawned");
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(err) => warn!(%err, program, "spawn failed"),
    }
}

fn pipe(
    id: window::Id,
    argv: &[String],
    input: Vec<u8>,
    then: Option<panel::Then>,
    forced_env: &[&str],
) -> Task<Message> {
    use tokio::io::AsyncWriteExt as _;

    let Some((program, args)) = argv.split_first() else {
        return Task::none();
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    for key in forced_env {
        command.env_remove(key);
    }
    let program = program.clone();
    let run = async move {
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!(%err, program, "spawn failed");
                return;
            }
        };
        info!(program, "spawned with input");
        if let Some(mut stdin) = child.stdin.take()
            && let Err(err) = stdin.write_all(&input).await
        {
            warn!(%err, program, "cannot write the input");
        }
        let _ = child.wait().await;
    };
    match then {
        Some(then) => Task::future(run).map(move |()| Message::Panel(id, then.message())),
        None => Task::future(run).discard(),
    }
}

fn escape() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, window| match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key::Named::Escape),
            ..
        }) => Some(Message::Escape(window)),
        _ => None,
    })
}

fn requests(feed: &Feed<mpsc::Receiver<Request>>) -> impl Stream<Item = Request> + use<> {
    stream::iter(feed.take()).flat_map(|requests| {
        stream::unfold(requests, |mut requests| async move {
            let request = requests.recv().await?;
            Some((request, requests))
        })
    })
}

fn capture_events(
    feed: &Feed<deskshell_wl::Events>,
) -> impl Stream<Item = deskshell_wl::Event> + use<> {
    stream::iter(feed.take()).flatten()
}

fn changes<T: Clone + Send + Sync + 'static>(
    feed: &Feed<watch::Receiver<T>>,
) -> impl Stream<Item = T> + use<T> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            let value = receiver.borrow_and_update().clone();
            Some((value, receiver))
        })
    })
}

fn notify_changes(feed: &Feed<watch::Receiver<NotifyState>>) -> impl Stream<Item = ()> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            receiver.mark_unchanged();
            Some(((), receiver))
        })
    })
}

fn notify_effects(
    feed: &Feed<mpsc::UnboundedReceiver<NotifyEffect>>,
) -> impl Stream<Item = Vec<NotifyEffect>> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            let first = receiver.recv().await?;
            let mut batch = vec![first];
            while let Ok(next) = receiver.try_recv() {
                batch.push(next);
            }
            Some((batch, receiver))
        })
    })
}

fn urgent_notices(feed: &Feed<broadcast::Receiver<Notice>>) -> impl Stream<Item = u64> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(Notice::Urgent(address)) => return Some((address.0, receiver)),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return None,
                }
            }
        })
    })
}

fn focused_monitor_changes(source: &MonitorSource) -> impl Stream<Item = Option<String>> + use<> {
    let mut receiver = source.receiver.clone();
    let last = receiver
        .borrow_and_update()
        .focused_monitor
        .clone();
    stream::unfold(
        (receiver, last),
        |(mut receiver, mut last)| async move {
            loop {
                receiver.changed().await.ok()?;
                let state = receiver.borrow_and_update();
                if state.focused_monitor != last {
                    last.clone_from(&state.focused_monitor);
                    drop(state);
                    return Some((last.clone(), (receiver, last)));
                }
            }
        },
    )
}

fn signal_stream(feed: &Feed<Signals>) -> impl Stream<Item = ()> + use<> {
    stream::iter(feed.take()).flat_map(|signals| {
        let terminate = stream::unfold(signals.terminate, |mut signal| async move {
            signal.recv().await?;
            Some(((), signal))
        });
        let interrupt = stream::unfold(signals.interrupt, |mut signal| async move {
            signal.recv().await?;
            Some(((), signal))
        });
        stream::select(terminate, interrupt)
    })
}

fn toast_settings(output: Option<u32>, height: u32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: LayerSize::px(toasts::WIDTH as u32, height.max(1)),
        layer: Layer::Top,
        anchor: Anchor::Top | Anchor::Right,
        exclusive_zone: Some(-1),
        margin: Some(Margin {
            top: toasts::MARGIN_TOP,
            right: toasts::MARGIN_RIGHT,
            bottom: 0,
            left: 0,
        }),
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: output.map_or(OutputOption::Active, OutputOption::GlobalName),
        namespace: Some(String::from(toasts::NAMESPACE)),
        blur_option: BlurOption::FullRegion,
        ..NewLayerShellSettings::default()
    }
}

fn selector_settings(output: u32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: LayerSize::FILL,
        layer: Layer::Overlay,
        anchor: Anchor::all(),
        exclusive_zone: Some(-1),
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        output_option: OutputOption::GlobalName(output),
        namespace: Some(String::from(screenshot::NAMESPACE)),
        blur_option: BlurOption::None,
        ..NewLayerShellSettings::default()
    }
}

fn pill_settings(output: u32) -> NewLayerShellSettings {
    let (width, height) = screenshot::PILL_SIZE;
    NewLayerShellSettings {
        size: LayerSize::px(width, height),
        layer: Layer::Overlay,
        anchor: Anchor::Top,
        exclusive_zone: Some(-1),
        margin: Some(Margin { top: screenshot::PILL_TOP, right: 0, bottom: 0, left: 0 }),
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::GlobalName(output),
        events_transparent: true,
        namespace: Some(String::from(screenshot::NAMESPACE)),
        blur_option: BlurOption::None,
    }
}

// The column reports its real height after the first frame, which is still transparent at the
// start of the enter fade.
fn toast_height_guess(toasts: &Toasts) -> u32 {
    let cards = toasts.toasts().len() as u32;
    let pill = if toasts.hidden() > 0 { 40 } else { 0 };
    (cards * 110).saturating_sub(10) + pill
}

fn px(size: f32) -> u32 {
    size.round().max(1.0) as u32
}

// Every panel ignores other exclusive zones, so popover margins count from the screen edge.
fn panel_settings(
    panel: Panel,
    output: Option<u32>,
    logical: Option<(i32, i32)>,
) -> NewLayerShellSettings {
    let width = logical.map(|(width, _)| width);
    let height = logical.map(|(_, height)| height);
    let below_bar = |right: i32| Margin { top: POPOVER_TOP, right, bottom: 0, left: 0 };
    let (size, anchor, margin) = match panel {
        Panel::Overview
        | Panel::Clipboard
        | Panel::PowerMenu
        | Panel::Keybindings
        | Panel::Launcher => (LayerSize::FILL, Anchor::all(), None),
        Panel::Calendar => (
            LayerSize::px(840, 468),
            Anchor::Top | Anchor::Right,
            Some(below_bar(10)),
        ),
        Panel::Media => (
            LayerSize::px(480, clamp(680, height.map(|h| h - 88))),
            Anchor::Top,
            Some(below_bar(0)),
        ),
        Panel::Hub => (
            LayerSize::px(clamp(1560, width.map(|w| w - 40)), 640),
            Anchor::Top,
            Some(below_bar(0)),
        ),
        Panel::Updates => (
            LayerSize::px(580, UPDATES_HEIGHT),
            Anchor::Top | Anchor::Right,
            Some(below_bar(12)),
        ),
        Panel::SharePicker => (LayerSize::FILL, Anchor::all(), None),
    };
    NewLayerShellSettings {
        size,
        layer: panel_layer(panel),
        anchor,
        exclusive_zone: Some(-1),
        margin,
        keyboard_interactivity: KeyboardInteractivity::OnDemand,
        output_option: output.map_or(OutputOption::Active, OutputOption::GlobalName),
        namespace: Some(panel.namespace().to_owned()),
        // iced_exwlshell gives every surface an `ext-background-effect` object, so Hyprland
        // blurs only the region the client asks for and ignores the `blur` layer rule.
        blur_option: BlurOption::FullRegion,
        ..NewLayerShellSettings::default()
    }
}

fn panel_layer(panel: Panel) -> Layer {
    match panel {
        Panel::SharePicker => Layer::Overlay,
        _ => Layer::Top,
    }
}

fn clamp(limit: u32, room: Option<i32>) -> u32 {
    match room {
        Some(room) => limit.min(room.max(1) as u32),
        None => limit,
    }
}

fn card_size(panel: Panel) -> (f32, f32) {
    match panel {
        Panel::Launcher => (680.0, 420.0),
        _ => (720.0, 420.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_ticks_redraw_nothing() {
        let id = window::Id::unique();
        let now = Instant::now();
        let scope = |message: toasts::Message| redraw_scope(&Message::Toast(id, message));
        assert_eq!(scope(toasts::Message::Tick(now)), Scope::None);
        assert_eq!(
            scope(toasts::Message::Hover(1, true)),
            Scope::None
        );
        assert_eq!(scope(toasts::Message::Height(120.0)), Scope::None);
        assert_eq!(
            scope(toasts::Message::Frame(now)),
            Scope::Window(id)
        );
        assert_eq!(
            scope(toasts::Message::Click(1)),
            Scope::Window(id)
        );
        assert_eq!(scope(toasts::Message::Redraw), Scope::Window(id));
        assert_eq!(redraw_scope(&Message::NotifyChanged), Scope::None);
        assert_eq!(
            redraw_scope(&Message::Toasts(toasts::Message::Redraw)),
            Scope::None
        );
        assert_eq!(redraw_scope(&Message::Urgent(1)), Scope::None);
    }

    #[test]
    fn a_capture_submit_redraws_nothing() {
        assert_eq!(
            redraw_scope(&Message::Capture(deskshell_wl::Event::Submit)),
            Scope::None
        );
    }

    #[test]
    fn the_toasts_surface() {
        let settings = toast_settings(Some(7), 0);
        assert_eq!(
            settings.namespace.as_deref(),
            Some("deskshell-toasts")
        );
        assert_eq!(settings.layer, Layer::Top);
        assert_eq!(settings.anchor, Anchor::Top | Anchor::Right);
        assert_eq!(settings.exclusive_zone, Some(-1));
        assert_eq!(
            settings.keyboard_interactivity,
            KeyboardInteractivity::None
        );
        assert_eq!(
            settings.size,
            LayerSize::px(430, 1),
            "never 0, which panics"
        );
        let margin = settings.margin.unwrap();
        assert_eq!((margin.top, margin.right), (60, 12));
    }

    #[test]
    fn selector_messages_redraw_only_their_surface() {
        let id = window::Id::unique();
        let scope = |message| redraw_scope(&Message::Shot(id, message));
        assert_eq!(
            scope(screenshot::Message::Move(1.0, 2.0)),
            Scope::Window(id)
        );
        assert_eq!(
            scope(screenshot::Message::Tick),
            Scope::Window(id)
        );
        assert_eq!(
            scope(screenshot::Message::Resized(1.0, 1.0)),
            Scope::None
        );
        assert_eq!(
            redraw_scope(&Message::Shots(screenshot::Message::Tick)),
            Scope::None
        );
    }

    #[test]
    fn the_picker_surfaces() {
        let share = panel_settings(Panel::SharePicker, Some(7), Some((1280, 720)));
        assert_eq!(share.layer, Layer::Overlay);
        assert_eq!(share.anchor, Anchor::all());
        assert_eq!(
            share.namespace.as_deref(),
            Some("deskshell-share-picker")
        );
        assert_eq!(
            share.keyboard_interactivity,
            KeyboardInteractivity::OnDemand
        );
        assert_eq!(
            panel_settings(Panel::Overview, None, None).layer,
            Layer::Top,
            "other panels stay on Top"
        );

        let selector = selector_settings(7);
        assert_eq!(selector.layer, Layer::Overlay);
        assert_eq!(selector.anchor, Anchor::all());
        assert_eq!(
            selector.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        );
        assert_eq!(
            selector.output_option,
            OutputOption::GlobalName(7)
        );
        assert_eq!(
            selector.namespace.as_deref(),
            Some("deskshell-screenshot")
        );

        let pill = pill_settings(7);
        assert_eq!(pill.layer, Layer::Overlay);
        assert_eq!(pill.anchor, Anchor::Top);
        assert_eq!(pill.margin.unwrap().top, 24);
        assert_eq!(
            pill.keyboard_interactivity,
            KeyboardInteractivity::None
        );
        assert!(pill.events_transparent, "clicks pass through");
        assert_eq!(
            pill.namespace.as_deref(),
            Some("deskshell-screenshot")
        );
    }

    #[test]
    fn overview_and_share_frames_redraw_only_their_surface() {
        let id = window::Id::unique();
        let now = Instant::now();
        let scope = |message| redraw_scope(&Message::Panel(id, message));
        assert_eq!(
            scope(panel::Message::Overview(
                panel::overview::Message::Animate(now)
            )),
            Scope::Window(id)
        );
        assert_eq!(
            scope(panel::Message::Overview(
                panel::overview::Message::Hover(Some(1))
            )),
            Scope::Window(id),
            "a start draws its surface once, so frames begin"
        );
        assert_eq!(
            scope(panel::Message::Share(share::Message::Animate(
                now
            ))),
            Scope::Window(id)
        );
        assert_eq!(
            scope(panel::Message::Share(share::Message::Hover(
                0, true
            ))),
            Scope::Window(id)
        );
    }
}
