//! Image loading for icons, notification images, tray pixmaps and media art.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::panic::UnwindSafe;
use std::path::Path;
use std::sync::Arc;

use iced::widget::image::Handle;
use image::{ImageFormat, ImageReader, Limits, RgbaImage, imageops};
use resvg::{tiny_skia, usvg};

use crate::zone::catch_zone;

pub const MAX_FILE: u64 = 32 << 20;
pub const MAX_SIDE: u32 = 8192;
pub const DEFAULT_BUDGET: usize = 32 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Read(String),
    Decode(String),
    Unsupported,
    Panicked(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(err) => write!(f, "cannot read the image: {err}"),
            Self::Decode(err) => write!(f, "cannot decode the image: {err}"),
            Self::Unsupported => f.write_str("not a PNG, JPEG or SVG image"),
            Self::Panicked(err) => write!(f, "the image decoder panicked: {err}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone)]
pub struct Image {
    pub handle: Handle,
    pub width: u32,
    pub height: u32,
}

impl Image {
    fn from_rgba(rgba: RgbaImage) -> Self {
        let (width, height) = rgba.dimensions();
        Self { handle: Handle::from_rgba(width, height, rgba.into_raw()), width, height }
    }

    pub fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Svg,
}

impl Kind {
    pub fn detect(path: &Path, bytes: &[u8]) -> Option<Self> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str());
        if extension
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz"))
        {
            return Some(Self::Svg);
        }
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else {
            None
        }
    }
}

// iced gets decoded RGBA only. Given a path or encoded bytes, it would decode them on the
// render path, outside a catch zone.
pub fn load(path: &Path, size: u32) -> Result<Image, Error> {
    let (bytes, _) = read_regular(path, MAX_FILE)?;
    let kind = Kind::detect(path, &bytes).ok_or(Error::Unsupported)?;
    decode(&bytes, kind, size)
}

/// Refuses anything but a regular file and stops after `max + 1` bytes, so a file that grows
/// or lies about its size takes no more memory. The open does not block on a FIFO.
pub fn read_regular(path: &Path, max: u64) -> Result<(Vec<u8>, fs::Metadata), Error> {
    let read = |err: std::io::Error| Error::Read(err.to_string());
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::NOCTTY
        | rustix::fs::OFlags::CLOEXEC;
    let fd =
        rustix::fs::open(path, flags, rustix::fs::Mode::empty()).map_err(|err| read(err.into()))?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(read)?;
    if !metadata.file_type().is_file() {
        return Err(Error::Read(String::from("not a regular file")));
    }
    if metadata.len() > max {
        return Err(Error::Read(format!(
            "{} bytes is too large",
            metadata.len()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(read)?;
    if bytes.len() as u64 > max {
        return Err(Error::Read(format!("more than {max} bytes")));
    }
    Ok((bytes, metadata))
}

pub fn decode(bytes: &[u8], kind: Kind, size: u32) -> Result<Image, Error> {
    let size = size.clamp(1, MAX_SIDE);
    guarded(|| match kind {
        Kind::Png => raster(bytes, ImageFormat::Png, size),
        Kind::Jpeg => raster(bytes, ImageFormat::Jpeg, size),
        Kind::Svg => svg(bytes, size),
    })
}

pub fn guarded<T>(decoder: impl FnOnce() -> Result<T, Error> + UnwindSafe) -> Result<T, Error> {
    catch_zone(decoder).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
            })
            .unwrap_or_default();
        Err(Error::Panicked(message))
    })
}

fn raster(bytes: &[u8], format: ImageFormat, size: u32) -> Result<Image, Error> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .map_err(|err| Error::Decode(err.to_string()))?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    if width <= size && height <= size {
        return Ok(Image::from_rgba(rgba));
    }
    let (fit_width, fit_height) = fit(width as f32, height as f32, size);
    // CatmullRom's kernel grows with the scale factor (about 30 ms for 512 px to 36 px). A box
    // average down to twice the size first keeps it under a millisecond.
    let rgba = if width > fit_width * 2 && height > fit_height * 2 {
        imageops::thumbnail(&rgba, fit_width * 2, fit_height * 2)
    } else {
        rgba
    };
    Ok(Image::from_rgba(imageops::resize(
        &rgba,
        fit_width,
        fit_height,
        imageops::FilterType::CatmullRom,
    )))
}

