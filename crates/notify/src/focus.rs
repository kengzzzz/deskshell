//! The focus grant for invoked notification actions, as a pure state machine.

use std::time::Duration;

pub const GRANT_MS: i64 = 2000;
/// Past Hyprland's 200 ms `windowsMove` animation.
pub const SETTLE: Duration = Duration::from_millis(250);
pub const MOVE: Duration = Duration::from_millis(80);
pub const MAX_TRIES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// Rounds halves up, also for negative coordinates.
    pub fn centre(&self) -> (i32, i32) {
        let half_up = |start: i32, len: i32| {
            let doubled = 2 * i64::from(start) + i64::from(len) + 1;
            doubled
                .div_euclid(2)
                .clamp(i32::MIN.into(), i32::MAX.into()) as i32
        };
        (
            half_up(self.x, self.width),
            half_up(self.y, self.height),
        )
    }

    pub fn contains(&self, (x, y): (i32, i32)) -> bool {
        let (x, y) = (i64::from(x), i64::from(y));
        let (left, top) = (i64::from(self.x), i64::from(self.y));
        x >= left
            && x < left + i64::from(self.width)
            && y >= top
            && y < top + i64::from(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    Settle,
    Move,
}

/// Only the most recently scheduled timer counts, so a stale one is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    pub kind: TimerKind,
    pub address: u64,
    seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Dispatch(String),
    RefreshWindows,
    Schedule {
        timer: Timer,
        after: Duration,
    },
    /// The user invoked `key` on a notification the shell posted.
    Action {
        owner: String,
        key: String,
    },
}

#[derive(Debug, Default)]
// With no activation token, Hyprland marks an invoked app urgent instead of focusing it.
// The cursor then moves onto the window, or `follow_mouse` would hand focus back.
pub struct FocusGrant {
    grant: Option<Grant>,
    handoff: Option<Handoff>,
    seq: u64,
}

#[derive(Debug)]
struct Grant {
    until: i64,
    keys: Vec<String>,
}

#[derive(Debug)]
struct Handoff {
    address: u64,
    tries: u32,
    pending: u64,
}

impl FocusGrant {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn invoke(&mut self, now: i64, app_name: &str, desktop_entry: &str) {
        self.handoff = None;
        let mut keys = Vec::with_capacity(2);
        for name in [app_name, desktop_entry] {
            push_key(&mut keys, normalize(name));
        }
        self.grant = Some(Grant { until: now + GRANT_MS, keys });
    }

