//! Prints the system state on every change. `--ddc` reads the real monitor and stalls a frame.
//!
//! ```text
//! cargo run -p deskshell-system --example system -- [--poll] [--ddc]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use deskshell_system::ddc::LinuxI2c;
use deskshell_system::{
    Backends, Command, Config, Service, State, UpdateRun, UpdateSource, hdr_enabled, keyboard_flag,
    update_counts,
};

const USAGE: &str = "usage: system [--poll] [--ddc]";

struct Args {
    poll: bool,
    ddc: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut poll = false;
    let mut ddc = false;
    for flag in std::env::args().skip(1) {
        match flag.as_str() {
            "--poll" => poll = true,
            "--ddc" => ddc = true,
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(Args { poll, ddc })
}

struct NoUpdates;

impl UpdateSource for NoUpdates {
    fn check(&self) -> UpdateRun {
        Box::pin(async { Err(String::from("the example runs no update check")) })
    }
}

fn print_state(state: &State) {
    let stats = &state.stats;
    println!(
        "stats: cpu={}% memory={}% cpu_temp={:?} gpu_temp={:?} gpu_load={:?} gpu_name={:?}",
        stats.cpu, stats.memory, stats.cpu_temp, stats.gpu_temp, stats.gpu_load, stats.gpu_name,
    );
    let power = &state.cpu_power;
    println!(
        "cpu power: text={:?} mode={:?} tooltip={:?}",
        power.text, power.mode, power.tooltip,
    );
    let updates = &state.updates;
    let counts = update_counts(&updates.packages);
    println!(
        "updates: ready={} checking={} total={} official={} aur={} listed={} checked_at={:?}",
        updates.ready,
        updates.checking,
        updates.total,
        updates.official,
        updates.aur,
        counts.total,
        updates.checked_at,
    );
    for package in &updates.packages {
        println!(
            "  {} {} -> {} aur={} reboot={}",
            package.name, package.old, package.new, package.aur, package.reboot,
        );
    }
    let display = &state.display;
    println!(
        "display: night_light={:?} vrr={} brightness={:?}",
        display.night_light, display.vrr, display.brightness,
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let config = Config::new();
    let mut backends = Backends::new(Arc::new(NoUpdates));
    if args.ddc {
        backends.i2c = Some(Arc::new(LinuxI2c::new(PathBuf::from("/dev"))));
    }
    let (service, mut state, mut effects) = Service::start(config, backends);
    println!(
        "flags: us={} th={} other={}, hdr(\"hdr\")={}",
        keyboard_flag("English (US)"),
        keyboard_flag("Thai"),
        keyboard_flag("German"),
        hdr_enabled("hdr"),
    );
    if args.poll {
        service.send(Command::Poll(true));
    }
    print_state(&state.borrow_and_update());
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                print_state(&state.borrow_and_update());
            }
            Some(effect) = effects.recv() => println!("effect: {effect:?}"),
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    service.shutdown().await;
    println!("stopped");
    ExitCode::SUCCESS
}
