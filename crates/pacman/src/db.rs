//! The `local` database dir and the sync database tars.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::time::Instant;

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

use crate::Limits;
use crate::conf::PacmanConf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub name: String,
    pub version: String,
    pub ignored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sync {
    pub version: String,
    pub ignored: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Desc<'a> {
    name: &'a str,
    version: &'a str,
    groups: &'a str,
}

fn desc(text: &str) -> Desc<'_> {
    let mut out = Desc::default();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        rest = &rest[start..];
        let Some((header, body)) = rest.split_once('\n') else {
            break;
        };
        let end = body.find("\n\n").unwrap_or(body.len());
        let value = body[..end].trim_end_matches('\n');
        match header.trim_end() {
            "%NAME%" => out.name = value,
            "%VERSION%" => out.version = value,
            "%GROUPS%" => out.groups = value,
            _ => {}
        }
        rest = &body[end..];
    }
    out
}

fn dir_matches(dir: &[u8], desc: &Desc<'_>) -> bool {
    let (name, version) = (desc.name.as_bytes(), desc.version.as_bytes());
    dir.len() == name.len() + 1 + version.len()
        && dir.starts_with(name)
        && dir[name.len()] == b'-'
        && dir.ends_with(version)
}

fn invalid(dir: &[u8], desc: &Desc<'_>) -> Option<String> {
    let dir = String::from_utf8_lossy(dir);
    if desc.name.is_empty() || desc.version.is_empty() {
        Some(format!(
            "{dir}/desc has no %NAME% or no %VERSION%"
        ))
    } else if !dir_matches(dir.as_bytes(), desc) {
        Some(format!(
            "{dir}/desc names {}-{}",
            desc.name, desc.version
        ))
    } else {
        None
    }
}

/// A package dir without a `desc` is being written by pacman and is skipped. A broken `desc`
/// is an error, because skipping it would hide the package's update.
pub fn read_local(dir: &Path, conf: &PacmanConf) -> io::Result<Vec<Local>> {
    let mut packages = Vec::new();
    let mut text = String::new();
    let mut path = dir.to_path_buf();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        path.push(&name);
        path.push("desc");
        text.clear();
        let read = File::open(&path).and_then(|mut file| file.read_to_string(&mut text));
        path.pop();
        path.pop();
        match read {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        }
        let desc = desc(&text);
        if let Some(error) = invalid(name.as_encoded_bytes(), &desc) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
        packages.push(Local {
            name: desc.name.to_owned(),
            version: desc.version.to_owned(),
            ignored: conf.ignored(desc.name, desc.groups),
        });
    }
    Ok(packages)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    TimedOut,
    Bad(String),
}

/// The stream is read past the tar's end, so the gzip and zstd checksums are checked.
/// `limits` bound the unpacked bytes and each `desc`.
pub fn read_sync(
    path: &Path,
    installed: &HashSet<&str>,
    conf: &PacmanConf,
    limits: &Limits,
    deadline: Instant,
) -> Result<HashMap<String, Sync>, ReadError> {
    let bad = |err: &dyn std::fmt::Display| {
        if Instant::now() >= deadline {
            ReadError::TimedOut
        } else {
            ReadError::Bad(err.to_string())
        }
    };
    let file = File::open(path).map_err(|err| bad(&err))?;
    let mut reader = Clocked { inner: BufReader::with_capacity(64 * 1024, file), deadline };
    let head = reader
        .fill_buf()
        .map_err(|err| bad(&err))?;
    if head.is_empty() {
        return Err(ReadError::Bad(String::from("the file is empty")));
    }
    let stream: Box<dyn Read> = match Format::of(head) {
        Format::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(reader)),
        Format::Zstd => {
            Box::new(Zstd::new(reader, deadline).map_err(|err| bad(&format!("zstd: {err}")))?)
        }
        Format::Tar => Box::new(reader),
        Format::Other(name) => {
            return Err(ReadError::Bad(format!(
                "{name} compression is not supported"
            )));
        }
    };
    let bounded =
        Bounded { inner: stream, read: 0, limit: limits.unpacked, deadline, next_check: 0 };
    let mut packages = HashMap::new();
    let mut text = String::new();
    let mut dir = Vec::new();
    let mut archive = tar::Archive::new(bounded);
    for entry in archive
        .entries()
        .map_err(|err| bad(&err))?
    {
        let mut entry = entry.map_err(|err| bad(&err))?;
        dir.clear();
        match entry
            .path_bytes()
            .strip_suffix(b"/desc")
        {
            Some(name) if !name.contains(&b'/') => dir.extend_from_slice(name),
            _ => continue,
        }
        if entry.size() > limits.desc {
            return Err(ReadError::Bad(format!(
                "{}/desc is larger than {} KiB",
                String::from_utf8_lossy(&dir),
                limits.desc >> 10
            )));
        }
        text.clear();
        entry
            .by_ref()
            .take(limits.desc)
            .read_to_string(&mut text)
            .map_err(|err| bad(&err))?;
        let desc = desc(&text);
        if let Some(error) = invalid(&dir, &desc) {
            return Err(ReadError::Bad(error));
        }
        if installed.contains(desc.name) {
            packages.insert(
                desc.name.to_owned(),
                Sync {
                    version: desc.version.to_owned(),
                    ignored: conf.ignored(desc.name, desc.groups),
                },
            );
        }
    }
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink()).map_err(|err| bad(&err))?;
    if Instant::now() >= deadline {
        return Err(ReadError::TimedOut);
    }
    Ok(packages)
}

fn expired(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out",
        ))
    } else {
        Ok(())
    }
}

/// Empty gzip members or zstd frames unpack to nothing but still take reads, so the deadline
/// is checked here, where [`Bounded`] sees no bytes.
struct Clocked<R> {
    inner: R,
    deadline: Instant,
}

