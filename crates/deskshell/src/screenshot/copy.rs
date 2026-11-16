//! What a screenshot offers on the clipboard: `image/png` and a BMP under five names.

use std::io::Cursor;

use deskshell_core::screenshot::bmp;
use deskshell_core::screenshot::pixels::{Frame, Shot};
use deskshell_wl::clipboard::Offer;
use tracing::warn;

use crate::clipboard_cmd::MAX_DATA;

pub const PNG_TYPE: &str = "image/png";
pub const BMP_TYPES: [&str; 5] =
    ["image/bmp", "image/x-bmp", "image/x-MS-bmp", "image/x-ms-bmp", "image/x-win-bitmap"];

pub fn bmp_beside(frame: Frame<'_>, png_len: usize) -> Option<Vec<u8>> {
    let size = bmp::HEADER + frame.width() as usize * frame.height() as usize * 4;
    if png_len.saturating_add(size) > MAX_DATA {
        warn!(
            png_len,
            bmp = size,
            "too large for a BMP beside the PNG, the PNG goes alone"
        );
        return None;
    }
    let mut out = Vec::new();
    bmp::encode(frame, &mut out)?;
    Some(out)
}

pub const MAX_PIXELS: usize = MAX_DATA / 4;

pub fn decode_png(bytes: &[u8]) -> Result<Shot, String> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits { bytes: MAX_DATA * 2 },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|err| format!("not a PNG: {err}"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width as usize * height as usize > MAX_PIXELS {
        return Err(format!("{width}x{height} is too large"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or("the PNG is too large")?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|err| format!("cannot decode the PNG: {err}"))?;
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(String::from("a palette PNG was not expanded")),
    };
    let packed = width as usize * height as usize * 4;
    if channels == 4 && info.line_size == width as usize * 4 && buffer.len() >= packed {
        buffer.truncate(packed);
        for pixel in buffer.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        return Ok(Shot { width, height, pixels: buffer });
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in buffer
        .chunks(info.line_size)
        .take(height as usize)
    {
        for pixel in row[..width as usize * channels].chunks_exact(channels) {
            pixels.extend_from_slice(&match *pixel {
                [gray] => [gray, gray, gray, 0xff],
                [gray, alpha] => [gray, gray, gray, alpha],
                [r, g, b] => [b, g, r, 0xff],
                [r, g, b, a] => [b, g, r, a],
                _ => unreachable!("one to four channels"),
            });
        }
    }
    Ok(Shot { width, height, pixels })
}

pub fn offers(png: Vec<u8>, bmp: Option<Vec<u8>>) -> Vec<Offer> {
    let mut offers = vec![Offer { types: vec![PNG_TYPE.to_owned()], data: png }];
    if let Some(bmp) = bmp {
        offers.push(Offer {
            types: BMP_TYPES
                .iter()
                .map(|mime| (*mime).to_owned())
                .collect(),
            data: bmp,
        });
    }
    offers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_then_bmp_under_five_names() {
        let pixels = [1, 2, 3, 0, 4, 5, 6, 0];
        let frame = Frame::new(2, 1, 8, &pixels).unwrap();
        let bmp = bmp_beside(frame, 10).unwrap();
        assert_eq!(bmp.len(), bmp::HEADER + 8);
        let offers = offers(b"png".to_vec(), Some(bmp));
        assert_eq!(offers.len(), 2);
        assert_eq!(offers[0].types, ["image/png"]);
        assert_eq!(offers[0].data, b"png");
        assert_eq!(offers[1].types, BMP_TYPES);
        assert!(offers[1].data.starts_with(b"BM"));
        let header = crate::clipboard_cmd::header(&offers);
        assert!(
            crate::clipboard_cmd::parse(&{
                let mut input = header;
                input.extend_from_slice(&offers[0].data);
                input.extend_from_slice(&offers[1].data);
                input
            })
            .is_ok()
        );
    }

    fn png(color: png::ColorType, depth: png::BitDepth, width: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, 1);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if color == png::ColorType::Indexed {
            encoder.set_palette(vec![10, 20, 30, 40, 50, 60]);
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn pngs_of_every_color_type_decode_to_bgra() {
        use png::{BitDepth, ColorType};
        let cases: [(ColorType, BitDepth, &[u8], [u8; 8]); 5] = [
            (
                ColorType::Rgb,
                BitDepth::Eight,
                &[1, 2, 3, 4, 5, 6],
                [3, 2, 1, 255, 6, 5, 4, 255],
            ),
            (
                ColorType::Rgba,
                BitDepth::Eight,
                &[1, 2, 3, 9, 4, 5, 6, 8],
                [3, 2, 1, 9, 6, 5, 4, 8],
            ),
            (
                ColorType::Grayscale,
                BitDepth::Eight,
                &[7, 8],
                [7, 7, 7, 255, 8, 8, 8, 255],
            ),
            (
                ColorType::Indexed,
                BitDepth::Eight,
                &[1, 0],
                [60, 50, 40, 255, 30, 20, 10, 255],
            ),
            (
                ColorType::Rgb,
                BitDepth::Sixteen,
                &[1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0],
                [3, 2, 1, 255, 6, 5, 4, 255],
            ),
        ];
        for (color, depth, data, expected) in cases {
            let shot = decode_png(&png(color, depth, 2, data)).unwrap();
            assert_eq!((shot.width, shot.height), (2, 1), "{color:?}");
            assert_eq!(shot.pixels, expected, "{color:?} {depth:?}");
        }
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn a_huge_png_is_refused_before_decoding() {
        let mut out = png(
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            1,
            &[0, 0, 0],
        );
        // IHDR's width and height sit at bytes 16..24, its CRC over bytes 12..29 at 29..33.
        out[16..20].copy_from_slice(&8192u32.to_be_bytes());
        out[20..24].copy_from_slice(&8192u32.to_be_bytes());
        let crc = crc32(&out[12..29]);
        out[29..33].copy_from_slice(&crc.to_be_bytes());
        let err = decode_png(&out).unwrap_err();
        assert!(err.contains("8192x8192 is too large"), "{err}");
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    #[test]
    fn a_bmp_past_the_owners_cap_is_left_out() {
        let pixels = [0u8; 8];
        let frame = Frame::new(2, 1, 8, &pixels).unwrap();
        assert!(bmp_beside(frame, MAX_DATA - 10).is_none());
        assert_eq!(offers(b"png".to_vec(), None).len(), 1);
    }
}
