//! One `record` at a time: an exclusive `flock` on `record/lock`, which names the owner.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::fs::OFlags;
use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};

use super::handoff::Proc;
use super::procfs;

pub const LOCK: &str = "lock";
const PUBLISH_WAIT: Duration = Duration::from_millis(500);
const PUBLISH_RETRY: Duration = Duration::from_millis(20);

/// Dropping it empties the file through the held descriptor first, so a press never finds a
/// previous owner's pid.
#[derive(Debug)]
pub struct Owner(File);

pub fn take(dir: &Path) -> io::Result<Option<Owner>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Owner(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

impl Owner {
    pub fn publish(&mut self, me: Proc) -> io::Result<()> {
        self.0.set_len(0)?;
        self.0.seek(SeekFrom::Start(0))?;
        writeln!(self.0, "{} {}", me.pid, me.start)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
    }
}

/// A name that fails the checks, or whose signal fails, is a previous owner. The press reads
/// the lock again for up to [`PUBLISH_WAIT`], until a new owner has written itself.
pub fn press(
    dir: &Path,
    proc_root: &Path,
    own_comm: &str,
    mut signal: impl FnMut(Proc) -> io::Result<()>,
) -> io::Result<Option<Proc>> {
    let deadline = Instant::now() + PUBLISH_WAIT;
    loop {
        let named = fs::read_to_string(dir.join(LOCK))?;
        let owner = named
            .trim()
            .split_once(' ')
            .and_then(|(pid, start)| {
                Some(Proc { pid: pid.parse().ok()?, start: start.parse().ok()? })
            })
            .filter(|owner| owner.pid > 1);
        let running = owner.filter(|owner| {
            procfs::read(owner.pid, proc_root).is_some_and(|info| {
                info.alive && info.start == owner.start && info.comm == own_comm
            })
        });
        if let Some(owner) = running
            && signal(owner).is_ok()
        {
            return Ok(Some(owner));
        }
        if Instant::now() + PUBLISH_RETRY > deadline {
            return Ok(None);
        }
        std::thread::sleep(PUBLISH_RETRY);
    }
}

/// Signals through a pidfd, which pins the process, after a last check of its start time.
pub fn send_press(owner: Proc, proc_root: &Path) -> io::Result<()> {
    let pid = i32::try_from(owner.pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::from(ErrorKind::InvalidInput))?;
    let pidfd = pidfd_open(pid, PidfdFlags::empty())?;
    if Proc::of(owner.pid, proc_root) != Some(owner) {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "the owner is gone",
        ));
    }
    pidfd_send_signal(&pidfd, Signal::USR1).map_err(io::Error::from)
}

