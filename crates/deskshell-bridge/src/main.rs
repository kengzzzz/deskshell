//! The deskshell-bridge binary.

use std::env;
use std::io::{self, Write};
use std::panic;
use std::path::Path;
use std::process::{self, ExitCode};

use deskshell_bridge::cli::{self, Command, USAGE};
use deskshell_bridge::shortcuts::{self, Shortcuts};
use deskshell_bridge::tray::{self, Tray};
use deskshell_bridge::{Bus, Result, launch, notify, wayland};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    install_panic_hook();
    let command = match cli::parse(env::args_os().skip(1)) {
        Ok(Command::Help) => {
            let _ = io::stdout().write_all(USAGE.as_bytes());
            return ExitCode::SUCCESS;
        }
        Ok(command) => command,
        Err(msg) => {
            let _ = writeln!(io::stderr(), "deskshell-bridge: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    init_logging();
    match run(command) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(io::stderr(), "deskshell-bridge: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Help => {}
        Command::Wayland { socket, app_id } => wayland::serve(&socket, &app_id)?,
        Command::Globals => wayland::globals()?,
        Command::Run { socket, wait, raw, argv } => {
            let code = launch::run(
                &socket,
                wait,
                raw,
                &argv,
                &mut io::stdout().lock(),
                &mut io::stderr().lock(),
            )?;
            return Ok(ExitCode::from(code));
        }
        Command::Launchd { allow_uid } => launch::serve_activated(allow_uid)?,
        Command::NotifyForward { socket } => {
            block_on(notify::forward(&Bus::Session, &socket))?;
        }
        Command::NotifyRecv { socket, allow_uid } => {
            let timing = notify::Timing::default();
            block_on(notify::receive(
                &Bus::Session,
                &socket,
                allow_uid,
                timing,
            ))?;
        }
        Command::SyncShortcuts { src, apps, icons, home, socket } => {
            let exe = env::current_exe()?;
            let paths = Shortcuts {
                src: &src,
                apps: &apps,
                icons: &icons,
                home: &home,
                socket: &socket,
                exe: &exe,
            };
            shortcuts::sync(&paths)?;
        }
        Command::Tray { uid, socket } => {
            let tray = Tray { uid, socket: &socket, proc: Path::new(tray::PROC), poll: tray::POLL };
            block_on(tray::serve(&Bus::Session, tray))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A blocking job still running at the end is left behind, so the process exits even while a
/// launch waits on its timeouts.
fn block_on<T>(task: impl Future<Output = Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(task);
    runtime.shutdown_background();
    result
}

/// `launchd` serves each request on its own thread, and an unwinding panic there would leave
/// a connection half served.
fn install_panic_hook() {
    let print = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        print(info);
        process::abort();
    }));
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_ansi(false)
        .with_writer(io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}
