//! Temporary directories for the unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "deskshell-auth-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// mlock splits locked pages into a mapping of their own, so its smaps `VmFlags` show it.
/// `None` when `RLIMIT_MEMLOCK` leaves no room to lock.
pub fn page_locked(ptr: *const u8) -> Option<bool> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes one rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &raw mut limit) } != 0
        || limit.rlim_cur < 1 << 16
    {
        return None;
    }
    let addr = ptr as usize;
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
    let mut inside = false;
    for line in smaps.lines() {
        if let Some((range, _)) = line.split_once(' ')
            && let Some((start, end)) = range.split_once('-')
            && let (Ok(start), Ok(end)) = (
                usize::from_str_radix(start, 16),
                usize::from_str_radix(end, 16),
            )
        {
            inside = (start..end).contains(&addr);
            continue;
        }
        if inside && let Some(flags) = line.strip_prefix("VmFlags:") {
            return Some(
                flags
                    .split_whitespace()
                    .any(|flag| flag == "lo"),
            );
        }
    }
    Some(false)
}

#[cfg(feature = "async")]
pub fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a NUL-terminated string that outlives the call.
    let made = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(made, 0, "{}", std::io::Error::last_os_error());
}

/// Root opens files whatever their mode says.
#[cfg(feature = "async")]
pub fn is_root() -> bool {
    // SAFETY: geteuid takes nothing and cannot fail.
    unsafe { libc::geteuid() == 0 }
}
