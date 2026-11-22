//! Hyprland 0.57 IPC: requests, the socket2 event stream and a resyncing state store.

mod animation;
mod event;
mod ipc;
mod model;
mod store;
pub mod sunset;

pub use event::{Address, Event, ParseError, parse_line};
pub use ipc::{Batch, Client, DEFAULT_TIMEOUT, Error, Paths};
pub use model::{
    Bind, Devices, HardwareDetails, Keyboard, Monitor, Window, Workspace, WorkspaceKind,
    WorkspaceRef,
};
pub use store::{Notice, State, Store};
