//! The CPU power mode over sysfs. A udev rule grants the writes, so nothing here runs as root.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::model::{CpuPower, Effect, PowerMode};

pub const DRIVER: &str = "amd-pstate-epp";
pub const STATE_FILE: &str = "cpu-power-mode";
pub const TRIGGER: &str = "sudo udevadm trigger --subsystem-match=cpu --action=add";
pub const UNAVAILABLE_TEXT: &str = "CPU?";

const SUMMARY: &str = "CPU Power";

pub fn settings(mode: PowerMode) -> (&'static str, &'static str) {
    match mode {
        PowerMode::Eco => ("powersave", "power"),
        PowerMode::Balanced => ("powersave", "balance_performance"),
        PowerMode::Performance => ("performance", "performance"),
    }
}

pub fn next(live: Option<PowerMode>) -> PowerMode {
    match live {
        Some(PowerMode::Eco) | None => PowerMode::Balanced,
        Some(PowerMode::Balanced) => PowerMode::Performance,
        Some(PowerMode::Performance) => PowerMode::Eco,
    }
}

pub fn title(mode: Option<PowerMode>) -> &'static str {
    match mode {
        Some(PowerMode::Eco) => "Eco",
        Some(PowerMode::Balanced) => "Balanced",
        Some(PowerMode::Performance) => "Performance",
        None => "Custom",
    }
}

fn glyph(mode: Option<PowerMode>) -> &'static str {
    match mode {
        Some(PowerMode::Eco) => "◔",
        Some(PowerMode::Balanced) => "◑",
        Some(PowerMode::Performance) => "◕",
        None => CpuPower::DEFAULT_TEXT,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub governor: String,
    pub epp: Option<String>,
    pub driver: Option<String>,
}

impl Reading {
    pub fn mode(&self) -> Option<PowerMode> {
        PowerMode::ALL
            .into_iter()
            .find(|&mode| (self.governor.as_str(), self.epp.as_deref()) == pair(mode))
    }

    fn epp(&self) -> &str {
        self.epp
            .as_deref()
            .unwrap_or("unavailable")
    }

    fn driver(&self) -> &str {
        self.driver
            .as_deref()
            .unwrap_or("unknown")
    }

    pub fn cpu_power(&self, boosted: bool) -> CpuPower {
        let mode = self.mode();
        let mut tooltip = format!(
            "Mode: {}\nGovernor: {}\nEPP: {}\nDriver: {}",
            title(mode),
            self.governor,
            self.epp(),
            self.driver()
        );
        if boosted {
            tooltip.push_str("\nGame boost: on");
        }
        CpuPower { text: glyph(mode).to_owned(), mode, tooltip, boosted }
    }

    pub fn toast(&self) -> Effect {
        Effect::Notify {
            summary: format!("{SUMMARY}: {}", title(self.mode())),
            body: format!(
                "Governor: {}\nEPP: {}\nDriver: {}",
                self.governor,
                self.epp(),
                self.driver()
            ),
            critical: false,
        }
    }
}

fn pair(mode: PowerMode) -> (&'static str, Option<&'static str>) {
    let (governor, epp) = settings(mode);
    (governor, Some(epp))
}

pub fn unavailable() -> CpuPower {
    CpuPower {
        text: UNAVAILABLE_TEXT.to_owned(),
        mode: None,
        tooltip: CpuPower::DEFAULT_TOOLTIP.to_owned(),
        boosted: false,
    }
}

fn critical(body: String) -> Effect {
    Effect::Notify { summary: SUMMARY.to_owned(), body, critical: true }
}

fn unavailable_toast() -> Effect {
    critical(CpuPower::DEFAULT_TOOLTIP.to_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("CPU frequency driver information is unavailable.")]
    NoDriver,
    #[error("Unsupported CPU frequency driver: {0}")]
    Driver(String),
    #[error("No active CPU frequency policy.")]
    NoPolicy,
    #[error(
        "Cannot write {}. The deskshell-power udev rule is not applied, or you are not in the \
         deskshell-power group. Apply the rule with: {TRIGGER}",
        .0.display()
    )]
    Permission(PathBuf),
    #[error("Cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ApplyError {
    pub fn is_permission(&self) -> bool {
        matches!(self, Self::Permission(_))
    }

    pub fn toast(&self) -> Effect {
        critical(self.to_string())
    }
}

pub trait CpuFreq: Send + Sync {
    fn read(&self) -> io::Result<Reading>;

    fn apply(&self, mode: PowerMode) -> Result<(), ApplyError>;
}

#[derive(Debug, Clone)]
pub struct Sysfs {
    cpu: PathBuf,
    governor: PathBuf,
    epp: PathBuf,
    driver: PathBuf,
}

impl Sysfs {
    pub fn new(sys_root: &Path) -> Self {
        let cpu = sys_root.join("devices/system/cpu");
        let cpu0 = cpu.join("cpu0/cpufreq");
        Self {
            governor: cpu0.join("scaling_governor"),
            epp: cpu0.join("energy_performance_preference"),
            driver: cpu0.join("scaling_driver"),
            cpu,
        }
    }

