//! The control socket behind `deskshell ipc`: the call table, the server and the client.

mod client;
mod server;
mod table;
mod wire;

pub use client::{CONNECT_LIMIT, ClientError, DEFAULT_TIMEOUT, call, call_on, connect};
pub use server::{
    BindError, DEFAULT_HANDLER_TIMEOUT, Error, Hangup, Request, SOCKET_NAME, Server, socket_path,
};
pub use table::{
    ArgType, CallError, Command, CpuPower, CpuPowerMode, Display, Function, Hub,
    LOCK_HANDLER_TIMEOUT, LOCK_WAIT, Lock, Media, Notifications, Panel, Param, Reply, ReturnType,
    SHARE_PICK_HANDLER_TIMEOUT, Screenshot, SharePicker, Status, TARGETS, Target,
    UPDATES_CANCEL_HANDLER_TIMEOUT, Updates, Windows,
};
pub use wire::MAX_REQUEST;
