//! Pure parts of the screenshot selector: delay keys, drags and their scaling to frame pixels.

pub mod bmp;
pub mod pixels;

pub const MIN_PIXELS: i64 = 4;
pub const DELAYS: [u32; 3] = [3, 5, 10];
pub const LABEL_GAP: f64 = 8.0;

pub fn delay_for_key(key: char) -> Option<u32> {
    match key {
        '3' => Some(3),
        '5' => Some(5),
        '0' => Some(10),
        _ => None,
    }
}

pub fn pixel_scale(frame_width: u32, surface_width: f64) -> f64 {
    if frame_width > 0 && surface_width > 0.0 {
        f64::from(frame_width) / surface_width
    } else {
        1.0
    }
}

/// Halves round up like JS `Math.round`, not away from zero like `f64::round`.
fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn visible(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Drag {
    pub start: (f64, f64),
    pub end: (f64, f64),
}

impl Drag {
    pub fn at(x: f64, y: f64) -> Self {
        Self { start: (x, y), end: (x, y) }
    }

    pub fn move_to(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.end = (x.min(width).max(0.0), y.min(height).max(0.0));
    }

    pub fn rect(&self) -> Rect {
        Rect {
            x: self.start.0.min(self.end.0),
            y: self.start.1.min(self.end.1),
            width: (self.end.0 - self.start.0).abs(),
            height: (self.end.1 - self.start.1).abs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub width: i64,
    pub height: i64,
    pub x: i64,
    pub y: i64,
}

impl Region {
    pub fn of(rect: Rect, scale: f64) -> Self {
        Self {
            width: js_round(rect.width * scale),
            height: js_round(rect.height * scale),
            x: js_round(rect.x * scale),
            y: js_round(rect.y * scale),
        }
    }

    pub fn big_enough(&self) -> bool {
        self.width >= MIN_PIXELS && self.height >= MIN_PIXELS
    }
}

pub fn label_position(rect: Rect, label: (f64, f64), surface_width: f64) -> (f64, f64) {
    let (width, height) = label;
    let above = rect.y >= height + LABEL_GAP;
    let x = rect
        .x
        .min(surface_width - width - LABEL_GAP);
    let y = if above { rect.y - height - LABEL_GAP } else { rect.y + LABEL_GAP };
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(from: (f64, f64), to: (f64, f64), surface: (f64, f64)) -> Drag {
        let mut drag = Drag::at(from.0, from.1);
        drag.move_to(to.0, to.1, surface.0, surface.1);
        drag
    }

    #[test]
    fn delay_keys() {
        assert_eq!(delay_for_key('3'), Some(3));
        assert_eq!(delay_for_key('5'), Some(5));
        assert_eq!(delay_for_key('0'), Some(10));
        assert_eq!(delay_for_key('1'), None);
        assert_eq!(delay_for_key('a'), None);
        assert_eq!(DELAYS, [3, 5, 10]);
    }

    #[test]
    fn the_scale_is_frame_pixels_per_logical_pixel() {
        assert_eq!(pixel_scale(1280, 1280.0), 1.0);
        assert_eq!(pixel_scale(2560, 1280.0), 2.0);
        assert_eq!(pixel_scale(3840, 2560.0), 1.5);
        assert_eq!(pixel_scale(2400, 1920.0), 1.25);
        assert_eq!(pixel_scale(0, 1280.0), 1.0, "no frame yet");
        assert_eq!(pixel_scale(1280, 0.0), 1.0, "no size yet");
    }

    #[test]
    fn a_drag_in_any_direction() {
        let surface = (1280.0, 720.0);
        let forward = drag((100.0, 50.0), (300.5, 250.25), surface);
        assert_eq!(
            forward.rect(),
            Rect { x: 100.0, y: 50.0, width: 200.5, height: 200.25 }
        );
        let backward = drag((300.0, 250.0), (100.0, 50.0), surface);
        assert_eq!(
            backward.rect(),
            Rect { x: 100.0, y: 50.0, width: 200.0, height: 200.0 }
        );
        let outside = drag((1200.0, 700.0), (1500.0, -40.0), surface);
        assert_eq!(
            outside.rect(),
            Rect { x: 1200.0, y: 0.0, width: 80.0, height: 700.0 },
            "the end is clamped to the surface"
        );
        assert!(!Drag::at(5.0, 5.0).rect().visible());
    }

    #[test]
    fn regions_at_scale_one_and_two() {
        let rect = drag((100.0, 50.0), (300.5, 250.25), (1280.0, 720.0)).rect();
        let one = Region::of(rect, 1.0);
        assert_eq!(
            one,
            Region { width: 201, height: 200, x: 100, y: 50 },
            "200.5 rounds up"
        );
        assert_eq!(
            Region::of(rect, 2.0),
            Region { width: 401, height: 401, x: 200, y: 100 }
        );
    }

    #[test]
    fn fractional_scales() {
        // A 2560 px wide output at scale 1.5 is 1706.67 logical pixels wide. Hyprland
        // rounds the layer to 1707, so pixelScale is a little under 1.5.
        let scale = pixel_scale(2560, 1707.0);
        let rect = drag((10.25, 20.5), (110.75, 70.5), (1707.0, 960.0)).rect();
        let region = Region::of(rect, scale);
        assert_eq!(
            region,
            Region { width: 151, height: 75, x: 15, y: 31 }
        );

        let rect = drag((0.4, 0.4), (1000.4, 500.4), (1536.0, 864.0)).rect();
        assert_eq!(
            Region::of(rect, 1.25),
            Region { width: 1250, height: 625, x: 1, y: 1 }
        );
        assert_eq!(
            Region::of(rect, 1.5),
            Region { width: 1500, height: 750, x: 1, y: 1 }
        );
    }

    #[test]
    fn the_four_pixel_minimum_counts_frame_pixels() {
        let small = |logical: f64, scale: f64| {
            let rect = drag(
                (10.0, 10.0),
                (10.0 + logical, 10.0 + logical),
                (1280.0, 720.0),
            )
            .rect();
            Region::of(rect, scale).big_enough()
        };
        assert!(!small(3.0, 1.0));
        assert!(small(4.0, 1.0));
        assert!(!small(3.4, 1.0), "3.4 rounds to 3");
        assert!(small(3.5, 1.0), "3.5 rounds to 4");
        assert!(
            small(2.0, 2.0),
            "2 logical pixels are 4 frame pixels at scale 2"
        );
        assert!(!small(1.5, 2.0));
        assert!(small(3.0, 1.25), "3.75 rounds to 4");
        assert!(!small(2.5, 1.25), "3.125 rounds to 3");
        let thin = drag((10.0, 10.0), (200.0, 13.0), (1280.0, 720.0)).rect();
        assert!(!Region::of(thin, 1.0).big_enough(), "each way");
    }

    #[test]
    fn the_size_label_goes_above_when_it_fits() {
        let label = (120.0, 32.0);
        let rect = Rect { x: 100.0, y: 200.0, width: 50.0, height: 50.0 };
        assert_eq!(
            label_position(rect, label, 1280.0),
            (100.0, 160.0)
        );
        let top = Rect { y: 39.0, ..rect };
        assert_eq!(
            label_position(top, label, 1280.0),
            (100.0, 47.0),
            "inside the top"
        );
        let fits = Rect { y: 40.0, ..rect };
        assert_eq!(label_position(fits, label, 1280.0), (100.0, 0.0));
        let right = Rect { x: 1250.0, ..rect };
        assert_eq!(
            label_position(right, label, 1280.0),
            (1152.0, 160.0)
        );
    }
}
