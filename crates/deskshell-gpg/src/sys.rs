//! The system calls std and tokio don't cover, through rustix.

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use rustix::process::{Pid, PidfdFlags, Resource, Rlimit};

pub const CONTROL_LEN: usize = 4096;
/// The kernel's `SCM_MAX_FD`: at most this many descriptors go with one message.
const MAX_FDS: usize = 253;

pub struct Control(pub [MaybeUninit<u8>; CONTROL_LEN]);

impl Control {
    pub fn new() -> Box<Self> {
        Box::new(Self([MaybeUninit::uninit(); CONTROL_LEN]))
    }
}

/// Every descriptor received goes to `fds`, so none leaks. A truncated control message is an
/// error, after its descriptors were taken.
pub fn recv_msg(
    fd: BorrowedFd<'_>,
    data: &mut [u8],
    control: &mut [MaybeUninit<u8>],
    fds: &mut Vec<OwnedFd>,
) -> io::Result<usize> {
    let mut ancillary = RecvAncillaryBuffer::new(control);
    let msg = rustix::net::recvmsg(
        fd,
        &mut [IoSliceMut::new(data)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT,
    )?;
    for message in ancillary.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    if msg.flags.contains(ReturnFlags::CTRUNC) {
        return Err(io::Error::other("control message truncated"));
    }
    Ok(msg.bytes)
}

pub fn send_msg(fd: BorrowedFd<'_>, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::Error::other(
            "too many descriptors for one message",
        ));
    }
    let sent = rustix::net::sendmsg(
        fd,
        &[IoSlice::new(data)],
        &mut ancillary,
        SendFlags::NOSIGNAL | SendFlags::DONTWAIT,
    )?;
    Ok(sent)
}

pub fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    let pid = Pid::from_raw(pid).ok_or(Errno::INVAL)?;
    Ok(rustix::process::pidfd_open(
        pid,
        PidfdFlags::empty(),
    )?)
}

/// `kill(pid, 0)`: only `ESRCH` means the process is gone.
pub fn process_exists(pid: i32) -> bool {
    Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid) != Err(Errno::SRCH))
}

pub fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Core limit 0, as gpg-agent's `disable_core_dumps` does. `PR_SET_DUMPABLE` alone would not
/// stop a dump that `core_pattern` pipes to systemd-coredump while `fs.suid_dumpable` is 2.
pub fn disable_core_dumps() -> io::Result<()> {
    let none = Rlimit { current: Some(0), maximum: Some(0) };
    Ok(rustix::process::setrlimit(Resource::Core, none)?)
}

pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(rustix::fs::open(path, flags, Mode::empty())?)
}

/// Opens a file for reading without blocking on a FIFO.
pub fn open_nonblocking(path: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(rustix::fs::open(path, flags, Mode::empty())?)
}

pub fn try_lock_exclusive(fd: BorrowedFd<'_>) -> io::Result<()> {
    Ok(rustix::fs::flock(
        fd,
        FlockOperation::NonBlockingLockExclusive,
    )?)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn descriptors_pass_and_a_truncated_message_is_an_error() {
        let (a, b) = UnixStream::pair().unwrap();
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"through").unwrap();
        drop(writer);
        let sent = send_msg(a.as_fd(), b"x", &[reader.as_fd()]).unwrap();
        assert_eq!(sent, 1);
        let mut data = [0; 8];
        let mut control = Control::new();
        let mut fds = Vec::new();
        let n = recv_msg(b.as_fd(), &mut data, &mut control.0, &mut fds).unwrap();
        assert_eq!(&data[..n], b"x");
        assert_eq!(fds.len(), 1);
        let mut text = String::new();
        File::from(fds.pop().unwrap())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "through");

        let four = [reader.as_fd(), reader.as_fd(), reader.as_fd(), reader.as_fd()];
        send_msg(a.as_fd(), b"y", &four).unwrap();
        // One header and room for two descriptors at most.
        let mut small = [MaybeUninit::uninit(); 24];
        let err = recv_msg(b.as_fd(), &mut data, &mut small, &mut fds).unwrap_err();
        assert_eq!(err.to_string(), "control message truncated");
        assert!(
            fds.len() <= 2,
            "what fit was taken, so dropping it closes it"
        );
        drop(fds);
        reader
            .read_to_end(&mut Vec::new())
            .unwrap();
    }

    #[test]
    fn processes() {
        assert!(process_exists(std::process::id() as i32));
        assert!(!process_exists(0));
        assert!(pidfd_open(std::process::id() as i32).is_ok());
        assert!(pidfd_open(0).is_err());
    }
}
