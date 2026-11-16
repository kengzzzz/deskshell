//! The game boost: performance while a Steam game runs, and the earlier mode after it.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{Mode, OFlags, RawDir};

use crate::model::PowerMode;

pub const INTERVAL: Duration = Duration::from_secs(2);

/// A process with a huge `cmdline` or `environ` cannot make a scan allocate without bound.
pub const READ_LIMIT: usize = 128 * 1024;

/// `comm` holds at most 16 bytes.
const COMM_LIMIT: usize = 64;

#[derive(Debug)]
pub struct Scanner {
    proc_root: PathBuf,
    path: PathBuf,
    dents: Vec<u8>,
    buf: Vec<u8>,
}

impl Scanner {
    pub fn new(proc_root: &Path) -> Self {
        Self {
            proc_root: proc_root.to_owned(),
            path: PathBuf::new(),
            dents: Vec::with_capacity(16 * 1024),
            buf: Vec::with_capacity(4096),
        }
    }

    pub fn game_running(&mut self) -> bool {
        let Self { proc_root, path, dents, buf } = self;
        let Ok(dir) = rustix::fs::open(
            proc_root.as_path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return false;
        };
        let mut entries = RawDir::new(&dir, dents.spare_capacity_mut());
        while let Some(entry) = entries.next() {
            let Ok(entry) = entry else {
                return false;
            };
            let pid = entry.file_name().to_bytes();
            if !pid.is_empty()
                && pid.iter().all(u8::is_ascii_digit)
                && is_game(proc_root, OsStr::from_bytes(pid), path, buf)
            {
                return true;
            }
        }
        false
    }
}

fn is_game(proc_root: &Path, pid: &OsStr, path: &mut PathBuf, buf: &mut Vec<u8>) -> bool {
    path.as_mut_os_string().clear();
    path.push(proc_root);
    path.push(pid);
    path.push("comm");
    if read_into(path, buf, COMM_LIMIT).is_none() || buf.trim_suffix(b"\n") != b"reaper" {
        return false;
    }
    // A cut `cmdline` without the match is no game.
    path.set_file_name("cmdline");
    if read_into(path, buf, READ_LIMIT).is_none() || !steam_launch(buf) {
        return false;
    }
    // Another user's `environ` is closed to us, so the game counts. A cut `environ` without the
    // opt-out may hold it past the cut, so the process is skipped.
    path.set_file_name("environ");
    match try_read_into(path, buf, READ_LIMIT) {
        Ok(Contents::Whole) => !opted_out(buf),
        Ok(Contents::Cut) => false,
        Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
    }
}

enum Contents {
    Whole,
    Cut,
}

fn read_into(path: &Path, buf: &mut Vec<u8>, limit: usize) -> Option<Contents> {
    try_read_into(path, buf, limit).ok()
}

fn try_read_into(path: &Path, buf: &mut Vec<u8>, limit: usize) -> io::Result<Contents> {
    buf.clear();
    let mut file = File::open(path)?.take(limit as u64 + 1);
    file.read_to_end(buf)?;
    if buf.len() > limit {
        buf.truncate(limit);
        Ok(Contents::Cut)
    } else {
        Ok(Contents::Whole)
    }
}

fn steam_launch(cmdline: &[u8]) -> bool {
    let mut after_launch = false;
    for arg in cmdline.split(|&byte| byte == 0) {
        if after_launch && arg.starts_with(b"AppId=") {
            return true;
        }
        after_launch = arg == b"SteamLaunch";
    }
    false
}

fn opted_out(environ: &[u8]) -> bool {
    environ
        .split(|&byte| byte == 0)
        .any(|entry| entry == b"GAME_CPU_BOOST=0")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Boosted { before: Option<PowerMode> },
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boost {
    phase: Phase,
}

impl Boost {
    pub fn start(
        game: bool,
        live: Option<PowerMode>,
        saved: Option<PowerMode>,
    ) -> (Self, Option<PowerMode>) {
        let phase = match (game, live) {
            (true, Some(PowerMode::Performance)) => Phase::Boosted { before: saved },
            (true, _) => Phase::Ended,
            (false, _) => {
                let restore = saved.filter(|&saved| live != Some(saved));
                return (Self { phase: Phase::Idle }, restore);
            }
        };
        (Self { phase }, None)
    }

    pub fn scan(
        &mut self,
        game: bool,
        live: impl FnOnce() -> Option<PowerMode>,
    ) -> Option<PowerMode> {
        match (self.phase, game) {
            (Phase::Idle, true) => {
                let live = live();
                self.phase = Phase::Boosted { before: live };
                performance_unless(live)
            }
            (Phase::Boosted { before }, false) => {
                self.phase = Phase::Idle;
                let before = before
                    .filter(|before| matches!(before, PowerMode::Eco | PowerMode::Balanced))?;
                (live() == Some(PowerMode::Performance)).then_some(before)
            }
            (Phase::Ended, false) => {
                self.phase = Phase::Idle;
                None
            }
            _ => None,
        }
    }

    pub fn pick(&mut self) {
        if self.phase != Phase::Idle {
            self.phase = Phase::Ended;
        }
    }

    pub fn picked_during(&mut self, game: bool) {
        if game && self.phase == Phase::Idle {
            self.phase = Phase::Ended;
        }
    }

    pub fn boosted(&self) -> bool {
        matches!(self.phase, Phase::Boosted { .. })
    }

    pub fn game(&self) -> bool {
        self.phase != Phase::Idle
    }
}

fn performance_unless(live: Option<PowerMode>) -> Option<PowerMode> {
    (live != Some(PowerMode::Performance)).then_some(PowerMode::Performance)
}
