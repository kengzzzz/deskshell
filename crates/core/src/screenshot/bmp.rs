//! A small BMP encoder for clipboard screenshots: BGRA pixels in, opaque bottom-up rows out.

use super::pixels::{BYTES, Frame};

pub const HEADER: usize = 54;
const PIXELS_PER_METER: u32 = 2835;

pub fn header(width: u32, height: u32) -> Option<[u8; HEADER]> {
    let image = width
        .checked_mul(height)?
        .checked_mul(BYTES as u32)?;
    let file = image.checked_add(HEADER as u32)?;
    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    let mut out = [0u8; HEADER];
    out[0..2].copy_from_slice(b"BM");
    out[2..6].copy_from_slice(&file.to_le_bytes());
    out[10..14].copy_from_slice(&(HEADER as u32).to_le_bytes());
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&width.to_le_bytes());
    out[22..26].copy_from_slice(&height.to_le_bytes());
    out[26..28].copy_from_slice(&1u16.to_le_bytes());
    out[28..30].copy_from_slice(&32u16.to_le_bytes());
    out[34..38].copy_from_slice(&image.to_le_bytes());
    out[38..42].copy_from_slice(&PIXELS_PER_METER.to_le_bytes());
    out[42..46].copy_from_slice(&PIXELS_PER_METER.to_le_bytes());
    Some(out)
}

pub fn encode(frame: Frame<'_>, out: &mut Vec<u8>) -> Option<()> {
    let header = header(frame.width(), frame.height())?;
    out.clear();
    out.reserve(HEADER + frame.width() as usize * frame.height() as usize * BYTES);
    out.extend_from_slice(&header);
    for y in (0..frame.height()).rev() {
        out.extend(
            frame
                .row(y)
                .as_chunks::<BYTES>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0xff]),
        );
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_of_a_two_by_three_image() {
        let expected: [u8; HEADER] = [
            b'B', b'M', 78, 0, 0, 0, 0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0,
            1, 0, 32, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0x13, 0x0b, 0, 0, 0x13, 0x0b, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];
        assert_eq!(header(2, 3), Some(expected));
    }

    #[test]
    fn sizes_past_32_bits_are_refused() {
        assert!(header(65536, 16384).is_none());
        assert!(header(3440, 1440).is_some());
    }

    #[test]
    fn rows_go_bottom_up_and_opaque() {
        let pixels = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let frame = Frame::new(2, 2, 8, &pixels).unwrap();
        let mut out = Vec::new();
        encode(frame, &mut out).unwrap();
        assert_eq!(out.len(), HEADER + 16);
        assert_eq!(
            &out[HEADER..],
            &[7, 8, 9, 0xff, 10, 11, 12, 0xff, 1, 2, 3, 0xff, 4, 5, 6, 0xff]
        );
    }
}
