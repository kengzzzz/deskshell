//! The stats sampler and CPU sensor lookup against fake hwmon, `/proc` and GPU readers.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{FakeGpu, TempDir, write};
use deskshell_system::hwmon::{CpuSensor, find_cpu_input};
use deskshell_system::stats::Sampler;
use deskshell_system::{NoGpu, Nvml, Stats};

const MEMINFO: &str = "MemTotal:       32000000 kB\nMemFree:         2000000 kB\n\
                       MemAvailable:   24000000 kB\nBuffers:          100000 kB\n";

fn channel(device: &Path, index: u32, label: Option<&str>, millidegrees: i64) {
    write(
        &device.join(format!("temp{index}_input")),
        format!("{millidegrees}\n"),
    );
    if let Some(label) = label {
        write(
            &device.join(format!("temp{index}_label")),
            format!("{label}\n"),
        );
    }
}

fn device(sys: &Path, index: u32, name: &str) -> std::path::PathBuf {
    let dir = sys.join(format!("class/hwmon/hwmon{index}"));
    write(&dir.join("name"), format!("{name}\n"));
    dir
}

/// The host's layout with the k10temp channels shuffled: Tctl is not the first channel, and
/// devices with lower numbers come first.
fn amd_tree(sys: &Path) {
    let nvme = device(sys, 0, "nvme");
    channel(&nvme, 1, Some("Composite"), 38850);
    let other = device(sys, 2, "prom21_xhci");
    channel(&other, 1, None, 41000);
    let k10 = device(sys, 10, "k10temp");
    channel(&k10, 1, Some("Tccd1"), 35000);
    channel(&k10, 2, Some("Tctl"), 47450);
    channel(&k10, 4, Some("Tccd2"), 34500);
}

#[test]
fn k10temp_by_name_and_label() {
    let root = TempDir::new("k10temp");
    let sys = root.path().join("sys");
    amd_tree(&sys);
    let input = find_cpu_input(&sys.join("class/hwmon")).unwrap();
    assert_eq!(input, sys.join("class/hwmon/hwmon10/temp2_input"));

    let mut sensor = CpuSensor::new(sys.join("class/hwmon"));
    let mut buf = String::new();
    // sensors shows 47.45 as "+47.5°C", and that rounds to 48.
    assert_eq!(sensor.read(&mut buf), Some(48));
    write(&input, "47375\n");
    assert_eq!(sensor.read(&mut buf), Some(47));
}

#[test]
fn other_cpus_use_the_first_channel_of_the_first_cpu_device() {
    let root = TempDir::new("coretemp");
    let sys = root.path().join("sys");
    let nvme = device(&sys, 0, "nvme");
    channel(&nvme, 1, Some("Composite"), 38850);
    let coretemp = device(&sys, 3, "coretemp");
    channel(&coretemp, 2, Some("Core 0"), 50000);
    channel(&coretemp, 1, Some("Package id 0"), 52500);
    let later = device(&sys, 4, "k10temp");
    channel(&later, 1, None, 60000);
    let mut sensor = CpuSensor::new(sys.join("class/hwmon"));
    assert_eq!(sensor.read(&mut String::new()), Some(53));
    assert_eq!(
        sensor.input(),
        Some(
            sys.join("class/hwmon/hwmon3/temp1_input")
                .as_path()
        )
    );

    let root = TempDir::new("k10temp-nolabel");
    let sys = root.path().join("sys");
    let k10 = device(&sys, 1, "k10temp");
    channel(&k10, 3, Some("Tccd1"), 40000);
    channel(&k10, 1, None, 45600);
    assert_eq!(
        find_cpu_input(&sys.join("class/hwmon")),
        Some(sys.join("class/hwmon/hwmon1/temp1_input"))
    );
}

