//! The iced session lock: one view on every output's lock surface and the driver behind it.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::Path;
use std::process::{self, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
#[cfg(debug_assertions)]
use std::{env, fs, path::PathBuf, time::Duration};

use deskshell_auth::faillock::{Locked, Status};
use deskshell_auth::flow::Event as FlowEvent;
use deskshell_auth::key;
use deskshell_auth::touch::TouchState;
use deskshell_hypr::{Client, Store};
use deskshell_ui::Theme;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::image::Handle;
use iced::{Event, Subscription, Task, event, window};
use iced_exwlshell::shell::{ExWlOutputInfo, ShellEvent, ShellReceiver, ShellType};
use iced_exwlshell::to_sessionlock_message;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::attempt::{Attempts, Report};
use crate::clock::{self, Clock};
use crate::driver::{Driver, Effect, Input, UnlockToken};
use crate::polls::{self, Feed};
use crate::{exit, layout, notify, view, wallpaper};

/// Set right before the unlock, so `main` exits 0 only after it.
pub static UNLOCKED: AtomicBool = AtomicBool::new(false);
pub static RESTORE_LAYOUT: AtomicBool = AtomicBool::new(false);

const AVATAR_SIZE: u32 = 128;

/// A key press the lock reacts to. The typed character never shows in `Debug`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    Backspace,
    Clear,
    Submit,
}

impl fmt::Debug for KeyInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Char(_) => f.write_str("Char(..)"),
            Self::Backspace => f.write_str("Backspace"),
            Self::Clear => f.write_str("Clear"),
            Self::Submit => f.write_str("Submit"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Suspend,
    Poweroff,
}

#[to_sessionlock_message]
#[derive(Debug, Clone)]
pub enum Message {
    Shell(ShellEvent),
    Opened(window::Id),
    Key(KeyInput),
    Submit,
    KeyChecked(bool),
    Faillock(Status),
    Touch(TouchState),
    Layout(String),
    Report(Report),
    Minute,
    LockoutTick,
    Notifications(Option<u32>),
    Wallpaper(Option<Handle>),
    Avatar(Option<Handle>),
    Power(Power),
    Unlocked(UnlockToken),
    #[cfg(debug_assertions)]
    TestPanic,
}

pub struct Boot {
    pub theme: Theme,
    pub shell_events: ShellReceiver,
    pub faillock: polls::Faillock,
    pub touch: Feed<watch::Receiver<TouchState>>,
    pub hypr: Option<(
        Store,
        Feed<watch::Receiver<deskshell_hypr::State>>,
        Client,
    )>,
    pub attempts: Attempts,
    pub reports: Feed<mpsc::UnboundedReceiver<Report>>,
    pub user: String,
    pub started: Instant,
}

pub struct Lock {
    pub(crate) theme: Theme,
    pub(crate) driver: Driver,
    pub(crate) clock: Clock,
    pub(crate) texts: view::Texts,
    pub(crate) wallpaper: Option<Handle>,
    pub(crate) avatar: Option<Handle>,
    pub(crate) initial: Option<char>,
    pub(crate) notifications: Option<String>,
    shell_events: ShellReceiver,
    faillock: polls::Faillock,
    touch: Feed<watch::Receiver<TouchState>>,
    layout: Option<Feed<watch::Receiver<deskshell_hypr::State>>>,
    hypr: Option<Client>,
    _store: Option<Store>,
    attempts: Attempts,
    reports: Feed<mpsc::UnboundedReceiver<Report>>,
    outputs: Vec<(u32, (u32, u32))>,
    status: Status,
    locked: bool,
    first_frame: bool,
    started: Instant,
}

impl Lock {
    pub fn new(boot: Boot) -> Self {
        let restore_layout = layout::marker().is_some_and(|path| path.exists());
        if restore_layout {
            info!("a layout restore is pending from an earlier lock");
        }
        let (store, layout, hypr) = match boot.hypr {
            Some((store, feed, client)) => (Some(store), Some(feed), Some(client)),
            None => (None, None, None),
        };
        let status = Status::default();
        Self {
            texts: view::Texts::new(&status, &boot.faillock.config, polls::now_secs()),
            theme: boot.theme,
            driver: Driver::new(restore_layout),
            clock: Clock::now(),
            wallpaper: None,
            avatar: None,
            initial: boot.user.chars().next(),
            notifications: None,
            shell_events: boot.shell_events,
            faillock: boot.faillock,
            touch: boot.touch,
            layout,
            hypr,
            _store: store,
            attempts: boot.attempts,
            reports: boot.reports,
            outputs: Vec::new(),
            status,
            locked: false,
            first_frame: false,
            started: boot.started,
        }
    }

