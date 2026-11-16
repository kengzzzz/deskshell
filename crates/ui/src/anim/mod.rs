//! Tweens that move a value toward a target, frame by frame.

use std::time::{Duration, Instant};

use iced::Color;

mod smoothed;
pub use smoothed::Smoothed;
mod expo;
pub use expo::out_expo;
mod sine;
pub use sine::in_out_sine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    OutCubic,
}

impl Easing {
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::OutCubic => 1.0 - (1.0 - t).powi(3),
        }
    }
}

pub trait Lerp: Copy + PartialEq {
    fn lerp(self, to: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, to: Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

/// Each sRGB component on its own, alpha unpremultiplied, as Qt interpolates a `QColor`.
impl Lerp for Color {
    fn lerp(self, to: Self, t: f32) -> Self {
        Color {
            r: self.r.lerp(to.r, t),
            g: self.g.lerp(to.g, t),
            b: self.b.lerp(to.b, t),
            a: self.a.lerp(to.a, t),
        }
    }
}

/// A new target restarts from the current value over the full duration. The same target again
/// changes nothing.
#[derive(Debug, Clone)]
pub struct Tween<T> {
    value: T,
    target: T,
    run: Option<(T, Instant)>,
    duration: Duration,
    easing: Easing,
}

impl<T: Lerp> Tween<T> {
    pub fn new(value: T, duration: Duration, easing: Easing) -> Self {
        Self { value, target: value, run: None, duration, easing }
    }

    pub fn value(&self) -> T {
        self.value
    }

    pub fn target(&self) -> T {
        self.target
    }

    pub fn moving(&self) -> bool {
        self.run.is_some()
    }

    pub fn set(&mut self, target: T, now: Instant) -> bool {
        if target == self.target {
            return false;
        }
        let from = self.at(now);
        self.target = target;
        if from == target {
            self.run = None;
            self.value = target;
            return false;
        }
        self.run = Some((from, now));
        true
    }

    pub fn jump(&mut self, target: T) {
        self.target = target;
        self.value = target;
        self.run = None;
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        self.value = self.at(now);
        if self.value == self.target {
            self.run = None;
        }
        self.run.is_some()
    }

    fn at(&self, now: Instant) -> T {
        let Some((from, start)) = self.run else {
            return self.value;
        };
        let t = progress(now, start, self.duration);
        if t >= 1.0 { self.target } else { from.lerp(self.target, self.easing.apply(t)) }
    }
}

pub fn progress(now: Instant, start: Instant, length: Duration) -> f32 {
    if length.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(start)
        .as_secs_f32()
        / length.as_secs_f32())
    .min(1.0)
}

#[derive(Debug, Clone)]
pub struct Tweens<K, T> {
    items: Vec<(K, Tween<T>)>,
    duration: Duration,
    easing: Easing,
}

impl<K: PartialEq, T: Lerp> Tweens<K, T> {
    pub fn new(duration: Duration, easing: Easing) -> Self {
        Self { items: Vec::new(), duration, easing }
    }

    /// A key seen for the first time starts at `target`, so a new item does not animate in.
    pub fn set(&mut self, key: K, target: T, now: Instant) -> bool {
        match self
            .items
            .iter_mut()
            .find(|(item, _)| *item == key)
        {
            Some((_, tween)) => tween.set(target, now),
            None => {
                self.items.push((
                    key,
                    Tween::new(target, self.duration, self.easing),
                ));
                false
            }
        }
    }

    pub fn jump(&mut self, key: K, target: T) {
        match self
            .items
            .iter_mut()
            .find(|(item, _)| *item == key)
        {
            Some((_, tween)) => tween.jump(target),
            None => self.items.push((
                key,
                Tween::new(target, self.duration, self.easing),
            )),
        }
    }

    pub fn value(&self, key: &K) -> Option<T> {
        self.items
            .iter()
            .find(|(item, _)| item == key)
            .map(|(_, tween)| tween.value())
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.items.retain(|(key, _)| keep(key));
    }

