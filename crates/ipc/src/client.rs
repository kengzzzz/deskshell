//! The blocking control socket client.

use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::Reply;
use crate::server::NO_ANSWER;
use crate::wire::{self, MAX_REPLY};

/// Longer than the server's handler timeout, so the client normally sees the server's error.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("No running instances for {}: {source}", .path.display())]
    NotRunning { path: PathBuf, source: io::Error },
    #[error("No reply within {} ms.", .0.as_millis())]
    Timeout(Duration),
    #[error("The shell closed the connection without replying.")]
    Closed,
    #[error("{0}")]
    Server(String),
    #[error("Malformed reply from the shell.")]
    Malformed,
    #[error("IPC I/O error: {0}")]
    Io(#[from] io::Error),
}

impl ClientError {
    /// Whether the call ran out of time, on the client's side or in the server's handler
    /// timeout. The shell was reached in both cases.
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Timeout(_) => true,
            Self::Server(message) => message.starts_with(NO_ANSWER),
            _ => false,
        }
    }
}

pub fn call(
    path: &Path,
    target: &str,
    function: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Reply, ClientError> {
    let deadline = Instant::now() + timeout;
    let stream = connect(path, timeout.min(CONNECT_LIMIT))?;
    call_on(stream, target, function, args, deadline, timeout)
}

pub const CONNECT_LIMIT: Duration = Duration::from_secs(2);
const CONNECT_RETRY: Duration = Duration::from_millis(10);

/// A server that does not accept in time counts as not running, so a caller with a
/// fallback runs it.
pub fn connect(path: &Path, limit: Duration) -> Result<UnixStream, ClientError> {
    let not_running = |source| ClientError::NotRunning { path: path.to_owned(), source };
    let deadline = Instant::now() + limit;
    loop {
        match connect_once(path) {
            Ok(stream) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if Instant::now() + CONNECT_RETRY > deadline {
                    return Err(not_running(io::Error::new(
                        ErrorKind::TimedOut,
                        format!(
                            "the shell accepted no connection within {} ms",
                            limit.as_millis()
                        ),
                    )));
                }
                std::thread::sleep(CONNECT_RETRY);
            }
            Err(err) => return Err(not_running(err)),
        }
    }
}

/// A full backlog fails with `EAGAIN` at once.
fn connect_once(path: &Path) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain data, for which all zeroes is valid.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "the socket path does not fit a Unix socket address",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    // SAFETY: socket has no memory preconditions. A valid fd is owned at once.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a new socket that nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: address is an initialised sockaddr_un and length is within it.
    let done = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            length as libc::socklen_t,
        )
    };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(socket))
}

pub fn call_on(
    mut stream: UnixStream,
    target: &str,
    function: &str,
    args: &[&str],
    deadline: Instant,
    timeout: Duration,
) -> Result<Reply, ClientError> {
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(ClientError::Timeout(timeout))
    };
    let request = wire::request_frame(target, function, args);

    stream.set_write_timeout(Some(remaining()?))?;
    match stream.write_all(&request) {
        Ok(()) => {}
        // The server closes early when it rejects a request, but its error reply is
        // already waiting to be read.
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
            ) => {}
        Err(err) => return Err(io_error(err, timeout)),
    }

    stream.set_read_timeout(Some(remaining()?))?;
    let mut reader = BufReader::with_capacity(512, &stream);
    let mut header = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|err| io_error(err, timeout))?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_REPLY {
        return Err(ClientError::Malformed);
    }
    let mut payload = vec![0; len];
    reader
        .read_exact(&mut payload)
        .map_err(|err| io_error(err, timeout))?;
    match wire::decode_reply(&payload) {
        Some(Ok(reply)) => Ok(reply),
        Some(Err(message)) => Err(ClientError::Server(message)),
        None => Err(ClientError::Malformed),
    }
}

fn io_error(err: io::Error, timeout: Duration) -> ClientError {
    match err.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => ClientError::Timeout(timeout),
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => {
            ClientError::Closed
        }
        _ => ClientError::Io(err),
    }
}