    fn millis(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shell(event) => self.shell_event(event),
            Message::Opened(id) => {
                if !self.first_frame {
                    self.first_frame = true;
                    info!(
                        ?id,
                        ms = self.millis(),
                        "first lock frame presented"
                    );
                }
                Task::none()
            }
            Message::Key(input) => {
                let event = match input {
                    KeyInput::Char(c) => FlowEvent::Type(c),
                    KeyInput::Backspace => FlowEvent::Backspace,
                    KeyInput::Clear => FlowEvent::Clear,
                    KeyInput::Submit => FlowEvent::Submit,
                };
                self.input(Input::Flow(event))
            }
            Message::Submit => self.input(Input::Flow(FlowEvent::Submit)),
            Message::KeyChecked(present) => self.input(Input::Flow(FlowEvent::KeyChecked(present))),
            Message::Faillock(status) => {
                self.status = status.clone();
                self.refresh_texts();
                self.input(Input::Flow(FlowEvent::Faillock(status)))
            }
            Message::Touch(touch) => self.input(Input::Flow(FlowEvent::Touch(touch))),
            Message::Layout(layout) => {
                info!(%layout, "keyboard layout");
                self.input(Input::Layout(layout))
            }
            Message::Report(report) => self.input(match report {
                Report::Worker { attempt, message } => {
                    Input::Flow(FlowEvent::Worker { attempt, message })
                }
                Report::Gone { attempt } => Input::Flow(FlowEvent::WorkerGone { attempt }),
                Report::Timeout { attempt } => Input::Flow(FlowEvent::Timeout { attempt }),
                Report::Finished { attempt, outcome } => Input::Finished { attempt, outcome },
            }),
            Message::Minute => {
                self.clock = Clock::now();
                Task::none()
            }
            Message::LockoutTick => {
                self.refresh_texts();
                match self.status.locked {
                    Some(Locked::Until(until)) if until <= polls::now_secs() => {
                        self.run(vec![Effect::RefreshFaillock])
                    }
                    _ => Task::none(),
                }
            }
            Message::Notifications(count) => {
                self.notifications = count.map(|count| count.to_string());
                Task::none()
            }
            Message::Wallpaper(handle) => {
                self.wallpaper = handle;
                Task::none()
            }
            Message::Avatar(handle) => {
                self.avatar = handle;
                Task::none()
            }
            Message::Power(power) => {
                power_action(power);
                Task::none()
            }
            Message::Unlocked(_token) => {
                info!(
                    ms = self.millis(),
                    "PAM success confirmed, telling systemd the lock stops"
                );
                match stop_unit(notify::socket().as_deref()) {
                    Ok(notify::Stopped::Confirmed) => info!("systemd confirmed the stop"),
                    Ok(notify::Stopped::NoUnit) => info!("no NOTIFY_SOCKET, no unit to stop"),
                    Err(err) => {
                        // `process::exit` runs no destructor and sends no `unlock_and_destroy`,
                        // and Hyprland keeps the session locked when the client is gone.
                        error!(
                            %err,
                            "systemd did not confirm the stop, leaving the session locked"
                        );
                        process::exit(exit::UNCONFIRMED.into());
                    }
                }
                UNLOCKED.store(true, Ordering::SeqCst);
                info!(ms = self.millis(), "unlocking");
                // The only `iced::exit()`. While locked it sends `unlock_and_destroy`, so it is
                // the unlock. Every other way out leaves the session locked.
                iced::exit()
            }
            #[cfg(debug_assertions)]
            Message::TestPanic => panic!("DESKSHELL_LOCK_TEST_PANIC: a test panic while locked"),
        }
    }

    fn input(&mut self, input: Input) -> Task<Message> {
        let effects = self.driver.handle(input);
        self.run(effects)
    }

    fn run(&mut self, effects: Vec<Effect>) -> Task<Message> {
        let mut tasks = Vec::new();
        for effect in effects {
            match effect {
                Effect::CheckKey => tasks.push(Task::perform(
                    async { key::key_present(Path::new(key::HIDRAW_CLASS)) },
                    Message::KeyChecked,
                )),
                Effect::StartPam { attempt } => self.attempts.start(attempt),
                Effect::Answer { attempt, secret } => self.attempts.answer(attempt, secret),
                Effect::Kill { attempt } => self.attempts.kill(attempt),
                Effect::Finish { attempt } => self.attempts.finish(attempt),
                Effect::RefreshFaillock => {
                    let source = self.faillock.clone();
                    tasks.push(Task::perform(
                        async move { source.status() },
                        Message::Faillock,
                    ));
                }
                Effect::SwitchToFirstLayout => {
                    if let Some(path) = layout::marker() {
                        layout::remember(&path);
                    }
                    if let Some(client) = self.hypr.clone() {
                        tasks.push(
                            Task::future(async move {
                                match layout::switch(&client, 0).await {
                                    Ok(()) => info!("switched the keyboard layout to 0"),
                                    Err(err) => warn!(%err, "cannot switch the keyboard layout"),
                                }
                            })
                            .discard(),
                        );
                    }
                }
                Effect::Unlock { token, restore_layout } => {
                    // `main` restores the layout after the unlock, so a slow or failed
                    // restore never holds the session.
                    RESTORE_LAYOUT.store(restore_layout, Ordering::SeqCst);
                    tasks.push(Task::done(Message::Unlocked(token)));
                }
                Effect::Fatal(reason) => {
                    error!(reason, "leaving the session locked and exiting");
                    process::exit(exit::FATAL.into());
                }
            }
        }
        Task::batch(tasks)
    }

    fn refresh_texts(&mut self) {
        self.texts = view::Texts::new(
            &self.status,
            &self.faillock.config,
            polls::now_secs(),
        );
    }

    fn shell_event(&mut self, event: ShellEvent) -> Task<Message> {
        match event {
            ShellEvent::Locked => {
                if self.locked {
                    return Task::none();
                }
                self.locked = true;
                info!(ms = self.millis(), "locked");
                match notify::ready() {
                    Ok(true) => info!("told systemd the session is locked"),
                    Ok(false) => {}
                    Err(err) => warn!(%err, "cannot notify systemd"),
                }
                return Task::batch([self.load_wallpaper(), load_avatar(), test_panic()]);
            }
            ShellEvent::LockDenied => {
                error!("the compositor refused the lock");
                process::exit(exit::DENIED.into());
            }
            ShellEvent::LockedFinished => {
                warn!("the compositor ended the lock");
                process::exit(exit::FINISHED.into());
            }
            ShellEvent::NewShell(shell) if shell.shell == ShellType::SessionLock => {
                info!(window = ?shell.window, ms = self.millis(), "lock surface created");
            }
            ShellEvent::OutputAdded(output) | ShellEvent::OutputUpdated(output) => {
                info!(name = ?output.name, id = output.id, "output");
                let size = pixel_size(&output);
                match self
                    .outputs
                    .iter_mut()
                    .find(|(id, _)| *id == output.id)
                {
                    Some((_, known)) => *known = size,
                    None => self.outputs.push((output.id, size)),
                }
            }
            ShellEvent::OutputRemoved(output) => {
                info!(name = ?output.name, id = output.id, "output removed");
                self.outputs
                    .retain(|(id, _)| *id != output.id);
            }
            ShellEvent::Closed(window) => info!(?window, "lock surface closed"),
            _ => {}
        }
        Task::none()
    }

    fn load_wallpaper(&self) -> Task<Message> {
        let Some((width, height)) = self
            .outputs
            .iter()
            .map(|(_, size)| *size)
            .filter(|(width, height)| *width > 0 && *height > 0)
            .max_by_key(|(width, height)| u64::from(*width) * u64::from(*height))
        else {
            warn!("no output size known, no wallpaper");
            return Task::none();
        };
        let (Some(source), Some(dir)) = (wallpaper::source_path(), wallpaper::cache_dir()) else {
            return Task::none();
        };
        let started = Instant::now();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    wallpaper::prepare(&source, &dir, width, height)
                })
                .await
                .map_err(|err| err.to_string())
                .and_then(|result| result)
            },
            move |result| match result {
                Ok(image) => {
                    info!(
                        width,
                        height,
                        ms = started.elapsed().as_millis(),
                        "wallpaper ready"
                    );
                    let (width, height) = image.dimensions();
                    Message::Wallpaper(Some(Handle::from_rgba(
                        width,
                        height,
                        image.into_raw(),
                    )))
                }
                Err(err) => {
                    info!(%err, "no wallpaper");
                    Message::Wallpaper(None)
                }
            },
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let flow = self.driver.flow();
        let mut subscriptions = vec![
            self.shell_events
                .listen()
                .map(Message::Shell),
            window::open_events().map(Message::Opened),
            event::listen_with(key_input),
            Subscription::run_with(self.reports.clone(), polls::reports).map(Message::Report),
            Subscription::run_with(self.faillock.clone(), polls::faillock).map(Message::Faillock),
            Subscription::run_with(self.touch.clone(), polls::touch).map(Message::Touch),
            Subscription::run_with((), polls::notification_count).map(Message::Notifications),
            Subscription::run(clock::minutes).map(|()| Message::Minute),
        ];
        if let Some(layout) = &self.layout {
            subscriptions
                .push(Subscription::run_with(layout.clone(), polls::layout).map(Message::Layout));
        }
        if flow.idle() {
            subscriptions
                .push(Subscription::run_with((), polls::key_presence).map(Message::KeyChecked));
        }
        if let Some(Locked::Until(until)) = self.status.locked {
            subscriptions.push(
                Subscription::run_with(until, polls::lockout_ticks).map(|()| Message::LockoutTick),
            );
        }
        Subscription::batch(subscriptions)
    }
}

