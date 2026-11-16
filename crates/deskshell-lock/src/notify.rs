//! systemd's notify protocol over `$NOTIFY_SOCKET`, without libsystemd.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, IoSlice};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use rustix::net::{
    self, AddressFamily, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix,
    SocketFlags, SocketType,
};
use rustix::pipe::{self, PipeFlags};

/// Without systemd's confirmation in this time the lock stays locked. systemd answers in ms.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(1);

pub fn socket() -> Option<OsString> {
    env::var_os("NOTIFY_SOCKET").filter(|socket| !socket.is_empty())
}

pub fn ready() -> io::Result<bool> {
    match socket() {
        Some(socket) => send(&socket, b"READY=1").map(|()| true),
        None => Ok(false),
    }
}

/// A stop after which the unlock may go out.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// systemd closed the barrier's pipe, and it handles a barrier only after every message
    /// sent before it, so it has processed `STOPPING=1`.
    Confirmed,
    /// No `$NOTIFY_SOCKET`, so no unit runs this lock.
    NoUnit,
}

/// Sends `STOPPING=1` and waits on a `BARRIER=1` pipe, as `sd_notify_barrier` does, so the unlock
/// goes out only after systemd processed it. Else a racing `start` could find the unit active.
pub fn stop(socket: Option<&OsStr>, timeout: Duration) -> io::Result<Stopped> {
    stop_with(socket, timeout, false)
}

/// [`stop`], but it keeps the barrier write end, so the wait times out. Tests the fail-closed path.
#[cfg(debug_assertions)]
pub fn stop_unconfirmed(socket: Option<&OsStr>, timeout: Duration) -> io::Result<Stopped> {
    stop_with(socket, timeout, true)
}

fn stop_with(
    socket: Option<&OsStr>,
    timeout: Duration,
    keep_write_end: bool,
) -> io::Result<Stopped> {
    let Some(socket) = socket.filter(|socket| !socket.is_empty()) else {
        return Ok(Stopped::NoUnit);
    };
    let deadline = Instant::now() + timeout;
    let address = address(socket)?;
    let sender = unbound()?;
    send_message(&sender, &address, b"STOPPING=1", None)?;
    let (read_end, write_end) = pipe::pipe_with(PipeFlags::CLOEXEC)?;
    send_message(
        &sender,
        &address,
        b"BARRIER=1",
        Some(write_end.as_fd()),
    )?;
    let _kept = if keep_write_end {
        Some(write_end)
    } else {
        drop(write_end);
        None
    };
    wait_for_hangup(&read_end, deadline)?;
    Ok(Stopped::Confirmed)
}

/// Sends one datagram to `socket`, a path or an abstract name starting with `@`.
pub fn send(socket: &OsStr, message: &[u8]) -> io::Result<()> {
    send_message(&unbound()?, &address(socket)?, message, None)
}

fn address(socket: &OsStr) -> io::Result<SocketAddrUnix> {
    let address = match socket.as_bytes().strip_prefix(b"@") {
        Some(name) => SocketAddrUnix::new_abstract_name(name)?,
        None => SocketAddrUnix::new(socket)?,
    };
    Ok(address)
}

fn unbound() -> io::Result<OwnedFd> {
    Ok(net::socket_with(
        AddressFamily::UNIX,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        None,
    )?)
}

/// Never blocks: a full queue at systemd is an error, not a wait past the deadline.
fn send_message(
    sender: &OwnedFd,
    address: &SocketAddrUnix,
    message: &[u8],
    fd: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    let fds = fd.as_slice();
    if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::Error::other("no room for the descriptor"));
    }
    let sent = net::sendmsg_addr(
        sender,
        address,
        &[IoSlice::new(message)],
        &mut control,
        SendFlags::DONTWAIT,
    )?;
    if sent != message.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "the datagram went out short",
        ));
    }
    Ok(())
}