    pub fn urgent(&mut self, now: i64, address: u64, class: &str) -> Vec<Effect> {
        let Some(grant) = &self.grant else {
            return Vec::new();
        };
        if now > grant.until
            || !class_keys(class)
                .iter()
                .any(|key| grant.keys.contains(key))
        {
            return Vec::new();
        }
        self.grant = None;
        self.handoff = Some(Handoff { address, tries: 0, pending: 0 });
        vec![
            Effect::Dispatch(focus(address)),
            Effect::Dispatch(r#"hl.dsp.layout("fit_into_view")"#.to_owned()),
            self.schedule(TimerKind::Settle),
        ]
    }

    pub fn timer(&mut self, timer: Timer, window: Option<Rect>, monitors: &[Rect]) -> Vec<Effect> {
        let Some(handoff) = &mut self.handoff else {
            return Vec::new();
        };
        if handoff.pending != timer.seq {
            return Vec::new();
        }
        if timer.kind == TimerKind::Settle {
            return vec![Effect::RefreshWindows, self.schedule(TimerKind::Move)];
        }

        let on_screen = |point| {
            monitors
                .iter()
                .any(|monitor| monitor.contains(point))
        };
        let centre = window.map(|window| window.centre());
        // An off-screen centre is geometry from before fit_into_view, since a refresh can be
        // skipped while one is pending.
        if let Some(point) = centre
            && !on_screen(point)
        {
            handoff.tries += 1;
            if handoff.tries < MAX_TRIES {
                return vec![Effect::RefreshWindows, self.schedule(TimerKind::Move)];
            }
        }

        let address = handoff.address;
        self.handoff = None;
        match centre {
            Some((x, y)) if on_screen((x, y)) => vec![
                Effect::Dispatch(format!(
                    "hl.dsp.cursor.move({{ x = {x}, y = {y} }})"
                )),
                Effect::Dispatch(focus(address)),
            ],
            _ => Vec::new(),
        }
    }

    pub fn handing_off(&self) -> bool {
        self.handoff.is_some()
    }

    fn schedule(&mut self, kind: TimerKind) -> Effect {
        self.seq += 1;
        let handoff = self
            .handoff
            .as_mut()
            .expect("schedule needs a handoff");
        handoff.pending = self.seq;
        let after = match kind {
            TimerKind::Settle => SETTLE,
            TimerKind::Move => MOVE,
        };
        Effect::Schedule { timer: Timer { kind, address: handoff.address, seq: self.seq }, after }
    }
}

fn focus(address: u64) -> String {
    format!(r#"hl.dsp.focus({{ window = "address:0x{address:x}" }})"#)
}

fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

fn push_key(keys: &mut Vec<String>, key: String) {
    if !key.is_empty() && !keys.contains(&key) {
        keys.push(key);
    }
}

fn class_keys(class: &str) -> Vec<String> {
    let lower = class.to_lowercase();
    let mut keys = Vec::with_capacity(2);
    push_key(&mut keys, normalize(&lower));
    push_key(
        &mut keys,
        normalize(
            lower
                .rsplit('.')
                .next()
                .unwrap_or_default(),
        ),
    );
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: u64 = 0x55d4_c2a0_e8b0;
    const MONITORS: [Rect; 2] = [
        Rect { x: -1920, y: 0, width: 1920, height: 1080 },
        Rect { x: 0, y: 0, width: 2560, height: 1440 },
    ];
    const ON_SCREEN: Rect = Rect { x: 100, y: 50, width: 801, height: 601 };
    const OFF_SCREEN: Rect = Rect { x: 4000, y: 50, width: 800, height: 600 };

    fn scheduled(effects: &[Effect]) -> Timer {
        match effects.last() {
            Some(Effect::Schedule { timer, .. }) => *timer,
            other => panic!("expected a timer, got {other:?}"),
        }
    }

    fn to_move(machine: &mut FocusGrant, class: &str) -> Timer {
        let effects = machine.urgent(1000, ADDRESS, class);
        assert_eq!(effects.len(), 3, "{class} should match");
        let settle = scheduled(&effects);
        assert_eq!(settle.kind, TimerKind::Settle);
        let effects = machine.timer(settle, None, &MONITORS);
        assert_eq!(effects[0], Effect::RefreshWindows);
        let next = scheduled(&effects);
        assert_eq!(next.kind, TimerKind::Move);
        next
    }

    #[test]
    fn keys_drop_everything_outside_a_z_0_9() {
        assert_eq!(normalize("Org.Example-Chat 2"), "orgexamplechat2");
        assert_eq!(normalize("Ünïcode"), "ncode");
        assert_eq!(
            class_keys("org.example.Chat"),
            ["orgexamplechat", "chat"]
        );
        assert_eq!(class_keys("kitty"), ["kitty"]);
        assert!(class_keys("").is_empty());
    }

    #[test]
    fn matches_by_desktop_entry() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat Client", "org.example.Chat");
        let effects = machine.urgent(1000, ADDRESS, "org.example.Chat");
        assert_eq!(
            effects[..2],
            [
                Effect::Dispatch(r#"hl.dsp.focus({ window = "address:0x55d4c2a0e8b0" })"#.into()),
                Effect::Dispatch(r#"hl.dsp.layout("fit_into_view")"#.into()),
            ]
        );
        assert_eq!(
            effects[2],
            Effect::Schedule { timer: scheduled(&effects), after: SETTLE }
        );
        assert!(machine.handing_off());
    }

    #[test]
    fn matches_by_app_name() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Mail-Reader", "");
        assert_eq!(
            machine
                .urgent(1000, ADDRESS, "mailreader")
                .len(),
            3
        );
    }

    #[test]
    fn matches_by_class_suffix() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "chat", "");
        assert_eq!(
            machine
                .urgent(1000, ADDRESS, "org.example.Chat")
                .len(),
            3
        );
    }

