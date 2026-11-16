//! The session lock: `ext-session-lock-v1` on every output and one `deskshell-pam` per attempt.

mod app;
mod attempt;
mod clock;
mod driver;
mod layout;
mod notify;
mod polls;
mod probe;
mod runtime;
mod view;
mod wallpaper;

use std::env;
use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::Path;
use std::process::{self, ExitCode};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Instant;

use deskshell_auth::faillock::{self, Config};
use deskshell_auth::touch;
use deskshell_auth::worker::sibling_program;
use deskshell_hypr::{Client, Paths, Store};
use deskshell_ui::{Theme, fonts, theme};
use iced_exwlshell::reexport::WlConnection;
use iced_exwlshell::sessionlock;
use iced_exwlshell::settings::ExWlSettings;
use iced_exwlshell::shell;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::uptime;

use crate::app::{Boot, Lock, RESTORE_LAYOUT, UNLOCKED};
use crate::attempt::Attempts;
use crate::polls::Feed;

const USAGE: &str = "\
usage: deskshell-lock [-h | --help]

Locks the session until the user's PIN and key pass PAM (the login service), then exits.
It is meant to run as deskshell-lock.service. Exit status:

  0  unlocked after PAM succeeded
  1  could not start: no Wayland display, or the event loop failed
  2  usage error
  3  the compositor refused the lock
  4  the compositor ended the lock itself
  5  an unlock check failed, the session stays locked
  6  systemd did not confirm the stop before the unlock, the session stays locked
";

pub mod exit {
    pub const FAILED: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const DENIED: u8 = 3;
    pub const FINISHED: u8 = 4;
    pub const FATAL: u8 = 5;
    pub const UNCONFIRMED: u8 = 6;
}

const LOG_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";

fn main() -> ExitCode {
    let started = Instant::now();
    let mut args = env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => {}
        (Some(arg), None) if arg == "-h" || arg == "--help" => {
            let _ = io::stdout().write_all(USAGE.as_bytes());
            return ExitCode::SUCCESS;
        }
        (Some(arg), _) => {
            let _ = writeln!(
                io::stderr(),
                "unexpected argument {}\n\n{USAGE}",
                arg.to_string_lossy()
            );
            return ExitCode::from(exit::USAGE);
        }
    }

    let hardened = harden();
    // SAFETY: nothing has started a thread yet.
    unsafe { force_vulkan() };
    install_panic_hook();
    init_logging();
    if let Err(err) = hardened {
        warn!(%err, "cannot make the lock non-dumpable, it runs anyway");
    }

    if !has_wayland() {
        error!("no Wayland display: WAYLAND_DISPLAY and WAYLAND_SOCKET are not set");
        return ExitCode::from(exit::FAILED);
    }
    let runtime = match runtime::get() {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(%err, "cannot start the async runtime");
            return ExitCode::from(exit::FAILED);
        }
    };
    warm_up_fonts();

    let user = user_name();
    let config = Config::load(Path::new(faillock::CONFIG_PATH));
    info!(%user, ?config, "deskshell-lock starts");
    let program = match sibling_program("deskshell-pam") {
        Ok(program) => Some(program),
        Err(err) => {
            error!(%err, "deskshell-pam is missing, no unlock is possible");
            None
        }
    };
    let (report_sender, reports) = mpsc::unbounded_channel();
    let (touch, hypr) = {
        let _context = runtime.enter();
        let touch = touch::spawn(touch::Sources {
            fido: Some(touch::HidPaths::system()),
            gpg_socket: None,
        });
        let hypr = match Paths::from_env() {
            Ok(paths) => {
                let client = Client::new(paths);
                let (store, state, _notices) = Store::spawn(client.clone());
                Some((store, Feed::new(state), client))
            }
            Err(err) => {
                warn!(%err, "no Hyprland: no layout switch and no layout warning");
                None
            }
        };
        (touch, hypr)
    };
    let theme = load_theme();
    let restore_client = hypr
        .as_ref()
        .map(|(_, _, client)| client.clone());

    let (shell_broadcast, shell_events) = shell::channel();
    let boot = std::cell::Cell::new(Some(Boot {
        theme,
        shell_events,
        faillock: polls::Faillock { config, user: user.clone() },
        touch: Feed::new(touch),
        hypr,
        attempts: Attempts::new(runtime.handle().clone(), program, report_sender),
        reports: Feed::new(reports),
        user,
        started,
    }));
    let mut app = sessionlock::application(
        move || {
            Lock::new(
                boot.take()
                    .expect("iced boots the lock once"),
            )
        },
        Lock::update,
        view::view,
    )
    .wl_settings(ExWlSettings { shell_broadcast, ..ExWlSettings::default() })
    .with_connection(move || {
        let connection = WlConnection::connect_to_env();
        info!(
            ms = started.elapsed().as_millis(),
            "wayland connected, the lock request goes out next"
        );
        connection
    })
    .subscription(Lock::subscription)
    .style(|lock: &Lock, _| iced::theme::Style {
        background_color: view::BACKDROP,
        text_color: lock.theme.colors.text_primary,
    })
    .theme(|_: &Lock| iced::Theme::Dark)
    .executor::<runtime::Executor>()
    .default_font(fonts::PLEX);
    for font in fonts::all() {
        app = app.font(*font);
    }
    let result = app.run();
    let frames = probe::total();
    match result {
        Ok(()) if UNLOCKED.load(Ordering::SeqCst) => {
            info!(frames, "unlocked");
            if RESTORE_LAYOUT.load(Ordering::SeqCst) {
                let marker = layout::marker();
                match restore_client {
                    Some(client) => {
                        if let Err(err) =
                            runtime.block_on(layout::restore(&client, marker.as_deref()))
                        {
                            warn!(%err, "the layout stays switched, the next lock restores it");
                        }
                    }
                    None => warn!("no Hyprland to restore the layout with"),
                }
            }
            ExitCode::SUCCESS
        }
        Ok(()) => {
            error!(frames, "the event loop ended without an unlock");
            ExitCode::from(exit::FATAL)
        }
        Err(err) => {
            error!(%err, frames, "the event loop failed");
            ExitCode::from(exit::FAILED)
        }
    }
}

