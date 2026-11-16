//! Night light, VRR and HDR toggles through Hyprland and hyprsunset.

use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use deskshell_hypr::sunset::Sunset;
use deskshell_hypr::{Client, Paths};
use serde_json::Value;
use tokio::time::Instant;

pub use deskshell_hypr::{HardwareDetails, Monitor};

use crate::helpers::hdr_enabled;
use crate::model::{Effect, NightLight};

pub type DisplayFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VrrMode {
    Off,
    On,
    Fullscreen,
}

impl VrrMode {
    pub fn number(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Fullscreen => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "Always on",
            Self::Fullscreen => "Fullscreen only",
        }
    }

    pub fn next(current: i32) -> Self {
        if current == 0 { Self::Fullscreen } else { Self::Off }
    }
}

pub trait DisplayBackend: Send + Sync {
    fn vrr(&self) -> DisplayFuture<'_, Result<i32, String>>;
    fn set_vrr(&self, mode: VrrMode) -> DisplayFuture<'_, Result<(), String>>;
    fn monitors(&self) -> DisplayFuture<'_, Result<Vec<Monitor>, String>>;
    fn eval_monitor<'a>(&'a self, rule: &'a str) -> DisplayFuture<'a, Result<(), String>>;
    fn night_light(&self) -> DisplayFuture<'_, NightLight>;
    fn toggle_night_light(&self) -> DisplayFuture<'_, ()>;
    fn resume_night_light(&self) -> DisplayFuture<'_, ()>;
}

#[derive(Clone)]
pub struct DisplayControl {
    pub backend: Arc<dyn DisplayBackend>,
    /// Hyprland's `misc.lua` and `hdr.lua` read the state files here at start.
    pub state_dir: PathBuf,
}

#[derive(Debug)]
pub struct HyprDisplay {
    client: Client,
    sunset: Sunset,
}

impl HyprDisplay {
    pub fn new(paths: Paths) -> Self {
        Self { sunset: Sunset::for_instance(&paths), client: Client::new(paths) }
    }
}

impl DisplayBackend for HyprDisplay {
    fn vrr(&self) -> DisplayFuture<'_, Result<i32, String>> {
        Box::pin(async move {
            let reply: Value = self
                .client
                .json("getoption misc:vrr")
                .await
                .map_err(|error| error.to_string())?;
            vrr_from_option(&reply)
        })
    }

    fn set_vrr(&self, mode: VrrMode) -> DisplayFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.client
                .eval(&format!(
                    "hl.config({{ misc = {{ vrr = {} }} }})",
                    mode.number()
                ))
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn monitors(&self) -> DisplayFuture<'_, Result<Vec<Monitor>, String>> {
        Box::pin(async move {
            self.client
                .json("monitors")
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn eval_monitor<'a>(&'a self, rule: &'a str) -> DisplayFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.client
                .eval(rule)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn night_light(&self) -> DisplayFuture<'_, NightLight> {
        Box::pin(async move {
            let status = self.sunset.status().await;
            NightLight {
                available: status.available,
                enabled: status.enabled,
                scheduled: status.scheduled,
                temperature: status.temperature,
            }
        })
    }

    fn toggle_night_light(&self) -> DisplayFuture<'_, ()> {
        Box::pin(self.sunset.toggle())
    }

    fn resume_night_light(&self) -> DisplayFuture<'_, ()> {
        Box::pin(self.sunset.resume_schedule())
    }
}

pub fn vrr_from_option(reply: &Value) -> Result<i32, String> {
    reply
        .get("int")
        .and_then(Value::as_i64)
        .and_then(|mode| i32::try_from(mode).ok())
        .ok_or_else(|| "Hyprland gave no number for misc:vrr.".to_owned())
}

/// A monitor name that cannot end a Lua string or leave the state dir.
pub fn valid_monitor_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

pub fn hdr_rule(monitor: &Monitor, on: bool) -> Option<String> {
    if !valid_monitor_name(&monitor.name) {
        return None;
    }
    let color = if on {
        r#"bitdepth=10, cm="hdr", sdrbrightness=1.20, sdrsaturation=1.00"#
    } else {
        r#"bitdepth=8, cm="srgb", sdrbrightness=1.00, sdrsaturation=1.00"#
    };
    Some(format!(
        r#"hl.monitor({{ output="{}", mode="{}x{}@{}", position="{}x{}", scale={}, transform={}, {color} }})"#,
        monitor.name,
        monitor.width,
        monitor.height,
        monitor.refresh_rate,
        monitor.x,
        monitor.y,
        monitor.scale,
        monitor.transform,
    ))
}

