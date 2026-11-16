//! Media players over MPRIS, the default audio devices, the output spectrum and mic users.

pub mod audio;
pub mod helpers;
pub mod mic;
pub mod mpris;
mod pulse;
pub mod spectrum;
pub mod streams;

pub use audio::{
    Audio, AudioConfig, AudioService, Device, DeviceInfo, DeviceKind, OsdEvent, OsdKind,
};
pub use helpers::{format_time, mic_icon, percent_label, volume_icon};
pub use mic::{Mic, MicConfig, MicService, MicUser};
pub use mpris::{MprisError, MprisService, PlaybackStatus, Player, Players};
pub use spectrum::{Frame, SpectrumConfig, SpectrumService};
pub use streams::{ListError, SinkInput, sink_inputs};