/// Polls with no events, so only the hangup (or an error) wakes it, as
/// `sd_notify_barrier` does. A signal restarts the poll with the time left.
fn wait_for_hangup(read_end: &OwnedFd, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let left = Timespec::try_from(left).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(read_end, PollFlags::empty())];
        match rustix::event::poll(&mut fds, Some(&left)) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "systemd did not confirm the stop in time",
                ));
            }
            Ok(_)
                if fds[0]
                    .revents()
                    .contains(PollFlags::HUP) =>
            {
                return Ok(());
            }
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "unexpected poll events {:?} on the barrier pipe",
                    fds[0].revents()
                )));
            }
            Err(Errno::INTR) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::IoSliceMut;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::path::PathBuf;
    use std::thread::{self, JoinHandle};

    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "deskshell-lock-notify-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    type Received = Vec<(String, usize)>;

    /// A fake systemd that closes each received descriptor at once, as systemd does with a
    /// barrier's, or with `keep` holds them until the test joins it.
    fn manager(
        socket: UnixDatagram,
        count: usize,
        keep: bool,
    ) -> JoinHandle<(Received, Vec<OwnedFd>)> {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        thread::spawn(move || {
            let mut received = Vec::new();
            let mut kept = Vec::new();
            for _ in 0..count {
                let mut payload = [0; 64];
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
                let mut control = RecvAncillaryBuffer::new(&mut space);
                let message = net::recvmsg(
                    &socket,
                    &mut [IoSliceMut::new(&mut payload)],
                    &mut control,
                    RecvFlags::CMSG_CLOEXEC,
                )
                .unwrap();
                let mut fds = Vec::new();
                for item in control.drain() {
                    if let RecvAncillaryMessage::ScmRights(rights) = item {
                        fds.extend(rights);
                    }
                }
                let text = String::from_utf8_lossy(&payload[..message.bytes]).into_owned();
                received.push((text, fds.len()));
                if keep {
                    kept.extend(fds);
                }
            }
            (received, kept)
        })
    }

    fn expected() -> Received {
        vec![(String::from("STOPPING=1"), 0), (String::from("BARRIER=1"), 1)]
    }

    #[test]
    fn sends_to_a_path_and_to_an_abstract_name() {
        let dir = temp("send");
        let path = dir.join("notify");
        let server = UnixDatagram::bind(&path).unwrap();
        send(path.as_os_str(), b"READY=1").unwrap();
        let mut buffer = [0; 16];
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"READY=1");

        let name = format!("deskshell-lock-test-{}", std::process::id());
        let server =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
                .unwrap();
        let socket = std::ffi::OsString::from_vec(format!("@{name}").into_bytes());
        send(&socket, b"READY=1").unwrap();
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"READY=1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_is_confirmed_once_systemd_closes_the_barrier_fd() {
        let dir = temp("confirmed");
        let path = dir.join("notify");
        let fake = manager(UnixDatagram::bind(&path).unwrap(), 2, false);
        let stopped = stop(Some(path.as_os_str()), STOP_TIMEOUT).unwrap();
        assert_eq!(stopped, Stopped::Confirmed);
        let (received, _) = fake.join().unwrap();
        assert_eq!(received, expected());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_reaches_an_abstract_socket() {
        let name = format!("deskshell-lock-stop-{}", std::process::id());
        let server =
            UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
                .unwrap();
        let fake = manager(server, 2, false);
        let socket = std::ffi::OsString::from_vec(format!("@{name}").into_bytes());
        assert_eq!(
            stop(Some(&socket), STOP_TIMEOUT).unwrap(),
            Stopped::Confirmed
        );
        assert_eq!(fake.join().unwrap().0, expected());
    }

    #[test]
    fn stop_times_out_while_systemd_keeps_the_barrier_fd() {
        let dir = temp("kept");
        let path = dir.join("notify");
        let fake = manager(UnixDatagram::bind(&path).unwrap(), 2, true);
        let timeout = Duration::from_millis(200);
        let started = Instant::now();
        let err = stop(Some(path.as_os_str()), timeout).unwrap_err();
        let waited = started.elapsed();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(waited >= timeout, "{waited:?}");
        assert!(waited < STOP_TIMEOUT, "{waited:?}");
        let (received, kept) = fake.join().unwrap();
        assert_eq!(received, expected());
        assert_eq!(kept.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_fails_when_the_send_fails() {
        let dir = temp("refused");
        let path = dir.join("nobody");
        let err = stop(Some(path.as_os_str()), STOP_TIMEOUT).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::TimedOut, "{err}");

        // A dead socket file made with mknod. A socket bound and dropped here could live on in a
        // parallel test's fork until it execs, take both messages and hang up the barrier.
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Socket,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        assert!(stop(Some(path.as_os_str()), STOP_TIMEOUT).is_err());

        let socket = std::ffi::OsString::from_vec(
            format!("@deskshell-lock-nobody-{}", std::process::id()).into_bytes(),
        );
        assert!(stop(Some(&socket), STOP_TIMEOUT).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_notify_socket_means_no_unit() {
        assert_eq!(stop(None, STOP_TIMEOUT).unwrap(), Stopped::NoUnit);
        assert_eq!(
            stop(Some(OsStr::new("")), STOP_TIMEOUT).unwrap(),
            Stopped::NoUnit
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_test_hook_times_out_although_systemd_closes_its_copy() {
        let dir = temp("hook");
        let path = dir.join("notify");
        let fake = manager(UnixDatagram::bind(&path).unwrap(), 2, false);
        let err = stop_unconfirmed(Some(path.as_os_str()), Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(fake.join().unwrap().0, expected());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
