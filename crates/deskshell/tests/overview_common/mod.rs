//! A made-up two-monitor desktop for the overview tests and its offscreen render.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use deskshell_hypr::{Address, Monitor, State, Window, Workspace, WorkspaceKind, WorkspaceRef};

pub const LEFT: &str = "DP-1";
pub const RIGHT: &str = "DP-2";

pub fn workspace_ref(id: i64) -> WorkspaceRef {
    WorkspaceRef {
        address: id.to_string(),
        id: Some(id),
        name: id.to_string(),
        ..Default::default()
    }
}

pub fn monitor(name: &str, x: i32, workspace: i64, focused: bool) -> Monitor {
    Monitor {
        name: name.to_owned(),
        width: 2560,
        height: 1440,
        x,
        y: 0,
        scale: 1.0,
        focused,
        active_workspace: Some(workspace_ref(workspace)),
        ..Monitor::default()
    }
}

pub fn window(address: u64, workspace: i64, x: i32, title: &str, class: &str) -> Window {
    Window {
        address: Address(address),
        at: [x, 40],
        size: [1500, 1000],
        workspace: workspace_ref(workspace),
        class: class.to_owned(),
        title: title.to_owned(),
        mapped: true,
        ..Window::default()
    }
}

pub fn workspace(id: i64, monitor: &str) -> Workspace {
    Workspace {
        address: id.to_string(),
        id: Some(id),
        name: id.to_string(),
        monitor: monitor.to_owned(),
        ..Workspace::default()
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
        monitors: vec![monitor(LEFT, 0, 1, false), monitor(RIGHT, 2560, 3, true)],
        workspaces: vec![workspace(3, RIGHT), workspace(1, LEFT), workspace(5, LEFT)],
        windows: vec![
            window(0xa1, 1, 0, "~/src: foot", "foot"),
            window(
                0xb1,
                3,
                3560,
                "แชทกลุ่ม: สวัสดีครับ - Browser",
                "browser",
            ),
            window(0xa2, 1, 1600, "Calculator", "calc"),
            window(0xb2, 3, 2560, "Files: Documents", "files"),
            window(0xb3, 3, 4560, "", "foot"),
            Window { workspace: special, ..window(0xc1, 0, 0, "Scratchpad", "foot") },
            window(0xb4, 3, 5560, "Unknown app window", "UnknownApp"),
            window(
                0xb5,
                3,
                6560,
                "A very long window title that runs past its card",
                "foot",
            ),
        ],
        active_workspace: Some(3),
        focused_monitor: Some(String::from(RIGHT)),
        active_window: Some(Address(0xb1)),
        keyboard_layout: String::from("English (US)"),
    }
}