#[test]
fn missing_hwmon_device() {
    let root = TempDir::new("nohwmon");
    let mut sensor = CpuSensor::new(root.path().join("sys/class/hwmon"));
    assert_eq!(sensor.read(&mut String::new()), None);

    let sys = root.path().join("sys2");
    let nvme = device(&sys, 0, "nvme");
    channel(&nvme, 1, Some("Composite"), 38850);
    let mut sensor = CpuSensor::new(sys.join("class/hwmon"));
    assert_eq!(sensor.read(&mut String::new()), None);
}

#[test]
fn a_vanished_device_is_found_again() {
    let root = TempDir::new("vanish");
    let sys = root.path().join("sys");
    amd_tree(&sys);
    let mut sensor = CpuSensor::new(sys.join("class/hwmon"));
    let mut buf = String::new();
    assert_eq!(sensor.read(&mut buf), Some(48));

    fs::remove_dir_all(sys.join("class/hwmon/hwmon10")).unwrap();
    assert_eq!(sensor.read(&mut buf), None);
    let k10 = device(&sys, 11, "k10temp");
    channel(&k10, 1, Some("Tctl"), 44000);
    assert_eq!(sensor.read(&mut buf), Some(44));
    assert_eq!(
        sensor.input(),
        Some(
            sys.join("class/hwmon/hwmon11/temp1_input")
                .as_path()
        )
    );
}

fn proc_tree(root: &Path, stat: &str) {
    write(&root.join("proc/stat"), stat);
    write(&root.join("proc/meminfo"), MEMINFO);
}

#[test]
fn cpu_and_memory_use() {
    let root = TempDir::new("proc");
    proc_tree(
        root.path(),
        "cpu  1000 0 1000 7000 1000 0 0 0 0 0\n",
    );
    let sys = root.path().join("sys");
    amd_tree(&sys);
    let mut sampler = Sampler::new(&root.path().join("proc"), &sys, Box::new(NoGpu));
    sampler.set_first_window(Duration::ZERO);
    let mut stats = Stats::default();

    sampler.sample(&mut stats, true);
    assert_eq!(
        stats.cpu, 0,
        "the file did not change within the first window"
    );
    assert_eq!(stats.memory, 25);
    assert_eq!(stats.cpu_temp, Some(48));
    assert_eq!(
        (stats.gpu_temp, stats.gpu_name.as_str()),
        (None, "")
    );

    // idle + iowait goes from 8000 to 8600, total from 10000 to 11000: 100 * 400 / 1000.
    write(
        &root.path().join("proc/stat"),
        "cpu  1200 0 1200 7500 1100 0 0 0 0 0\n",
    );
    sampler.sample(&mut stats, true);
    assert_eq!(stats.cpu, 40);

    // Polling keeps the previous sample: 11000 to 11300 total, 8600 to 8800 idle.
    write(
        &root.path().join("proc/stat"),
        "cpu  1250 0 1250 7700 1100 0 0 0 0 0\n",
    );
    sampler.sample(&mut stats, true);
    assert_eq!(stats.cpu, 33);

    // Stopping drops it, so the next read takes its own window and sees no change.
    sampler.stop();
    write(
        &root.path().join("proc/stat"),
        "cpu  9999 0 9999 7700 1100 0 0 0 0 0\n",
    );
    sampler.sample(&mut stats, false);
    assert_eq!(stats.cpu, 0);
}

#[test]
fn the_first_read_waits_its_window() {
    let root = TempDir::new("window");
    proc_tree(root.path(), "cpu  1 0 1 7 1 0 0 0 0 0\n");
    let mut sampler = Sampler::new(
        &root.path().join("proc"),
        &root.path().join("sys"),
        Box::new(NoGpu),
    );
    let window = Duration::from_millis(400);
    sampler.set_first_window(window);
    let mut stats = Stats::default();
    let start = Instant::now();
    sampler.sample(&mut stats, true);
    assert!(start.elapsed() >= window);
    let start = Instant::now();
    sampler.sample(&mut stats, true);
    assert!(start.elapsed() < window, "a poll needs no sleep");
}

