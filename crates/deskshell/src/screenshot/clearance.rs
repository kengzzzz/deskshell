//! When the screen is free of deskshell surfaces that are closing or fading out.

use std::time::{Duration, Instant};

use iced::window;

use super::flow::SETTLE;

/// About one frame, so a capture comes after the last faded frame.
const FRAME: Duration = Duration::from_millis(50);

/// Hyprland's built-in 800 ms fade and a frame, for a fade of unknown length.
const UNKNOWN: Duration = Duration::from_millis(850);

/// A requested close that never reports back counts as done after this.
const CLOSE_LIMIT: Duration = Duration::from_millis(500);

/// How often a pending close is looked at again.
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fade {
    Known(Duration),
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy)]
struct Closing {
    id: window::Id,
    since: Instant,
    fades: bool,
}

#[derive(Debug, Default)]
pub struct Clearance {
    fade: Fade,
    closing: Vec<Closing>,
    clear_at: Option<Instant>,
}

impl Clearance {
    pub fn set_fade(&mut self, fade: Fade) {
        self.fade = fade;
    }

    /// The close of `id` was requested. `fades` is false for a surface with `no_anim`.
    pub fn closing(&mut self, id: window::Id, now: Instant, fades: bool) {
        self.closing
            .retain(|closing| closing.id != id);
        self.closing
            .push(Closing { id, since: now, fades });
    }

    /// Only a close requested through [`Clearance::closing`] counts, so toasts and tooltips
    /// that come and go cannot hold a capture back.
    pub fn closed(&mut self, id: window::Id, now: Instant) {
        let Some(index) = self
            .closing
            .iter()
            .position(|closing| closing.id == id)
        else {
            return;
        };
        let fades = self.closing.swap_remove(index).fades;
        self.clear_after(now + self.after_close(fades));
    }

    /// `None` when a capture can start now, else when to ask again.
    pub fn wait(&mut self, now: Instant) -> Option<Duration> {
        let mut index = 0;
        while index < self.closing.len() {
            let closing = self.closing[index];
            let limit = closing.since + CLOSE_LIMIT;
            if now >= limit {
                self.closing.swap_remove(index);
                self.clear_after(limit + self.after_close(closing.fades));
            } else {
                index += 1;
            }
        }
        if let Some(limit) = self
            .closing
            .iter()
            .map(|closing| closing.since + CLOSE_LIMIT)
            .min()
        {
            return Some((limit - now).min(POLL));
        }
        self.clear_at
            .map(|at| at.saturating_duration_since(now))
            .filter(|wait| !wait.is_zero())
    }

    fn clear_after(&mut self, at: Instant) {
        self.clear_at = Some(
            self.clear_at
                .map_or(at, |clear_at| clear_at.max(at)),
        );
    }

    fn after_close(&self, fades: bool) -> Duration {
        match (fades, self.fade) {
            (false, _) => SETTLE,
            (true, Fade::Known(fade)) if fade.is_zero() => SETTLE,
            (true, Fade::Known(fade)) => (fade + FRAME).max(SETTLE),
            (true, Fade::Unknown) => UNKNOWN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(ms: u64) -> Clearance {
        let mut clearance = Clearance::default();
        clearance.set_fade(Fade::Known(Duration::from_millis(ms)));
        clearance
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn nothing_closed_is_clear() {
        assert_eq!(Clearance::default().wait(Instant::now()), None);
    }

    #[test]
    fn the_wait_starts_when_the_surface_is_gone() {
        let mut clearance = known(500);
        let id = window::Id::unique();
        let t0 = Instant::now();
        clearance.closing(id, t0, true);
        assert_eq!(clearance.wait(t0 + ms(5)), Some(POLL));
        clearance.closed(id, t0 + ms(90));
        assert_eq!(clearance.wait(t0 + ms(100)), Some(ms(540)));
        assert_eq!(clearance.wait(t0 + ms(640)), None);
    }

    fn closed(clearance: &mut Clearance, now: Instant) {
        let id = window::Id::unique();
        clearance.closing(id, now, true);
        clearance.closed(id, now);
    }

    #[test]
    fn a_later_capture_inherits_the_outstanding_wait() {
        let mut clearance = known(500);
        let t0 = Instant::now();
        closed(&mut clearance, t0);
        assert_eq!(clearance.wait(t0 + ms(450)), Some(ms(100)));
    }

    #[test]
    fn a_close_not_requested_here_is_ignored() {
        let mut clearance = known(500);
        clearance.closed(window::Id::unique(), Instant::now());
        assert_eq!(clearance.wait(Instant::now()), None);
    }

    #[test]
    fn a_surface_without_a_fade_waits_the_short_settle() {
        let mut clearance = known(500);
        let id = window::Id::unique();
        let t0 = Instant::now();
        clearance.closing(id, t0, false);
        clearance.closed(id, t0);
        assert_eq!(clearance.wait(t0), Some(SETTLE));
    }

    #[test]
    fn an_unknown_or_turned_off_fade() {
        let t0 = Instant::now();
        let mut clearance = Clearance::default();
        closed(&mut clearance, t0);
        assert_eq!(clearance.wait(t0), Some(UNKNOWN));
        let mut clearance = known(0);
        closed(&mut clearance, t0);
        assert_eq!(clearance.wait(t0), Some(SETTLE));
    }

    #[test]
    fn a_close_that_never_reports_back_expires() {
        let mut clearance = known(500);
        let t0 = Instant::now();
        clearance.closing(window::Id::unique(), t0, true);
        assert_eq!(clearance.wait(t0 + ms(490)), Some(ms(10)));
        assert_eq!(clearance.wait(t0 + ms(500)), Some(ms(550)));
        assert_eq!(clearance.wait(t0 + ms(1050)), None);
    }
}