/// Non-dumpable with a zero core limit, so no process of the user can ptrace it or read the PIN
/// through `/proc`, and no core handler stores it. Call it before any thread starts.
fn harden() -> io::Result<()> {
    use rustix::process::{self as process, DumpableBehavior, Resource, Rlimit};
    process::set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    process::setrlimit(
        Resource::Core,
        Rlimit { current: Some(0), maximum: Some(0) },
    )?;
    Ok(())
}

/// Aborts on any panic, so the lock never runs on in an unplanned state. The session stays locked
/// and systemd starts a new lock. A panicking wallpaper is marked first so the new lock skips it.
fn install_panic_hook() {
    let print = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        print(info);
        wallpaper::note_panic();
        process::abort();
    }));
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

/// Nothing bridges the `log` crate, so iced_wgpu does not enumerate every adapter at start.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(LOG_FILTER));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(uptime())
        .with_thread_names(true)
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

/// cosmic-text scans the system fonts the first time text is shaped, which can take tens
/// of milliseconds. A helper thread starts it now, while the lock request goes out.
fn warm_up_fonts() {
    let _ = thread::Builder::new()
        .name(String::from("font-warmup"))
        .spawn(|| {
            let started = Instant::now();
            let _ = iced::advanced::graphics::text::font_system();
            info!(
                ms = started.elapsed().as_millis(),
                "font system ready"
            );
        });
}

fn user_name() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|key| {
            env::var(key)
                .ok()
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_default()
}

fn load_theme() -> Theme {
    let Some(path) = theme::default_path() else {
        return Theme::default();
    };
    Theme::load(&path).unwrap_or_else(|err| {
        warn!(path = %path.display(), "theme file rejected, using the defaults: {err}");
        Theme::default()
    })
}

#[cfg(test)]
mod tests {
    use rustix::process::{self as process, DumpableBehavior, Resource};

    #[test]
    fn harden_makes_the_process_non_dumpable_with_no_core() {
        super::harden().unwrap();
        assert_eq!(
            process::dumpable_behavior().unwrap(),
            DumpableBehavior::NotDumpable
        );
        let core = process::getrlimit(Resource::Core);
        assert_eq!((core.current, core.maximum), (Some(0), Some(0)));
    }
}