fn svg(bytes: &[u8], size: u32) -> Result<Image, Error> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
        .map_err(|err| Error::Decode(err.to_string()))?;
    let natural = tree.size();
    let (width, height) = fit(natural.width(), natural.height(), size);
    let scale = (width as f32 / natural.width()).min(height as f32 / natural.height());
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| Error::Decode(String::from("empty SVG")))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        rgba.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    RgbaImage::from_raw(width, height, rgba)
        .map(Image::from_rgba)
        .ok_or_else(|| Error::Decode(String::from("pixel buffer size")))
}

fn fit(width: f32, height: f32, size: u32) -> (u32, u32) {
    let longest = width.max(height).max(f32::MIN_POSITIVE);
    let scale = size as f32 / longest;
    (
        (width * scale)
            .round()
            .clamp(1.0, size as f32) as u32,
        (height * scale)
            .round()
            .clamp(1.0, size as f32) as u32,
    )
}

#[derive(Debug, Clone)]
pub enum Slot {
    Loading,
    Ready(Image),
    Failed(Error),
}

#[derive(Debug)]
struct Entry {
    size: u32,
    slot: Slot,
    drawn: Cell<u64>,
}

#[derive(Debug)]
pub struct Cache {
    budget: usize,
    used: usize,
    clock: Cell<u64>,
    entries: HashMap<Arc<Path>, Vec<Entry>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET)
    }
}

impl Cache {
    pub fn new(budget: usize) -> Self {
        Self { budget, used: 0, clock: Cell::new(0), entries: HashMap::new() }
    }

    pub fn get(&self, path: &Path, size: u32) -> Option<&Slot> {
        let entry = self
            .entries
            .get(path)?
            .iter()
            .find(|entry| entry.size == size)?;
        let now = self.clock.get() + 1;
        self.clock.set(now);
        entry.drawn.set(now);
        Some(&entry.slot)
    }

    pub fn handle(&self, path: &Path, size: u32) -> Option<&Handle> {
        match self.get(path, size)? {
            Slot::Ready(image) => Some(&image.handle),
            Slot::Loading | Slot::Failed(_) => None,
        }
    }

    /// False when it is loading or done already. Start a load only on true.
    pub fn start(&mut self, path: &Arc<Path>, size: u32) -> bool {
        let sizes = self
            .entries
            .entry(Arc::clone(path))
            .or_default();
        if sizes
            .iter()
            .any(|entry| entry.size == size)
        {
            return false;
        }
        sizes.push(Entry { size, slot: Slot::Loading, drawn: Cell::new(self.clock.get()) });
        true
    }

    pub fn finish(&mut self, path: &Arc<Path>, size: u32, result: Result<Image, Error>) {
        let slot = match result {
            Ok(image) => {
                self.used += image.bytes();
                Slot::Ready(image)
            }
            Err(err) => Slot::Failed(err),
        };
        let sizes = self
            .entries
            .entry(Arc::clone(path))
            .or_default();
        match sizes
            .iter_mut()
            .find(|entry| entry.size == size)
        {
            Some(entry) => {
                if let Slot::Ready(old) = &entry.slot {
                    self.used -= old.bytes();
                }
                entry.slot = slot;
            }
            None => sizes.push(Entry { size, slot, drawn: Cell::new(self.clock.get()) }),
        }
        self.evict();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }

    pub fn used(&self) -> usize {
        self.used
    }

