//! Screenshot pixels: BGRA frames (DRM ARGB8888 in memory), crops, the `full` composition.

use super::Region;

/// The largest side of a composed shot, so a broken layout cannot ask for gigabytes.
pub const MAX_SIDE: u32 = 32768;
pub const BYTES: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    width: u32,
    height: u32,
    stride: usize,
    pixels: &'a [u8],
}

impl<'a> Frame<'a> {
    pub fn new(width: u32, height: u32, stride: usize, pixels: &'a [u8]) -> Option<Self> {
        let row = (width as usize).checked_mul(BYTES)?;
        if stride < row {
            return None;
        }
        let needed = match height {
            0 => 0,
            height => stride
                .checked_mul(height as usize - 1)?
                .checked_add(row)?,
        };
        (pixels.len() >= needed).then_some(Self { width, height, stride, pixels })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn row(&self, y: u32) -> &'a [u8] {
        let start = y as usize * self.stride;
        &self.pixels[start..start + self.width as usize * BYTES]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Shot {
    pub fn frame(&self) -> Frame<'_> {
        Frame {
            width: self.width,
            height: self.height,
            stride: self.width as usize * BYTES,
            pixels: &self.pixels,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Clamped to the frame, since rounding can put the region's far edge one pixel past it.
pub fn crop_rect(region: Region, width: u32, height: u32) -> Option<Rect> {
    let clamp = |value: i64, max: u32| value.clamp(0, i64::from(max)) as u32;
    let x = clamp(region.x, width);
    let y = clamp(region.y, height);
    let right = clamp(region.x.saturating_add(region.width), width);
    let bottom = clamp(region.y.saturating_add(region.height), height);
    (right > x && bottom > y).then_some(Rect { x, y, width: right - x, height: bottom - y })
}

pub fn crop(frame: Frame<'_>, rect: Rect) -> Shot {
    let row = rect.width as usize * BYTES;
    let left = rect.x as usize * BYTES;
    let mut pixels = Vec::with_capacity(row * rect.height as usize);
    for y in rect.y..rect.y + rect.height {
        pixels.extend_from_slice(&frame.row(y)[left..left + row]);
    }
    Shot { width: rect.width, height: rect.height, pixels }
}

pub fn whole(frame: Frame<'_>) -> Shot {
    crop(
        frame,
        Rect { x: 0, y: 0, width: frame.width, height: frame.height },
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub boxes: Vec<Rect>,
}

/// Halves round up like JS `Math.round`, not away from zero like `f64::round`.
fn round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

pub fn layout(placements: &[Placement]) -> Option<Layout> {
    let scale_of = |placement: &Placement| {
        if placement.scale > 0.0 && placement.scale.is_finite() { placement.scale } else { 1.0 }
    };
    let canvas_scale = placements
        .iter()
        .map(scale_of)
        .fold(None, |max: Option<f64>, scale| {
            Some(max.map_or(scale, |max| max.max(scale)))
        })?;
    let logical = |placement: &Placement| {
        let scale = scale_of(placement);
        (
            placement.x,
            placement.y,
            f64::from(placement.width) / scale,
            f64::from(placement.height) / scale,
        )
    };
    let (mut left, mut top) = (f64::INFINITY, f64::INFINITY);
    let (mut right, mut bottom) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for placement in placements {
        let (x, y, width, height) = logical(placement);
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + width);
        bottom = bottom.max(y + height);
    }
    let side = |value: f64| {
        let pixels = round(value * canvas_scale);
        (1..=i64::from(MAX_SIDE))
            .contains(&pixels)
            .then_some(pixels as u32)
    };
    let width = side(right - left)?;
    let height = side(bottom - top)?;
    let mut boxes: Vec<Rect> = placements
        .iter()
        .map(|placement| {
            let (x, y, w, h) = logical(placement);
            let x = round((x - left) * canvas_scale).clamp(0, i64::from(width)) as u32;
            let y = round((y - top) * canvas_scale).clamp(0, i64::from(height)) as u32;
            let w = round(w * canvas_scale).clamp(0, i64::from(width - x)) as u32;
            let h = round(h * canvas_scale).clamp(0, i64::from(height - y)) as u32;
            Rect { x, y, width: w, height: h }
        })
        .collect();
    close_seams(&mut boxes, placements);
    let fits = |rect: &Rect| {
        u64::from(rect.x) + u64::from(rect.width) <= u64::from(width)
            && u64::from(rect.y) + u64::from(rect.height) <= u64::from(height)
    };
    boxes
        .iter()
        .all(fits)
        .then_some(Layout { width, height, scale: canvas_scale, boxes })
}

/// A fractional scale leaves a sub-pixel gap between neighbors, a black seam once rounded.
/// The box that is scaled anyway takes that pixel, so an output drawn 1:1 stays sharp.
fn close_seams(boxes: &mut [Rect], placements: &[Placement]) {
    let original = boxes.to_vec();
    let native = |index: usize| {
        original[index].width == placements[index].width
            && original[index].height == placements[index].height
    };
    let overlaps = |a: u32, a_len: u32, b: u32, b_len: u32| a < b + b_len && b < a + a_len;
    let count = original.len();
    let (mut grow_right, mut grow_down) = (vec![false; count], vec![false; count]);
    let (mut grow_left, mut grow_up) = (vec![false; count], vec![false; count]);
    for (first, rect) in original.iter().enumerate() {
        for (second, other) in original.iter().enumerate() {
            let take_second = native(first) && !native(second);
            if other.x == rect.x + rect.width + 1
                && overlaps(rect.y, rect.height, other.y, other.height)
            {
                if take_second {
                    grow_left[second] = true;
                } else {
                    grow_right[first] = true;
                }
            }
            if other.y == rect.y + rect.height + 1
                && overlaps(rect.x, rect.width, other.x, other.width)
            {
                if take_second {
                    grow_up[second] = true;
                } else {
                    grow_down[first] = true;
                }
            }
        }
    }
    for (index, rect) in boxes.iter_mut().enumerate() {
        let left = original[index].x - u32::from(grow_left[index]);
        let top = original[index].y - u32::from(grow_up[index]);
        let right = original[index].x + original[index].width + u32::from(grow_right[index]);
        let bottom = original[index].y + original[index].height + u32::from(grow_down[index]);
        *rect = Rect { x: left, y: top, width: right - left, height: bottom - top };
    }
}

pub fn compose(layout: &Layout, frames: &[Frame<'_>]) -> Shot {
    let stride = layout.width as usize * BYTES;
    let mut pixels = vec![0; stride * layout.height as usize];
    for pixel in pixels
        .as_chunks_mut::<BYTES>()
        .0
        .iter_mut()
    {
        pixel[3] = 0xff;
    }
    let mut columns = Vec::new();
    for (target, frame) in layout.boxes.iter().zip(frames) {
        let outside = u64::from(target.x) + u64::from(target.width) > u64::from(layout.width)
            || u64::from(target.y) + u64::from(target.height) > u64::from(layout.height);
        if outside
            || target.width == 0
            || target.height == 0
            || frame.width == 0
            || frame.height == 0
        {
            continue;
        }
        if target.width == frame.width && target.height == frame.height {
            let left = target.x as usize * BYTES;
            let row = target.width as usize * BYTES;
            for y in 0..target.height {
                let start = (target.y + y) as usize * stride + left;
                pixels[start..start + row].copy_from_slice(frame.row(y));
            }
            continue;
        }
        taps(target.width, frame.width, &mut columns);
        for y in 0..target.height {
            let (top, bottom, fy) = tap(y, target.height, frame.height);
            let (upper, lower) = (frame.row(top), frame.row(bottom));
            let start = (target.y + y) as usize * stride + target.x as usize * BYTES;
            let out = &mut pixels[start..start + target.width as usize * BYTES];
            for (pixel, &(left, right, fx)) in out
                .as_chunks_mut::<BYTES>()
                .0
                .iter_mut()
                .zip(&columns)
            {
                let (left, right) = (left as usize * BYTES, right as usize * BYTES);
                for channel in 0..3 {
                    let mix = |row: &[u8]| {
                        u32::from(row[left + channel]) * (256 - fx)
                            + u32::from(row[right + channel]) * fx
                    };
                    let value = (mix(upper) * (256 - fy) + mix(lower) * fy + (1 << 15)) >> 16;
                    pixel[channel] = value as u8;
                }
            }
        }
    }
    Shot { width: layout.width, height: layout.height, pixels }
}

fn taps(target: u32, source: u32, out: &mut Vec<(u32, u32, u32)>) {
    out.clear();
    out.reserve(target as usize);
    for index in 0..target {
        out.push(tap(index, target, source));
    }
}

fn tap(index: u32, target: u32, source: u32) -> (u32, u32, u32) {
    let position =
        ((f64::from(index) + 0.5) * f64::from(source) / f64::from(target) - 0.5).max(0.0);
    let first = (position.floor() as u32).min(source - 1);
    let second = (first + 1).min(source - 1);
    let weight = ((position - f64::from(first)) * 256.0).round() as u32;
    (first, second, weight.min(256))
}

pub fn to_rgba(frame: Frame<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.width as usize * frame.height as usize * BYTES);
    for y in 0..frame.height {
        out.extend(
            frame
                .row(y)
                .as_chunks::<BYTES>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 0xff]),
        );
    }
    out
}

pub fn to_rgb(frame: Frame<'_>, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(frame.width as usize * frame.height as usize * 3);
    for y in 0..frame.height {
        out.extend(
            frame
                .row(y)
                .as_chunks::<BYTES>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, padding: usize, tag: u8) -> (usize, Vec<u8>) {
        let stride = width as usize * BYTES + padding;
        let mut pixels = vec![0xee; stride * height as usize];
        for y in 0..height {
            for x in 0..width {
                let at = y as usize * stride + x as usize * BYTES;
                pixels[at..at + 4].copy_from_slice(&[x as u8, y as u8, tag, 0]);
            }
        }
        (stride, pixels)
    }

    fn pixel(shot: &Shot, x: u32, y: u32) -> [u8; 4] {
        let at = (y * shot.width + x) as usize * BYTES;
        shot.pixels[at..at + 4]
            .try_into()
            .unwrap()
    }

    #[test]
    fn frames_check_their_stride_and_length() {
        assert!(Frame::new(2, 2, 8, &[0; 16]).is_some());
        assert!(
            Frame::new(2, 2, 12, &[0; 20]).is_some(),
            "the last row needs no padding"
        );
        assert!(Frame::new(2, 2, 12, &[0; 19]).is_none());
        assert!(
            Frame::new(2, 2, 7, &[0; 64]).is_none(),
            "stride under a row"
        );
        assert!(Frame::new(0, 0, 0, &[]).is_some());
    }

    #[test]
    fn crop_rects_are_clamped_to_the_frame() {
        let region = |width, height, x, y| Region { width, height, x, y };
        assert_eq!(
            crop_rect(region(201, 200, 100, 50), 1280, 720),
            Some(Rect { x: 100, y: 50, width: 201, height: 200 })
        );
        assert_eq!(
            crop_rect(region(1281, 721, 0, 0), 1280, 720),
            Some(Rect { x: 0, y: 0, width: 1280, height: 720 }),
            "rounding past the edge"
        );
        assert_eq!(
            crop_rect(region(10, 10, -3, 715), 1280, 720),
            Some(Rect { x: 0, y: 715, width: 7, height: 5 })
        );
        assert_eq!(
            crop_rect(region(10, 10, 1280, 0), 1280, 720),
            None
        );
        assert_eq!(crop_rect(region(0, 10, 5, 5), 1280, 720), None);
    }

    #[test]
    fn a_crop_skips_the_row_padding() {
        let (stride, pixels) = frame(6, 4, 8, 7);
        let frame = Frame::new(6, 4, stride, &pixels).unwrap();
        let shot = crop(frame, Rect { x: 2, y: 1, width: 3, height: 2 });
        assert_eq!((shot.width, shot.height), (3, 2));
        assert_eq!(shot.pixels.len(), 3 * 2 * 4);
        assert_eq!(pixel(&shot, 0, 0), [2, 1, 7, 0]);
        assert_eq!(pixel(&shot, 2, 1), [4, 2, 7, 0]);
        let all = whole(frame);
        assert_eq!((all.width, all.height), (6, 4));
        assert_eq!(pixel(&all, 5, 3), [5, 3, 7, 0]);
    }

    fn place(x: f64, y: f64, scale: f64, width: u32, height: u32) -> Placement {
        Placement { x, y, scale, width, height }
    }

    #[test]
    fn one_output_is_its_own_canvas() {
        let layout = layout(&[place(0.0, 0.0, 1.0, 3440, 1440)]).unwrap();
        assert_eq!((layout.width, layout.height), (3440, 1440));
        assert_eq!(
            layout.boxes,
            [Rect { x: 0, y: 0, width: 3440, height: 1440 }]
        );
    }

    #[test]
    fn mixed_scales_compose_at_the_highest() {
        let layout =
            layout(&[place(0.0, 0.0, 1.0, 1280, 720), place(1280.0, 0.0, 1.5, 2560, 1440)])
                .unwrap();
        assert_eq!(layout.scale, 1.5);
        assert_eq!((layout.width, layout.height), (4480, 1440));
        assert_eq!(
            layout.boxes,
            [
                Rect { x: 0, y: 0, width: 1920, height: 1080 },
                Rect { x: 1920, y: 0, width: 2560, height: 1440 },
            ]
        );
    }

    #[test]
    fn a_fractional_scale_leaves_no_seam() {
        // 2560x1440 at scale 1.5 is 1706.67 logical pixels wide, and Hyprland puts the
        // neighbor at 1707. At scale 1.5 the neighbor starts at 2561.
        let layout =
            layout(&[place(0.0, 0.0, 1.5, 2560, 1440), place(1707.0, 0.0, 1.0, 1280, 720)])
                .unwrap();
        assert_eq!(
            layout.boxes,
            [
                Rect { x: 0, y: 0, width: 2560, height: 1440 },
                Rect { x: 2560, y: 0, width: 1921, height: 1080 },
            ],
            "the scaled output takes the pixel"
        );
        let stacked =
            layout_of(&[place(0.0, 0.0, 1.5, 2560, 1440), place(0.0, 961.0, 1.0, 1280, 720)]);
        assert_eq!(
            stacked.boxes[0].height, 1440,
            "two rows short is a real gap"
        );
        let below =
            layout_of(&[place(0.0, 0.0, 1.5, 2560, 1441), place(0.0, 961.0, 1.0, 1280, 720)]);
        assert_eq!(
            below.boxes[0].height, 1441,
            "one row short, the native one stays"
        );
        assert_eq!(below.boxes[1].y, 1441);
        assert_eq!(below.boxes[1].height, 1081);
    }

    #[test]
    fn one_seam_shared_by_three_neighbors_widens_once() {
        let placements = [
            place(0.0, 0.0, 1.0, 4, 3),
            place(5.0, 0.0, 1.0, 1, 1),
            place(5.0, 1.0, 1.0, 1, 1),
            place(5.0, 2.0, 1.0, 1, 1),
        ];
        let layout = layout_of(&placements);
        assert_eq!((layout.width, layout.height), (6, 3));
        assert_eq!(
            layout.boxes[0],
            Rect { x: 0, y: 0, width: 5, height: 3 }
        );
        let pixels = [vec![9u8; 4 * 3 * 4], vec![1; 4], vec![2; 4], vec![3; 4]];
        let frames: Vec<Frame<'_>> = placements
            .iter()
            .zip(&pixels)
            .map(|(place, pixels)| {
                Frame::new(
                    place.width,
                    place.height,
                    place.width as usize * 4,
                    pixels,
                )
                .unwrap()
            })
            .collect();
        let shot = compose(&layout, &frames);
        assert_eq!(shot.pixels.len(), 6 * 3 * 4);
    }

    #[test]
    fn compose_skips_a_box_outside_its_canvas() {
        let pixels = [0u8; 16];
        let frame = Frame::new(2, 2, 8, &pixels).unwrap();
        let layout = Layout {
            width: 2,
            height: 2,
            scale: 1.0,
            boxes: vec![Rect { x: 1, y: 0, width: 2, height: 2 }],
        };
        assert_eq!(compose(&layout, &[frame]).pixels.len(), 16);
    }

    #[test]
    fn random_small_layouts_never_panic() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |range: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % range
        };
        let scales = [1.0, 1.25, 1.5, 1.6, 2.0];
        for _ in 0..3000 {
            let count = 1 + next(5) as usize;
            let placements: Vec<Placement> = (0..count)
                .map(|_| {
                    let scale = scales[next(scales.len() as u64) as usize];
                    place(
                        next(12) as f64,
                        next(12) as f64,
                        scale,
                        1 + next(9) as u32,
                        1 + next(9) as u32,
                    )
                })
                .collect();
            let Some(layout) = layout(&placements) else {
                continue;
            };
            for rect in &layout.boxes {
                assert!(
                    rect.x + rect.width <= layout.width && rect.y + rect.height <= layout.height
                );
            }
            let pixels: Vec<Vec<u8>> = placements
                .iter()
                .map(|place| vec![7u8; place.width as usize * place.height as usize * 4])
                .collect();
            let frames: Vec<Frame<'_>> = placements
                .iter()
                .zip(&pixels)
                .map(|(place, pixels)| {
                    Frame::new(
                        place.width,
                        place.height,
                        place.width as usize * 4,
                        pixels,
                    )
                    .unwrap()
                })
                .collect();
            let shot = compose(&layout, &frames);
            assert_eq!(
                shot.pixels.len(),
                layout.width as usize * layout.height as usize * 4
            );
        }
    }

    fn layout_of(placements: &[Placement]) -> Layout {
        layout(placements).unwrap()
    }

    #[test]
    fn negative_positions_and_a_gap() {
        let layout =
            layout(&[place(-1000.0, 200.0, 1.0, 1000, 500), place(100.0, 0.0, 1.0, 800, 600)])
                .unwrap();
        assert_eq!((layout.width, layout.height), (1900, 700));
        assert_eq!(
            layout.boxes,
            [
                Rect { x: 0, y: 200, width: 1000, height: 500 },
                Rect { x: 1100, y: 0, width: 800, height: 600 },
            ]
        );
    }

    #[test]
    fn a_rotated_output_uses_its_upright_frame() {
        let layout =
            layout(&[place(0.0, 0.0, 1.0, 1280, 720), place(1280.0, 0.0, 1.0, 1080, 1920)])
                .unwrap();
        assert_eq!((layout.width, layout.height), (2360, 1920));
        assert_eq!(
            layout.boxes[1],
            Rect { x: 1280, y: 0, width: 1080, height: 1920 }
        );
    }

    #[test]
    fn broken_layouts_are_refused() {
        assert_eq!(layout(&[]), None);
        assert_eq!(layout(&[place(0.0, 0.0, 1.0, 0, 0)]), None);
        assert_eq!(
            layout(&[place(0.0, 0.0, 1.0, 100, 100), place(40000.0, 0.0, 1.0, 100, 100)]),
            None,
            "past MAX_SIDE"
        );
        let zero = layout(&[place(0.0, 0.0, 0.0, 100, 50)]).unwrap();
        assert_eq!(
            (zero.width, zero.height, zero.scale),
            (100, 50, 1.0)
        );
    }

    #[test]
    fn compose_copies_same_size_frames_and_fills_gaps() {
        let (stride_a, a) = frame(2, 2, 4, 1);
        let (stride_b, b) = frame(3, 1, 0, 2);
        let frames =
            [Frame::new(2, 2, stride_a, &a).unwrap(), Frame::new(3, 1, stride_b, &b).unwrap()];
        let layout = layout(&[place(0.0, 0.0, 1.0, 2, 2), place(4.0, 1.0, 1.0, 3, 1)]).unwrap();
        let shot = compose(&layout, &frames);
        assert_eq!((shot.width, shot.height), (7, 2));
        assert_eq!(pixel(&shot, 1, 1), [1, 1, 1, 0]);
        assert_eq!(pixel(&shot, 4, 1), [0, 0, 2, 0]);
        assert_eq!(pixel(&shot, 6, 1), [2, 0, 2, 0]);
        assert_eq!(pixel(&shot, 2, 0), [0, 0, 0, 0xff], "a gap");
        assert_eq!(pixel(&shot, 3, 1), [0, 0, 0, 0xff], "a gap");
        assert_eq!(pixel(&shot, 5, 0), [0, 0, 0, 0xff], "a gap");
    }

    #[test]
    fn compose_scales_a_lower_scale_output_up() {
        let small = [10, 0, 0, 0, 30, 0, 0, 0];
        let (stride, big) = frame(4, 2, 0, 9);
        let frames =
            [Frame::new(2, 1, 8, &small).unwrap(), Frame::new(4, 2, stride, &big).unwrap()];
        let layout = layout(&[place(0.0, 0.0, 1.0, 2, 1), place(2.0, 0.0, 2.0, 4, 2)]).unwrap();
        assert_eq!((layout.width, layout.height), (8, 2));
        let shot = compose(&layout, &frames);
        let blues: Vec<u8> = (0..4)
            .map(|x| pixel(&shot, x, 0)[0])
            .collect();
        assert_eq!(
            blues,
            [10, 15, 25, 30],
            "bilinear between 10 and 30"
        );
        assert_eq!(pixel(&shot, 0, 1)[0], 10, "the one row repeats");
        assert_eq!(
            pixel(&shot, 7, 1),
            [3, 1, 9, 0],
            "the scale-2 frame is copied"
        );
    }

    #[test]
    fn rgb_and_rgba_swap_blue_and_red() {
        let (stride, pixels) = frame(2, 2, 4, 200);
        let frame = Frame::new(2, 2, stride, &pixels).unwrap();
        let rgba = to_rgba(frame);
        assert_eq!(rgba.len(), 16);
        assert_eq!(&rgba[4..8], &[200, 0, 1, 0xff]);
        let mut rgb = vec![1, 2, 3];
        to_rgb(frame, &mut rgb);
        assert_eq!(rgb.len(), 12);
        assert_eq!(&rgb[9..12], &[200, 1, 1]);
    }
}