fn pixel_size(output: &ExWlOutputInfo) -> (u32, u32) {
    let scale = output.scale_factor.max(1);
    let logical = output
        .logical_size
        .map(|(width, height)| (width * scale, height * scale));
    let mode = output
        .modes
        .iter()
        .find(|mode| mode.current)
        .map(|mode| mode.dimensions);
    let (width, height) = logical.or(mode).unwrap_or((0, 0));
    (
        width.max(0).unsigned_abs(),
        height.max(0).unsigned_abs(),
    )
}

fn load_avatar() -> Task<Message> {
    let Some(path) = polls::avatar_path() else {
        return Task::none();
    };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || deskshell_ui::image::load(&path, AVATAR_SIZE))
                .await
                .ok()
                .and_then(Result::ok)
        },
        |image| Message::Avatar(image.map(|image| image.handle)),
    )
}

fn power_action(power: Power) {
    let verb = match power {
        Power::Suspend => "suspend",
        Power::Poweroff => "poweroff",
    };
    info!(verb, "power button");
    let spawned = Command::new("systemctl")
        .arg(verb)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        // Reaped on a helper thread, so no zombie is left.
        Ok(mut child) => {
            let _ = std::thread::Builder::new()
                .name(String::from("power"))
                .spawn(move || child.wait());
        }
        Err(err) => warn!(%err, verb, "cannot run systemctl"),
    }
}