    fn evict(&mut self) {
        while self.used > self.budget {
            let oldest = self
                .entries
                .iter()
                .flat_map(|(path, sizes)| {
                    sizes
                        .iter()
                        .filter(|entry| matches!(entry.slot, Slot::Ready(_)))
                        .map(move |entry| (entry.drawn.get(), path, entry.size))
                })
                .min_by_key(|(drawn, ..)| *drawn)
                .map(|(_, path, size)| (Arc::clone(path), size));
            let Some((path, size)) = oldest else {
                break;
            };
            if let Some(sizes) = self.entries.get_mut(&path) {
                if let Some(index) = sizes
                    .iter()
                    .position(|entry| entry.size == size)
                {
                    let entry = sizes.swap_remove(index);
                    if let Slot::Ready(image) = entry.slot {
                        self.used -= image.bytes();
                    }
                }
                if sizes.is_empty() {
                    self.entries.remove(&path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 128]))
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn decodes_and_fits() {
        let small = decode(&png(16, 8), Kind::Png, 72).unwrap();
        assert_eq!(
            (small.width, small.height),
            (16, 8),
            "smaller images keep their size"
        );
        let large = decode(&png(256, 128), Kind::Png, 72).unwrap();
        assert_eq!((large.width, large.height), (72, 36));
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="24" height="12" fill="#ff000080"/></svg>"##;
        let drawn = decode(svg, Kind::Svg, 72).unwrap();
        assert_eq!(
            (drawn.width, drawn.height),
            (72, 36),
            "SVGs are drawn at the box size"
        );
        let Handle::Rgba { pixels, .. } = &drawn.handle else {
            panic!("an RGBA handle");
        };
        assert_eq!(&pixels[..4], &[255, 0, 0, 128], "straight alpha");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-ui-image-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_only_bounded_regular_files() {
        let dir = scratch("read");
        let file = dir.join("ok.png");
        fs::write(&file, png(4, 4)).unwrap();
        let (bytes, metadata) = read_regular(&file, MAX_FILE).unwrap();
        assert_eq!(bytes.len() as u64, metadata.len());
        assert!(load(&file, 8).is_ok(), "load keeps working");

        assert!(
            read_regular(&dir, MAX_FILE).is_err(),
            "a directory"
        );
        let fifo = dir.join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        assert!(
            read_regular(&fifo, MAX_FILE).is_err(),
            "a FIFO, refused without blocking"
        );

        let sparse = dir.join("sparse");
        File::create(&sparse)
            .unwrap()
            .set_len(65)
            .unwrap();
        assert!(
            read_regular(&sparse, 64).is_err(),
            "over the size limit"
        );
        assert!(read_regular(&sparse, 65).is_ok(), "at the limit");

        // procfs reports a size of 0 and then reads more, as a file that grows would.
        let maps = Path::new("/proc/self/maps");
        assert_eq!(fs::metadata(maps).unwrap().len(), 0);
        assert_eq!(
            read_regular(maps, 16).unwrap_err(),
            Error::Read(String::from("more than 16 bytes")),
            "the read stops after the limit, whatever the size said"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_kinds() {
        assert_eq!(
            Kind::detect(Path::new("a.png"), &png(1, 1)),
            Some(Kind::Png)
        );
        assert_eq!(
            Kind::detect(Path::new("a.SVG"), b""),
            Some(Kind::Svg)
        );
        assert_eq!(
            Kind::detect(Path::new("a"), &[0xff, 0xd8, 0xff, 0xe0]),
            Some(Kind::Jpeg)
        );
        assert_eq!(
            Kind::detect(Path::new("a.xpm"), b"/* XPM */"),
            None
        );
    }

    #[test]
    fn cache_budget_drops_the_least_recently_drawn() {
        let image = || decode(&png(8, 8), Kind::Png, 8).unwrap();
        let per = image().bytes();
        let mut cache = Cache::new(per * 2);
        let a: Arc<Path> = Arc::from(Path::new("/a.png"));
        let b: Arc<Path> = Arc::from(Path::new("/b.png"));
        let c: Arc<Path> = Arc::from(Path::new("/c.png"));
        assert!(cache.start(&a, 8));
        assert!(!cache.start(&a, 8), "a load already runs");
        assert_matches!(cache.get(&a, 8), Some(Slot::Loading));
        cache.finish(&a, 8, Ok(image()));
        assert!(cache.start(&b, 8));
        cache.finish(&b, 8, Ok(image()));
        assert!(cache.handle(&a, 8).is_some());
        assert!(cache.start(&c, 8));
        cache.finish(&c, 8, Ok(image()));
        assert_eq!(cache.used(), per * 2);
        assert!(
            cache.handle(&b, 8).is_none(),
            "b was drawn least recently"
        );
        assert!(cache.handle(&a, 8).is_some());
        assert!(cache.handle(&c, 8).is_some());
        assert!(
            cache.start(&a, 16),
            "another size is another slot"
        );
        cache.finish(&a, 16, Err(Error::Unsupported));
        assert_matches!(
            cache.get(&a, 16),
            Some(Slot::Failed(Error::Unsupported))
        );
        cache.clear();
        assert_eq!(cache.used(), 0);
        assert!(cache.get(&a, 8).is_none());
    }
}