/// The temp file's name never ends in `.hdr`, since `hdr.lua` reads every `*.hdr` file.
/// A symlinked state file, as from a stow package, stays a link. The write replaces its target.
pub fn write_state(dir: &Path, name: &str, contents: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let target = follow_links(&path)?;
    let (Some(target_dir), Some(target_name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::other(
            "the state file's link has no file name",
        ));
    };
    let temp = target_dir.join(format!(
        ".{}.{}.tmp",
        target_name.to_string_lossy(),
        std::process::id()
    ));
    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&temp, &target)) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(path)
}

fn follow_links(path: &Path) -> io::Result<PathBuf> {
    const MAX_HOPS: usize = 40;
    let mut path = path.to_path_buf();
    for _ in 0..MAX_HOPS {
        match fs::read_link(&path) {
            // `join` keeps an absolute target as it is.
            Ok(target) => {
                path = path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(target)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
                ) =>
            {
                return Ok(path);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "too many levels of symbolic links",
    ))
}

async fn save_state(dir: &Path, name: String, contents: String) -> io::Result<PathBuf> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || write_state(&dir, &name, &contents))
        .await
        .unwrap_or_else(|error| Err(io::Error::other(error)))
}

pub const VRR_STATE: &str = "vrr-mode";
/// Hyprland applies a monitor rule only after the eval has replied.
pub const HDR_SWITCH_TIMEOUT: Duration = Duration::from_secs(2);
const HDR_POLL: Duration = Duration::from_millis(50);

async fn hdr_switched(backend: &dyn DisplayBackend, name: &str, on: bool) -> bool {
    let deadline = Instant::now() + HDR_SWITCH_TIMEOUT;
    loop {
        if let Ok(monitors) = backend.monitors().await
            && monitors.iter().any(|monitor| {
                monitor.name == name && hdr_enabled(&monitor.color_management_preset) == on
            })
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(HDR_POLL).await;
    }
}

fn toast(summary: impl Into<String>, body: impl Into<String>, critical: bool) -> Effect {
    Effect::Notify { summary: summary.into(), body: body.into(), critical }
}

pub(crate) async fn toggle_vrr(backend: &dyn DisplayBackend, state_dir: &Path) -> Effect {
    const TOGGLE: &str = "VRR Toggle";
    let current = match backend.vrr().await {
        Ok(mode) => mode,
        Err(reason) => {
            return toast(
                TOGGLE,
                format!("Unable to read current VRR mode: {reason}"),
                true,
            );
        }
    };
    let next = VrrMode::next(current);
    if let Err(reason) = backend.set_vrr(next).await {
        return toast(
            TOGGLE,
            format!("Failed to set VRR mode: {reason}"),
            true,
        );
    }
    let number = next.number();
    if let Err(error) = save_state(
        state_dir,
        VRR_STATE.to_owned(),
        format!("{number}\n"),
    )
    .await
    {
        tracing::warn!("cannot save the VRR mode: {error}");
        return toast(
            TOGGLE,
            format!(
                "VRR is {} now, but it will not survive a restart: cannot write {}: {error}",
                next.label(),
                state_dir.join(VRR_STATE).display()
            ),
            true,
        );
    }
    toast(
        format!("VRR: {}", next.label()),
        format!("Hyprland misc:vrr set to {number}"),
        false,
    )
}

