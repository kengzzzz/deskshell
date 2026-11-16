//! The service's state, commands and effects.

use std::time::SystemTime;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub stats: Stats,
    pub cpu_power: CpuPower,
    pub updates: Updates,
    pub display: Display,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub cpu: u8,
    pub memory: u8,
    pub cpu_temp: Option<i32>,
    /// Read only while polling, so it stays `None` until the first poll.
    pub gpu_temp: Option<i32>,
    pub gpu_name: String,
    /// Busy percent over NVML's last sample period, not since the last poll.
    pub gpu_load: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuPower {
    pub text: String,
    pub mode: Option<PowerMode>,
    pub tooltip: String,
    pub boosted: bool,
}

impl CpuPower {
    pub const DEFAULT_TEXT: &str = "○";
    pub const DEFAULT_TOOLTIP: &str = "CPU power state is unavailable.";
}

impl Default for CpuPower {
    fn default() -> Self {
        Self {
            text: Self::DEFAULT_TEXT.to_owned(),
            mode: None,
            tooltip: Self::DEFAULT_TOOLTIP.to_owned(),
            boosted: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerMode {
    Eco,
    Balanced,
    Performance,
}

impl PowerMode {
    pub const ALL: [Self; 3] = [Self::Eco, Self::Balanced, Self::Performance];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eco => "eco",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }

    pub fn from_class(class: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == class)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Updates {
    pub ready: bool,
    pub checking: bool,
    pub total: u32,
    pub official: u32,
    pub aur: u32,
    pub packages: Vec<Package>,
    pub checked_at: Option<SystemTime>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub old: String,
    pub new: String,
    pub aur: bool,
    pub reboot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub night_light: NightLight,
    /// Hyprland's `misc:vrr`, or -1 when unknown.
    pub vrr: i32,
    pub brightness: Option<Brightness>,
}

impl Default for Display {
    fn default() -> Self {
        Self { night_light: NightLight::default(), vrr: -1, brightness: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NightLight {
    pub available: bool,
    pub enabled: bool,
    pub scheduled: bool,
    /// Kelvin, 0 when unknown.
    pub temperature: u32,
}

impl Default for NightLight {
    fn default() -> Self {
        Self { available: false, enabled: false, scheduled: true, temperature: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brightness {
    pub value: u16,
    pub max: u16,
    pub monitor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Poll(bool),
    RefreshSystem,
    SetCpuMode(PowerMode),
    CycleCpuMode,
    ShowCpuDetails,
    RefreshUpdates,
    RefreshDisplay,
    ToggleNightLight,
    ResumeNightLightSchedule,
    ToggleVrr,
    ToggleHdr,
    SetBrightness(i32),
    /// Reads the brightness even when known. A read stalls a compositor frame, so the Hub
    /// never sends it.
    ReadBrightness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    RefreshMonitors,
    Notify { summary: String, body: String, critical: bool },
}
