//! The systemd watchdog, pinged from the main loop.

use std::env;
use std::ffi::OsStr;
use std::io;
use std::process;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::sd_notify::NotifySocket;

const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";
const WATCHDOG_USEC: &str = "WATCHDOG_USEC";
const WATCHDOG_PID: &str = "WATCHDOG_PID";
const LATE_MARGIN: Duration = Duration::from_secs(2);

pub struct Watchdog {
    socket: NotifySocket,
    interval: Duration,
    last: Option<Instant>,
    failing: bool,
}

impl Watchdog {
    /// Removes the settings from the environment, so no child inherits them.
    ///
    /// # Safety
    ///
    /// No other thread may exist yet, because it writes the environment.
    pub unsafe fn take() -> Option<io::Result<Self>> {
        let address = env::var_os(NOTIFY_SOCKET);
        let interval = interval(
            env::var_os(WATCHDOG_USEC).as_deref(),
            env::var_os(WATCHDOG_PID).as_deref(),
            process::id(),
        );
        for key in [NOTIFY_SOCKET, WATCHDOG_USEC, WATCHDOG_PID] {
            // SAFETY: the caller guarantees that no other thread runs yet.
            unsafe { env::remove_var(key) };
        }
        let (address, interval) = (address?, interval?);
        Some(NotifySocket::open(&address).map(|socket| Self::new(socket, interval)))
    }

    fn new(socket: NotifySocket, interval: Duration) -> Self {
        Self { socket, interval, last: None, failing: false }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn ping(&mut self, now: Instant) {
        if let Some(gap) = self
            .last
            .and_then(|last| late(last, now, self.interval))
        {
            warn!(
                gap_ms = gap.as_millis(),
                "the main loop was blocked between watchdog pings"
            );
        }
        self.last = Some(now);
        match self.socket.send(b"WATCHDOG=1") {
            Ok(()) => self.failing = false,
            Err(err) if !self.failing => {
                self.failing = true;
                warn!(%err, "cannot ping the systemd watchdog");
            }
            Err(_) => {}
        }
    }
}

fn interval(usec: Option<&OsStr>, pid: Option<&OsStr>, own: u32) -> Option<Duration> {
    if let Some(pid) = pid
        && pid
            .to_str()
            .and_then(|pid| pid.parse::<u32>().ok())
            != Some(own)
    {
        return None;
    }
    let usec = usec?
        .to_str()?
        .parse::<u64>()
        .ok()
        .filter(|&usec| usec > 0)?;
    Some(Duration::from_micros(usec) / 3)
}

fn late(last: Instant, now: Instant, interval: Duration) -> Option<Duration> {
    let gap = now.saturating_duration_since(last);
    (gap > interval + LATE_MARGIN).then_some(gap)
}

#[cfg(feature = "test-hooks")]
pub fn test_stall() {
    let Some(dir) = env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    if std::fs::remove_file(std::path::Path::new(&dir).join("deskshell-test-stall")).is_ok() {
        warn!("test hook: the main loop blocks for good");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};

    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn interval_is_a_third_of_the_timeout() {
        let usec = os("10000000");
        assert_eq!(
            interval(Some(&usec), None, 7),
            Some(Duration::from_secs(10) / 3)
        );
        assert_eq!(
            interval(Some(&usec), Some(&os("7")), 7),
            Some(Duration::from_secs(10) / 3)
        );
    }

    #[test]
    fn no_interval_without_a_usable_timeout_or_for_another_pid() {
        let usec = os("10000000");
        assert_eq!(interval(None, None, 7), None);
        assert_eq!(interval(Some(&os("0")), None, 7), None);
        assert_eq!(interval(Some(&os("ten")), None, 7), None);
        assert_eq!(interval(Some(&usec), Some(&os("8")), 7), None);
        assert_eq!(interval(Some(&usec), Some(&os("pid")), 7), None);
    }

    #[test]
    fn late_only_past_the_margin() {
        let start = Instant::now();
        let interval = Duration::from_secs(3);
        assert_eq!(
            late(start, start + Duration::from_secs(5), interval),
            None
        );
        assert_eq!(
            late(start, start + Duration::from_secs(6), interval),
            Some(Duration::from_secs(6))
        );
        assert_eq!(
            late(start + Duration::from_secs(1), start, interval),
            None
        );
    }

    #[test]
    fn ping_sends_watchdog_to_the_socket() {
        let name = format!("deskshell-test-watchdog-{}", process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).expect("abstract name");
        let server = UnixDatagram::bind_addr(&address).expect("bind a private socket");
        let socket = NotifySocket::open(&os(&format!("@{name}"))).expect("open");
        let mut watchdog = Watchdog::new(socket, Duration::from_secs(3));
        watchdog.ping(Instant::now());
        watchdog.ping(Instant::now());
        let mut buf = [0; 32];
        for _ in 0..2 {
            let len = server.recv(&mut buf).expect("a ping");
            assert_eq!(&buf[..len], b"WATCHDOG=1");
        }
        assert!(!watchdog.failing);
    }
}