    #[test]
    fn grant_keys_have_no_suffix_rule() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "", "org.example.Chat");
        assert!(
            machine
                .urgent(1000, ADDRESS, "chat")
                .is_empty()
        );
    }

    #[test]
    fn no_match_keeps_the_grant() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        assert!(
            machine
                .urgent(500, ADDRESS, "browser")
                .is_empty()
        );
        assert!(!machine.handing_off());
        assert_eq!(
            machine
                .urgent(600, ADDRESS, "chat")
                .len(),
            3
        );
        assert!(
            machine
                .urgent(700, ADDRESS, "chat")
                .is_empty(),
            "a match consumes the grant"
        );
    }

    #[test]
    fn expired_grant() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        assert!(
            machine
                .urgent(GRANT_MS + 1, ADDRESS, "chat")
                .is_empty()
        );
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        assert_eq!(
            machine
                .urgent(GRANT_MS, ADDRESS, "chat")
                .len(),
            3,
            "the last millisecond counts"
        );
    }

    #[test]
    fn no_grant_no_match() {
        let mut machine = FocusGrant::new();
        assert!(
            machine
                .urgent(0, ADDRESS, "chat")
                .is_empty()
        );
    }

    #[test]
    fn moves_the_cursor_to_the_centre() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let timer = to_move(&mut machine, "chat");
        let effects = machine.timer(timer, Some(ON_SCREEN), &MONITORS);
        assert_eq!(
            effects,
            [
                Effect::Dispatch("hl.dsp.cursor.move({ x = 501, y = 351 })".into()),
                Effect::Dispatch(r#"hl.dsp.focus({ window = "address:0x55d4c2a0e8b0" })"#.into()),
            ]
        );
        assert!(!machine.handing_off());
    }

    #[test]
    fn off_screen_retry_stops_after_three_tries() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let mut timer = to_move(&mut machine, "chat");
        for _ in 1..MAX_TRIES {
            let effects = machine.timer(timer, Some(OFF_SCREEN), &MONITORS);
            assert_eq!(effects[0], Effect::RefreshWindows);
            assert_eq!(
                effects[1],
                Effect::Schedule { timer: scheduled(&effects), after: MOVE }
            );
            timer = scheduled(&effects);
        }
        assert!(
            machine
                .timer(timer, Some(OFF_SCREEN), &MONITORS)
                .is_empty()
        );
        assert!(!machine.handing_off());
    }

    #[test]
    fn off_screen_then_on_screen() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let timer = to_move(&mut machine, "chat");
        let retry = scheduled(&machine.timer(timer, Some(OFF_SCREEN), &MONITORS));
        let effects = machine.timer(retry, Some(ON_SCREEN), &MONITORS);
        assert_eq!(effects.len(), 2);
    }

    #[test]
    fn a_vanished_window_ends_the_handoff() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let timer = to_move(&mut machine, "chat");
        assert!(
            machine
                .timer(timer, None, &MONITORS)
                .is_empty()
        );
        assert!(!machine.handing_off());
    }

    #[test]
    fn centre_rounding() {
        let odd = Rect { x: 10, y: 20, width: 5, height: 7 };
        assert_eq!(odd.centre(), (13, 24));
        let even = Rect { x: 10, y: 20, width: 4, height: 6 };
        assert_eq!(even.centre(), (12, 23));
        // Math.round(-10 + 2.5) is -7, where Rust's f64::round gives -8.
        let negative = Rect { x: -10, y: -1081, width: 5, height: 3 };
        assert_eq!(negative.centre(), (-7, -1079));
        assert_eq!(
            Rect { x: i32::MAX, y: 0, width: 1, height: 0 }
                .centre()
                .0,
            i32::MAX
        );
    }

    #[test]
    fn centre_on_a_monitor_left_of_the_origin() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let timer = to_move(&mut machine, "chat");
        let window = Rect { x: -1910, y: 10, width: 801, height: 600 };
        let effects = machine.timer(timer, Some(window), &MONITORS);
        assert_eq!(
            effects[0],
            Effect::Dispatch("hl.dsp.cursor.move({ x = -1509, y = 310 })".into())
        );
    }

    #[test]
    fn monitor_edges() {
        let monitor = Rect { x: 0, y: 0, width: 100, height: 50 };
        assert!(monitor.contains((0, 0)));
        assert!(monitor.contains((99, 49)));
        assert!(!monitor.contains((100, 10)));
        assert!(!monitor.contains((10, 50)));
        assert!(!monitor.contains((-1, 10)));
    }

    #[test]
    fn a_new_invoke_cancels_a_handoff() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let settle = scheduled(&machine.urgent(1000, ADDRESS, "chat"));
        machine.invoke(1100, "Mail", "");
        assert!(!machine.handing_off());
        assert!(
            machine
                .timer(settle, Some(ON_SCREEN), &MONITORS)
                .is_empty()
        );

        machine.invoke(1200, "Chat", "");
        let timer = to_move(&mut machine, "chat");
        machine.invoke(1300, "Chat", "");
        assert!(
            machine
                .timer(timer, Some(ON_SCREEN), &MONITORS)
                .is_empty()
        );
    }

    #[test]
    fn a_stale_timer_is_ignored() {
        let mut machine = FocusGrant::new();
        machine.invoke(0, "Chat", "");
        let old = scheduled(&machine.urgent(1000, ADDRESS, "chat"));
        machine.invoke(1100, "Chat", "");
        let new = scheduled(&machine.urgent(1200, ADDRESS, "chat"));
        assert!(
            machine
                .timer(old, None, &MONITORS)
                .is_empty()
        );
        assert!(machine.handing_off());
        assert_eq!(
            machine.timer(new, None, &MONITORS)[0],
            Effect::RefreshWindows
        );
    }
}
