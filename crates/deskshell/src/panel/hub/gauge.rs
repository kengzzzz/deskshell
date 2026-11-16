//! The Hub's stats gauges, rasterized here because the shell builds iced without canvas.

use deskshell_ui::Theme;
use iced::Color;
use iced::widget::image::Handle;

pub const SIZE: f32 = 44.0;
const RADIUS: f32 = 18.0;
const STROKE: f32 = 4.0;
pub const SCALE: u32 = 2;
pub const PIXELS: u32 = SIZE as u32 * SCALE;

pub fn rasterize(fraction: Option<f32>, track: Color, arc: Color) -> Vec<u8> {
    let arc = if fraction.is_some() { arc } else { Color::TRANSPARENT };
    let sweep = std::f32::consts::TAU * fraction.unwrap_or(0.0).clamp(0.01, 1.0);
    let half = STROKE / 2.0;
    let pixel = 1.0 / SCALE as f32;
    let center = SIZE / 2.0;
    let start = (center, center - RADIUS);
    let end = (
        center + RADIUS * sweep.sin(),
        center - RADIUS * sweep.cos(),
    );
    let coverage = |distance: f32| ((half - distance) / pixel + 0.5).clamp(0.0, 1.0);
    let mut out = Vec::with_capacity((PIXELS * PIXELS * 4) as usize);
    for row in 0..PIXELS {
        for column in 0..PIXELS {
            let x = (column as f32 + 0.5) * pixel;
            let y = (row as f32 + 0.5) * pixel;
            let (dx, dy) = (x - center, y - center);
            let ring = ((dx * dx + dy * dy).sqrt() - RADIUS).abs();
            // Clockwise from 12 o'clock, in 0 to 2π.
            let angle = dx
                .atan2(-dy)
                .rem_euclid(std::f32::consts::TAU);
            let arc_distance = if angle <= sweep {
                ring
            } else {
                let to = |(px, py): (f32, f32)| ((x - px).powi(2) + (y - py).powi(2)).sqrt();
                to(start).min(to(end))
            };
            let track_alpha = coverage(ring) * track.a;
            let arc_alpha = coverage(arc_distance) * arc.a;
            let alpha = arc_alpha + track_alpha * (1.0 - arc_alpha);
            let mix = |a: f32, t: f32| {
                if alpha <= 0.0 {
                    0.0
                } else {
                    (a * arc_alpha + t * track_alpha * (1.0 - arc_alpha)) / alpha
                }
            };
            let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            out.extend_from_slice(&[
                byte(mix(arc.r, track.r)),
                byte(mix(arc.g, track.g)),
                byte(mix(arc.b, track.b)),
                byte(alpha),
            ]);
        }
    }
    out
}

#[derive(Debug, Clone)]
struct Ring {
    key: (Option<u16>, [u8; 4], [u8; 4]),
    handle: Handle,
}

#[derive(Debug, Clone, Default)]
pub struct Gauges {
    rings: [Option<Ring>; 3],
}

impl Gauges {
    pub fn update(&mut self, theme: &Theme, values: [Option<f32>; 3]) {
        let track = theme.colors.container_highest;
        let arc = theme.colors.tint;
        for (slot, value) in self.rings.iter_mut().zip(values) {
            let key = (
                value.map(|value| (value.clamp(0.01, 1.0) * 1000.0).round() as u16),
                track.into_rgba8(),
                arc.into_rgba8(),
            );
            if slot
                .as_ref()
                .is_some_and(|ring| ring.key == key)
            {
                continue;
            }
            let fraction = key
                .0
                .map(|sweep| f32::from(sweep) / 1000.0);
            let pixels = rasterize(fraction, track, arc);
            *slot = Some(Ring { key, handle: Handle::from_rgba(PIXELS, PIXELS, pixels) });
        }
    }

    pub fn handle(&self, index: usize) -> Option<&Handle> {
        self.rings
            .get(index)?
            .as_ref()
            .map(|ring| &ring.handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: Color = Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
    const ARC: Color = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };

    fn at(pixels: &[u8], x: f32, y: f32) -> [u8; 4] {
        let column = (x * SCALE as f32) as usize;
        let row = (y * SCALE as f32) as usize;
        let index = (row * PIXELS as usize + column) * 4;
        pixels[index..index + 4]
            .try_into()
            .unwrap()
    }

    #[test]
    fn a_quarter_runs_from_twelve_to_three() {
        let pixels = rasterize(Some(0.25), TRACK, ARC);
        assert_eq!(pixels.len(), (PIXELS * PIXELS * 4) as usize);
        let c = SIZE / 2.0;
        assert_eq!(
            at(&pixels, c + 3.0, c - RADIUS),
            [255, 0, 0, 255],
            "arc at 12:30"
        );
        assert_eq!(
            at(&pixels, c + RADIUS, c + 3.0),
            [0, 0, 255, 255],
            "track past 3"
        );
        assert_eq!(
            at(&pixels, c, c + RADIUS),
            [0, 0, 255, 255],
            "track at 6"
        );
        assert_eq!(
            at(&pixels, c - RADIUS, c),
            [0, 0, 255, 255],
            "track at 9"
        );
        assert_eq!(at(&pixels, c, c)[3], 0, "the middle stays clear");
        assert_eq!(
            at(&pixels, 1.0, 1.0)[3],
            0,
            "the corners stay clear"
        );
    }

    #[test]
    fn caps_are_round() {
        let pixels = rasterize(Some(0.25), TRACK, ARC);
        let c = SIZE / 2.0;
        assert_eq!(at(&pixels, c - 1.0, c - RADIUS), [255, 0, 0, 255]);
        assert_eq!(at(&pixels, c + RADIUS, c + 1.0), [255, 0, 0, 255]);
    }

    #[test]
    fn empty_and_full() {
        let empty = rasterize(Some(0.0), TRACK, ARC);
        let c = SIZE / 2.0;
        assert_eq!(at(&empty, c, c + RADIUS), [0, 0, 255, 255]);
        assert_eq!(
            at(&empty, c, c - RADIUS)[0],
            255,
            "1 % still shows a dot"
        );
        let full = rasterize(Some(1.0), TRACK, ARC);
        for (x, y) in [(c, c + RADIUS), (c - RADIUS, c), (c + RADIUS, c)] {
            assert_eq!(at(&full, x, y), [255, 0, 0, 255]);
        }
    }

    #[test]
    fn no_reading_draws_the_track_alone() {
        let pixels = rasterize(None, TRACK, ARC);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[0] == 0)
        );
        let c = SIZE / 2.0;
        assert_eq!(at(&pixels, c, c - RADIUS), [0, 0, 255, 255]);
    }

    #[test]
    fn rings_are_drawn_again_only_on_change() {
        let theme = Theme::default();
        let mut gauges = Gauges::default();
        gauges.update(&theme, [Some(0.5), Some(0.25), Some(0.0)]);
        let first = gauges.handle(0).unwrap().id();
        gauges.update(&theme, [Some(0.5), Some(0.3), Some(0.0)]);
        assert_eq!(gauges.handle(0).unwrap().id(), first);
        gauges.update(&theme, [Some(0.6), Some(0.3), Some(0.0)]);
        assert_ne!(gauges.handle(0).unwrap().id(), first);
        assert!(gauges.handle(3).is_none());
        let zero = gauges.handle(2).unwrap().id();
        gauges.update(&theme, [Some(0.6), Some(0.3), None]);
        assert_ne!(
            gauges.handle(2).unwrap().id(),
            zero,
            "no reading is not 0"
        );
    }
}
