//! The install lock, so a start and a `--restore` never rename in the socket dir at once.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::install::Names;
use crate::sys;

#[derive(Debug)]
pub struct Lock {
    _fd: OwnedFd,
}

#[derive(Debug)]
pub enum LockError {
    Held,
    Failed(io::Error),
}

impl Lock {
    /// An `flock` on the directory itself creates nothing there, with a lock file as fallback.
    /// The kernel drops it when the process dies, so `--restore` after a crash always gets it.
    pub fn try_acquire(agent_socket: &Path) -> Result<Self, LockError> {
        let dir = match agent_socket.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let dir = sys::open_dir(dir).map_err(LockError::Failed)?;
        match lock(dir) {
            Err(LockError::Failed(err)) => {
                tracing::debug!(%err, "cannot lock the socket directory, using a lock file");
                let mut name = Names::new(agent_socket)
                    .backend
                    .into_os_string();
                name.push(".lock");
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(name)
                    .map_err(LockError::Failed)?;
                lock(file.into())
            }
            locked => locked,
        }
    }
}

fn lock(fd: OwnedFd) -> Result<Lock, LockError> {
    match sys::try_lock_exclusive(fd.as_fd()) {
        Ok(()) => Ok(Lock { _fd: fd }),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Err(LockError::Held),
        Err(err) => Err(LockError::Failed(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::{TempDir, forking};
    use std::assert_matches;

    #[test]
    fn one_holder_at_a_time_and_nothing_created() {
        let _fork = forking();
        let dir = TempDir::new("lock");
        let socket = dir.path().join("S.gpg-agent");
        let first = Lock::try_acquire(&socket).unwrap();
        assert_matches!(Lock::try_acquire(&socket), Err(LockError::Held));
        drop(first);
        let again = Lock::try_acquire(&socket).unwrap();
        drop(again);
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .count(),
            0,
            "no lock file"
        );
        assert_matches!(
            Lock::try_acquire(&dir.path().join("missing/S.gpg-agent")),
            Err(LockError::Failed(_))
        );
    }
}
