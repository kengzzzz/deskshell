//! Notification images: raw `image-data` pixels to a handle, and what `image-path` points at.

use std::path::PathBuf;

use deskshell_notify::Pixels;
use deskshell_ui::image::{Error, guarded};
use iced::widget::image::Handle;
use image::{RgbaImage, imageops};

/// Senders may pad rows, and RGB images get an opaque alpha.
pub fn handle(pixels: &Pixels, max: u32) -> Result<Handle, Error> {
    let width = pixels.width as usize;
    let height = pixels.height as usize;
    let channels = pixels.channels as usize;
    let stride = pixels.rowstride as usize;
    let alpha = pixels.has_alpha && channels >= 4;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in pixels
        .data
        .chunks(stride.max(1))
        .take(height)
    {
        for pixel in row
            .chunks_exact(channels.max(1))
            .take(width)
        {
            let a = if alpha { pixel[3] } else { 255 };
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], a]);
        }
    }
    if rgba.len() != width * height * 4 || width == 0 || height == 0 {
        return Err(Error::Decode(String::from(
            "image-data does not fill its size",
        )));
    }
    let (width, height) = (pixels.width, pixels.height);
    if width <= max && height <= max {
        return Ok(Handle::from_rgba(width, height, rgba));
    }
    guarded(move || {
        let image = RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| Error::Decode(String::from("pixel buffer size")))?;
        let scale = max as f32 / width.max(height) as f32;
        let fit = |side: u32| ((side as f32 * scale).round() as u32).clamp(1, max);
        let small = imageops::thumbnail(&image, fit(width), fit(height));
        let (width, height) = small.dimensions();
        Ok(Handle::from_rgba(width, height, small.into_raw()))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<'a> {
    File(PathBuf),
    Icon(&'a str),
}

pub fn source(text: &str) -> Option<Source<'_>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(uri) = text.strip_prefix("file://") {
        // A host part, as in file://localhost/tmp/a.png, is skipped.
        let path = &uri[uri.find('/')?..];
        return Some(Source::File(PathBuf::from(percent_decode(path))));
    }
    if text.starts_with('/') {
        return Some(Source::File(PathBuf::from(text)));
    }
    Some(Source::Icon(text))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy_owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(width: u32, height: u32, channels: u32, rowstride: u32) -> Pixels {
        let mut data = vec![0u8; (rowstride * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let at = (y * rowstride + x * channels) as usize;
                data[at] = x as u8;
                data[at + 1] = y as u8;
                data[at + 2] = 7;
                if channels == 4 {
                    data[at + 3] = 128;
                }
            }
        }
        Pixels {
            width,
            height,
            rowstride,
            has_alpha: channels == 4,
            channels,
            data: data.into_boxed_slice(),
        }
    }

    fn rgba(handle: &Handle) -> (u32, u32, Vec<u8>) {
        match handle {
            Handle::Rgba { width, height, pixels, .. } => (*width, *height, pixels.to_vec()),
            other => panic!("expected RGBA, got {other:?}"),
        }
    }

    #[test]
    fn padded_rgb_rows_become_opaque_rgba() {
        let handle = handle(&pixels(3, 2, 3, 12), 80).unwrap();
        let (width, height, data) = rgba(&handle);
        assert_eq!((width, height), (3, 2));
        assert_eq!(&data[..8], &[0, 0, 7, 255, 1, 0, 7, 255]);
        assert_eq!(
            &data[12..16],
            &[0, 1, 7, 255],
            "the second row skips the padding"
        );
    }

    #[test]
    fn alpha_is_kept_and_large_images_shrink() {
        let (_, _, data) = rgba(&handle(&pixels(2, 2, 4, 8), 80).unwrap());
        assert_eq!(data[3], 128);
        let (width, height, _) = rgba(&handle(&pixels(200, 100, 4, 800), 80).unwrap());
        assert_eq!((width, height), (80, 40));
    }

    #[test]
    fn sources() {
        assert_eq!(
            source("file:///tmp/a%20b.png"),
            Some(Source::File(PathBuf::from("/tmp/a b.png")))
        );
        assert_eq!(
            source("file://localhost/tmp/c.png"),
            Some(Source::File(PathBuf::from("/tmp/c.png")))
        );
        assert_eq!(
            source("/srv/icons/d.svg"),
            Some(Source::File(PathBuf::from("/srv/icons/d.svg")))
        );
        assert_eq!(
            source("mail-unread"),
            Some(Source::Icon("mail-unread"))
        );
        assert_eq!(source("  "), None);
    }
}
