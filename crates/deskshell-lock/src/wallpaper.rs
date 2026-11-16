//! The blurred wallpaper behind the lock, cached as raw pixels.

use std::cell::RefCell;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, RgbaImage};

const MAGIC: &[u8] = b"deskshell-lock wallpaper 1\n";
const PREFIX: &str = "lock-wallpaper-";
const SUFFIX: &str = ".rgba";
const MARKER: &str = "lock-wallpaper.building";
/// At a quarter of the output size, so the visible blur radius is about 48 screen pixels.
const SIGMA: f32 = 4.0;
const QUARTER: u32 = 4;
const MAX_SIDE: u32 = 16_384;

pub fn source_path() -> Option<PathBuf> {
    let home = env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(Path::new(&home).join(".config/desktop/assets/wallpaper.png"))
}

pub fn cache_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| Path::new(&home).join(".cache"))
        })?;
    Some(base.join("deskshell"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub path: PathBuf,
    pub len: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub width: u32,
    pub height: u32,
}

impl Key {
    pub fn of(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self {
            path: path.to_owned(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            width,
            height,
        })
    }

    /// The hash is FNV-1a over the header, so it is the same in every build.
    pub fn file_name(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.header() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!(
            "{PREFIX}{}x{}-{hash:016x}{SUFFIX}",
            self.width, self.height
        )
    }

    fn header(&self) -> Vec<u8> {
        let path = self.path.as_os_str().as_bytes();
        let mut header = Vec::with_capacity(MAGIC.len() + path.len() + 36);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&(path.len() as u64).to_le_bytes());
        header.extend_from_slice(path);
        header.extend_from_slice(&self.len.to_le_bytes());
        header.extend_from_slice(&self.mtime.to_le_bytes());
        header.extend_from_slice(&self.mtime_nsec.to_le_bytes());
        header.extend_from_slice(&self.width.to_le_bytes());
        header.extend_from_slice(&self.height.to_le_bytes());
        header
    }

    fn pixel_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

pub fn load(dir: &Path, key: &Key) -> Option<RgbaImage> {
    let mut file = fs::File::open(dir.join(key.file_name())).ok()?;
    let expected = key.header();
    let mut header = vec![0; expected.len()];
    file.read_exact(&mut header).ok()?;
    if header != expected {
        return None;
    }
    let mut pixels = Vec::with_capacity(key.pixel_bytes() + 1);
    file.read_to_end(&mut pixels).ok()?;
    if pixels.len() != key.pixel_bytes() {
        return None;
    }
    RgbaImage::from_raw(key.width, key.height, pixels)
}

pub fn store(dir: &Path, key: &Key, image: &RgbaImage) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let name = key.file_name();
    let temporary = dir.join(format!(".{name}.tmp"));
    let mut file = fs::File::create(&temporary)?;
    file.write_all(&key.header())?;
    file.write_all(image.as_raw())?;
    drop(file);
    fs::rename(&temporary, dir.join(&name))?;
    for entry in fs::read_dir(dir)?.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else {
            continue;
        };
        if file != name && file.starts_with(PREFIX) && file.ends_with(SUFFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub fn build(key: &Key) -> Result<RgbaImage, String> {
    if key.width == 0 || key.height == 0 || key.width > MAX_SIDE || key.height > MAX_SIDE {
        return Err(format!(
            "bad output size {}x{}",
            key.width, key.height
        ));
    }
    let source = ImageReader::open(&key.path)
        .and_then(ImageReader::with_guessed_format)
        .map_err(|err| err.to_string())?
        .decode()
        .map_err(|err| err.to_string())?;
    Ok(blur_to(&source, key.width, key.height))
}

fn blur_to(source: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let small = source.resize_to_fill(
        width.div_ceil(QUARTER),
        height.div_ceil(QUARTER),
        FilterType::Triangle,
    );
    let blurred = image::imageops::blur(&small.to_rgba8(), SIGMA);
    image::imageops::resize(&blurred, width, height, FilterType::Triangle)
}

thread_local! {
    static BUILDING: RefCell<Option<(PathBuf, String)>> = const { RefCell::new(None) };
}

pub fn poisoned(dir: &Path, key: &Key) -> bool {
    fs::read(dir.join(MARKER)).is_ok_and(|marker| marker == key.file_name().as_bytes())
}

/// Called by the panic hook, which always aborts. A panic during a build writes a marker, so the
/// restarted lock skips that wallpaper instead of crashing on it again.
pub fn note_panic() {
    let _ = BUILDING.try_with(|building| {
        if let Ok(building) = building.try_borrow()
            && let Some((dir, name)) = building.as_ref()
        {
            let _ = mark(dir, name);
        }
    });
}

fn mark(dir: &Path, name: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join(MARKER), name)
}

fn building<T>(dir: &Path, key: &Key, build: impl FnOnce() -> T) -> T {
    BUILDING.with(|building| *building.borrow_mut() = Some((dir.to_owned(), key.file_name())));
    let result = build();
    BUILDING.with(|building| *building.borrow_mut() = None);
    result
}

/// Blocks, so run it on a helper thread.
pub fn prepare(source: &Path, dir: &Path, width: u32, height: u32) -> Result<RgbaImage, String> {
    let key =
        Key::of(source, width, height).map_err(|err| format!("{}: {err}", source.display()))?;
    if let Some(image) = load(dir, &key) {
        return Ok(image);
    }
    if poisoned(dir, &key) {
        return Err(String::from(
            "an earlier build of this wallpaper panicked, skipping it",
        ));
    }
    let image = building(dir, &key, || build(&key));
    if let Ok(image) = &image
        && let Err(err) = store(dir, &key, image)
    {
        tracing::warn!(%err, "cannot cache the wallpaper");
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "deskshell-lock-wallpaper-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key() -> Key {
        Key {
            path: PathBuf::from("/home/user/.config/desktop/assets/wallpaper.png"),
            len: 10_000_000,
            mtime: 1_760_000_000,
            mtime_nsec: 5,
            width: 3440,
            height: 1440,
        }
    }

    #[test]
    fn every_part_of_the_key_changes_the_file_name() {
        let base = key();
        let name = base.file_name();
        assert!(name.starts_with("lock-wallpaper-3440x1440-"));
        assert!(name.ends_with(".rgba"));
        assert_eq!(name, key().file_name(), "the name is stable");
        let variants = [
            Key { path: PathBuf::from("/other.png"), ..key() },
            Key { len: base.len + 1, ..key() },
            Key { mtime: base.mtime + 1, ..key() },
            Key { mtime_nsec: base.mtime_nsec + 1, ..key() },
            Key { width: 1920, ..key() },
            Key { height: 1080, ..key() },
            Key { width: 1440, height: 3440, ..key() },
        ];
        for variant in variants {
            assert_ne!(variant.file_name(), name, "{variant:?}");
        }
    }

    #[test]
    fn the_cache_round_trips_and_rejects_other_keys() {
        let dir = temp("cache");
        let small = Key { width: 4, height: 2, ..key() };
        let image = RgbaImage::from_fn(4, 2, |x, y| {
            image::Rgba([x as u8, y as u8, 7, 255])
        });
        store(&dir, &small, &image).unwrap();
        assert_eq!(load(&dir, &small), Some(image.clone()));
        assert_eq!(
            load(
                &dir,
                &Key { mtime: small.mtime + 1, ..small.clone() }
            ),
            None
        );

        let name = dir.join(small.file_name());
        let mut bytes = fs::read(&name).unwrap();
        bytes.pop();
        fs::write(&name, &bytes).unwrap();
        assert_eq!(
            load(&dir, &small),
            None,
            "a short file is rejected"
        );

        let other = Key { width: 2, height: 2, ..small.clone() };
        store(&dir, &small, &image).unwrap();
        store(
            &dir,
            &other,
            &RgbaImage::from_pixel(2, 2, image::Rgba([0; 4])),
        )
        .unwrap();
        assert_eq!(
            load(&dir, &small),
            None,
            "a new key removes the old file"
        );
        assert!(load(&dir, &other).is_some());
        let files = fs::read_dir(&dir).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn prepare_builds_caches_and_skips_a_poisoned_build() {
        let dir = temp("prepare");
        let source = dir.join("wallpaper.png");
        let picture = RgbaImage::from_fn(64, 36, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 7) as u8, 128, 255])
        });
        picture.save(&source).unwrap();
        let cache = dir.join("cache");

        let built = prepare(&source, &cache, 32, 24).unwrap();
        assert_eq!(built.dimensions(), (32, 24));
        let key = Key::of(&source, 32, 24).unwrap();
        assert_eq!(load(&cache, &key), Some(built.clone()));
        assert!(
            !poisoned(&cache, &key),
            "a build that ends writes no marker"
        );
        assert_eq!(
            prepare(&source, &cache, 32, 24).unwrap(),
            built,
            "the second lock reads the cache"
        );

        let other = Key::of(&source, 16, 16).unwrap();
        note_panic();
        assert!(
            !poisoned(&cache, &other),
            "a panic outside a build writes no marker"
        );
        building(&cache, &other, note_panic);
        assert!(
            poisoned(&cache, &other),
            "a panic during a build poisons its key"
        );
        assert!(prepare(&source, &cache, 16, 16).is_err());
        assert!(
            prepare(&source, &cache, 32, 24).is_ok(),
            "another key still loads"
        );

        assert!(prepare(&dir.join("missing.png"), &cache, 32, 24).is_err());
    }
}
