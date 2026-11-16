//! A 16 bar spectrum of what the default sink plays, recorded from its monitor.

mod analyser;
mod service;

pub use analyser::{
    Analyser, BARS, FFT_LEN, FRAME_RATE, Frame, HIGH_CUTOFF, LOW_CUTOFF, NOISE_REDUCTION, Output,
    SLEEP_SECONDS, band_edges,
};
pub use service::{SpectrumConfig, SpectrumService, monitor_source};
