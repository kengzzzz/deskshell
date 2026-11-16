//! A vertical list's mouse wheel, with the eased steps and bound behaviour of Qt's Flickable.

use std::time::{Duration, Instant};

use crate::anim::out_expo;

pub const NOTCH: f32 = 72.0;
pub const STEP: Duration = Duration::from_millis(300);
const RETURN: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bounds {
    #[default]
    Stop,
    Overshoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Curve {
    OutExpo,
    InQuad,
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    to: f64,
    length: Duration,
    curve: Curve,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    start: Instant,
    from: f64,
    segment: Segment,
    next: Option<Segment>,
    fixup: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Wheel {
    bounds: Bounds,
    position: f64,
    max: f64,
    run: Option<Run>,
}

impl Wheel {
    pub fn new(bounds: Bounds) -> Self {
        Self { bounds, position: 0.0, max: 0.0, run: None }
    }

    pub fn position(&self) -> f32 {
        self.position as f32
    }

    pub fn max(&self) -> f32 {
        self.max as f32
    }

    pub fn target(&self) -> f32 {
        let Some(run) = self.run else {
            return self.position();
        };
        run.next.unwrap_or(run.segment).to as f32
    }

    pub fn moving(&self) -> bool {
        self.run.is_some()
    }

    pub fn set_max(&mut self, max: f32) {
        let max = f64::from(max.max(0.0));
        if max == self.max {
            return;
        }
        self.max = max;
        if self.bounds != Bounds::Stop {
            return;
        }
        if self.position > max {
            self.position = max;
            self.run = None;
        } else if let Some(run) = &mut self.run {
            run.segment.to = run.segment.to.min(max);
        }
    }

    /// `lines` notches, positive toward the top. False passes the event on to the parent.
    /// Each notch starts from where the content is, not where the last step was headed.
    pub fn notch(&mut self, lines: f32, now: Instant) -> bool {
        if lines == 0.0 {
            return false;
        }
        let step = -f64::from(lines) * f64::from(NOTCH);
        let value = self.position;
        if (step < 0.0 && value <= 0.0) || (step > 0.0 && value >= self.max) {
            return false;
        }
        let mut to = value + step;
        let mut accepted = true;
        if self.bounds == Bounds::Stop {
            if step < 0.0 && to < 0.0 {
                to = 0.0;
                accepted = false;
            } else if step > 0.0 && to > self.max {
                to = self.max;
                accepted = false;
            }
        }
        self.run = Some(Run {
            start: now,
            from: value,
            segment: Segment { to, length: STEP, curve: Curve::OutExpo },
            next: None,
            fixup: true,
        });
        accepted
    }

    pub fn drag(&mut self, pixels: f32) -> bool {
        if pixels != 0.0 {
            self.run = None;
            self.position = (self.position - f64::from(pixels)).clamp(0.0, self.max);
        }
        self.position > 0.0 && self.position < self.max
    }

    pub fn stop(&mut self) {
        self.run = None;
    }

    pub fn jump(&mut self, position: f32) {
        self.run = None;
        self.position = f64::from(position);
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        while let Some(run) = self.run {
            let elapsed = now.saturating_duration_since(run.start);
            let Segment { to, length, curve } = run.segment;
            if elapsed < length {
                let t = elapsed.as_secs_f64() / length.as_secs_f64();
                let eased = match curve {
                    Curve::OutExpo => out_expo(t),
                    Curve::InQuad => t * t,
                };
                self.position = run.from + (to - run.from) * eased;
                return true;
            }
            self.position = to;
            let end = run.start + length;
            self.run = run.next.map(|segment| Run {
                start: end,
                from: to,
                segment,
                next: None,
                fixup: false,
            });
            if run.fixup {
                self.fixup(end, to > run.from);
            }
        }
        false
    }

    fn fixup(&mut self, at: Instant, down: bool) {
        let value = self.position;
        // Qt's bound branches take `move >= minExtent` and `move <= maxExtent`, so a
        // step that rests on a bound between two pixels stays there.
        let bound = if value <= 0.0 {
            0.0
        } else if value >= self.max {
            self.max
        } else {
            let rounded = value.round();
            self.position = if (rounded - value).abs() < 0.25 {
                rounded
            } else if down {
                value.floor()
            } else {
                value.ceil()
            };
            return;
        };
        if value == bound {
            return;
        }
        let half = value + (bound - value) / 2.0;
        self.run = Some(Run {
            start: at,
            from: value,
            segment: Segment { to: half, length: RETURN, curve: Curve::InQuad },
            next: Some(Segment { to: bound, length: STEP, curve: Curve::OutExpo }),
            fixup: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);
    const FRAME: Duration = Duration::from_micros(16_667);

    /// `easeOutExpo` written out again, so a slip in `out_expo` shows.
    fn expo(t: f64) -> f64 {
        1.001 * (1.0 - 2f64.powf(-10.0 * t))
    }

    fn close(a: f32, b: f64) -> bool {
        (f64::from(a) - b).abs() < 1e-3
    }

    fn list(max: f32, position: f32) -> Wheel {
        let mut wheel = Wheel::new(Bounds::Stop);
        wheel.set_max(max);
        wheel.jump(position);
        wheel
    }

    #[test]
    fn one_notch_moves_72_px_over_300_ms() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        assert!(wheel.notch(-1.0, start), "a notch down");
        assert_eq!(wheel.target(), 72.0);
        assert!(wheel.settle(start));
        assert_eq!(wheel.position(), 0.0, "time 0 is the base");
        assert!(wheel.settle(start + 30 * MS));
        assert!(close(wheel.position(), 72.0 * expo(0.1)));
        assert!(close(wheel.position(), 36.036));
        assert!(wheel.settle(start + 150 * MS));
        assert!(close(wheel.position(), 72.0 * expo(0.5)));
        assert!(wheel.settle(start + 299 * MS));
        assert!(!wheel.settle(start + 300 * MS));
        assert_eq!(wheel.position(), 72.0);
        assert!(!wheel.moving());
    }

    #[test]
    fn a_notch_up_moves_toward_the_top() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 500.0);
        assert!(wheel.notch(1.0, start));
        wheel.settle(start + 300 * MS);
        assert_eq!(wheel.position(), 428.0);
    }

    #[test]
    fn a_partial_notch_moves_its_share() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        wheel.notch(-0.5, start);
        wheel.settle(start + STEP);
        assert_eq!(wheel.position(), 36.0);
    }

    #[test]
    fn about_18_frames_at_60_hz() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        wheel.notch(-1.0, start);
        let mut frames = 1;
        let mut now = start;
        while wheel.settle(now) {
            frames += 1;
            now += FRAME;
        }
        assert_eq!(frames, 19, "frames at 0, 16.7, .. 300 ms");
    }

    #[test]
    fn a_quick_notch_restarts_from_where_the_content_is() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        wheel.notch(-1.0, start);
        wheel.settle(start + 50 * MS);
        let mid = 72.0 * expo(50.0 / 300.0);
        assert!(close(wheel.position(), mid));
        // The second notch comes after that frame, between two frames.
        let second = start + 58 * MS;
        assert!(wheel.notch(-1.0, second));
        assert!(close(wheel.target(), mid + 72.0));
        wheel.settle(second);
        assert!(
            close(wheel.position(), mid),
            "it starts from the last frame"
        );
        wheel.settle(second + 30 * MS);
        assert!(close(wheel.position(), mid + 72.0 * expo(0.1)));
        assert!(wheel.settle(second + 299 * MS));
        assert!(!wheel.settle(second + 300 * MS));
        // 121.37 ends 0.37 from a pixel, so fixup rounds against the motion.
        assert!((mid + 72.0 - 121.3707).abs() < 1e-3);
        assert_eq!(wheel.position(), 121.0);
    }

    #[test]
    fn two_notches_before_a_frame_move_one() {
        // Each notch resets the timeline, and no tick ran between them.
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        wheel.notch(-1.0, start);
        wheel.notch(-1.0, start + 4 * MS);
        wheel.settle(start + 400 * MS);
        assert_eq!(wheel.position(), 72.0);
    }

    #[test]
    fn a_rest_near_a_pixel_rounds_to_it() {
        let start = Instant::now();
        let mut near = list(1000.0, 10.2);
        near.notch(-1.0, start);
        near.settle(start + STEP);
        assert_eq!(near.position(), 82.0);
        let mut down = list(1000.0, 10.6);
        down.notch(-1.0, start);
        down.settle(start + STEP);
        assert_eq!(
            down.position(),
            82.0,
            "82.6 moving down rounds down"
        );
        let mut up = list(1000.0, 100.3);
        up.notch(1.0, start);
        up.settle(start + STEP);
        assert_eq!(up.position(), 29.0, "28.3 moving up rounds up");
    }

    #[test]
    fn stop_at_bounds_cuts_the_step_and_passes_the_event_on() {
        let start = Instant::now();
        let mut wheel = list(300.0, 30.0);
        assert!(
            !wheel.notch(1.0, start),
            "a cut step is not accepted"
        );
        assert_eq!(wheel.target(), 0.0);
        wheel.settle(start + 150 * MS);
        assert!(close(wheel.position(), 30.0 - 30.0 * expo(0.5)));
        assert!(!wheel.settle(start + STEP));
        assert_eq!(wheel.position(), 0.0);
        assert!(!wheel.notch(1.0, start + STEP), "at the top");
        assert!(!wheel.moving());

        let mut bottom = list(300.0, 250.0);
        assert!(!bottom.notch(-1.0, start));
        bottom.settle(start + STEP);
        assert_eq!(bottom.position(), 300.0);
        assert!(!bottom.notch(-1.0, start + STEP));
        assert!(!bottom.moving());
        assert!(
            bottom.notch(1.0, start + STEP),
            "away from the bound"
        );
    }

    #[test]
    fn a_step_rests_on_a_bound_between_pixels() {
        let start = Instant::now();
        let mut wheel = list(317.6, 300.0);
        assert!(!wheel.notch(-1.0, start));
        assert!(!wheel.settle(start + STEP));
        assert_eq!(
            wheel.position(),
            317.6,
            "not rounded off the bound"
        );
        assert!(!wheel.notch(-1.0, start + STEP), "and at it");
        assert!(!wheel.moving());
        let mut top = list(317.6, 30.4);
        top.notch(1.0, start);
        top.settle(start + STEP);
        assert_eq!(top.position(), 0.0);
    }

    #[test]
    fn nothing_moves_when_the_content_fits() {
        let start = Instant::now();
        let mut wheel = list(0.0, 0.0);
        assert!(!wheel.notch(1.0, start));
        assert!(!wheel.notch(-1.0, start));
        assert!(!wheel.moving());
        assert!(!wheel.drag(-20.0));
        assert_eq!(wheel.position(), 0.0);
    }

    #[test]
    fn overshoot_passes_the_bound_and_eases_back() {
        let start = Instant::now();
        let mut wheel = Wheel::new(Bounds::Overshoot);
        wheel.set_max(300.0);
        wheel.jump(30.0);
        assert!(wheel.notch(1.0, start));
        assert!(wheel.settle(start + 150 * MS));
        assert!(close(wheel.position(), 30.0 - 72.0 * expo(0.5)));
        // The step ends at -42, then fixup goes halfway with InQuad over 100 ms ...
        assert!(wheel.settle(start + 300 * MS));
        assert_eq!(wheel.position(), -42.0);
        assert!(wheel.settle(start + 350 * MS));
        assert!(close(wheel.position(), -42.0 + 21.0 * 0.25));
        assert!(wheel.settle(start + 400 * MS));
        assert!(close(wheel.position(), -21.0));
        // ... and to the bound with OutExpo over 300 ms.
        assert!(wheel.settle(start + 430 * MS));
        assert!(close(wheel.position(), -21.0 + 21.0 * expo(0.1)));
        assert!(wheel.settle(start + 699 * MS));
        assert!(!wheel.settle(start + 700 * MS));
        assert_eq!(wheel.position(), 0.0);
    }

    #[test]
    fn overshoot_at_the_bottom_and_a_frame_late() {
        // A frame that comes after the step's end still runs the way back from its end.
        let start = Instant::now();
        let mut wheel = Wheel::new(Bounds::Overshoot);
        wheel.set_max(300.0);
        wheel.jump(280.0);
        wheel.notch(-1.0, start);
        assert!(wheel.settle(start + 350 * MS));
        assert!(close(wheel.position(), 352.0 - 26.0 * 0.25));
        assert_eq!(wheel.target(), 300.0);
        assert!(!wheel.settle(start + 1000 * MS));
        assert_eq!(wheel.position(), 300.0);
    }

    #[test]
    fn a_notch_toward_a_passed_bound_does_nothing() {
        let start = Instant::now();
        let mut wheel = Wheel::new(Bounds::Overshoot);
        wheel.set_max(300.0);
        wheel.jump(30.0);
        wheel.notch(1.0, start);
        wheel.settle(start + 350 * MS);
        assert!(!wheel.notch(1.0, start + 360 * MS));
        assert!(
            wheel.notch(-1.0, start + 360 * MS),
            "away from it"
        );
        wheel.settle(start + 360 * MS + STEP);
        assert_eq!(
            wheel.position(),
            35.0,
            "35.25, rounded against the motion"
        );
    }

    #[test]
    fn a_pixel_delta_follows_at_once_and_stops_a_step() {
        let start = Instant::now();
        let mut wheel = list(300.0, 100.0);
        assert!(wheel.drag(-12.5), "a touchpad moving down");
        assert_eq!(wheel.position(), 112.5);
        assert!(!wheel.moving());
        wheel.notch(-1.0, start);
        wheel.settle(start + 50 * MS);
        let mid = wheel.position();
        assert!(wheel.drag(10.0));
        assert!(!wheel.moving(), "drag() calls clearTimeline");
        assert!(close(wheel.position(), f64::from(mid) - 10.0));
        assert!(!wheel.drag(500.0), "it reached the top");
        assert_eq!(wheel.position(), 0.0);
        assert!(!wheel.drag(5.0));
        assert!(!wheel.drag(-400.0));
        assert_eq!(wheel.position(), 300.0);
    }

    #[test]
    fn shrinking_content_clamps() {
        let start = Instant::now();
        let mut wheel = list(300.0, 250.0);
        wheel.notch(1.0, start);
        wheel.set_max(400.0);
        assert_eq!(wheel.target(), 178.0, "growing changes nothing");
        wheel.set_max(100.0);
        assert!(!wheel.moving());
        assert_eq!(wheel.position(), 100.0);
        let mut bottom = list(300.0, 250.0);
        bottom.notch(-1.0, start);
        bottom.set_max(280.0);
        assert_eq!(
            bottom.target(),
            280.0,
            "a step headed past the bound ends at it"
        );
        bottom.settle(start + STEP);
        assert_eq!(bottom.position(), 280.0);
    }

    #[test]
    fn stop_and_jump_end_a_step() {
        let start = Instant::now();
        let mut wheel = list(1000.0, 0.0);
        wheel.notch(-1.0, start);
        wheel.settle(start + 40 * MS);
        let at = wheel.position();
        wheel.stop();
        assert!(!wheel.settle(start + 80 * MS));
        assert_eq!(wheel.position(), at);
        wheel.notch(-1.0, start + 80 * MS);
        wheel.jump(0.0);
        assert!(!wheel.settle(start + 100 * MS));
        assert_eq!(wheel.position(), 0.0);
    }
}
