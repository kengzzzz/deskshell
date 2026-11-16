//! Qt's `SmoothedAnimation`: eases toward a target at an average velocity, two Qt quirks kept.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Smoothed {
    velocity: f64,
    /// In seconds. `None` lets the eases fill the whole move.
    maximum_easing: Option<f64>,
    to: f64,
    value: f64,
    run: Option<Instant>,
    initial_value: f64,
    initial_velocity: f64,
    track_velocity: f64,
    invert: bool,
    curve: Curve,
}

#[derive(Debug, Clone, Copy, Default)]
struct Curve {
    /// Acceleration.
    a: f64,
    /// Deceleration.
    d: f64,
    /// Total time.
    tf: f64,
    /// When the peak velocity is reached.
    tp: f64,
    /// When the deceleration begins.
    td: f64,
    /// The velocity at `tp`.
    vp: f64,
    /// The distance at `tp`.
    sp: f64,
    /// The distance at `td`.
    sd: f64,
    /// The initial velocity, along the move.
    vi: f64,
    /// The whole distance, never negative.
    s: f64,
}

impl Smoothed {
    /// `velocity` is in units a second and must be positive.
    pub fn new(value: f32, velocity: f32, maximum_easing: Option<Duration>) -> Self {
        debug_assert!(velocity > 0.0, "a smoothed animation never stops");
        Self {
            velocity: f64::from(velocity),
            maximum_easing: maximum_easing.map(|time| time.as_secs_f64()),
            to: f64::from(value),
            value: f64::from(value),
            run: None,
            initial_value: f64::from(value),
            initial_velocity: 0.0,
            track_velocity: 0.0,
            invert: false,
            curve: Curve::default(),
        }
    }

    pub fn value(&self) -> f32 {
        self.value as f32
    }

    pub fn target(&self) -> f32 {
        self.to as f32
    }

    pub fn velocity(&self) -> f32 {
        self.velocity as f32
    }

    pub fn moving(&self) -> bool {
        self.run.is_some()
    }

    /// The move starts from the value and the velocity at `now`, so the curves join smoothly.
    pub fn set(&mut self, target: f32, now: Instant) -> bool {
        let to = f64::from(target);
        if to == self.to {
            return false;
        }
        self.advance(now);
        self.to = to;
        if self.run.is_none() && to == self.value {
            return false;
        }
        self.initial_velocity = self.track_velocity;
        self.init(now);
        self.run.is_some()
    }

    /// A running move restarts from where it is at `now`, at the velocity its last target
    /// started with, not the tracked one. Qt does the same.
    pub fn set_velocity(&mut self, velocity: f32, now: Instant) {
        debug_assert!(velocity > 0.0, "a smoothed animation never stops");
        let velocity = f64::from(velocity);
        if velocity == self.velocity {
            return;
        }
        self.velocity = velocity;
        self.advance(now);
        if self.run.is_some() {
            self.init(now);
        }
    }

    pub fn jump(&mut self, target: f32) {
        self.to = f64::from(target);
        self.value = self.to;
        self.run = None;
        self.track_velocity = 0.0;
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        self.advance(now);
        self.run.is_some()
    }

    fn advance(&mut self, now: Instant) {
        let Some(start) = self.run else {
            return;
        };
        let time = now
            .saturating_duration_since(start)
            .as_secs_f64();
        let offset = self.ease_follow(time);
        if time >= self.curve.tf {
            self.value = self.to;
            self.run = None;
        } else {
            self.value = self.initial_value + if self.invert { -offset } else { offset };
        }
    }

    fn init(&mut self, now: Instant) {
        self.initial_value = self.value;
        self.run = Some(now);
        if self.to == self.initial_value {
            self.run = None;
            return;
        }
        let reversed =
            self.track_velocity != 0.0 && (!self.invert) == (self.initial_value - self.to > 0.0);
        if reversed {
            self.initial_velocity = -self.track_velocity;
        }
        self.track_velocity = self.initial_velocity;
        self.invert = self.to < self.initial_value;
        self.recalc();
    }

    fn recalc(&mut self) {
        let sign = if self.invert { -1.0 } else { 1.0 };
        let s = sign * (self.to - self.initial_value);
        let vi = self.initial_velocity;
        let tf = s / self.velocity;
        self.curve = match self.maximum_easing {
            Some(0.0) => Curve {
                a: 0.0,
                d: 0.0,
                tf,
                tp: 0.0,
                td: tf,
                vp: self.velocity,
                sp: 0.0,
                sd: s,
                vi,
                s,
            },
            Some(met) if tf > met => {
                let ta = met / 2.0;
                let a = (s - (vi * tf - 0.5 * vi * ta)) / (tf * ta - ta * ta);
                let vp = vi + a * ta;
                let sp = vi * ta + 0.5 * a * ta * ta;
                Curve {
                    a,
                    d: vp / ta,
                    tf,
                    tp: ta,
                    td: tf - ta,
                    vp,
                    sp,
                    sd: sp + vp * (tf - 2.0 * ta),
                    vi,
                    s,
                }
            }
            _ => {
                let c1 = 0.25 * tf * tf;
                let c2 = 0.5 * vi * tf - s;
                let c3 = -0.25 * vi * vi;
                let a1 = (-c2 + (c2 * c2 - 4.0 * c1 * c3).sqrt()) / (2.0 * c1);
                let tp1 = 0.5 * tf - 0.5 * vi / a1;
                let vp1 = a1 * tp1 + vi;
                let sp1 = 0.5 * a1 * tp1 * tp1 + vi * tp1;
                Curve { a: a1, d: a1, tf, tp: tp1, td: tp1, vp: vp1, sp: sp1, sd: sp1, vi, s }
            }
        };
    }