pub(crate) async fn toggle_hdr(backend: &dyn DisplayBackend, state_dir: &Path) -> Effect {
    const TOGGLE: &str = "HDR Toggle";
    let monitors = match backend.monitors().await {
        Ok(monitors) => monitors,
        Err(reason) => {
            return toast(
                TOGGLE,
                format!("Cannot read the monitors: {reason}"),
                true,
            );
        }
    };
    let Some(monitor) = monitors
        .iter()
        .find(|monitor| monitor.focused && !monitor.disabled)
    else {
        return toast(TOGGLE, "No focused monitor to toggle.", true);
    };
    let name = &monitor.name;
    let on = !hdr_enabled(&monitor.color_management_preset);
    let Some(rule) = hdr_rule(monitor, on) else {
        return toast(
            TOGGLE,
            format!("Refused the monitor name {name:?}."),
            true,
        );
    };
    if on && !monitor.hdr_capable() {
        return toast(
            TOGGLE,
            format!("{name} does not support HDR"),
            true,
        );
    }
    if let Err(reason) = backend.eval_monitor(&rule).await {
        return toast(
            TOGGLE,
            format!("Failed to apply monitor state on {name}: {reason}"),
            true,
        );
    }
    // A queued toggle decides from the live preset, so this one ends only once Hyprland
    // shows it, or the next would read the old preset and ask for the same again.
    if !hdr_switched(backend, name, on).await {
        return toast(
            TOGGLE,
            format!("HDR did not switch on {name}"),
            true,
        );
    }
    let state = if on { "on" } else { "off" };
    let done = if on { "enabled" } else { "disabled" };
    if let Err(error) = save_state(
        state_dir,
        format!("{name}.hdr"),
        format!("{state}\n"),
    )
    .await
    {
        tracing::warn!("cannot save the HDR state of {name}: {error}");
        return toast(
            TOGGLE,
            format!("HDR {done} on {name}, but it will not survive a restart: {error}"),
            true,
        );
    }
    toast(TOGGLE, format!("HDR {done} on {name}"), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vrr_cycle() {
        assert_eq!(VrrMode::next(0), VrrMode::Fullscreen);
        assert_eq!(VrrMode::next(1), VrrMode::Off);
        assert_eq!(VrrMode::next(2), VrrMode::Off);
        assert_eq!(VrrMode::next(-1), VrrMode::Off);
    }

    #[test]
    fn vrr_option() {
        let live: Value =
            serde_json::from_str(r#"{"option": "misc:vrr", "int": 2, "set": true }"#).unwrap();
        assert_eq!(vrr_from_option(&live), Ok(2));
        let no_int: Value =
            serde_json::from_str(r#"{"option": "misc:vrr", "set": true }"#).unwrap();
        assert!(vrr_from_option(&no_int).is_err());
        let text: Value = serde_json::from_str(r#"{"int": "2"}"#).unwrap();
        assert!(vrr_from_option(&text).is_err());
    }

    #[test]
    fn monitor_names() {
        for name in ["DP-3", "HDMI-A-1", "eDP-1", "Virtual-1", "a_b.c"] {
            assert!(valid_monitor_name(name), "{name}");
        }
        for name in ["", "DP 3", "DP-3\"", "a/b", "x\")hl.exec(\"", "ÄP-1", "DP-3\n"] {
            assert!(!valid_monitor_name(name), "{name}");
        }
    }

    #[test]
    fn state_files_replace_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-display-state-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let state = dir.join("hypr");
        let path = write_state(&state, "DP-3.hdr", "on\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "on\n");
        write_state(&state, "DP-3.hdr", "off\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "off\n");
        let names: Vec<_> = fs::read_dir(&state)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["DP-3.hdr"]);

        fs::write(dir.join("file"), "").unwrap();
        assert!(write_state(&dir.join("file"), VRR_STATE, "0\n").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_linked_state_file_stays_a_link() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "deskshell-display-link-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let state = dir.join("state/hypr");
        let stow = dir.join("dotfiles/workstation/.local/state/hypr");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&stow).unwrap();
        fs::write(stow.join("HDMI-A-1.hdr"), "off\n").unwrap();
        symlink(
            "../../dotfiles/workstation/.local/state/hypr/HDMI-A-1.hdr",
            state.join("HDMI-A-1.hdr"),
        )
        .unwrap();
        fs::write(stow.join("real-vrr"), "0\n").unwrap();
        symlink(stow.join("real-vrr"), stow.join(VRR_STATE)).unwrap();
        symlink(
            "../../dotfiles/workstation/.local/state/hypr/vrr-mode",
            state.join(VRR_STATE),
        )
        .unwrap();

        let path = write_state(&state, "HDMI-A-1.hdr", "on\n").unwrap();
        assert_eq!(path, state.join("HDMI-A-1.hdr"));
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(stow.join("HDMI-A-1.hdr")).unwrap(),
            "on\n"
        );

        write_state(&state, VRR_STATE, "2\n").unwrap();
        assert!(
            fs::symlink_metadata(state.join(VRR_STATE))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(stow.join(VRR_STATE))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(stow.join("real-vrr")).unwrap(),
            "2\n"
        );

        let mut names: Vec<_> = fs::read_dir(&stow)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["HDMI-A-1.hdr", "real-vrr", "vrr-mode"],
            "no temp file is left"
        );

        symlink(stow.join("DP-3.hdr"), state.join("DP-3.hdr")).unwrap();
        write_state(&state, "DP-3.hdr", "on\n").unwrap();
        assert_eq!(
            fs::read_to_string(stow.join("DP-3.hdr")).unwrap(),
            "on\n"
        );
        assert!(
            fs::symlink_metadata(state.join("DP-3.hdr"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        symlink("loop", state.join("loop")).unwrap();
        assert!(write_state(&state, "loop", "on\n").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
