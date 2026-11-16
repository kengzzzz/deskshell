//! Read-only checks against the host, run by hand:
//!
//! ```text
//! cargo test -p deskshell-system --test live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! They read `/proc`, hwmon and NVML only. Nothing here opens I2C or runs a script.

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use deskshell_system::stats::Sampler;
use deskshell_system::{GpuSensor, NoGpu, Nvml, Stats, hwmon};

#[test]
#[ignore = "reads the host's /proc and hwmon"]
fn host_proc_and_hwmon() {
    let mut sampler = Sampler::new(
        Path::new("/proc"),
        Path::new("/sys"),
        Box::new(NoGpu),
    );
    let mut stats = Stats::default();
    sampler.sample(&mut stats, true);
    println!("first read (200 ms window): {stats:?}");
    sleep(Duration::from_secs(1));
    sampler.sample(&mut stats, true);
    println!("second read (1 s since the first): {stats:?}");
    sampler.stop();
    assert!(stats.memory > 0 && stats.memory <= 100);
    assert!(stats.cpu <= 100);

    let input = hwmon::find_cpu_input(Path::new("/sys/class/hwmon"));
    println!("cpu sensor input: {input:?}");
    let name = input
        .as_deref()
        .and_then(Path::parent)
        .and_then(|dir| fs::read_to_string(dir.join("name")).ok());
    println!(
        "cpu sensor device: {name:?}, temperature: {:?}",
        stats.cpu_temp
    );
    assert!(stats.cpu_temp.is_some());
}

#[derive(Debug, Default)]
struct Threads {
    count: usize,
    switches: u64,
    names: Vec<(u32, String, u64)>,
}

fn threads() -> Threads {
    let mut threads = Threads::default();
    for entry in fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
    {
        let tid: u32 = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let status = fs::read_to_string(entry.path().join("status")).unwrap_or_default();
        let switches: u64 = status
            .lines()
            .filter(|line| line.contains("ctxt_switches:"))
            .filter_map(|line| {
                line.split_ascii_whitespace()
                    .nth(1)?
                    .parse::<u64>()
                    .ok()
            })
            .sum();
        let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        threads.count += 1;
        threads.switches += switches;
        threads
            .names
            .push((tid, comm.trim().to_owned(), switches));
    }
    threads.names.sort();
    threads
}

fn idle_window(label: &str, seconds: u64) {
    let before = threads();
    sleep(Duration::from_secs(seconds));
    let after = threads();
    println!(
        "{label}: {} threads, {} context switches in {seconds} s",
        after.count,
        after.switches - before.switches.min(after.switches),
    );
    for (tid, name, switches) in &after.names {
        let earlier = before
            .names
            .iter()
            .find(|(t, ..)| t == tid)
            .map_or(0, |(.., s)| *s);
        println!(
            "  {tid} {name}: {} switches",
            switches - earlier.min(*switches)
        );
    }
}

#[test]
#[ignore = "loads the host's libnvidia-ml.so.1 and reads the temperature and load"]
fn host_nvml() {
    let mut nvml = Nvml::new();
    println!("before NVML: {} fds", fds());
    idle_window("before NVML", 10);

    nvml.open();
    assert!(nvml.loaded(), "libnvidia-ml.so.1 did not load");
    let reading = nvml
        .read()
        .map(|(t, n)| (t, n.to_owned()));
    let load = nvml.load();
    println!(
        "NVML read: {reading:?}, load {load:?}, {} fds",
        fds()
    );
    assert!(reading.is_some());
    assert!(load.is_some_and(|load| load <= 100));
    idle_window("NVML initialized, idle", 10);

    nvml.close();
    idle_window("after nvmlShutdown, library still loaded", 10);

    for cycle in 0..3 {
        let start = Instant::now();
        nvml.open();
        let init = start.elapsed();
        let reading = nvml
            .read()
            .map(|(t, n)| (t, n.to_owned()));
        let open = (threads().count, fds());
        nvml.close();
        println!(
            "cycle {cycle}: init {init:?}, read {reading:?}, open {} threads {} fds, closed {} threads {} fds",
            open.0,
            open.1,
            threads().count,
            fds(),
        );
    }
}

fn fds() -> usize {
    fs::read_dir("/proc/self/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}