    // The kernel answers EBUSY for a policy whose CPUs are all offline, as after SMT is turned
    // off, so such a policy is skipped.
    fn policies(&self) -> io::Result<Vec<PathBuf>> {
        let mut policies = Vec::new();
        for entry in fs::read_dir(self.cpu.join("cpufreq"))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(number) = name
                .to_str()
                .and_then(|name| name.strip_prefix("policy"))
                .and_then(|number| number.parse::<u32>().ok())
            else {
                continue;
            };
            let path = entry.path();
            if read_value(&path.join("affected_cpus")).is_ok_and(|cpus| !cpus.is_empty()) {
                policies.push((number, path));
            }
        }
        policies.sort_unstable_by_key(|&(number, _)| number);
        Ok(policies
            .into_iter()
            .map(|(_, path)| path)
            .collect())
    }
}

impl CpuFreq for Sysfs {
    fn read(&self) -> io::Result<Reading> {
        Ok(Reading {
            governor: read_value(&self.governor)?,
            epp: read_value(&self.epp).ok(),
            driver: read_value(&self.driver).ok(),
        })
    }

    fn apply(&self, mode: PowerMode) -> Result<(), ApplyError> {
        let driver = read_value(&self.driver).map_err(|_| ApplyError::NoDriver)?;
        if driver != DRIVER {
            return Err(ApplyError::Driver(driver));
        }
        let policies = self
            .policies()
            .map_err(|source| ApplyError::Write { path: self.cpu.join("cpufreq"), source })?;
        if policies.is_empty() {
            return Err(ApplyError::NoPolicy);
        }
        // All governors go first. amd-pstate refuses an EPP other than `performance` while the
        // governor is `performance`.
        let (governor, epp) = settings(mode);
        for (file, value) in
            [("scaling_governor", governor), ("energy_performance_preference", epp)]
        {
            for policy in &policies {
                write_value(&policy.join(file), value)?;
            }
        }
        Ok(())
    }
}

fn read_value(path: &Path) -> io::Result<String> {
    let mut value = fs::read_to_string(path)?;
    value.truncate(value.trim_end().len());
    Ok(value)
}

/// One `write` of the value and a newline. Never creates the file.
fn write_value(path: &Path, value: &str) -> Result<(), ApplyError> {
    let error = |source: io::Error| {
        if source.kind() == io::ErrorKind::PermissionDenied {
            ApplyError::Permission(path.to_owned())
        } else {
            ApplyError::Write { path: path.to_owned(), source }
        }
    };
    let mut line = String::with_capacity(value.len() + 1);
    line.push_str(value);
    line.push('\n');
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(error)?
        .write_all(line.as_bytes())
        .map_err(error)
}

pub fn read_saved(path: &Path) -> Option<PowerMode> {
    let saved = fs::read_to_string(path).ok()?;
    PowerMode::from_class(saved.trim_end_matches('\n'))
}

pub fn save(path: &Path, mode: PowerMode) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = OsString::from(path.as_os_str());
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let mut line = String::with_capacity(16);
    line.push_str(mode.as_str());
    line.push('\n');
    fs::write(&temp, line)?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub power: CpuPower,
    pub toasts: Vec<Effect>,
}

pub fn set(freq: &dyn CpuFreq, state_file: Option<&Path>, mode: PowerMode) -> Outcome {
    match freq.read() {
        Ok(_) => change(freq, state_file, mode),
        Err(_) => Outcome { power: unavailable(), toasts: vec![unavailable_toast()] },
    }
}

pub fn cycle(freq: &dyn CpuFreq, state_file: Option<&Path>) -> Outcome {
    match freq.read() {
        Ok(reading) => change(freq, state_file, next(reading.mode())),
        Err(_) => Outcome { power: unavailable(), toasts: vec![unavailable_toast()] },
    }
}

fn change(freq: &dyn CpuFreq, state_file: Option<&Path>, mode: PowerMode) -> Outcome {
    let mut toasts = Vec::with_capacity(2);
    let applied = freq.apply(mode);
    if let Err(error) = &applied {
        toasts.push(error.toast());
    }
    let save_error = match (applied, state_file) {
        (Ok(()), Some(path)) => save(path, mode)
            .err()
            .map(|error| (path, error)),
        _ => None,
    };
    let power = match freq.read() {
        Ok(reading) => {
            if toasts.is_empty() {
                toasts.push(reading.toast());
            }
            reading.cpu_power(false)
        }
        Err(_) => {
            if toasts.is_empty() {
                toasts.push(unavailable_toast());
            }
            unavailable()
        }
    };
    if let Some((path, error)) = save_error {
        toasts.push(critical(format!(
            "The mode will not survive a restart. Cannot write {}: {error}",
            path.display()
        )));
    }
    Outcome { power, toasts }
}

pub fn details(freq: &dyn CpuFreq) -> Effect {
    freq.read()
        .map_or_else(|_| unavailable_toast(), |reading| reading.toast())
}
