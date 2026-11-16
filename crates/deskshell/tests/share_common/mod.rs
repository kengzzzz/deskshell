//! A made-up desktop and XDPH window list for the share picker tests and its offscreen render.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use deskshell::panel::share::Screen;
use deskshell_hypr::{Address, Monitor, State, Window, WorkspaceKind, WorkspaceRef};

pub const LEFT: &str = "DP-1";
pub const RIGHT: &str = "DP-2";
pub const A1: u64 = 0xa1;
pub const A2: u64 = 0xa2;
pub const B1: u64 = 0xb1;
pub const B2: u64 = 0xb2;
pub const C1: u64 = 0xc1;
pub const GHOST: u64 = 0xdead;

fn workspace_ref(id: i64) -> WorkspaceRef {
    WorkspaceRef {
        address: id.to_string(),
        id: Some(id),
        name: id.to_string(),
        ..Default::default()
    }
}

fn monitor(name: &str, x: i32, focused: bool) -> Monitor {
    Monitor {
        name: name.to_owned(),
        width: 2560,
        height: 1440,
        x,
        y: 0,
        scale: 1.0,
        focused,
        ..Monitor::default()
    }
}

fn window(address: u64, workspace: WorkspaceRef, x: i32, title: &str, class: &str) -> Window {
    Window {
        address: Address(address),
        at: [x, 40],
        size: [1500, 1000],
        workspace,
        class: class.to_owned(),
        title: title.to_owned(),
        mapped: true,
        ..Window::default()
    }
}

pub fn state() -> State {
    let special = WorkspaceRef {
        address: String::from("special:scratch"),
        id: None,
        kind: WorkspaceKind::Special,
        name: String::from("special:scratch"),
    };
    State {
        connected: true,
        monitors: vec![monitor(LEFT, 0, false), monitor(RIGHT, 2560, true)],
        windows: vec![
            window(A1, workspace_ref(1), 0, "~/src: foot", "foot"),
            window(
                B1,
                workspace_ref(3),
                3560,
                "แชทกลุ่ม: สวัสดีครับ - Browser",
                "browser",
            ),
            window(A2, workspace_ref(1), 1600, "Calculator", "calc"),
            window(
                B2,
                workspace_ref(3),
                2560,
                "Files: Documents",
                "files",
            ),
            window(C1, special, 0, "Scratchpad", "foot"),
        ],
        active_workspace: Some(3),
        focused_monitor: Some(String::from(RIGHT)),
        active_window: Some(Address(B1)),
        ..State::default()
    }
}

fn entry(id: u32, class: &str, title: &str, address: u64) -> String {
    format!("{id}[HC>]{class}[HT>]{title}[HE>]{address}[HA>]")
}

pub fn list() -> String {
    [
        entry(101, "foot", "~/src: foot", A1),
        entry(102, "calc", "Calculator", A2),
        entry(103, "browser", "แชทกลุ่ม: สวัสดีครับ - Browser", B1),
        entry(104, "files", "Files: Documents", B2),
        entry(105, "foot", "Scratchpad", C1),
        entry(106, "ghost", "", GHOST),
    ]
    .concat()
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            output: 31,
            name: String::from(LEFT),
            model: String::from("Display A"),
            width: 2560,
            height: 1440,
        },
        Screen {
            output: 44,
            name: String::from(RIGHT),
            model: String::new(),
            width: 2560,
            height: 1440,
        },
    ]
}
