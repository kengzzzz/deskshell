//! Watches the camera nodes, or scans once, and prints what it finds. It never opens a node.
//!
//! ```text
//! cargo run -p deskshell-system --example camera_watch -- probe [--repeat N] [ROOTS]
//! cargo run -p deskshell-system --example camera_watch -- watch [--seconds S] [ROOTS]
//! ROOTS: [--proc DIR] [--sys DIR] [--dev DIR] [--hold-ms MS]
//! ```

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deskshell_system::camera::{self, CameraConfig, CameraService};

const USAGE: &str = "usage: camera_watch probe|watch [--repeat N] [--seconds S] [--proc DIR] \
                     [--sys DIR] [--dev DIR] [--hold-ms MS]";

struct Args {
    watch: bool,
    repeat: u32,
    seconds: Option<u64>,
    config: CameraConfig,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let watch = match args.next().as_deref() {
        Some("probe") => false,
        Some("watch") => true,
        _ => return Err(USAGE.to_owned()),
    };
    let mut parsed = Args { watch, repeat: 1, seconds: None, config: CameraConfig::new() };
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        let number = || {
            value
                .parse::<u64>()
                .map_err(|err| format!("{flag}: {err}"))
        };
        match flag.as_str() {
            "--repeat" => parsed.repeat = u32::try_from(number()?).map_err(|e| e.to_string())?,
            "--seconds" => parsed.seconds = Some(number()?),
            "--hold-ms" => parsed.config.hold = Duration::from_millis(number()?),
            "--proc" => parsed.config.proc_root = PathBuf::from(&value),
            "--sys" => parsed.config.sys_root = PathBuf::from(&value),
            "--dev" => parsed.config.dev_root = PathBuf::from(&value),
            _ => return Err(USAGE.to_owned()),
        }
    }
    Ok(parsed)
}

fn comm(config: &CameraConfig, pid: u32) -> String {
    fs::read_to_string(
        config
            .proc_root
            .join(pid.to_string())
            .join("comm"),
    )
    .map(|comm| comm.trim().to_owned())
    .unwrap_or_default()
}

fn probe(args: &Args) -> Result<(), String> {
    let mut times = Vec::new();
    let mut last = None;
    for _ in 0..args.repeat.max(1) {
        let probe = camera::probe(&args.config).map_err(|err| err.to_string())?;
        times.push(probe.scan_time);
        last = Some(probe);
    }
    let Some(probe) = last else {
        return Ok(());
    };
    for node in &probe.nodes {
        let class = if node.hardware { "hardware" } else { "virtual" };
        let holders: Vec<String> = node
            .holders
            .iter()
            .map(|&(pid, pipewire)| {
                let tag = if pipewire { " pipewire" } else { "" };
                format!("{pid} ({}){tag}", comm(&args.config, pid))
            })
            .collect();
        println!(
            "{} {class} holders: [{}]",
            node.name,
            holders.join(", ")
        );
    }
    times.sort();
    let total: Duration = times.iter().sum();
    println!(
        "scan: {} processes, {} unreadable, {} fds; min {:?} median {:?} max {:?} mean {:?} over {}",
        probe.processes,
        probe.unreadable,
        probe.fds,
        times[0],
        times[times.len() / 2],
        times[times.len() - 1],
        total / times.len() as u32,
        times.len()
    );
    Ok(())
}

fn thread_switches(name: &str) -> Option<(u64, u64)> {
    for task in fs::read_dir("/proc/self/task")
        .ok()?
        .flatten()
    {
        let path = task.path();
        let comm = fs::read_to_string(path.join("comm")).unwrap_or_default();
        if comm.trim() != name {
            continue;
        }
        let status = fs::read_to_string(path.join("status")).ok()?;
        let field = |key: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|value| value.trim().parse().ok())
        };
        return Some((
            field("voluntary_ctxt_switches:")?,
            field("nonvoluntary_ctxt_switches:")?,
        ));
    }
    None
}

fn wall() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn watch(args: &Args) -> Result<(), String> {
    let (service, mut rx) =
        CameraService::start(args.config.clone()).map_err(|err| err.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    println!("{:.3} {:?}", wall(), rx.borrow().users);
    let print = async {
        while rx.changed().await.is_ok() {
            let users = rx.borrow_and_update().users.clone();
            println!("{:.3} {users:?}", wall());
        }
    };
    match args.seconds {
        None => runtime.block_on(print),
        Some(seconds) => {
            let measure = async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let before = thread_switches("camera-watch");
                let window = Duration::from_secs(seconds.saturating_sub(1).max(1));
                let mut previous = before;
                for _ in 0..window.as_secs() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let now = thread_switches("camera-watch");
                    if let (Some((v0, n0)), Some((v1, n1))) = (previous, now) {
                        println!(
                            "{:.3} switches in the last second: {} voluntary, {} involuntary",
                            wall(),
                            v1 - v0,
                            n1 - n0
                        );
                    }
                    previous = now;
                }
                (before, previous, window)
            };
            let (before, after, window) = runtime.block_on(async {
                tokio::select! {
                    measured = measure => measured,
                    () = print => (None, None, Duration::ZERO),
                }
            });
            match (before, after) {
                (Some((v0, n0)), Some((v1, n1))) => println!(
                    "camera-watch over {window:?}: {} voluntary, {} involuntary context switches",
                    v1 - v0,
                    n1 - n0
                ),
                _ => println!("camera-watch thread not found"),
            }
        }
    }
    service.shutdown();
    sleep(Duration::from_millis(10));
    Ok(())
}

fn main() -> ExitCode {
    let result = parse_args().and_then(|args| if args.watch { watch(&args) } else { probe(&args) });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
