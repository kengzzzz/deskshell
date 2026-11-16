//! MPRIS players, followed over D-Bus.

mod model;
mod props;
mod service;

pub use model::{Caps, Command, PlaybackStatus, Player, Players, Position, Track};
pub use service::{MprisError, MprisService};
