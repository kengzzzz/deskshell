//! CPU, memory, CPU temperature and GPU stats from `/proc`, hwmon and a [`GpuSensor`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::gpu::GpuSensor;
use crate::hwmon::CpuSensor;
use crate::model::Stats;

pub const FIRST_WINDOW: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    /// Guest time is already in user and nice, so it is not added.
    pub total: u64,
}

impl CpuTimes {
    pub fn parse(stat: &str) -> Option<Self> {
        let line = stat.lines().next()?;
        let mut fields = line.split_ascii_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let mut values = [0_u64; 8];
        for (slot, field) in values.iter_mut().zip(fields) {
            *slot = field.parse().ok()?;
        }
        let [user, nice, system, idle, iowait, irq, softirq, steal] = values;
        Some(Self {
            idle: idle + iowait,
            total: user + nice + system + idle + iowait + irq + softirq + steal,
        })
    }
}

/// Clamped to 0..=100 because iowait can run backwards between samples.
pub fn cpu_percent(previous: CpuTimes, current: CpuTimes) -> u8 {
    let total = i128::from(current.total) - i128::from(previous.total);
    let idle = i128::from(current.idle) - i128::from(previous.idle);
    if total <= 0 {
        return 0;
    }
    let percent = (100 * (total - idle) / total).clamp(0, 100);
    u8::try_from(percent).unwrap_or(0)
}

pub fn memory_percent(meminfo: &str) -> u8 {
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = kilobytes(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = kilobytes(rest);
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    match (total, available) {
        (Some(total), Some(available)) if total > 0 => {
            let used = i128::from(total) - i128::from(available);
            u8::try_from((100 * used / i128::from(total)).clamp(0, 100)).unwrap_or(0)
        }
        _ => 0,
    }
}

fn read_file(path: &Path, buf: &mut String) -> bool {
    buf.clear();
    fs::File::open(path)
        .and_then(|mut file| file.read_to_string(buf))
        .is_ok()
}

fn kilobytes(rest: &str) -> Option<u64> {
    rest.split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

pub struct Sampler {
    stat: PathBuf,
    meminfo: PathBuf,
    cpu_sensor: CpuSensor,
    gpu: Box<dyn GpuSensor>,
    gpu_open: bool,
    buf: String,
    previous: Option<CpuTimes>,
    first_window: Duration,
}

impl Sampler {
    pub fn new(proc_root: &Path, sys_root: &Path, gpu: Box<dyn GpuSensor>) -> Self {
        Self {
            stat: proc_root.join("stat"),
            meminfo: proc_root.join("meminfo"),
            cpu_sensor: CpuSensor::new(sys_root.join("class/hwmon")),
            gpu,
            gpu_open: false,
            buf: String::new(),
            previous: None,
            first_window: FIRST_WINDOW,
        }
    }

    pub fn set_first_window(&mut self, window: Duration) {
        self.first_window = window;
    }

    fn cpu_times(&mut self) -> Option<CpuTimes> {
        read_file(&self.stat, &mut self.buf).then(|| CpuTimes::parse(&self.buf))?
    }

    /// Outside polling the GPU is not read. NVML's first init starts a thread and opens device
    /// files that `nvmlShutdown` never releases.
    pub fn sample(&mut self, stats: &mut Stats, polling: bool) {
        let previous = match self.previous {
            Some(previous) => Some(previous),
            None => {
                let first = self.cpu_times();
                std::thread::sleep(self.first_window);
                first
            }
        };
        let current = self.cpu_times();
        stats.cpu = match (previous, current) {
            (Some(previous), Some(current)) => cpu_percent(previous, current),
            _ => 0,
        };
        self.previous = if polling { current } else { None };

        stats.memory =
            if read_file(&self.meminfo, &mut self.buf) { memory_percent(&self.buf) } else { 0 };
        stats.cpu_temp = self.cpu_sensor.read(&mut self.buf);

        if !polling {
            self.previous = None;
            return;
        }
        if !self.gpu_open {
            self.gpu.open();
            self.gpu_open = true;
        }
        match self.gpu.read() {
            Some((temperature, name)) => {
                stats.gpu_temp = Some(temperature);
                if stats.gpu_name != name {
                    stats.gpu_name.clear();
                    stats.gpu_name.push_str(name);
                }
            }
            None => {
                stats.gpu_temp = None;
                stats.gpu_name.clear();
            }
        }
        stats.gpu_load = self.gpu.load();
    }

    pub fn stop(&mut self) {
        self.previous = None;
        self.cpu_sensor.reset();
        if self.gpu_open {
            self.gpu.close();
            self.gpu_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat() {
        let stat = "cpu  4705 356 584 3699176 23060 0 277 0 0 0\ncpu0 1393 280 278 924616 \
                    5825 0 178 0 0 0\nintr 1462898\n";
        assert_eq!(
            CpuTimes::parse(stat),
            Some(CpuTimes { idle: 3_699_176 + 23_060, total: 3_728_158 })
        );
        assert_eq!(
            CpuTimes::parse("cpu 1 2 3 4\n"),
            Some(CpuTimes { idle: 4, total: 10 })
        );
        assert_eq!(CpuTimes::parse("cpu0 1 2 3 4\n"), None);
        assert_eq!(CpuTimes::parse("cpu 1 x 3\n"), None);
        assert_eq!(CpuTimes::parse(""), None);
    }

    #[test]
    fn cpu_use() {
        let a = CpuTimes { idle: 1000, total: 2000 };
        assert_eq!(
            cpu_percent(a, CpuTimes { idle: 1150, total: 2200 }),
            25
        );
        assert_eq!(
            cpu_percent(a, CpuTimes { idle: 1067, total: 2200 }),
            66
        );
        assert_eq!(cpu_percent(a, a), 0);
        assert_eq!(
            cpu_percent(a, CpuTimes { idle: 990, total: 2010 }),
            100
        );
        assert_eq!(
            cpu_percent(a, CpuTimes { idle: 1300, total: 2100 }),
            0
        );
    }

    #[test]
    fn memory_use() {
        let meminfo = "MemTotal:       65536000 kB\nMemFree:         1000000 kB\n\
                       MemAvailable:   49152000 kB\nBuffers:          100 kB\n";
        assert_eq!(memory_percent(meminfo), 25);
        assert_eq!(
            memory_percent("MemTotal: 3 kB\nMemAvailable: 1 kB\n"),
            66
        );
        assert_eq!(memory_percent("MemTotal: 100 kB\n"), 0);
        assert_eq!(
            memory_percent("MemTotal: 0 kB\nMemAvailable: 0 kB\n"),
            0
        );
        assert_eq!(memory_percent(""), 0);
    }
}
