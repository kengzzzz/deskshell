//! The idle blank: clears the PIN and turns the screen off after [`TIMEOUT`] without input.

use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    On,
    /// `woken` is set by input while the off command is on its way.
    TurningOff {
        woken: bool,
    },
    Off,
    TurningOn,
}

#[derive(Clone, Debug)]
pub struct Idle {
    timeout: Duration,
    last: Instant,
    allowed: bool,
    screen: Screen,
    pointer: Option<(f32, f32)>,
}

impl Idle {
    pub fn new(timeout: Duration, now: Instant, allowed: bool) -> Self {
        Self { timeout, last: now, allowed, screen: Screen::On, pointer: None }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn allow(&mut self, now: Instant, allowed: bool) {
        if allowed && !self.allowed {
            self.last = now;
        }
        self.allowed = allowed;
    }

    /// True when the key must be swallowed. Hyprland hands the waking key to the client
    /// before it turns the screen on, so it would land unseen in the PIN.
    pub fn key(&mut self, now: Instant) -> bool {
        self.activity(now);
        match &mut self.screen {
            Screen::On => false,
            Screen::TurningOff { woken } => {
                *woken = true;
                true
            }
            Screen::Off => {
                self.screen = Screen::On;
                true
            }
            Screen::TurningOn => true,
        }
    }

    /// Hyprland repeats hover events, so the same position again is not input.
    pub fn pointer(&mut self, now: Instant, position: (f32, f32)) {
        if self.pointer == Some(position) {
            return;
        }
        self.pointer = Some(position);
        self.press(now);
    }

    pub fn press(&mut self, now: Instant) {
        self.activity(now);
        match &mut self.screen {
            Screen::TurningOff { woken } => *woken = true,
            Screen::Off => self.screen = Screen::On,
            Screen::On | Screen::TurningOn => {}
        }
    }

    fn activity(&mut self, now: Instant) {
        if self.allowed {
            self.last = now;
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.allowed
            .then(|| self.last + self.timeout)
    }

    pub fn fire(&mut self, now: Instant) -> bool {
        match self.deadline() {
            Some(deadline) if now >= deadline => {
                self.last = now;
                if self.screen == Screen::On {
                    self.screen = Screen::TurningOff { woken: false };
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// True when the driver must send the on command. Hyprland runs a request before it
    /// answers, so a lost answer may follow a screen that did turn off.
    pub fn off_done(&mut self, confirmed: bool) -> bool {
        let Screen::TurningOff { woken } = self.screen else {
            return false;
        };
        self.screen = if confirmed && !woken { Screen::Off } else { Screen::TurningOn };
        self.screen == Screen::TurningOn
    }

    /// A failed on command counts the screen as off, so the next key is swallowed as its waker.
    pub fn on_done(&mut self, confirmed: bool) {
        if self.screen == Screen::TurningOn {
            self.screen = if confirmed { Screen::On } else { Screen::Off };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(300);

    fn secs(start: Instant, n: u64) -> Instant {
        start + Duration::from_secs(n)
    }

    fn blank(idle: &mut Idle, now: Instant) {
        assert!(idle.fire(now));
        assert!(!idle.off_done(true));
        assert_eq!(idle.screen(), Screen::Off);
    }

    #[test]
    fn blanks_after_the_timeout_and_repeats() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert_eq!(idle.deadline(), Some(secs(start, 300)));
        assert!(!idle.fire(secs(start, 299)));
        blank(&mut idle, secs(start, 300));
        assert_eq!(
            idle.deadline(),
            Some(secs(start, 600)),
            "the timer repeats"
        );
        assert!(
            !idle.fire(secs(start, 600)),
            "an off screen gets no second off command"
        );
        assert_eq!(idle.deadline(), Some(secs(start, 900)));
    }

    #[test]
    fn activity_moves_the_deadline() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        idle.pointer(secs(start, 100), (10.0, 10.0));
        assert!(
            !idle.fire(secs(start, 300)),
            "an early sleep finds nothing due"
        );
        assert_eq!(idle.deadline(), Some(secs(start, 400)));
        idle.key(secs(start, 200));
        assert_eq!(idle.deadline(), Some(secs(start, 500)));
        idle.press(secs(start, 250));
        assert_eq!(idle.deadline(), Some(secs(start, 550)));
    }

    #[test]
    fn the_same_pointer_position_is_not_input() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        idle.pointer(secs(start, 10), (5.0, 5.0));
        idle.pointer(secs(start, 290), (5.0, 5.0));
        blank(&mut idle, secs(start, 310));
        idle.pointer(secs(start, 320), (5.0, 5.0));
        assert_eq!(
            idle.screen(),
            Screen::Off,
            "a repeated hover does not wake the screen"
        );
        idle.pointer(secs(start, 330), (6.0, 5.0));
        assert_eq!(idle.screen(), Screen::On);
        assert!(
            !idle.key(secs(start, 331)),
            "the pointer woke it, so keys type"
        );
    }

    #[test]
    fn the_waking_key_is_swallowed() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(!idle.key(secs(start, 1)));
        blank(&mut idle, secs(start, 301));
        assert!(idle.key(secs(start, 400)), "swallowed");
        assert!(!idle.key(secs(start, 401)), "the next key types");
    }

    #[test]
    fn a_key_before_the_off_command_lands_never_types() {
        // The timer fires, a key comes before Hyprland ran the off command, and the screen
        // goes off behind it.
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(idle.fire(secs(start, 300)));
        assert!(
            idle.key(secs(start, 300)),
            "swallowed while the off command is out"
        );
        assert!(
            idle.key(secs(start, 300)),
            "every key until the screen is on again"
        );
        assert!(
            idle.off_done(true),
            "the driver must turn the screen back on"
        );
        assert_eq!(idle.screen(), Screen::TurningOn);
        assert!(
            idle.key(secs(start, 301)),
            "still swallowed while the on command is out"
        );
        idle.on_done(true);
        assert_eq!(idle.screen(), Screen::On);
        assert!(
            !idle.key(secs(start, 302)),
            "the screen is on, so keys type"
        );
        assert_eq!(
            idle.deadline(),
            Some(secs(start, 602)),
            "the keys counted as input"
        );
    }

    #[test]
    fn pointer_input_before_the_off_command_lands_turns_it_back_on() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(idle.fire(secs(start, 300)));
        idle.pointer(secs(start, 300), (1.0, 2.0));
        assert!(idle.off_done(true));
        idle.on_done(true);
        assert!(!idle.key(secs(start, 301)));
    }

    #[test]
    fn a_lost_off_answer_is_recovered_with_an_on_command() {
        // Hyprland may have turned the screen off before the answer was lost.
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(idle.fire(secs(start, 300)));
        assert!(
            idle.off_done(false),
            "the driver sends the on command"
        );
        assert_eq!(idle.screen(), Screen::TurningOn);
        assert!(
            idle.key(secs(start, 301)),
            "swallowed until the on command is confirmed"
        );
        idle.on_done(true);
        assert!(
            !idle.key(secs(start, 302)),
            "confirmed on, so keys type"
        );
    }

    #[test]
    fn a_lost_on_answer_counts_the_screen_as_off() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(idle.fire(secs(start, 300)));
        assert!(
            idle.key(secs(start, 300)),
            "a key while the off command is out"
        );
        assert!(
            idle.off_done(false),
            "lost answer and input: the on command goes out"
        );
        idle.on_done(false);
        assert_eq!(idle.screen(), Screen::Off, "not confirmed on");
        assert!(
            idle.key(secs(start, 301)),
            "the next key is the one that wakes it"
        );
        assert!(
            !idle.key(secs(start, 302)),
            "and the one after types"
        );

        // The same with the off answer confirmed and the on answer lost.
        let mut idle = Idle::new(T, start, true);
        assert!(idle.fire(secs(start, 300)));
        idle.pointer(secs(start, 300), (3.0, 4.0));
        assert!(idle.off_done(true));
        idle.on_done(false);
        assert!(idle.key(secs(start, 301)));
        assert!(!idle.key(secs(start, 302)));
    }

    #[test]
    fn stray_completions_change_nothing() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        assert!(!idle.off_done(true));
        idle.on_done(false);
        assert_eq!(
            idle.screen(),
            Screen::On,
            "a stray failed on changes nothing"
        );
        assert!(idle.fire(secs(start, 300)));
        idle.on_done(true);
        assert_eq!(idle.screen(), Screen::TurningOff { woken: false });
        assert!(
            !idle.fire(secs(start, 600)),
            "no second off command while one is out"
        );
    }

    #[test]
    fn nothing_blanks_while_not_allowed() {
        let start = Instant::now();
        let mut idle = Idle::new(T, start, true);
        idle.allow(secs(start, 10), false);
        assert_eq!(idle.deadline(), None);
        assert!(!idle.fire(secs(start, 1000)));
        idle.key(secs(start, 1000));
        idle.allow(secs(start, 2000), true);
        assert_eq!(
            idle.deadline(),
            Some(secs(start, 2300)),
            "restarts when allowed again"
        );
        idle.allow(secs(start, 2100), true);
        assert_eq!(
            idle.deadline(),
            Some(secs(start, 2300)),
            "staying allowed changes nothing"
        );
    }
}
