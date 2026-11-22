//! The avatars and the wallpaper, decoded on a blocking thread behind a crash marker.

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use deskshell_ui::image::{self as ui_image, Kind};
use iced::widget::image::Handle;
use image::imageops::{self, FilterType};
use image::{ImageReader, Limits};
use tracing::warn;

pub const AVATAR_SIZE: u32 = 128;
pub const AVATAR_MAX: u64 = ui_image::MAX_FILE;
pub const WALLPAPER_MAX: u64 = 64 << 20;
const LOADING: &str = "picture.loading";
const POISONED: &str = "pictures.poisoned";
const SIGMA: f32 = 4.0;
const QUARTER: u32 = 4;
const MAX_SIDE: u32 = 16_384;

#[derive(Debug, Default)]
pub struct Guard {
    dir: Option<PathBuf>,
    poisoned: Vec<String>,
    /// One marker file, so loads run one at a time and the marker names the running decode.
    loading: Mutex<()>,
}

impl Guard {
    pub fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else {
            return Self::default();
        };
        let list = dir.join(POISONED);
        let mut poisoned: Vec<String> = fs::read_to_string(&list)
            .map(|text| {
                text.lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(key) = fs::read_to_string(dir.join(LOADING)) {
            warn!(file = %key.split('\t').next().unwrap_or(""), "a picture crashed the last greeter, skipping it");
            if !poisoned.contains(&key) {
                poisoned.push(key);
                if let Err(err) = fs::write(&list, poisoned.join("\n")) {
                    warn!(%err, "cannot save the poisoned pictures");
                }
            }
            let _ = fs::remove_file(dir.join(LOADING));
        }
        Self { dir: Some(dir.to_owned()), poisoned, loading: Mutex::new(()) }
    }

    /// The greeter aborts on panic, so a file that crashes the decoder would do so on every
    /// restart. The marker names it, and the next greeter poisons it until it changes.
    pub fn load<T>(
        &self,
        path: &Path,
        max: u64,
        decode: impl FnOnce(&Path, &[u8]) -> Option<T>,
    ) -> Option<T> {
        let dir = self.dir.as_deref()?;
        let _one_at_a_time = self
            .loading
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (bytes, metadata) = ui_image::read_regular(path, max).ok()?;
        let key = key(path, &metadata)?;
        if self.poisoned.contains(&key) {
            return None;
        }
        let marker = dir.join(LOADING);
        if let Err(err) = fs::write(&marker, &key) {
            warn!(%err, "cannot write the picture marker, not decoding pictures");
            return None;
        }
        let picture = decode(path, &bytes);
        let _ = fs::remove_file(&marker);
        picture
    }
}

/// From the descriptor that was read, so the key names the bytes that were decoded.
fn key(path: &Path, metadata: &fs::Metadata) -> Option<String> {
    let name = path.to_str()?;
    if name.contains('\n') {
        return None;
    }
    Some(format!(
        "{name}\t{}\t{}\t{}\t{}.{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec()
    ))
}

pub fn avatar(path: &Path, bytes: &[u8]) -> Option<Handle> {
    let kind = Kind::detect(path, bytes)?;
    ui_image::decode(bytes, kind, AVATAR_SIZE)
        .ok()
        .map(|image| image.handle)
}

pub fn wallpaper(bytes: &[u8], width: u32, height: u32) -> Option<Handle> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(err) => {
            warn!(%err, "cannot decode the wallpaper");
            return None;
        }
    };
    let (width, height) = (
        (width / QUARTER).max(1),
        (height / QUARTER).max(1),
    );
    let small = image
        .resize_to_fill(width, height, FilterType::Triangle)
        .to_rgba8();
    let blurred = imageops::blur(&small, SIGMA);
    Some(Handle::from_rgba(
        width,
        height,
        blurred.into_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "deskshell-greeter-pictures-{tag}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key_of(path: &Path) -> String {
        key(path, &fs::metadata(path).unwrap()).unwrap()
    }

    #[test]
    fn a_load_that_crashed_is_never_retried() {
        let dir = TempDir::new("poison");
        let picture = dir.0.join("alice");
        fs::write(&picture, b"bad").unwrap();
        let guard = Guard::open(Some(&dir.0));
        assert_eq!(
            guard.load(&picture, 64, |_, bytes| Some(bytes.to_vec())),
            Some(b"bad".to_vec())
        );
        assert!(
            !dir.0.join(LOADING).exists(),
            "the marker goes after the load"
        );

        // A load that never returned, as when the decoder aborted the process.
        fs::write(dir.0.join(LOADING), key_of(&picture)).unwrap();
        let guard = Guard::open(Some(&dir.0));
        assert!(!dir.0.join(LOADING).exists());
        assert_eq!(
            guard.load(&picture, 64, |_, _| Some(2)),
            None,
            "poisoned"
        );
        let guard = Guard::open(Some(&dir.0));
        assert_eq!(
            guard.load(&picture, 64, |_, _| Some(3)),
            None,
            "still poisoned after a restart"
        );

        // A new file under the same name is decoded again.
        fs::write(&picture, b"a different picture").unwrap();
        assert_eq!(guard.load(&picture, 64, |_, _| Some(4)), Some(4));
    }

    #[test]
    fn nothing_is_decoded_without_a_place_for_the_marker() {
        let dir = TempDir::new("nodir");
        let picture = dir.0.join("alice");
        fs::write(&picture, b"x").unwrap();
        assert_eq!(
            Guard::open(None).load(&picture, 64, |_, _| Some(1)),
            None
        );
        let missing = Guard::open(Some(&dir.0.join("missing")));
        assert_eq!(missing.load(&picture, 64, |_, _| Some(1)), None);
        let guard = Guard::open(Some(&dir.0));
        assert_eq!(
            guard.load(&dir.0.join("nobody"), 64, |_, _| Some(1)),
            None,
            "no file"
        );
    }

    #[test]
    fn only_bounded_regular_files_are_decoded() {
        let dir = TempDir::new("bounds");
        let guard = Guard::open(Some(&dir.0));
        let big = dir.0.join("big");
        fs::write(&big, [7u8; 65]).unwrap();
        assert_eq!(
            guard.load(&big, 64, |_, _| Some(1)),
            None,
            "over the limit"
        );
        assert_eq!(
            guard.load(&dir.0, 64, |_, _| Some(1)),
            None,
            "a directory"
        );
        assert_eq!(
            guard.load(Path::new("/proc/self/maps"), 16, |_, _| Some(1)),
            None,
            "a file whose size said 0 but read more"
        );
        assert!(
            !dir.0.join(LOADING).exists(),
            "nothing was decoded, so no marker"
        );
    }

    #[test]
    fn overlapping_loads_keep_the_running_decode_marked() {
        let dir = TempDir::new("overlap");
        let (avatar, wallpaper) = (dir.0.join("avatar"), dir.0.join("wallpaper"));
        fs::write(&avatar, b"avatar").unwrap();
        fs::write(&wallpaper, b"wallpaper").unwrap();
        let guard = Arc::new(Guard::open(Some(&dir.0)));
        let marker = dir.0.join(LOADING);

        let (started, avatar_started) = mpsc::channel();
        let (release, avatar_released) = mpsc::channel::<()>();
        let first = {
            let (guard, avatar, marker) = (Arc::clone(&guard), avatar.clone(), marker.clone());
            std::thread::spawn(move || {
                guard.load(&avatar, 64, |path, _| {
                    started.send(()).unwrap();
                    avatar_released.recv().unwrap();
                    // The other load came and went meanwhile, or is still waiting: the
                    // marker must still name this decode.
                    assert_eq!(fs::read_to_string(&marker).unwrap(), key_of(path));
                    Some("avatar")
                })
            })
        };
        avatar_started.recv().unwrap();
        let (decoding, wallpaper_decoding) = mpsc::channel();
        let second = {
            let guard = Arc::clone(&guard);
            std::thread::spawn(move || {
                guard.load(&wallpaper, 64, |_, _| {
                    decoding.send(()).unwrap();
                    Some("wallpaper")
                })
            })
        };
        assert!(
            wallpaper_decoding
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "the second load waits while the first decodes"
        );
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            key_of(&avatar)
        );
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap(), Some("avatar"));
        assert_eq!(second.join().unwrap(), Some("wallpaper"));
        assert!(!marker.exists());
    }

    #[test]
    fn the_wallpaper_is_a_quarter_of_the_screen() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(64, 32, image::Rgba([200, 100, 50, 255]))
            .write_to(
                &mut Cursor::new(&mut png),
                image::ImageFormat::Png,
            )
            .unwrap();
        let handle = wallpaper(&png, 400, 300).expect("decoded");
        let Handle::Rgba { width, height, .. } = handle else {
            panic!("raw pixels");
        };
        assert_eq!((width, height), (100, 75));
        assert!(wallpaper(b"not a png", 400, 300).is_none());
    }
}
