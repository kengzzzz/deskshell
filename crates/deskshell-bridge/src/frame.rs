//! The wire format: a field count, then each field as a length and its bytes, little-endian u32.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::net::sockopt::Timeout;
use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub fn write_fields(w: &mut impl Write, fields: &[&[u8]]) -> io::Result<()> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&len_u32(fields.len())?.to_le_bytes());
    for field in fields {
        buf.extend_from_slice(&len_u32(field.len())?.to_le_bytes());
        buf.extend_from_slice(field);
    }
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_fields(
    r: &mut impl Read,
    max_fields: usize,
    max_bytes: usize,
) -> io::Result<Vec<Vec<u8>>> {
    let count = read_u32(r)? as usize;
    if count > max_fields {
        return Err(invalid("too many fields"));
    }
    let mut total = 0usize;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u32(r)? as usize;
        total = total.saturating_add(len);
        if total > max_bytes {
            return Err(invalid("message too large"));
        }
        let mut field = vec![0; len];
        r.read_exact(&mut field)?;
        fields.push(field);
    }
    Ok(fields)
}

/// The send timeout also bounds a blocking connect to a full backlog, so a peer that stops
/// accepting can't hold the caller.
pub fn connect(path: &Path, limit: Duration) -> io::Result<UnixStream> {
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )?;
    net::sockopt::set_socket_timeout(&fd, Timeout::Send, Some(limit))?;
    match net::connect(&fd, &SocketAddrUnix::new(path)?) {
        Ok(()) => Ok(UnixStream::from(fd)),
        Err(Errno::AGAIN) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{} accepted no connection in {limit:?}",
                path.display()
            ),
        )),
        Err(e) => Err(e.into()),
    }
}

/// Reads from `stream` with one deadline for the whole message, so a peer that trickles bytes
/// can't hold the connection open past it.
pub struct Deadline<'a> {
    stream: &'a UnixStream,
    limit: Duration,
    until: Option<Instant>,
}

impl<'a> Deadline<'a> {
    pub fn new(stream: &'a UnixStream, limit: Duration) -> Self {
        Self { stream, limit, until: Some(Instant::now() + limit) }
    }

    /// No limit until the first byte, because a reply may only come when a long command ends.
    /// Once it starts it must finish within `limit`.
    pub fn after_first_byte(stream: &'a UnixStream, limit: Duration) -> Self {
        Self { stream, limit, until: None }
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(until) = self.until else {
            self.stream.set_read_timeout(None)?;
            let n = (&mut &*self.stream).read(buf)?;
            if n > 0 {
                self.until = Some(Instant::now() + self.limit);
            }
            return Ok(n);
        };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(too_slow());
        }
        self.stream
            .set_read_timeout(Some(left))?;
        match (&mut &*self.stream).read(buf) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(too_slow()),
            result => result,
        }
    }
}

fn too_slow() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "peer too slow")
}

/// Replaces control characters (ESC, C1, DEL and the rest, except newline and tab) with
/// visible escapes, so output from the other user can't drive the terminal.
pub fn printable(bytes: &[u8]) -> Vec<u8> {
    let mut out = String::with_capacity(bytes.len());
    for c in String::from_utf8_lossy(bytes).chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out.into_bytes()
}

pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let cred = net::sockopt::socket_peercred(stream.as_fd())?;
    Ok(cred.uid.as_raw())
}

pub fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn len_u32(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| invalid("field too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_controls_are_escaped() {
        let osc52 = b"ok\n\x1b]52;c;cm0gLXJmIH4K\x07done\t\xc2\x9b2J";
        assert_eq!(
            printable(osc52),
            b"ok\n\\x1b]52;c;cm0gLXJmIH4K\\x07done\t\\x9b2J".to_vec()
        );
    }

    #[test]
    fn limits_are_checked_before_reading_a_field() {
        let mut too_many: &[u8] = &[3, 0, 0, 0];
        let err = read_fields(&mut too_many, 2, 100).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let mut too_big: &[u8] = &[1, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f];
        let err = read_fields(&mut too_big, 1, 1024).unwrap_err();
        assert_eq!(err.to_string(), "message too large");

        let mut cut: &[u8] = &[1, 0, 0, 0, 4, 0, 0, 0, b'a'];
        let err = read_fields(&mut cut, 1, 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
