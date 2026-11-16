//! Prints the touch readers' state at start and on every change, with readers' logs on stderr.
//!
//! It opens the real hidraw nodes, so it runs only in a `dev/vm` guest unless `--host` is given.
//!
//! ```sh
//! cargo build -p deskshell-auth --example fake_fido --example touch_probe
//! slot=$(dev/vm/vm up --share "$PWD/target")
//! dev/vm/vm ssh "$slot" as-tester /work/debug/examples/touch_probe
//! ```
//!
//! `crates/auth/examples/fido_guest.sh` runs the whole guest check.

use std::fmt;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use deskshell_auth::touch::{self, HidPaths, Sources, TouchState};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

const GUEST_HOSTNAME: &str = "deskshell-vm";
const USAGE: &str = "usage: touch_probe [--host]";

struct Epoch;

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        write!(
            f,
            "{}.{:03}",
            time.as_secs(),
            time.subsec_millis()
        )
    }
}

impl FormatTime for Epoch {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        write!(w, "{self}")
    }
}

struct Kind(Option<bool>);

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            None => "none",
            Some(false) => "false",
            Some(true) => "true",
        })
    }
}

fn print(state: TouchState) {
    println!(
        "{Epoch} u2f={} gpg={}",
        Kind(state.u2f),
        Kind(state.gpg)
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let host = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--host") => true,
        Some(_) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME && !host {
        eprintln!("touch_probe reads the real hidraw nodes and runs only in a dev/vm guest");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("deskshell_auth=trace"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_timer(Epoch)
        .init();

    let mut rx =
        touch::spawn(Sources { fido: Some(HidPaths::system()), gpg_socket: touch::gpg_socket() });
    loop {
        print(*rx.borrow_and_update());
        if rx.changed().await.is_err() {
            return ExitCode::SUCCESS;
        }
    }
}
