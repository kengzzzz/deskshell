//! The system update: the runner in a transient user unit, its askpass, and the shell's side.

pub mod askpass;
pub mod fake;
pub mod gate;
pub mod model;
pub mod parse;
mod pin;
pub mod protocol;
pub mod runner;
mod service;

pub use model::{
    Action, AuthStage, Job, Model, Notice, PamStage, Phase, Rejected, STEPS, STOPPED_ERROR, State,
    StepState, Unit, Written, format_duration,
};
pub use pin::{MAX_PIN, Pin, line_fits};
pub use protocol::{AuthView, Report};
pub use runner::{LAST_FILE, RUNTIME_SUBDIR};
pub use service::{CancelReply, Command, Config, Effect, Service};

pub const UNIT: &str = runner::UNIT;