fn key_input(event: Event, _status: event::Status, _window: window::Id) -> Option<Message> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) = event else {
        return None;
    };
    let input = match key.as_ref() {
        Key::Named(Named::Enter) => KeyInput::Submit,
        Key::Named(Named::Escape) => KeyInput::Clear,
        Key::Named(Named::Backspace) if modifiers.control() => KeyInput::Clear,
        Key::Named(Named::Backspace) => KeyInput::Backspace,
        _ => {
            if modifiers.control() || modifiers.alt() || modifiers.logo() {
                return None;
            }
            let mut chars = text.as_deref()?.chars();
            let c = chars.next()?;
            if chars.next().is_some() || c.is_control() {
                return None;
            }
            KeyInput::Char(c)
        }
    };
    Some(Message::Key(input))
}

/// Debug builds only. With `DESKSHELL_LOCK_TEST_PANIC` naming a file that exists, the lock
/// removes the file and panics 2 s after it locked.
#[cfg(debug_assertions)]
fn test_panic() -> Task<Message> {
    let Some(path) = env::var_os("DESKSHELL_LOCK_TEST_PANIC").map(PathBuf::from) else {
        return Task::none();
    };
    if fs::remove_file(&path).is_err() {
        return Task::none();
    }
    warn!(path = %path.display(), "test panic armed");
    Task::perform(tokio::time::sleep(Duration::from_secs(2)), |()| {
        Message::TestPanic
    })
}

#[cfg(not(debug_assertions))]
fn test_panic() -> Task<Message> {
    Task::none()
}

/// Debug builds only. With `DESKSHELL_LOCK_TEST_BARRIER` naming a file that exists, the lock
/// removes the file and runs a stop that never gets its confirmation, to test the fail-closed exit.
#[cfg(debug_assertions)]
fn stop_unit(socket: Option<&OsStr>) -> io::Result<notify::Stopped> {
    let armed = env::var_os("DESKSHELL_LOCK_TEST_BARRIER")
        .is_some_and(|path| fs::remove_file(path).is_ok());
    if armed {
        warn!("test barrier armed, systemd's confirmation will not arrive");
        return notify::stop_unconfirmed(socket, notify::STOP_TIMEOUT);
    }
    notify::stop(socket, notify::STOP_TIMEOUT)
}

#[cfg(not(debug_assertions))]
fn stop_unit(socket: Option<&OsStr>) -> io::Result<notify::Stopped> {
    notify::stop(socket, notify::STOP_TIMEOUT)
}
