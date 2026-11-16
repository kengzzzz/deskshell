//! CPU temperature from hwmon: k10temp's `Tctl`, else the first CPU sensor's lowest input.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const CPU_DRIVERS: [&str; 6] =
    ["k10temp", "zenpower", "coretemp", "k8temp", "via_cputemp", "cpu_thermal"];

#[derive(Debug)]
pub struct CpuSensor {
    hwmon: PathBuf,
    input: Option<PathBuf>,
    searched: bool,
}

impl CpuSensor {
    pub fn new(hwmon: PathBuf) -> Self {
        Self { hwmon, input: None, searched: false }
    }

    pub fn read(&mut self, buf: &mut String) -> Option<i32> {
        if self.input.is_none() && !self.searched {
            self.searched = true;
            self.input = find_cpu_input(&self.hwmon);
        }
        let input = self.input.as_deref()?;
        match read_millidegrees(input, buf) {
            Some(millidegrees) => Some(round_like_script(millidegrees)),
            None => {
                self.input = None;
                self.searched = false;
                None
            }
        }
    }

    pub fn reset(&mut self) {
        if self.input.is_none() {
            self.searched = false;
        }
    }

    pub fn input(&self) -> Option<&Path> {
        self.input.as_deref()
    }
}

fn read_trimmed(path: &Path, buf: &mut String) -> Option<()> {
    buf.clear();
    fs::File::open(path)
        .ok()?
        .read_to_string(buf)
        .ok()?;
    Some(())
}

fn read_millidegrees(path: &Path, buf: &mut String) -> Option<i64> {
    read_trimmed(path, buf)?;
    buf.trim().parse().ok()
}

fn hwmon_index(name: &str) -> Option<u32> {
    name.strip_prefix("hwmon")?.parse().ok()
}

fn channel_index(file: &str, suffix: &str) -> Option<u32> {
    file.strip_prefix("temp")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

pub fn find_cpu_input(hwmon: &Path) -> Option<PathBuf> {
    let mut devices: Vec<(u32, PathBuf, String)> = fs::read_dir(hwmon)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let index = hwmon_index(entry.file_name().to_str()?)?;
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).ok()?;
            Some((index, dir, name.trim().to_owned()))
        })
        .collect();
    devices.sort_unstable_by_key(|(index, ..)| *index);

    let mut buf = String::new();
    for (_, dir, name) in &devices {
        if name == "k10temp"
            && let Some(input) = labelled_input(dir, "Tctl", &mut buf)
        {
            return Some(input);
        }
    }
    devices
        .iter()
        .filter(|(_, _, name)| CPU_DRIVERS.contains(&name.as_str()))
        .find_map(|(_, dir, _)| first_input(dir))
}

fn labelled_input(dir: &Path, label: &str, buf: &mut String) -> Option<PathBuf> {
    let mut channels: Vec<u32> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| channel_index(entry.file_name().to_str()?, "_label"))
        .collect();
    channels.sort_unstable();
    channels
        .into_iter()
        .find(|channel| {
            read_trimmed(&dir.join(format!("temp{channel}_label")), buf).is_some()
                && buf.trim() == label
        })
        .map(|channel| dir.join(format!("temp{channel}_input")))
        .filter(|input| input.is_file())
}

fn first_input(dir: &Path) -> Option<PathBuf> {
    let channel = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| channel_index(entry.file_name().to_str()?, "_input"))
        .min()?;
    Some(dir.join(format!("temp{channel}_input")))
}

/// Rounds as `sensors` (`%.1f`, ties to even) and then awk's `int(x + 0.5)` did, so 47.45
/// becomes 48.
pub fn round_like_script(millidegrees: i64) -> i32 {
    let tenths = tenths_like_printf(millidegrees);
    i32::try_from((tenths + 5) / 10).unwrap_or(if tenths < 0 { i32::MIN } else { i32::MAX })
}

fn tenths_like_printf(millidegrees: i64) -> i64 {
    let below = millidegrees.div_euclid(100);
    let value = millidegrees as f64 / 1000.0;
    // The fused multiply-add rounds once, so its sign is the exact sign of the distance
    // between `value` and the midpoint of the two candidates, (2 * below + 1) / 20.
    let distance = 20.0_f64.mul_add(value, -((2 * below + 1) as f64));
    if distance > 0.0 || (distance == 0.0 && below % 2 != 0) { below + 1 } else { below }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_like_sensors_and_awk() {
        // Expected values from Python's '%.1f' % (m / 1000.0), then int(x + 0.5).
        for (millidegrees, expected) in [
            (47375, 47),
            (47450, 48),
            (1450, 1),
            (47250, 47),
            (47750, 48),
            (46500, 47),
            (47125, 47),
            (47625, 48),
            (47875, 48),
            (45000, 45),
            (999, 1),
            (104950, 105),
            (104850, 105),
            (-1450, 0),
            (-47450, -47),
            (-250, 0),
            (-750, 0),
            (-1600, -1),
        ] {
            assert_eq!(
                round_like_script(millidegrees),
                expected,
                "{millidegrees}"
            );
        }
    }
}
