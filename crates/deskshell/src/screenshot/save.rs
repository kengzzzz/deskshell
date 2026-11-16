//! Pixels to a private PNG file.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use deskshell_core::screenshot::pixels::{self, Placement, Shot};
use deskshell_wl::shot;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::flow::Picture;
use crate::panel::clipboard::store::open_private;

pub const DIR_NAME: &str = "screenshot-notifications";
pub const KEEP: usize = 100;
const PREFIX: &str = "screenshot-";
const SUFFIX: &str = ".png";
const RANDOM: usize = 8;
const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const ATTEMPTS: usize = 16;

pub fn dir_from(state: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf {
    state
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_default()).join(".local/state"))
        .join(DIR_NAME)
}

pub fn dir() -> PathBuf {
    dir_from(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub fn render(picture: &Picture, placements: &[Placement]) -> Result<Shot, String> {
    match picture {
        Picture::Crop { frames, index, rect } => {
            let frame = view(&frames[*index])?;
            if rect.x + rect.width > frame.width() || rect.y + rect.height > frame.height() {
                return Err(String::from("the region is outside the frame"));
            }
            Ok(pixels::crop(frame, *rect))
        }
        Picture::Compose(frames) => {
            let layout = pixels::layout(placements).ok_or("the outputs make no layout")?;
            let views = frames
                .iter()
                .map(view)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(pixels::compose(&layout, &views))
        }
    }
}

fn view(frame: &shot::Frame) -> Result<pixels::Frame<'_>, String> {
    pixels::Frame::new(
        frame.width,
        frame.height,
        frame.stride as usize,
        &frame.pixels,
    )
    .ok_or_else(|| {
        format!(
            "the frame of {} is shorter than its size",
            frame.output
        )
    })
}

pub fn encode(shot: &Shot) -> Result<Vec<u8>, png::EncodingError> {
    let mut rgb = Vec::new();
    pixels::to_rgb(shot.frame(), &mut rgb);
    let mut out = Vec::with_capacity(rgb.len() / 4);
    let mut encoder = png::Encoder::new(&mut out, shot.width, shot.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgb)?;
    writer.finish()?;
    Ok(out)
}

pub fn file_name(epoch_ns: u128, random: &[u8; RANDOM]) -> String {
    let mut name = format!("{PREFIX}{epoch_ns}-");
    name.extend(
        random
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()])),
    );
    name.push_str(SUFFIX);
    name
}

/// Whether `name` is one of [`file_name`]'s names, so an action key cannot name another file.
pub fn is_file_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
    else {
        return false;
    };
    let Some((epoch, random)) = rest.split_once('-') else {
        return false;
    };
    !epoch.is_empty()
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        && random.len() == RANDOM
        && random
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
}

/// `dir` opens as a verified directory fd: no symlinks, owned by the user, mode 0700. The file
/// is created relative to it with `O_EXCL` and `O_NOFOLLOW`, and pruning uses the same fd.
pub fn save(dir: &Path, png: &[u8]) -> io::Result<PathBuf> {
    let fd = open_private(dir)?;
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let mut random = [0u8; RANDOM];
        rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty())?;
        let name = file_name(epoch, &random);
        let created = rustix::fs::openat(
            &fd,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        );
        let mut file = match created {
            Ok(file) => File::from(file),
            Err(Errno::EXIST) => {
                last = Some(io::Error::from(Errno::EXIST));
                continue;
            }
            Err(errno) => return Err(errno.into()),
        };
        if let Err(err) = file.write_all(png) {
            let _ = rustix::fs::unlinkat(&fd, name.as_str(), AtFlags::empty());
            return Err(err);
        }
        if let Err(err) = prune_fd(&fd, KEEP) {
            tracing::warn!(%err, dir = %dir.display(), "old screenshots stay");
        }
        return Ok(dir.join(name));
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free file name")))
}

pub fn prune(dir: &Path, keep: usize) -> io::Result<usize> {
    prune_fd(&open_private(dir)?, keep)
}

// Only regular files with one of [`file_name`]'s names count and go.
fn prune_fd(dir: &OwnedFd, keep: usize) -> io::Result<usize> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir)?.flatten() {
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if is_file_name(name) && regular(dir, name).is_some() {
            names.push(name.to_owned());
        }
    }
    if names.len() <= keep {
        return Ok(0);
    }
    names.sort_unstable_by(|a, b| (epoch_of(a), a).cmp(&(epoch_of(b), b)));
    let excess = names.len() - keep;
    for name in &names[..excess] {
        if let Err(err) = rustix::fs::unlinkat(dir, name.as_str(), AtFlags::empty()) {
            tracing::warn!(%err, name, "an old screenshot stays");
        }
    }
    Ok(excess)
}

pub fn remove(dir: &Path, name: &str) {
    if !is_file_name(name) {
        return;
    }
    if let Ok(fd) = open_private(dir) {
        let _ = rustix::fs::unlinkat(&fd, name, AtFlags::empty());
    }
}

// By the number, so the order is by age even across a change in the number of digits.
fn epoch_of(name: &str) -> u128 {
    name.strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(epoch, _)| epoch.parse().ok())
        .unwrap_or(0)
}

fn regular(dir: &OwnedFd, name: &str) -> Option<(u64, u64)> {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
        .then_some((stat.st_dev as u64, stat.st_ino as u64))
}

/// Checked right before the launch: the name, the directory, a regular file opened without
/// following a symlink, and the path as satty will resolve it naming that same file.
pub fn edit_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    if !is_file_name(name) {
        return Err(io::Error::other("not a screenshot name"));
    }
    let fd = open_private(dir)?;
    let file = rustix::fs::openat(
        &fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let opened = rustix::fs::fstat(&file)?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
        return Err(io::Error::other("not a regular file"));
    }
    let identity = (opened.st_dev as u64, opened.st_ino as u64);
    if regular(&fd, name) != Some(identity) {
        return Err(io::Error::other(
            "the file changed while it was checked",
        ));
    }
    let path = dir.join(name);
    let resolved = std::fs::metadata(&path)?;
    if (resolved.dev(), resolved.ino()) != identity {
        return Err(io::Error::other("the path names another file"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_follows_xdg_state_home() {
        assert_eq!(
            dir_from(
                Some(OsStr::new("/state")),
                Some(OsStr::new("/home/u"))
            ),
            Path::new("/state/screenshot-notifications")
        );
        assert_eq!(
            dir_from(Some(OsStr::new("")), Some(OsStr::new("/home/u"))),
            Path::new("/home/u/.local/state/screenshot-notifications")
        );
        assert_eq!(
            dir_from(None, Some(OsStr::new("/home/u"))),
            Path::new("/home/u/.local/state/screenshot-notifications")
        );
    }

    #[test]
    fn names_carry_the_epoch_and_eight_characters() {
        let name = file_name(
            1_731_400_000_123_456_789,
            &[0, 1, 25, 26, 51, 52, 61, 62],
        );
        assert_eq!(
            name,
            "screenshot-1731400000123456789-ABZaz09A.png"
        );
        assert!(is_file_name(&name));
        for bad in [
            "screenshot-1-ABCDEFG.png",
            "screenshot-1-ABCDEFGHI.png",
            "screenshot--ABCDEFGH.png",
            "screenshot-1x-ABCDEFGH.png",
            "screenshot-1-ABCD/FGH.png",
            "screenshot-1-ABCDEFGH.png.png",
            "../screenshot-1-ABCDEFGH.png",
            "screenshot-1-ABCDEFGH.jpg",
        ] {
            assert!(!is_file_name(bad), "{bad}");
        }
    }
}
