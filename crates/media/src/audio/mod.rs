//! The default audio devices over the PulseAudio protocol, as `pipewire-pulse` serves it.

mod model;
mod service;

pub use model::{Audio, Command, Device, DeviceInfo, DeviceKind, OsdEvent, OsdKind, device_order};
pub use service::{AudioConfig, AudioService};
