//! CPU, memory, temperatures, power mode, updates and display controls, with no UI.

mod brightness;
pub mod camera;
pub mod ddc;
pub mod display;
pub mod gpu;
mod helpers;
pub mod hwmon;
pub mod install;
mod model;
pub mod nvml;
mod service;
pub mod stats;
pub mod systemd;
mod updates;

pub use camera::{Camera, CameraConfig, CameraService, CameraUser};
pub use gpu::{GpuSensor, NoGpu};
pub use helpers::{
    FLAG_OTHER, FLAG_THAI, FLAG_US, UpdateCounts, hdr_enabled, install_updates_argv,
    is_reboot_package, keyboard_flag, reboot_packages, split_updates, update_counts,
};
pub use model::{
    Brightness, Command, CpuPower, Display, Effect, NightLight, Package, PowerMode, State, Stats,
    Updates,
};
pub use nvml::Nvml;
pub use service::{Backends, Config, Service};
pub use updates::{Checked, UpdateRun, UpdateSource};

pub mod boost;
pub mod cpufreq;
