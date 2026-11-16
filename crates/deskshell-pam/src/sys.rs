//! libpam, the passwd lookup and process hardening, kept out of test builds.

use std::ffi::{CStr, CString, c_char, c_int};
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;

use deskshell_auth::worker::{Outcome, exit};

use crate::conv::{self, Pam};
use crate::ffi::{PAM_SUCCESS, PamConv};

const PAM_SYSTEM_ERR: c_int = 4;
const PAM_USER_UNKNOWN: c_int = 10;

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service_name: *const c_char,
        user: *const c_char,
        pam_conversation: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, pam_status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
}

struct LibPam;

impl Pam for LibPam {
    fn authenticate(&mut self, service: &CStr, user: &CStr, conv: &PamConv) -> c_int {
        let mut handle = std::ptr::null_mut();
        // SAFETY: the strings are NUL terminated, and pam_start copies `conv`. Its
        // appdata outlives the handle, which pam_end frees before this returns.
        let code = unsafe {
            pam_start(
                service.as_ptr(),
                user.as_ptr(),
                conv,
                &raw mut handle,
            )
        };
        if code != PAM_SUCCESS {
            return code;
        }
        if handle.is_null() {
            return PAM_SYSTEM_ERR;
        }
        lock_memory();
        // SAFETY: `handle` came from a successful pam_start and is ended once.
        unsafe {
            let code = pam_authenticate(handle, 0);
            pam_end(handle, code);
            code
        }
    }

    fn strerror(&self, code: c_int) -> String {
        // SAFETY: Linux-PAM ignores the handle and returns a static string.
        let text = unsafe { pam_strerror(std::ptr::null_mut(), code) };
        if text.is_null() {
            return format!("PAM error {code}");
        }
        // SAFETY: non-null strings from pam_strerror are static and NUL terminated.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

// Best effort, so the copies PAM modules make of the PIN stay out of swap. It runs after
// pam_start has loaded every module of the stack and before pam_authenticate.
fn lock_memory() {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes one rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &raw mut limit) } != 0 {
        return;
    }
    let mapped = mapped_bytes();
    let unlimited = limit.rlim_cur == libc::RLIM_INFINITY;
    if !crate::memory::lock_fits(mapped, (!unlimited).then_some(limit.rlim_cur)) {
        let _ = writeln!(
            std::io::stderr(),
            "deskshell-pam: memory not locked, {} bytes mapped and RLIMIT_MEMLOCK is {}",
            mapped.map_or_else(
                || String::from("unknown"),
                |bytes| bytes.to_string()
            ),
            limit.rlim_cur
        );
        return;
    }
    // SAFETY: mlockall takes plain flags.
    if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT) } != 0 {
        let _ = writeln!(
            std::io::stderr(),
            "deskshell-pam: mlockall failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

// The virtual size, not the resident one, is what mlockall would lock.
fn mapped_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * u64::try_from(page).ok()?)
}

/// Aborts on any panic and makes the process non-dumpable, so neither a core dump nor
/// ptrace from another process can read the PIN.
pub fn harden() {
    std::panic::set_hook(Box::new(|info| {
        let _ = writeln!(std::io::stderr(), "deskshell-pam: {info}");
        std::process::abort();
    }));
    // SAFETY: PR_SET_DUMPABLE takes plain integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        std::process::exit(i32::from(exit::PROTOCOL));
    }
}

fn user_name() -> Option<CString> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut buf: Vec<c_char> = vec![0; 1024];
    loop {
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live memory of the stated size.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        // SAFETY: on success pw_name points into `buf`, which is still alive.
        return Some(unsafe { CStr::from_ptr((*result).pw_name) }.to_owned());
    }
}

// Raw duplicates of fd 0 and 1, because Rust's stdin buffer would keep a copy of the PIN
// and its stdout is line buffered.
pub fn run(service: &CStr) -> u8 {
    let Ok(input) = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
    else {
        return exit::PROTOCOL;
    };
    let Ok(mut output) = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
    else {
        return exit::PROTOCOL;
    };
    let mut pam = LibPam;
    let Some(user) = user_name() else {
        let outcome =
            Outcome::Failure { code: PAM_USER_UNKNOWN, message: pam.strerror(PAM_USER_UNKNOWN) };
        return conv::report(&mut output, outcome);
    };
    conv::run(&mut pam, service, &user, input, output)
}
