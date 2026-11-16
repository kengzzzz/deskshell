//! Runs the install job against a user manager and prints its state.
//!
//! ```text
//! install --guest --bus ADDRESS --deskshell PATH --dir DIR
//! ```
//!
//! Stdin commands: `start N`, `pin TEXT`, `cancel`, `quit`.
//! It runs the real sudo and paru, so it refuses to run outside a VM guest.

use std::io::BufRead as _;
use std::path::PathBuf;

use deskshell_system::install::{self, Command, Config, Service};
use deskshell_system::systemd::Systemd;
use tokio::sync::mpsc;

const USAGE: &str = "usage: install --guest --bus ADDRESS --deskshell PATH --dir DIR";
const GUEST_HOSTNAME: &str = "deskshell-vm";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        fail("install runs the real sudo and only runs in a dev/vm guest");
    }
    let mut guest = false;
    let (mut bus, mut deskshell, mut dir) = (None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--guest" => guest = true,
            "--bus" => bus = args.next(),
            "--deskshell" => deskshell = args.next(),
            "--dir" => dir = args.next().map(PathBuf::from),
            _ => fail(USAGE),
        }
    }
    let (true, Some(bus), Some(deskshell), Some(dir)) = (guest, bus, deskshell, dir) else {
        fail(USAGE);
    };
    let conn = zbus::connection::Builder::address(bus.as_str())
        .build()
        .await
        .unwrap_or_else(|err| fail(&err.to_string()));
    let systemd = Systemd::new(&conn)
        .await
        .unwrap_or_else(|err| fail(&err.to_string()));
    let runner = vec![deskshell, String::from("update-run")];
    let (service, mut state, mut effects) = Service::start(Config::new(runner, dir), systemd);
    let (lines_tx, mut lines) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let state = state.borrow_and_update();
                let mut label = String::new();
                state.step_label(true, &mut label);
                println!(
                    "state run={} active={} auth={} stage={:?} attempt={:?} queued={} pin_sent={} can_stop={} phase={:?} step={} {}/{} error={:?} label={label:?}",
                    state.run_id,
                    state.active(),
                    state.auth_visible(),
                    state.pam_stage,
                    state.auth_attempt,
                    state.queued,
                    state.pin_sent,
                    state.can_stop(),
                    state.job.phase,
                    state.job.step,
                    state.done(),
                    state.total(),
                    state.job.error,
                );
            }
            effect = effects.recv() => match effect {
                Some(effect) => println!("effect {effect:?}"),
                None => break,
            },
            line = lines.recv() => match line {
                Some(line) => {
                    let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                    match verb {
                        "start" => service.send(Command::Start { total: rest.parse().unwrap_or(0) }),
                        "pin" => {
                            let command = Command::submit(install::Pin::new(rest.to_owned()), &state.borrow());
                            service.send(command);
                        }
                        "cancel" => service.send(Command::cancel(&state.borrow())),
                        "quit" => break,
                        _ => eprintln!("commands: start N, pin TEXT, cancel, quit"),
                    }
                }
                None => break,
            },
        }
    }
    service.shutdown().await;
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
