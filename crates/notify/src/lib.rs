//! The notification server, history, popups and do not disturb, with no UI.

pub mod focus;
mod hints;
mod model;
mod persist;
mod server;
mod store;
mod view;

pub use focus::{Effect, FocusGrant, Rect, Timer, TimerKind};
pub use model::{Action, Command, Entry, Extra, Image, Pixels, Post, State, Urgency, action_label};
pub use server::{
    BUS_NAME, CAPABILITIES, Config, Error, OBJECT_PATH, SERVER_NAME, SERVER_VENDOR, SERVER_VERSION,
    SPEC_VERSION, Service,
};
pub use view::{Group, group_by_app, relative_time};
