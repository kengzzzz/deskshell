//! The shell's side of `deskshell-notify`: its bus, state file and the Hyprland facts it needs.

use std::ffi::OsStr;
use std::path::PathBuf;

use deskshell_hypr::State;
use deskshell_notify::{Command, Config, Rect, Timer};

use crate::panels::{Panel, Route};

pub const STATE_FILE: &str = "deskshell-notifications.json";
pub fn config(state_home: Option<&OsStr>, home: Option<&OsStr>) -> Config {
    let dir = state_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        });
    match dir {
        Some(dir) => Config { state_path: Some(dir.join(STATE_FILE)), ..Config::default() },
        None => Config::default(),
    }
}

pub fn route_command(route: Route) -> Option<Command> {
    match route {
        Route::ToggleDnd => Some(Command::ToggleDnd),
        Route::ClearNotifications => Some(Command::Clear),
        _ => None,
    }
}

pub fn panel_opened(panel: Panel) -> Option<Command> {
    (panel == Panel::Hub).then_some(Command::ClearPopups)
}

/// `monitors -j` gives the position in logical pixels and the mode size in device pixels, so
/// the size is divided by the scale and turned for rotated transforms.
pub fn monitor_rects(state: &State) -> Vec<Rect> {
    state
        .monitors
        .iter()
        .filter(|monitor| !monitor.disabled)
        .map(|monitor| {
            let scale = if monitor.scale > 0.0 { monitor.scale } else { 1.0 };
            let width = (f64::from(monitor.width) / scale).round() as i32;
            let height = (f64::from(monitor.height) / scale).round() as i32;
            let (width, height) =
                if monitor.transform % 2 == 1 { (height, width) } else { (width, height) };
            Rect { x: monitor.x, y: monitor.y, width, height }
        })
        .collect()
}

pub fn window_rect(state: &State, address: u64) -> Option<Rect> {
    state
        .windows
        .iter()
        .find(|window| window.address.0 == address)
        .map(|window| Rect {
            x: window.at[0],
            y: window.at[1],
            width: window.size[0],
            height: window.size[1],
        })
}

pub fn urgent(state: &State, address: u64) -> Option<Command> {
    let window = state
        .windows
        .iter()
        .find(|window| window.address.0 == address)?;
    Some(Command::Urgent { address, class: window.class.clone() })
}

pub fn focus_timer(timer: Timer, state: &State) -> Command {
    Command::FocusTimer {
        timer,
        window: window_rect(state, timer.address),
        monitors: monitor_rects(state),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use deskshell_hypr::{Address, Monitor, Window};

    use super::*;

    #[test]
    fn state_paths() {
        let state = OsString::from("/srv/state");
        let home = OsString::from("/home/someone");
        let full = config(Some(&state), Some(&home));
        assert_eq!(
            full.state_path.as_deref(),
            Some(Path::new(
                "/srv/state/deskshell-notifications.json"
            ))
        );
        let empty = OsString::new();
        let fallback = config(Some(&empty), Some(&home));
        assert_eq!(
            fallback.state_path.as_deref(),
            Some(Path::new(
                "/home/someone/.local/state/deskshell-notifications.json"
            ))
        );
        assert_eq!(config(None, None).state_path, None);
    }

    #[test]
    fn routes_and_panels() {
        assert_eq!(
            route_command(Route::ToggleDnd),
            Some(Command::ToggleDnd)
        );
        assert_eq!(
            route_command(Route::ClearNotifications),
            Some(Command::Clear)
        );
        assert_eq!(route_command(Route::Drop), None);
        assert_eq!(
            panel_opened(Panel::Hub),
            Some(Command::ClearPopups)
        );
        assert_eq!(panel_opened(Panel::Launcher), None);
    }

    #[test]
    fn monitors_in_logical_pixels() {
        let state = State {
            monitors: vec![
                Monitor {
                    name: String::from("A"),
                    width: 3840,
                    height: 2160,
                    scale: 2.0,
                    ..Monitor::default()
                },
                Monitor {
                    name: String::from("B"),
                    x: 1920,
                    width: 1920,
                    height: 1080,
                    scale: 1.0,
                    transform: 1,
                    ..Monitor::default()
                },
                Monitor { name: String::from("C"), disabled: true, ..Monitor::default() },
            ],
            ..State::default()
        };
        assert_eq!(
            monitor_rects(&state),
            [
                Rect { x: 0, y: 0, width: 1920, height: 1080 },
                Rect { x: 1920, y: 0, width: 1080, height: 1920 },
            ]
        );
    }

    #[test]
    fn urgent_windows_carry_their_class() {
        let state = State {
            windows: vec![Window {
                address: Address(0xabc),
                class: String::from("foot"),
                at: [10, 20],
                size: [300, 200],
                ..Window::default()
            }],
            ..State::default()
        };
        assert_eq!(
            urgent(&state, 0xabc),
            Some(Command::Urgent { address: 0xabc, class: String::from("foot") })
        );
        assert_eq!(urgent(&state, 0xdef), None);
        assert_eq!(
            window_rect(&state, 0xabc),
            Some(Rect { x: 10, y: 20, width: 300, height: 200 })
        );
    }
}