    fn ease_follow(&mut self, time: f64) -> f64 {
        let Curve { a, d, tf, tp, td, vp, sp, sd, vi, s } = self.curve;
        if time < tp {
            self.track_velocity = vi + time * a;
            0.5 * a * time * time + vi * time
        } else if time < td {
            self.track_velocity = vp;
            sp + (time - tp) * vp
        } else if time < tf {
            let time = time - td;
            // Qt falls with the first ease's acceleration here, not with `d`. Keep it.
            self.track_velocity = vp - time * a;
            sd - 0.5 * d * time * time + vp * time
        } else {
            self.track_velocity = 0.0;
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VELOCITY: f32 = 1100.0;
    const EASING: Option<Duration> = Some(Duration::from_millis(180));

    fn at(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn check(smoothed: &mut Smoothed, start: Instant, seconds: f64, value: f64, velocity: f64) {
        assert!(smoothed.settle(at(start, seconds)), "{seconds}");
        assert!(
            close(smoothed.value, value) && close(smoothed.track_velocity, velocity),
            "at {seconds}: {} at {}, not {value} at {velocity}",
            smoothed.value,
            smoothed.track_velocity
        );
    }

    #[test]
    fn a_short_move_eases_in_and_out() {
        // 100 at 1100 a second takes 1/11 s, under the 180 ms of easing, so it is all
        // ease: from rest it peaks at twice the velocity halfway, at half the distance.
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        assert!(scroll.set(100.0, start));
        assert_eq!(
            scroll.value(),
            0.0,
            "the value moves only on a frame"
        );
        let curve = scroll.curve;
        assert!(close(curve.tf, 1.0 / 11.0) && close(curve.tp, 0.5 / 11.0));
        assert!(close(curve.vp, 2200.0) && close(curve.sp, 50.0));
        check(&mut scroll, start, 0.02, 9.68, 968.0);
        check(&mut scroll, start, 0.05, 59.5, 1980.0);
        check(&mut scroll, start, 0.08, 97.12, 528.0);
        assert!(!scroll.settle(at(start, 0.1)));
        assert_eq!(scroll.value(), 100.0);
        assert_eq!(scroll.track_velocity, 0.0);
        assert!(!scroll.moving());
    }

    #[test]
    fn a_long_move_eases_for_at_most_the_easing_time() {
        // 1100 takes 1 s: 90 ms of ease at each end and a steady speed between, so the
        // middle runs at 1100 / 0.91 and is halfway at 0.5 s.
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        let curve = scroll.curve;
        assert!(close(curve.tp, 0.09) && close(curve.td, 0.91) && close(curve.tf, 1.0));
        assert!(close(curve.vp, 1100.0 / 0.91));
        assert!(close(curve.sp, 0.5 * 1100.0 * 0.09 / 0.91));
        check(
            &mut scroll,
            start,
            0.05,
            16.788_766_788_766_79,
            671.550_671_550_671_6,
        );
        check(&mut scroll, start, 0.5, 550.0, 1100.0 / 0.91);
        check(
            &mut scroll,
            start,
            0.95,
            1_083.211_233_211_233,
            671.550_671_550_672_4,
        );
        assert!(!scroll.settle(at(start, 1.0)));
        assert_eq!(scroll.value(), 1100.0);
    }

    #[test]
    fn a_new_target_keeps_the_velocity() {
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        assert!(scroll.set(1400.0, at(start, 0.5)));
        // From 550 at 1100 / 0.91, 850 to go: 850 / 1100 s with a gentle slowdown into
        // the steady part.
        let curve = scroll.curve;
        assert!(close(curve.vi, 1100.0 / 0.91) && close(curve.s, 850.0));
        assert!(close(curve.tf, 850.0 / 1100.0));
        assert!(close(curve.a, -482.872_653_312_064_13));
        assert!(close(curve.vp, 1_165.332_669_993_122_9));
        check(
            &mut scroll,
            start,
            0.55,
            609.835_969_622_920_4,
            1_184.647_576_125_605_4,
        );
        check(
            &mut scroll,
            start,
            1.0,
            1_134.621_969_242_475_2,
            1_165.332_669_993_122_9,
        );
        assert!(!scroll.settle(at(start, 1.3)));
        assert_eq!(scroll.value(), 1400.0);
    }

    #[test]
    fn a_reversed_target_slows_down_first() {
        // Eased, the default reversingMode: the move back starts at minus the velocity.
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        assert!(scroll.set(0.0, at(start, 0.5)));
        let curve = scroll.curve;
        assert!(scroll.invert);
        assert!(close(curve.vi, -1100.0 / 0.91) && close(curve.s, 550.0) && close(curve.tf, 0.5));
        check(
            &mut scroll,
            start,
            0.55,
            573.176_687_810_834_1,
            281.723_696_357_844_1,
        );
        check(
            &mut scroll,
            start,
            0.6,
            523.318_145_269_364_8,
            1_474.135_620_477_083_6,
        );
        assert!(!scroll.settle(at(start, 1.0)));
        assert_eq!(scroll.value(), 0.0);
    }

    #[test]
    fn a_velocity_change_restarts_from_the_last_targets_velocity() {
        // The drag's auto scroll sets `900 + 2400 * intensity`. The move started from
        // rest, so it starts again from rest, at 550, with 550 to go at 3300 a second.
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        scroll.set_velocity(3300.0, at(start, 0.5));
        assert_eq!(scroll.velocity(), 3300.0);
        let curve = scroll.curve;
        assert!(close(scroll.value, 550.0) && close(curve.vi, 0.0));
        assert!(close(curve.tf, 1.0 / 6.0) && close(curve.vp, 6600.0));
        check(&mut scroll, start, 0.55, 649.0, 3960.0);
        check(&mut scroll, start, 0.6, 924.0, 5280.0);
        assert!(!scroll.settle(at(start, 0.7)));
        assert_eq!(scroll.value(), 1100.0);

        let mut rested = Smoothed::new(0.0, VELOCITY, EASING);
        rested.set_velocity(900.0, start);
        assert!(!rested.moving(), "a velocity alone moves nothing");
        assert_eq!(rested.velocity(), 900.0);
    }

    #[test]
    fn the_last_ease_tracks_the_first_eases_acceleration() {
        // After the retarget the first ease decelerates (a < 0). In the last ease Qt's
        // tracked velocity has risen by -a * t, and the next target starts from that.
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        scroll.set(1400.0, at(start, 0.5));
        scroll.settle(at(start, 1.22));
        assert!(close(scroll.value, 1_382.001_014_224_716));
        assert!(close(
            scroll.track_velocity,
            1_183.330_650_707_481_7
        ));
        assert!(scroll.set(2000.0, at(start, 1.22)));
        assert!(close(scroll.curve.vi, 1_183.330_650_707_481_7));
        check(
            &mut scroll,
            start,
            1.25,
            1_417.569_109_308_733_2,
            1_187.875_688_226_987_4,
        );
        check(
            &mut scroll,
            start,
            1.7,
            1_955.485_747_635_083_5,
            1_195.726_067_891_303_7,
        );
        assert!(!scroll.settle(at(start, 1.8)));
        assert_eq!(scroll.value(), 2000.0);
    }

    #[test]
    fn the_same_target_changes_nothing() {
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(100.0, start);
        assert!(
            !scroll.set(100.0, at(start, 0.05)),
            "the move goes on"
        );
        check(&mut scroll, start, 0.05, 59.5, 1980.0);

        let mut rested = Smoothed::new(40.0, VELOCITY, EASING);
        assert!(!rested.set(40.0, start));
        assert!(!rested.moving());
        assert!(!rested.settle(at(start, 1.0)));
    }

    #[test]
    fn a_move_back_to_the_value_needs_no_frames() {
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(100.0, start);
        scroll.settle(at(start, 1.0));
        assert!(scroll.set(0.0, at(start, 2.0)));
        assert!(
            !scroll.set(100.0, at(start, 2.0)),
            "nothing moved yet"
        );
        assert!(!scroll.moving());
        assert_eq!(scroll.value(), 100.0);
    }

    #[test]
    fn a_jump_rests_at_the_target() {
        let start = Instant::now();
        let mut scroll = Smoothed::new(0.0, VELOCITY, EASING);
        scroll.set(1100.0, start);
        scroll.settle(at(start, 0.5));
        scroll.jump(300.0);
        assert_eq!((scroll.value(), scroll.target()), (300.0, 300.0));
        assert!(!scroll.moving());
        assert!(scroll.set(400.0, at(start, 0.6)));
        assert!(close(scroll.curve.vi, 0.0), "from rest");
    }

    #[test]
    fn without_easing_time_the_whole_move_eases() {
        let start = Instant::now();
        let mut slow = Smoothed::new(0.0, VELOCITY, None);
        slow.set(1100.0, start);
        let curve = slow.curve;
        assert!(close(curve.tp, 0.5) && close(curve.vp, 2200.0));
        let mut linear = Smoothed::new(0.0, VELOCITY, Some(Duration::ZERO));
        linear.set(1100.0, start);
        check(&mut linear, start, 0.25, 275.0, 1100.0);
    }
}
