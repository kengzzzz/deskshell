//! The pipes of a transfer, read and written without blocking inside a poll loop.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::fs::OFlags;
use rustix::io::Errno;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Fill {
    More,
    Done,
    TooLarge,
    Failed(Errno),
}

/// `buf` may hold at most `cap` bytes.
pub(super) fn fill(fd: BorrowedFd<'_>, buf: &mut Vec<u8>, cap: usize, scratch: &mut [u8]) -> Fill {
    loop {
        match rustix::io::read(fd, &mut *scratch) {
            Ok(0) => return Fill::Done,
            Ok(read) => {
                if buf.len() + read > cap {
                    return Fill::TooLarge;
                }
                buf.extend_from_slice(&scratch[..read]);
            }
            Err(Errno::AGAIN) => return Fill::More,
            Err(Errno::INTR) => {}
            Err(errno) => return Fill::Failed(errno),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pump {
    Again,
    /// Every byte went out. Dropping the writer closes the pipe.
    Done,
    Failed(Errno),
}

/// The deadline moves with every write that takes bytes.
pub const STALL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct Writer {
    fd: OwnedFd,
    pub block: usize,
    written: usize,
    deadline: Instant,
}

impl Writer {
    pub fn new(fd: OwnedFd, block: usize) -> io::Result<Self> {
        let flags = rustix::fs::fcntl_getfl(&fd)?;
        rustix::fs::fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
        Ok(Self { fd, block, written: 0, deadline: Instant::now() + STALL })
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn pump(&mut self, data: &[u8]) -> Pump {
        while self.written < data.len() {
            match rustix::io::write(&self.fd, &data[self.written..]) {
                Ok(written) => {
                    self.written += written;
                    self.deadline = Instant::now() + STALL;
                }
                Err(Errno::AGAIN) => return Pump::Again,
                Err(Errno::INTR) => {}
                Err(errno) => return Pump::Failed(errno),
            }
        }
        Pump::Done
    }
}

#[cfg(test)]
mod tests {
    use rustix::pipe::{PipeFlags, pipe_with};

    use super::*;

    fn pipe() -> (OwnedFd, OwnedFd) {
        pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap()
    }

    #[test]
    fn fill_reads_until_the_writer_closes() {
        let (read, write) = pipe();
        let mut scratch = [0u8; 4];
        let mut buf = Vec::new();
        assert_eq!(
            fill(read.as_fd(), &mut buf, 100, &mut scratch),
            Fill::More
        );
        rustix::io::write(&write, b"made-up text").unwrap();
        assert_eq!(
            fill(read.as_fd(), &mut buf, 100, &mut scratch),
            Fill::More,
            "a small scratch buffer reads in pieces"
        );
        assert_eq!(buf, b"made-up text");
        drop(write);
        assert_eq!(
            fill(read.as_fd(), &mut buf, 100, &mut scratch),
            Fill::Done
        );
    }

    #[test]
    fn fill_stops_at_the_cap() {
        let (read, write) = pipe();
        rustix::io::write(&write, b"0123456789").unwrap();
        let mut buf = Vec::new();
        assert_eq!(
            fill(read.as_fd(), &mut buf, 9, &mut [0u8; 64]),
            Fill::TooLarge
        );
        let (read, write) = pipe();
        rustix::io::write(&write, b"0123456789").unwrap();
        drop(write);
        let mut buf = Vec::new();
        assert_eq!(
            fill(read.as_fd(), &mut buf, 10, &mut [0u8; 64]),
            Fill::Done,
            "exactly the cap is fine"
        );
    }

    #[test]
    fn a_writer_fills_a_slow_pipe_in_steps() {
        let (read, write) = pipe();
        let data: Vec<u8> = (0..300_000u32)
            .map(|n| n as u8)
            .collect();
        let mut writer = Writer::new(write, 0).unwrap();
        assert_eq!(writer.block, 0);
        assert_eq!(
            writer.pump(&data),
            Pump::Again,
            "more than a pipe holds"
        );
        let mut got = Vec::new();
        let mut scratch = [0u8; 65536];
        loop {
            let _ = fill(read.as_fd(), &mut got, usize::MAX, &mut scratch);
            if writer.pump(&data) == Pump::Done {
                break;
            }
        }
        drop(writer);
        assert_eq!(
            fill(read.as_fd(), &mut got, usize::MAX, &mut scratch),
            Fill::Done
        );
        assert!(got == data, "every byte in order");
    }

    #[test]
    fn a_reader_that_stops_reading_expires() {
        let (read, write) = pipe();
        let data = vec![1u8; 300_000];
        let mut writer = Writer::new(write, 0).unwrap();
        assert_eq!(writer.pump(&data), Pump::Again);
        let stalled_since = writer.deadline() - STALL;
        assert!(!writer.expired(stalled_since + STALL - Duration::from_millis(1)));
        assert!(
            writer.expired(stalled_since + STALL),
            "five seconds without progress"
        );
        std::thread::sleep(Duration::from_millis(5));
        let _ = fill(
            read.as_fd(),
            &mut Vec::new(),
            usize::MAX,
            &mut [0u8; 65536],
        );
        assert_eq!(writer.pump(&data), Pump::Again);
        assert!(
            writer.deadline() > stalled_since + STALL,
            "progress moves the deadline"
        );
    }

    #[test]
    fn a_gone_reader_fails_the_writer() {
        let (read, write) = pipe();
        let mut writer = Writer::new(write, 1).unwrap();
        drop(read);
        assert_eq!(writer.pump(b"made-up"), Pump::Failed(Errno::PIPE));
    }

    #[test]
    fn the_writer_makes_a_blocking_fd_nonblocking() {
        let (read, write) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        let writer = Writer::new(write, 0).unwrap();
        let flags = rustix::fs::fcntl_getfl(writer.fd()).unwrap();
        assert!(flags.contains(OFlags::NONBLOCK));
        drop(read);
    }
}
