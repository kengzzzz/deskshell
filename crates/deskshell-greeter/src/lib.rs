//! The greetd greeter: logs a user in through greetd's IPC socket, while greetd runs PAM.

mod app;
pub mod clock;
pub mod config;
pub mod flow;
pub mod greetd;
pub mod idle;
pub mod inputs;
mod pictures;
mod runtime;
pub mod view;

pub use app::{BLANK, IDLE_ENV, NAMESPACE, WAKE, harden, main, parse_idle};