impl<R: Read> Read for Clocked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        expired(self.deadline)?;
        self.inner.read(buf)
    }
}

impl<R: BufRead> BufRead for Clocked<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        expired(self.deadline)?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

const CHECK_EVERY: u64 = 256 * 1024;

struct Bounded<R> {
    inner: R,
    read: u64,
    limit: u64,
    deadline: Instant,
    next_check: u64,
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.read >= self.next_check {
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out",
                ));
            }
            self.next_check = self.read + CHECK_EVERY;
        }
        let count = self.inner.read(buf)?;
        self.read += count as u64;
        if self.read > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("more than {} KiB unpacked", self.limit >> 10),
            ));
        }
        Ok(count)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Format {
    Gzip,
    Zstd,
    Tar,
    Other(&'static str),
}

impl Format {
    fn of(head: &[u8]) -> Self {
        const MAGIC: &[(&[u8], &str)] = &[
            (b"\xfd7zXZ\0", "xz"),
            (b"BZh", "bzip2"),
            (b"\x04\x22\x4d\x18", "lz4"),
            (b"LZIP", "lzip"),
            (b"\x1f\x9d", "compress"),
            (b"\x89LZO", "lzop"),
        ];
        if head.starts_with(b"\x1f\x8b") {
            Self::Gzip
        } else if head.starts_with(b"\x28\xb5\x2f\xfd") {
            Self::Zstd
        } else if head.get(257..262) == Some(b"ustar") {
            Self::Tar
        } else {
            let name = MAGIC
                .iter()
                .find(|(magic, _)| head.starts_with(magic))
                .map_or("an unknown", |(_, name)| name);
            Self::Other(name)
        }
    }
}

/// ruzstd's `StreamingDecoder` reads one frame only.
struct Zstd<R> {
    source: R,
    frame: FrameDecoder,
    done: bool,
    deadline: Instant,
}

impl<R: BufRead> Zstd<R> {
    fn new(mut source: R, deadline: Instant) -> io::Result<Self> {
        let mut frame = FrameDecoder::new();
        let started = start_frame(&mut frame, &mut source, deadline)?;
        Ok(Self { source, frame, done: !started, deadline })
    }
}

fn start_frame(
    frame: &mut FrameDecoder,
    source: &mut impl BufRead,
    deadline: Instant,
) -> io::Result<bool> {
    loop {
        expired(deadline)?;
        if source.fill_buf()?.is_empty() {
            return Ok(false);
        }
        match frame.reset(&mut *source) {
            Ok(()) => return Ok(true),
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                let skipped = io::copy(
                    &mut source.by_ref().take(u64::from(length)),
                    &mut io::sink(),
                )?;
                if skipped != u64::from(length) {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
            Err(err) => return Err(io::Error::other(err)),
        }
    }
}

impl<R: BufRead> Read for Zstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.done || buf.is_empty() {
                return Ok(0);
            }
            expired(self.deadline)?;
            if self.frame.can_collect() > 0 {
                return self.frame.read(buf);
            }
            if self.frame.is_finished() {
                if let Some(expected) = self.frame.get_checksum_from_data()
                    && self.frame.get_calculated_checksum() != Some(expected)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "zstd checksum mismatch",
                    ));
                }
                self.done = !start_frame(&mut self.frame, &mut self.source, self.deadline)?;
                continue;
            }
            self.frame
                .decode_blocks(
                    &mut self.source,
                    BlockDecodingStrategy::UptoBlocks(1),
                )
                .map_err(io::Error::other)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_stops_reading() {
        let path = std::env::temp_dir().join(format!("deskshell-db-{}.db", std::process::id()));
        let mut tar = vec![0u8; 512 * 4];
        tar[257..262].copy_from_slice(b"ustar");
        fs::write(&path, &tar).unwrap();
        let conf = PacmanConf {
            db_path: path.clone(),
            arch: String::from("x86_64"),
            ignore_pkg: Vec::new(),
            ignore_group: Vec::new(),
            repos: Vec::new(),
        };
        let installed = HashSet::new();
        let limits = Limits::default();
        let read = read_sync(&path, &installed, &conf, &limits, Instant::now());
        assert_eq!(read, Err(ReadError::TimedOut));

        let mut slow = Bounded {
            inner: io::repeat(0),
            read: 0,
            limit: u64::MAX,
            deadline: Instant::now() + std::time::Duration::from_millis(50),
            next_check: 0,
        };
        let error = io::copy(&mut slow, &mut io::sink()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn descs() {
        let text = "%FILENAME%\nfoo-1.0-1-x86_64.pkg.tar.zst\n\n%NAME%\nfoo\n\n%BASE%\nfoo\n\n\
                    %VERSION%\n1:1.0-1\n\n%GROUPS%\nbase-devel\nxorg\n\n%DEPENDS%\nbar\n\n";
        assert_eq!(
            desc(text),
            Desc { name: "foo", version: "1:1.0-1", groups: "base-devel\nxorg" }
        );
        assert_eq!(desc(""), Desc::default());
        assert_eq!(
            desc("%NAME%\nfoo"),
            Desc { name: "foo", ..Desc::default() }
        );
    }

    #[test]
    fn formats() {
        assert_eq!(Format::of(b"\x1f\x8b\x08"), Format::Gzip);
        assert_eq!(Format::of(b"\x28\xb5\x2f\xfd\x00"), Format::Zstd);
        assert_eq!(Format::of(b"\xfd7zXZ\0rest"), Format::Other("xz"));
        assert_eq!(Format::of(b"BZh91AY"), Format::Other("bzip2"));
        assert_eq!(Format::of(b"<html>"), Format::Other("an unknown"));
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(Format::of(&tar), Format::Tar);
    }
}
