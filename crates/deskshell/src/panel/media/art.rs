//! Track art: loads file and http(s) URLs, crops to a rounded square and caches by URL.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use ::image::{RgbaImage, imageops};
use deskshell_ui::image::{self, Error, Image, Kind};
use iced::widget::image::Handle;
use tracing::debug;

pub const LARGE_PX: u32 = 192;
pub const SMALL_PX: u32 = 68;
const LARGE_ROUND: f32 = 20.0 / 96.0;
const SMALL_ROUND: f32 = 10.0 / 34.0;
/// `image::decode` fits the longest side. Twice the large size keeps the short side of a
/// 2:1 picture at the large size after the crop.
const DECODE_PX: u32 = 2 * LARGE_PX;
pub const CAPACITY: usize = 8;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Art {
    pub large: Image,
    pub small: Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<'a> {
    File(PathBuf),
    Http(&'a str),
}

/// A `file://` URL on this host, an absolute path, or an `http(s)://` URL. Anything
/// else is `None`.
pub fn source(url: &str) -> Option<Source<'_>> {
    if let Some(rest) = url.strip_prefix("file://") {
        let rest = rest
            .strip_prefix("localhost")
            .unwrap_or(rest);
        let path = rest
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        if !path.starts_with('/') {
            return None;
        }
        return Some(Source::File(PathBuf::from(OsString::from_vec(
            percent_decode(path),
        ))));
    }
    if url.starts_with('/') {
        return Some(Source::File(PathBuf::from(url)));
    }
    let scheme = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    (scheme("http://") || scheme("https://")).then_some(Source::Http(url))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| {
            bytes
                .get(at)
                .and_then(|&b| (b as char).to_digit(16))
        };
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// It blocks.
pub fn load(url: &str) -> Result<Art, Error> {
    let decoded = match source(url) {
        Some(Source::File(path)) => {
            // Any player picks the URL, and a device such as /dev/zero would read forever.
            if !fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                return Err(Error::Read(format!(
                    "{} is not a file",
                    path.display()
                )));
            }
            image::load(&path, DECODE_PX)?
        }
        Some(Source::Http(url)) => {
            let path = url
                .split(['?', '#'])
                .next()
                .unwrap_or_default();
            return load_bytes(&get(url)?, Path::new(path));
        }
        None => {
            return Err(Error::Read(String::from(
                "not a file or http URL",
            )));
        }
    };
    crop(&decoded)
}

pub fn load_bytes(bytes: &[u8], path: &Path) -> Result<Art, Error> {
    let kind = Kind::detect(path, bytes).ok_or(Error::Unsupported)?;
    crop(&image::decode(bytes, kind, DECODE_PX)?)
}

pub async fn fetch(url: Arc<str>) -> Result<Art, Error> {
    tokio::task::spawn_blocking(move || load(&url))
        .await
        .unwrap_or_else(|err| Err(Error::Panicked(err.to_string())))
}

fn get(url: &str) -> Result<Vec<u8>, Error> {
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .build()
            .new_agent()
    });
    let mut response = AGENT
        .get(url)
        .call()
        .map_err(|err| Error::Read(err.to_string()))?;
    response
        .body_mut()
        .with_config()
        .limit(image::MAX_FILE)
        .read_to_vec()
        .map_err(|err| Error::Read(err.to_string()))
}

fn crop(decoded: &Image) -> Result<Art, Error> {
    let Handle::Rgba { width, height, pixels, .. } = &decoded.handle else {
        return Err(Error::Decode(String::from("not RGBA pixels")));
    };
    let (width, height) = (*width, *height);
    let rgba = RgbaImage::from_raw(width, height, pixels.to_vec())
        .ok_or_else(|| Error::Decode(String::from("pixel buffer size")))?;
    let side = width.min(height);
    let square = imageops::crop_imm(
        &rgba,
        (width - side) / 2,
        (height - side) / 2,
        side,
        side,
    )
    .to_image();
    Ok(Art {
        large: rounded(scaled(&square, LARGE_PX), LARGE_ROUND),
        small: rounded(scaled(&square, SMALL_PX), SMALL_ROUND),
    })
}

fn scaled(square: &RgbaImage, side: u32) -> RgbaImage {
    if square.width() <= side {
        return square.clone();
    }
    imageops::resize(
        square,
        side,
        side,
        imageops::FilterType::CatmullRom,
    )
}

// The rounded corners go in the alpha channel, so any renderer draws them.
fn rounded(mut square: RgbaImage, round: f32) -> Image {
    let side = square.width() as f32;
    let radius = side * round;
    for (x, y, pixel) in square.enumerate_pixels_mut() {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = (radius - px).max(px - (side - radius));
        let dy = (radius - py).max(py - (side - radius));
        if dx > 0.0 && dy > 0.0 {
            let coverage = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
            pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
        }
    }
    let (width, height) = square.dimensions();
    Image { handle: Handle::from_rgba(width, height, square.into_raw()), width, height }
}

#[derive(Debug)]
pub enum Slot {
    Loading,
    Ready(Art),
    Failed,
}

#[derive(Debug)]
struct Entry {
    url: Arc<str>,
    slot: Slot,
    used: u64,
}

#[derive(Debug, Default)]
pub struct Store {
    entries: Vec<Entry>,
    clock: u64,
}

impl Store {
    pub fn get(&self, url: &str) -> Option<&Slot> {
        self.entries
            .iter()
            .find(|entry| &*entry.url == url)
            .map(|entry| &entry.slot)
    }

    pub fn ready(&self, url: &str) -> Option<&Art> {
        match self.get(url)? {
            Slot::Ready(art) => Some(art),
            Slot::Loading | Slot::Failed => None,
        }
    }

    pub fn want<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) -> Vec<Arc<str>> {
        self.clock += 1;
        let mut loads = Vec::new();
        for url in urls {
            if url.is_empty() {
                continue;
            }
            match self
                .entries
                .iter_mut()
                .find(|entry| &*entry.url == url)
            {
                Some(entry) => entry.used = self.clock,
                None => {
                    let url: Arc<str> = Arc::from(url);
                    loads.push(Arc::clone(&url));
                    self.entries
                        .push(Entry { url, slot: Slot::Loading, used: self.clock });
                }
            }
        }
        self.evict();
        loads
    }

    pub fn finish(&mut self, url: &str, result: Result<Art, Error>) {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| &*entry.url == url)
        else {
            return;
        };
        entry.slot = match result {
            Ok(art) => Slot::Ready(art),
            Err(err) => {
                debug!(url, %err, "media art failed");
                Slot::Failed
            }
        };
        self.evict();
    }

    /// The shell drops the results of loads started before the panel closed, so the panel
    /// starts them again when it opens.
    pub fn forget_unfinished(&mut self) {
        self.entries
            .retain(|entry| !matches!(entry.slot, Slot::Loading));
    }

    fn evict(&mut self) {
        let clock = self.clock;
        let spare = |entry: &Entry| entry.used != clock && !matches!(entry.slot, Slot::Loading);
        while self
            .entries
            .iter()
            .filter(|entry| spare(entry))
            .count()
            > CAPACITY
        {
            let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| spare(entry))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
            else {
                break;
            };
            self.entries.swap_remove(oldest);
        }
    }
}
