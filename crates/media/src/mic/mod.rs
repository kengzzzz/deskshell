//! Which apps record the microphone, read over the PulseAudio protocol.

mod hold;
mod model;
mod rules;
mod service;

pub use model::{Mic, MicUser};
pub use service::{MicConfig, MicService};
