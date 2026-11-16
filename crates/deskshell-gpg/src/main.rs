//! deskshell-gpg: the gpg-agent socket proxy that reports GPG touch waits.

use std::env;
use std::io::{self, Write};
use std::panic;
use std::process::{self, ExitCode};

use deskshell_auth::touch::gpg_socket;
use deskshell_gpg::cli::{self, AgentSocket, Env, USAGE};
use deskshell_gpg::{Exit, Paths, Timing, sys};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // Replies carry session keys and loopback passphrases, and a panic aborts.
    let no_core = sys::disable_core_dumps();
    install_panic_hook();
    let args = match cli::parse(env::args_os().skip(1)) {
        Ok(args) => args,
        Err(msg) => return usage_error(&msg),
    };
    if args.help {
        let _ = io::stdout().write_all(USAGE.as_bytes());
        return ExitCode::SUCCESS;
    }
    init_logging();
    if let Err(err) = no_core {
        warn!(%err, "cannot turn core dumps off");
    }

    let gnupghome = env::var_os("GNUPGHOME");
    let home = env::var_os("HOME");
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR");
    let state_socket = gpg_socket();
    let env = Env {
        gnupghome: gnupghome.as_deref(),
        home: home.as_deref(),
        runtime_dir: runtime_dir.as_deref(),
        state_socket: state_socket.as_deref(),
    };
    let resolved = match cli::resolve(&args, env) {
        Ok(resolved) => resolved,
        Err(msg) => return usage_error(&msg),
    };
    let state_socket = match (args.restore, resolved.state_socket) {
        (true, _) => None,
        (false, Some(path)) => Some(path),
        (false, None) => return usage_error("XDG_RUNTIME_DIR is not set, pass --state-socket"),
    };
    let agent_socket = match resolved.agent_socket {
        AgentSocket::Path(path) => path,
        AgentSocket::AskGpgconf => match cli::gpgconf_agent_socket(&resolved.home) {
            Ok(path) => path,
            Err(err) => {
                error!(%err, "cannot ask gpgconf for the agent socket");
                return ExitCode::from(Exit::Failed.code());
            }
        },
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(%err, "cannot start the async runtime");
            return ExitCode::from(Exit::Failed.code());
        }
    };
    let timing = Timing::default();
    let exit = match state_socket {
        None => runtime.block_on(deskshell_gpg::restore(&agent_socket, timing)),
        Some(state_socket) => {
            let paths = Paths { agent_socket, home: resolved.home, state_socket };
            runtime.block_on(async {
                let (Ok(mut term), Ok(mut int)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                ) else {
                    error!("cannot listen for signals");
                    return Exit::Failed;
                };
                let shutdown = async move {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = int.recv() => {}
                    }
                };
                deskshell_gpg::run(&paths, timing, shutdown).await
            })
        }
    };
    ExitCode::from(exit.code())
}

fn usage_error(msg: &str) -> ExitCode {
    let _ = writeln!(io::stderr(), "{msg}\n\n{USAGE}");
    ExitCode::from(Exit::Usage.code())
}

/// A panic aborts, as in the lock. The unit's `--restore` puts the agent's socket back and
/// the next start takes over again.
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
