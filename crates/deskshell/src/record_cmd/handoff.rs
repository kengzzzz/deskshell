//! The recorder's target, handed to the `share-picker` client so no picker shows.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use rustix::fs::OFlags;

use super::COMM;
use super::procfs;

pub const TARGET: &str = "target";
pub const MAX_AGE: Duration = Duration::from_secs(5);
const OWNER_COMM: &str = "deskshell";
const MAX_LEN: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff<'a> {
    pub recorder: Proc,
    pub owner: Proc,
    pub target: &'a str,
}

/// A pid and its start time, which together outlive pid reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub start: u64,
}

impl Proc {
    pub fn of(pid: u32, proc_root: &Path) -> Option<Self> {
        procfs::read(pid, proc_root)
            .filter(|info| info.alive)
            .map(|info| Self { pid, start: info.start })
    }

    fn parse(line: &str) -> Option<Self> {
        let (pid, start) = line.trim().split_once(' ')?;
        Some(Self { pid: pid.parse().ok()?, start: start.parse().ok()? })
    }
}

pub fn dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir
        .join("deskshell")
        .join("record")
}

/// Creates the state dir with mode 0700, and checks that an existing one is ours with that mode.
pub fn ensure_dir(runtime_dir: &Path, uid: u32) -> io::Result<PathBuf> {
    let dir = dir(runtime_dir);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{} is not a directory of this user",
                dir.display()
            ),
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Written as a new file with mode 0600, then renamed into place.
pub fn write(dir: &Path, handoff: &Handoff<'_>) -> io::Result<()> {
    let temp = dir.join(format!(".{TARGET}.{}", std::process::id()));
    let Handoff { recorder, owner, target } = handoff;
    let text = format!(
        "{} {}\n{} {}\n{target}\n",
        recorder.pid, recorder.start, owner.pid, owner.start
    );
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| fs::rename(&temp, dir.join(TARGET)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// xdph tells the picker nothing about the app that asked, so a handoff counts only when it is
/// private, fresh, names a running recorder, and was claimed by an atomic rename.
pub fn take(dir: &Path, now: SystemTime, uid: u32, proc_root: &Path) -> Option<String> {
    let claimed = claim(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(&claimed);
    let text = file
        .ok()
        .and_then(|file| read_fresh(file, now, uid));
    let _ = fs::remove_file(&claimed);
    let text = text?;
    let mut lines = text.lines();
    let recorder = Proc::parse(lines.next()?)?;
    let owner = Proc::parse(lines.next()?)?;
    let target = lines.next()?.trim();
    if target.is_empty() || !genuine(recorder, owner, proc_root) {
        return None;
    }
    Some(target.to_owned())
}

fn claim(dir: &Path) -> Option<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let claimed = dir.join(format!(
        ".claim.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::rename(dir.join(TARGET), &claimed)
        .ok()
        .map(|()| claimed)
}

// The recorder must run with the start time it names, as a child of the `deskshell` owner.
fn genuine(recorder: Proc, owner: Proc, proc_root: &Path) -> bool {
    let is = |proc: Proc, comm: &str| {
        procfs::read(proc.pid, proc_root)
            .filter(|info| info.alive && info.start == proc.start && info.comm == comm)
    };
    is(recorder, COMM).is_some_and(|info| info.ppid == owner.pid) && is(owner, OWNER_COMM).is_some()
}

fn read_fresh(file: File, now: SystemTime, uid: u32) -> Option<String> {
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != uid {
        return None;
    }
    let modified = meta.modified().ok()?;
    let fresh = match now.duration_since(modified) {
        Ok(age) => age <= MAX_AGE,
        // A clock step back by a little still counts as fresh.
        Err(ahead) => ahead.duration() <= Duration::from_secs(1),
    };
    if !fresh {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_LEN)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Barrier};

    use super::*;

    pub(crate) struct Temp(pub PathBuf);

    impl Temp {
        pub fn new(name: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "dsh-handoff-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn uid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    pub(crate) const fn p(pid: u32) -> Proc {
        Proc { pid, start: pid as u64 * 10 }
    }

    pub(crate) const RECORDER: Proc = p(4100);
    pub(crate) const OWNER: Proc = p(4000);

    pub(crate) fn proc_tree(root: &Path) -> PathBuf {
        let proc = root.join("proc");
        for (pid, ppid, comm) in [
            (OWNER.pid, 1, OWNER_COMM),
            (RECORDER.pid, OWNER.pid, COMM),
            (4200, 1, "obs"),
            (4300, 4200, COMM),
        ] {
            let dir = proc.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("stat"),
                procfs::stat_line(pid, comm, ppid, p(pid).start),
            )
            .unwrap();
            fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        }
        proc
    }

    pub(crate) fn handoff(target: &str) -> Handoff<'_> {
        Handoff { recorder: RECORDER, owner: OWNER, target }
    }

    #[test]
    fn a_fresh_handoff_of_a_running_recorder_is_used_once() {
        let temp = Temp::new("fresh");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        assert_eq!(
            fs::metadata(&dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        write(&dir, &handoff("address:94299")).unwrap();
        let meta = fs::metadata(dir.join(TARGET)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let now = SystemTime::now();
        assert_eq!(
            take(&dir, now, uid(), &proc).as_deref(),
            Some("address:94299")
        );
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing left"
        );
        assert_eq!(take(&dir, now, uid(), &proc), None, "used once");
    }

    #[test]
    fn a_handoff_needs_its_recorder_running_under_deskshell() {
        let temp = Temp::new("genuine");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        for (recorder, owner, why) in [
            (p(4999), OWNER, "the recorder is gone"),
            (p(4200), p(1), "not a recorder"),
            (RECORDER, p(4200), "another parent"),
            (p(4300), p(4200), "a recorder another app started"),
            (
                Proc { start: 7, ..RECORDER },
                OWNER,
                "the recorder's pid was reused",
            ),
            (
                RECORDER,
                Proc { start: 7, ..OWNER },
                "the owner's pid was reused",
            ),
        ] {
            write(
                &dir,
                &Handoff { recorder, owner, target: "screen:DP-1" },
            )
            .unwrap();
            assert_eq!(
                take(&dir, SystemTime::now(), uid(), &proc),
                None,
                "{why}"
            );
            assert_eq!(
                fs::read_dir(&dir).unwrap().count(),
                0,
                "{why}: removed"
            );
        }
    }

    #[test]
    fn an_old_handoff_is_removed_unused() {
        let temp = Temp::new("old");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        write(&dir, &handoff("screen:DP-1")).unwrap();
        let now = SystemTime::now();
        File::options()
            .write(true)
            .open(dir.join(TARGET))
            .unwrap()
            .set_modified(now - Duration::from_secs(6))
            .unwrap();
        assert_eq!(take(&dir, now, uid(), &proc), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);

        write(&dir, &handoff("screen:DP-1")).unwrap();
        assert_eq!(
            take(&dir, now + Duration::from_secs(5), uid(), &proc).as_deref(),
            Some("screen:DP-1"),
            "5 s is still fresh"
        );
    }

    #[test]
    fn a_symlink_is_removed_and_its_target_kept() {
        let temp = Temp::new("link");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        let elsewhere = temp.0.join("elsewhere");
        fs::write(
            &elsewhere,
            format!(
                "{} {}\n{} {}\nscreen:DP-1\n",
                RECORDER.pid, RECORDER.start, OWNER.pid, OWNER.start
            ),
        )
        .unwrap();
        symlink(&elsewhere, dir.join(TARGET)).unwrap();
        assert_eq!(take(&dir, SystemTime::now(), uid(), &proc), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert!(elsewhere.exists());
    }

    #[test]
    fn another_owner_or_a_broken_file_is_ignored() {
        let temp = Temp::new("owner");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        write(&dir, &handoff("screen:DP-1")).unwrap();
        assert_eq!(
            take(&dir, SystemTime::now(), uid() + 1, &proc),
            None
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        for text in [
            "",
            "screen:DP-1\n",
            "4100 41000\n4000 40000\n\n",
            "4100\n4000\nscreen:DP-1\n",
            "x 1\n4000 40000\nscreen:DP-1\n",
        ] {
            fs::write(dir.join(TARGET), text).unwrap();
            assert_eq!(
                take(&dir, SystemTime::now(), uid(), &proc),
                None,
                "{text:?}"
            );
        }
    }

    #[test]
    fn two_pickers_at_once_use_it_once() {
        let temp = Temp::new("race");
        let proc = proc_tree(&temp.0);
        let dir = ensure_dir(&temp.0, uid()).unwrap();
        for round in 0..50 {
            write(&dir, &handoff("screen:DP-1")).unwrap();
            let barrier = Arc::new(Barrier::new(2));
            #[expect(
                clippy::needless_collect,
                reason = "both threads must run before a join"
            )]
            let pickers: Vec<_> = (0..2)
                .map(|_| {
                    let (dir, proc, barrier) = (dir.clone(), proc.clone(), Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        barrier.wait();
                        take(&dir, SystemTime::now(), uid(), &proc)
                    })
                })
                .collect();
            let used = pickers
                .into_iter()
                .filter_map(|picker| picker.join().unwrap())
                .count();
            assert_eq!(used, 1, "round {round}");
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_loose_state_dir_is_tightened() {
        let temp = Temp::new("mode");
        let dir = dir(&temp.0);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_dir(&temp.0, uid()).unwrap();
        assert_eq!(
            fs::metadata(&dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(ensure_dir(&temp.0, uid() + 1).is_err());
    }
}