    pub fn moving(&self) -> bool {
        self.items
            .iter()
            .any(|(_, tween)| tween.moving())
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        let mut moving = false;
        for (_, tween) in &mut self.items {
            moving |= tween.settle(now);
        }
        moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn out_cubic_matches_qeasingcurve() {
        // QEasingCurve::OutCubic is (t - 1)^3 + 1.
        for (t, eased) in
            [(0.0, 0.0), (0.25, 0.578_125), (0.5, 0.875), (0.75, 0.984_375), (1.0, 1.0)]
        {
            assert!(close(Easing::OutCubic.apply(t), eased), "{t}");
        }
        assert!(close(Easing::Linear.apply(0.3), 0.3));
        assert!(close(Easing::OutCubic.apply(1.5), 1.0));
    }

    #[test]
    fn runs_to_the_target_and_stops() {
        let start = Instant::now();
        let mut width = Tween::new(8.0, 140 * MS, Easing::OutCubic);
        assert!(width.set(22.0, start));
        assert!(width.moving());
        assert_eq!(
            width.value(),
            8.0,
            "the value moves only on a frame"
        );
        assert!(width.settle(start + 70 * MS));
        assert!(close(width.value(), 8.0 + 14.0 * 0.875));
        assert!(!width.settle(start + 140 * MS));
        assert_eq!(width.value(), 22.0);
        assert!(!width.moving());
        assert!(
            !width.settle(start + 200 * MS),
            "a rested tween needs no frames"
        );
    }

    #[test]
    fn a_new_target_starts_from_the_current_value_over_the_full_duration() {
        let start = Instant::now();
        let mut width = Tween::new(0.0, 100 * MS, Easing::Linear);
        width.set(100.0, start);
        width.settle(start + 40 * MS);
        assert!(width.set(0.0, start + 50 * MS));
        // From 50 at 50 ms, back to 0 by 150 ms.
        width.settle(start + 100 * MS);
        assert!(close(width.value(), 25.0));
        assert!(width.settle(start + 149 * MS));
        assert!(!width.settle(start + 150 * MS));
        assert_eq!(width.value(), 0.0);
    }

    #[test]
    fn the_same_target_changes_nothing() {
        let start = Instant::now();
        let mut width = Tween::new(0.0, 100 * MS, Easing::Linear);
        width.set(100.0, start);
        assert!(
            !width.set(100.0, start + 50 * MS),
            "the running animation goes on"
        );
        width.settle(start + 50 * MS);
        assert!(close(width.value(), 50.0));
        let mut rested = Tween::new(4.0, 100 * MS, Easing::Linear);
        assert!(!rested.set(4.0, start));
        assert!(!rested.moving());
    }

    #[test]
    fn a_target_that_moves_every_frame_is_followed() {
        // The target changes while the tween runs.
        let start = Instant::now();
        let mut pill = Tween::new(100.0, 140 * MS, Easing::OutCubic);
        let mut now = start;
        for frame in 1..=20u32 {
            now = start + frame * 7 * MS;
            pill.set(100.0 + 2.0 * frame as f32, now);
            pill.settle(now);
            assert!(pill.value() >= 100.0 && pill.value() <= pill.target());
        }
        assert_eq!(pill.target(), 140.0);
        let end = now + 140 * MS;
        assert!(!pill.settle(end));
        assert_eq!(pill.value(), 140.0);
    }

    #[test]
    fn a_jump_stops_the_animation() {
        let start = Instant::now();
        let mut fill = Tween::new(0.0, 100 * MS, Easing::OutCubic);
        fill.set(50.0, start);
        fill.jump(80.0);
        assert_eq!(fill.value(), 80.0);
        assert!(!fill.moving());
        assert!(!fill.settle(start + 10 * MS));
    }

    #[test]
    fn a_target_equal_to_the_value_now_needs_no_frames() {
        let start = Instant::now();
        let mut opacity = Tween::new(1.0, 100 * MS, Easing::Linear);
        opacity.set(0.0, start);
        opacity.settle(start + 100 * MS);
        assert!(!opacity.set(0.0, start + 120 * MS));
        assert!(!opacity.moving());
    }

    #[test]
    fn colors_move_per_component() {
        let start = Instant::now();
        let from = Color::from_rgba(0.0, 0.2, 1.0, 0.5);
        let to = Color::from_rgba(1.0, 0.4, 0.0, 1.0);
        let mut border = Tween::new(from, 120 * MS, Easing::Linear);
        border.set(to, start);
        border.settle(start + 60 * MS);
        let half = border.value();
        assert!(
            close(half.r, 0.5) && close(half.g, 0.3) && close(half.b, 0.5) && close(half.a, 0.75)
        );
    }

    #[test]
    fn a_zero_duration_ends_on_the_first_frame() {
        let start = Instant::now();
        let mut tween = Tween::new(0.0, Duration::ZERO, Easing::OutCubic);
        tween.set(1.0, start);
        assert!(!tween.settle(start));
        assert_eq!(tween.value(), 1.0);
    }

    #[test]
    fn list_items_start_at_their_target_and_go_with_their_rows() {
        let start = Instant::now();
        let mut dots: Tweens<i64, f32> = Tweens::new(140 * MS, Easing::OutCubic);
        assert!(
            !dots.set(1, 22.0, start),
            "a new delegate does not animate"
        );
        assert!(!dots.set(2, 8.0, start));
        assert_eq!(dots.value(&1), Some(22.0));
        assert!(!dots.moving());
        assert!(dots.set(1, 8.0, start));
        assert!(dots.set(2, 22.0, start));
        assert!(dots.settle(start + 70 * MS));
        assert!(close(
            dots.value(&2).unwrap_or_default(),
            8.0 + 14.0 * 0.875
        ));
        dots.retain(|key| *key == 2);
        assert_eq!(dots.value(&1), None);
        assert!(!dots.settle(start + 140 * MS));
        assert_eq!(dots.value(&2), Some(22.0));
        dots.jump(3, 30.0);
        assert_eq!(dots.value(&3), Some(30.0));
        assert!(!dots.moving());
    }
}