pub fn own_comm(proc_root: &Path) -> io::Result<String> {
    procfs::read(std::process::id(), proc_root)
        .map(|info| info.comm)
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "no comm for this process"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::record_cmd::handoff::tests::Temp;

    fn me() -> Proc {
        Proc::of(std::process::id(), Path::new("/proc")).unwrap()
    }

    #[test]
    fn one_owner_at_a_time() {
        let temp = Temp::new("owner");
        let first = take(&temp.0)
            .unwrap()
            .expect("the first takes it");
        assert!(
            take(&temp.0).unwrap().is_none(),
            "a second record is a press"
        );
        drop(first);
        // Another test's child may hold a copy of the descriptor between its fork and exec.
        let deadline = Instant::now() + Duration::from_secs(2);
        while take(&temp.0).unwrap().is_none() {
            assert!(Instant::now() < deadline, "free again");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_press_signals_the_published_owner_only() {
        let temp = Temp::new("press");
        let proc = Path::new("/proc");
        let comm = own_comm(proc).unwrap();
        let mut owner = take(&temp.0).unwrap().unwrap();
        owner.publish(me()).unwrap();
        let signalled = Cell::new(None);
        let pressed = press(&temp.0, proc, &comm, |owner| {
            signalled.set(Some(owner));
            Ok(())
        })
        .unwrap();
        assert_eq!(pressed, Some(me()));
        assert_eq!(signalled.get(), Some(me()));

        owner
            .publish(Proc { start: me().start + 1, ..me() })
            .unwrap();
        let start = Instant::now();
        let pressed = press(&temp.0, proc, &comm, |_| panic!("a reused pid")).unwrap();
        assert_eq!(pressed, None);
        assert!(start.elapsed() >= PUBLISH_WAIT - PUBLISH_RETRY);

        owner.publish(me()).unwrap();
        let pressed = press(&temp.0, proc, "gpu-screen-reco", |_| {
            panic!("another program")
        })
        .unwrap();
        assert_eq!(pressed, None);
    }

    // A releases the lock and lingers for its toast, B takes it but has not written itself yet.
    // C presses: C never signals A, and signals B once B wrote itself.
    #[test]
    fn a_press_during_owner_turnover() {
        let temp = Temp::new("turnover");
        let proc = Path::new("/proc");
        let comm = own_comm(proc).unwrap();
        let a = me();
        let mut owner_a = take(&temp.0).unwrap().unwrap();
        owner_a.publish(a).unwrap();
        drop(owner_a);
        assert_eq!(
            fs::read_to_string(temp.0.join(LOCK)).unwrap(),
            "",
            "A emptied the file before it let go"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let owner_b = loop {
            if let Some(owner) = take(&temp.0).unwrap() {
                break owner;
            }
            assert!(Instant::now() < deadline, "B takes the lock");
            std::thread::sleep(Duration::from_millis(10));
        };
        let pressed = press(&temp.0, proc, &comm, |_| {
            panic!("nobody to signal")
        })
        .unwrap();
        assert_eq!(pressed, None, "B wrote nothing within the wait");

        let mut owner_b = owner_b;
        let dir = temp.0.clone();
        let b = me();
        let presser = std::thread::spawn(move || {
            let signalled = Cell::new(None);
            let pressed = press(&dir, Path::new("/proc"), &comm, |owner| {
                signalled.set(Some(owner));
                Ok(())
            })
            .unwrap();
            (pressed, signalled.get())
        });
        std::thread::sleep(Duration::from_millis(150));
        owner_b.publish(b).unwrap();
        let (pressed, signalled) = presser.join().unwrap();
        assert_eq!(pressed, Some(b));
        assert_eq!(
            signalled,
            Some(b),
            "C waited for B and signalled it"
        );
    }

    // A crashed owner that nobody reaped keeps its pid, start time and `comm` as a zombie. C's
    // press passes it by and reaches B, which holds the lock and writes itself late.
    #[test]
    fn a_press_passes_an_unreaped_owner_by() {
        let temp = Temp::new("zombie");
        let proc = Path::new("/proc");
        let mut a = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut b = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let a_proc = Proc::of(a.id(), proc).unwrap();
        let b_proc = Proc::of(b.id(), proc).unwrap();
        fs::write(
            temp.0.join(LOCK),
            format!("{} {}\n", a_proc.pid, a_proc.start),
        )
        .unwrap();
        a.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while procfs::read(a_proc.pid, proc).is_some_and(|info| info.alive) {
            assert!(
                Instant::now() < deadline,
                "A never became a zombie"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let zombie = procfs::read(a_proc.pid, proc).expect("A is not reaped");
        assert_eq!(
            (zombie.start, zombie.comm.as_str()),
            (a_proc.start, "sleep")
        );

        let mut owner_b = take(&temp.0)
            .unwrap()
            .expect("B takes the lock");
        // Taking the lock does not touch the file, so it still names A, as after a crash.
        assert!(
            fs::read_to_string(temp.0.join(LOCK))
                .unwrap()
                .starts_with(&a_proc.pid.to_string())
        );
        let dir = temp.0.clone();
        let presser = std::thread::spawn(move || {
            let mut signalled = Vec::new();
            let pressed = press(&dir, Path::new("/proc"), "sleep", |owner| {
                signalled.push(owner);
                Ok(())
            })
            .unwrap();
            (pressed, signalled)
        });
        std::thread::sleep(Duration::from_millis(150));
        owner_b.publish(b_proc).unwrap();
        let (pressed, signalled) = presser.join().unwrap();
        assert_eq!(pressed, Some(b_proc));
        assert_eq!(signalled, [b_proc], "A was never signalled");
        drop(owner_b);
        let _ = b.kill();
        let _ = a.wait();
        let _ = b.wait();
    }

    #[test]
    fn a_failed_signal_retries() {
        let temp = Temp::new("retry");
        let proc = Path::new("/proc");
        let comm = own_comm(proc).unwrap();
        let mut owner = take(&temp.0).unwrap().unwrap();
        owner.publish(me()).unwrap();
        let mut tries = 0;
        let pressed = press(&temp.0, proc, &comm, |_| {
            tries += 1;
            if tries < 3 { Err(io::Error::from(ErrorKind::NotFound)) } else { Ok(()) }
        })
        .unwrap();
        assert_eq!(pressed, Some(me()));
        assert_eq!(tries, 3);
    }

    #[test]
    fn send_press_needs_the_same_process() {
        let gone = Proc { pid: u32::MAX / 2, start: 1 };
        assert!(send_press(gone, Path::new("/proc")).is_err());
        let reused = Proc { start: me().start + 1, ..me() };
        assert!(
            send_press(reused, Path::new("/proc")).is_err(),
            "checked before the signal"
        );
    }
}
