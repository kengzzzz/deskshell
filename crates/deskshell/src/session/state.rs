//! The files a session keeps under `$XDG_RUNTIME_DIR/deskshell`.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct State {
    pub pid: u32,
    /// The start time tells the login process from a later process with the same PID.
    #[serde(default)]
    pub start: u64,
    pub restore: BTreeMap<String, Option<String>>,
}

pub(super) struct Files {
    dir: PathBuf,
}

impl Files {
    pub fn new(runtime_dir: &Path) -> Self {
        Self { dir: runtime_dir.join("deskshell") }
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join("session.state")
    }

    pub fn seat_path(&self) -> PathBuf {
        self.dir.join("session.env")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("session.lock")
    }

    /// An exclusive `flock`, which the kernel releases when the process ends, also by SIGKILL.
    pub fn try_lock(&self) -> io::Result<Option<File>> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.lock_path())?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }

    /// Each file goes to a temporary name first and is then renamed, so a reader never sees half a
    /// file.
    pub fn write(&self, state: &State, seat: &str) -> io::Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let json = serde_json::to_string(state).map_err(io::Error::other)?;
        replace(&self.seat_path(), seat.as_bytes())?;
        replace(&self.state_path(), json.as_bytes())
    }

    pub fn read(&self) -> Result<Option<State>, String> {
        let path = self.state_path();
        let text = match fs::read(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
        };
        serde_json::from_slice(&text)
            .map(Some)
            .map_err(|err| format!("{} is not a state file: {err}", path.display()))
    }

    pub fn remove(&self) -> io::Result<()> {
        for path in [self.state_path(), self.seat_path()] {
            match fs::remove_file(&path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        Ok(())
    }
}

fn replace(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".new");
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_state_file_reads_back_as_written() {
        let dir = std::env::temp_dir().join(format!(
            "dsh-session-state-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let files = Files::new(&dir);
        assert_eq!(files.read(), Ok(None));
        let state = State {
            pid: 1375,
            start: 4242,
            restore: BTreeMap::from([
                ("PATH".to_owned(), Some("/usr/bin".to_owned())),
                ("WAYLAND_DISPLAY".to_owned(), None),
                (
                    "ODD".to_owned(),
                    Some("a=b \"c\"\n\t$d".to_owned()),
                ),
            ]),
        };
        files
            .write(&state, "XDG_SEAT=seat0\n")
            .unwrap();
        assert_eq!(files.read(), Ok(Some(state)));
        assert_eq!(
            fs::read_to_string(files.seat_path()).unwrap(),
            "XDG_SEAT=seat0\n"
        );
        let mode = fs::metadata(dir.join("deskshell"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        files.remove().unwrap();
        files.remove().unwrap();
        assert_eq!(files.read(), Ok(None));
        assert!(!files.seat_path().exists());
        fs::write(files.state_path(), "{").unwrap();
        assert!(files.read().is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    // `flock` locks belong to an open file description, so two opens in one process
    // exclude each other as two processes do.
    #[test]
    fn the_lock_is_exclusive_until_it_is_dropped() {
        let dir = std::env::temp_dir().join(format!("dsh-session-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let files = Files::new(&dir);
        let held = files.try_lock().unwrap();
        assert!(held.is_some());
        assert!(files.try_lock().unwrap().is_none());
        drop(held);
        // Another test's child may hold a copy of the descriptor between its fork and exec.
        let deadline = Instant::now() + Duration::from_secs(2);
        while files.try_lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "free again");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mode = fs::metadata(files.lock_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}
