//! Untrusted `IconPixmap` data: `a(iiay)`, ARGB32 in network byte order, every size checked.

use std::sync::Arc;

use crate::model::Pixmap;

/// Larger pixmaps are skipped. Tray icons are a few dozen pixels.
pub const MAX_SIDE: u32 = 1024;

pub(crate) type RawPixmap<'a> = (i32, i32, &'a [u8]);

pub(crate) fn convert(raw: &[RawPixmap<'_>]) -> Arc<[Pixmap]> {
    raw.iter()
        .filter_map(|&(width, height, data)| convert_one(width, height, data))
        .collect()
}

// `None` unless both sides are in `1..=MAX_SIDE` and `data` holds `width * height * 4` bytes.
pub(crate) fn convert_one(width: i32, height: i32, data: &[u8]) -> Option<Pixmap> {
    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let argb = data.get(..len)?;
    let rgba = argb
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, r, g, b]| [r, g, b, a])
        .collect();
    Some(Pixmap { width, height, rgba })
}

/// The smallest pixmap that covers `width` by `height`, else the largest. Of two the same size,
/// the earlier one wins when it covers and the later one when it does not.
pub fn closest(pixmaps: &[Pixmap], width: u32, height: u32) -> Option<&Pixmap> {
    let mut best: Option<&Pixmap> = None;
    for pixmap in pixmaps {
        let Some(current) = best else {
            best = Some(pixmap);
            continue;
        };
        let current_covers = current.width >= width && current.height >= height;
        let new_covers = pixmap.width >= width && pixmap.height >= height;
        let new_smaller = pixmap.width < current.width || pixmap.height < current.height;
        if (current_covers && new_covers && new_smaller) || (!current_covers && !new_smaller) {
            best = Some(pixmap);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: i32, height: i32) -> Vec<u8> {
        let n = usize::try_from(width * height).unwrap();
        [0x80, 0x10, 0x20, 0x30].repeat(n)
    }

    fn sized(width: u32, height: u32) -> Pixmap {
        Pixmap { width, height, rgba: Arc::from(Vec::new()) }
    }

    #[test]
    fn argb_in_network_order_becomes_rgba() {
        let data = [0xff, 0x11, 0x22, 0x33, 0x00, 0xaa, 0xbb, 0xcc];
        let pixmap = convert_one(2, 1, &data).unwrap();
        assert_eq!((pixmap.width, pixmap.height), (2, 1));
        assert_eq!(
            &*pixmap.rgba,
            &[0x11, 0x22, 0x33, 0xff, 0xaa, 0xbb, 0xcc, 0x00]
        );
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let mut data = solid(2, 2);
        data.extend_from_slice(&[1, 2, 3]);
        let pixmap = convert_one(2, 2, &data).unwrap();
        assert_eq!(pixmap.rgba.len(), 16);
    }

    #[test]
    fn bad_pixmaps_are_skipped() {
        let good = solid(2, 2);
        assert!(convert_one(0, 2, &good).is_none());
        assert!(convert_one(2, 0, &good).is_none());
        assert!(convert_one(-2, 2, &good).is_none());
        assert!(convert_one(2, -2, &good).is_none());
        assert!(convert_one(3, 2, &good).is_none(), "too short");
        assert!(convert_one(2, 2, &[]).is_none());
        assert!(convert_one(i32::MAX, i32::MAX, &good).is_none());
        assert!(convert_one(i32::MIN, i32::MIN, &good).is_none());
        let max = i32::try_from(MAX_SIDE).unwrap();
        assert!(convert_one(max + 1, 1, &solid(max + 1, 1)).is_none());
        assert!(convert_one(max, 1, &solid(max, 1)).is_some());

        let short = solid(1, 1);
        let raw: [RawPixmap<'_>; 4] =
            [(2, 2, &good), (4, 4, &short), (1, 1, &short), (-1, 5, &good)];
        let pixmaps = convert(&raw);
        let sizes: Vec<_> = pixmaps
            .iter()
            .map(|p| (p.width, p.height))
            .collect();
        assert_eq!(sizes, [(2, 2), (1, 1)]);
    }

    #[test]
    fn closest_prefers_the_smallest_that_covers() {
        let pixmaps = [sized(16, 16), sized(64, 64), sized(32, 32), sized(128, 128)];
        let pick = |w, h| {
            let p = closest(&pixmaps, w, h).unwrap();
            (p.width, p.height)
        };
        assert_eq!(pick(20, 20), (32, 32));
        assert_eq!(pick(32, 32), (32, 32));
        assert_eq!(pick(16, 16), (16, 16));
        assert_eq!(pick(1, 1), (16, 16));
        assert_eq!(pick(100, 100), (128, 128));
        assert_eq!(
            pick(500, 500),
            (128, 128),
            "none covers, so the largest"
        );
        assert!(closest(&[], 16, 16).is_none());
    }

    #[test]
    fn closest_on_mixed_shapes() {
        let pixmaps = [sized(10, 40), sized(20, 40)];
        let p = closest(&pixmaps, 64, 64).unwrap();
        assert_eq!((p.width, p.height), (20, 40));
        let same = [
            Pixmap { width: 8, height: 8, rgba: Arc::from(vec![1]) },
            Pixmap { width: 8, height: 8, rgba: Arc::from(vec![2]) },
        ];
        assert_eq!(&*closest(&same, 64, 64).unwrap().rgba, &[2]);
    }
}