#[test]
fn missing_proc_files_read_as_zero() {
    let root = TempDir::new("noproc");
    let mut sampler = Sampler::new(
        &root.path().join("proc"),
        &root.path().join("sys"),
        Box::new(NoGpu),
    );
    sampler.set_first_window(Duration::ZERO);
    let mut stats = Stats { cpu: 9, memory: 9, cpu_temp: Some(1), ..Stats::default() };
    sampler.sample(&mut stats, false);
    assert_eq!(stats, Stats::default());
}

#[test]
fn gpu_through_the_fake() {
    let root = TempDir::new("gpu");
    proc_tree(root.path(), "cpu  1 0 1 7 1 0 0 0 0 0\n");
    let gpu = FakeGpu { reading: Some((61, "Test GPU 9000")), load: Some(0), ..FakeGpu::default() };
    let events = std::sync::Arc::clone(&gpu.events);
    let mut sampler = Sampler::new(
        &root.path().join("proc"),
        &root.path().join("sys"),
        Box::new(gpu),
    );
    sampler.set_first_window(Duration::ZERO);
    let mut stats = Stats::default();

    sampler.sample(&mut stats, true);
    sampler.sample(&mut stats, true);
    assert_eq!(
        (
            stats.gpu_temp,
            stats.gpu_load,
            stats.gpu_name.as_str()
        ),
        (Some(61), Some(0), "Test GPU 9000")
    );
    assert_eq!(
        *events.lock().unwrap(),
        ["open"],
        "polling keeps the GPU open"
    );
    sampler.stop();
    assert_eq!(*events.lock().unwrap(), ["open", "close"]);

    let mut one_off = Stats {
        gpu_temp: Some(1),
        gpu_load: Some(2),
        gpu_name: "Earlier".into(),
        ..Stats::default()
    };
    sampler.sample(&mut one_off, false);
    assert_eq!(*events.lock().unwrap(), ["open", "close"]);
    assert_eq!(
        (
            one_off.gpu_temp,
            one_off.gpu_load,
            one_off.gpu_name.as_str()
        ),
        (Some(1), Some(2), "Earlier")
    );
    assert_eq!(one_off.memory, 25);

    sampler.sample(&mut stats, true);
    assert_eq!(*events.lock().unwrap(), ["open", "close", "open"]);

    let mut sampler = Sampler::new(
        &root.path().join("proc"),
        &root.path().join("sys"),
        Box::new(FakeGpu::default()),
    );
    sampler.set_first_window(Duration::ZERO);
    sampler.sample(&mut stats, true);
    assert_eq!(
        (
            stats.gpu_temp,
            stats.gpu_load,
            stats.gpu_name.as_str()
        ),
        (None, None, ""),
        "no GPU empties all three"
    );

    for (reading, load) in [(None, Some(30)), (Some((50, "Test GPU 9000")), None)] {
        let mut sampler = Sampler::new(
            &root.path().join("proc"),
            &root.path().join("sys"),
            Box::new(FakeGpu { reading, load, ..FakeGpu::default() }),
        );
        sampler.set_first_window(Duration::ZERO);
        sampler.sample(&mut stats, true);
        assert_eq!(
            (stats.gpu_temp, stats.gpu_load),
            (reading.map(|(t, _)| t), load)
        );
    }
}

#[test]
fn missing_nvml_library() {
    let root = TempDir::new("nvml");
    proc_tree(root.path(), "cpu  1 0 1 7 1 0 0 0 0 0\n");
    let nvml = Nvml::with_library("libdeskshell-missing-nvml.so.1");
    let mut sampler = Sampler::new(
        &root.path().join("proc"),
        &root.path().join("sys"),
        Box::new(nvml),
    );
    sampler.set_first_window(Duration::ZERO);
    let mut stats =
        Stats { gpu_temp: Some(50), gpu_load: Some(5), gpu_name: "Old".into(), ..Stats::default() };
    sampler.sample(&mut stats, true);
    assert_eq!(
        (
            stats.gpu_temp,
            stats.gpu_load,
            stats.gpu_name.as_str()
        ),
        (None, None, "")
    );
    assert_eq!(stats.memory, 25);
}
