//! The OSD: the shown level, its hide timer and where it draws.

use std::time::Duration;

use deskshell_media::OsdEvent;

pub const HIDE_AFTER: Duration = Duration::from_millis(1800);
pub const NAMESPACE: &str = "deskshell-osd";
pub const POPUP_HEIGHT: u32 = 40;
pub const POPUP_TOP: i32 = 10;
pub const LEVEL_WIDTH: f32 = 30.0 + 12.0 + 200.0 + 12.0 + 48.0;
pub const POPUP_WIDTH: f32 = LEVEL_WIDTH + 30.0;

#[derive(Debug, Clone, Default)]
pub struct Osd {
    pub showing: bool,
    pub icon: &'static str,
    pub label: String,
    pub muted: bool,
    pub level: f32,
    generation: u64,
}

impl Osd {
    pub fn show(&mut self, event: &OsdEvent) -> u64 {
        self.showing = true;
        self.icon = event.icon();
        self.muted = event.muted;
        self.level = event.level();
        let label = event.label();
        self.label.clear();
        self.label.push_str(
            label
                .strip_suffix('%')
                .unwrap_or(&label),
        );
        self.generation += 1;
        self.generation
    }

    /// False for a stale generation: a newer event restarted the timer.
    pub fn hide(&mut self, generation: u64) -> bool {
        if generation != self.generation || !self.showing {
            return false;
        }
        self.showing = false;
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Hidden,
    Pill(String),
    Popup(String),
}

/// The popup stands in for the pill while a fullscreen window covers the bar.
pub fn place(showing: bool, focused: Option<(&str, bool)>) -> Place {
    match (showing, focused) {
        (true, Some((monitor, false))) => Place::Pill(monitor.to_owned()),
        (true, Some((monitor, true))) => Place::Popup(monitor.to_owned()),
        _ => Place::Hidden,
    }
}

#[cfg(test)]
mod tests {
    use deskshell_media::DeviceKind;

    use super::*;

    fn event(kind: DeviceKind, volume: f32, muted: bool) -> OsdEvent {
        OsdEvent { kind, volume, muted }
    }

    #[test]
    fn a_volume_change_and_a_mute() {
        let mut osd = Osd::default();
        let first = osd.show(&event(DeviceKind::Sink, 0.42, false));
        assert!(osd.showing);
        assert_eq!(osd.icon, "volume_down");
        assert_eq!(osd.label, "42");
        assert!((osd.level - 0.42).abs() < 1e-6);
        let second = osd.show(&event(DeviceKind::Sink, 0.42, true));
        assert_eq!(osd.icon, "volume_off");
        assert_eq!(osd.label, "Muted");
        assert_eq!(osd.level, 0.0);
        assert!(osd.muted);
        assert!(
            !osd.hide(first),
            "the second event restarted the timer"
        );
        assert!(osd.showing);
        assert!(osd.hide(second));
        assert!(!osd.showing);
        assert!(!osd.hide(second));
    }

    #[test]
    fn the_microphone_and_loud_volumes() {
        let mut osd = Osd::default();
        osd.show(&event(DeviceKind::Source, 0.7, false));
        assert_eq!(osd.icon, "mic");
        assert_eq!(osd.label, "70");
        osd.show(&event(DeviceKind::Sink, 1.5, false));
        assert_eq!(osd.label, "100", "capped at 100 %");
        assert_eq!(osd.level, 1.0);
    }

    #[test]
    fn the_pill_or_the_popup() {
        assert_eq!(place(false, Some(("DP-1", false))), Place::Hidden);
        assert_eq!(place(true, None), Place::Hidden);
        assert_eq!(
            place(true, Some(("DP-1", false))),
            Place::Pill(String::from("DP-1"))
        );
        assert_eq!(
            place(true, Some(("DP-1", true))),
            Place::Popup(String::from("DP-1"))
        );
        assert_eq!(POPUP_WIDTH, 332.0);
    }
}
