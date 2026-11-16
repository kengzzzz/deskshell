//! The Wi-Fi scanner of one device, with its interval and rate limit.

use std::time::Duration;

use tokio::time::Instant;

pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_millis(10_001);

#[derive(Debug, Default)]
pub(crate) struct Scanner {
    pub on: bool,
    last_request: Option<Instant>,
    pub deadline: Option<Instant>,
}

impl Scanner {
    pub(crate) fn switch(
        &mut self,
        on: bool,
        now: Instant,
        last_scan: Option<Instant>,
        interval: Duration,
    ) -> bool {
        if on == self.on {
            return false;
        }
        self.on = on;
        if on {
            self.timeout(now, last_scan, interval)
        } else {
            self.deadline = None;
            false
        }
    }

    pub(crate) fn timeout(
        &mut self,
        now: Instant,
        last_scan: Option<Instant>,
        interval: Duration,
    ) -> bool {
        if let Some(last) = last_scan
            && now.saturating_duration_since(last) < interval
        {
            self.deadline = Some(last + interval);
            return false;
        }
        if let Some(last) = self.last_request
            && now.saturating_duration_since(last) < interval
        {
            self.deadline = Some(last + interval);
            return false;
        }
        self.last_request = Some(now);
        self.deadline = Some(now + interval);
        true
    }
}

// `LastScan` is CLOCK_BOOTTIME in milliseconds, -1 before the first scan.
pub(crate) fn last_scan_instant(last_scan_ms: i64, boot_ms: i64, now: Instant) -> Option<Instant> {
    if last_scan_ms < 0 {
        return None;
    }
    let age = boot_ms - last_scan_ms;
    if age >= 0 {
        now.checked_sub(Duration::from_millis(age.unsigned_abs()))
    } else {
        Some(now + Duration::from_millis(age.unsigned_abs()))
    }
}

// `/proc/uptime` is CLOCK_BOOTTIME, in hundredths of a second.
pub(crate) fn boot_ms() -> Option<i64> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    parse_uptime(&uptime)
}

fn parse_uptime(uptime: &str) -> Option<i64> {
    let seconds = uptime.split_whitespace().next()?;
    let (whole, fraction) = seconds
        .split_once('.')
        .unwrap_or((seconds, "0"));
    let whole: i64 = whole.parse().ok()?;
    let mut millis = 0;
    let mut scale = 100;
    for digit in fraction.bytes().take(3) {
        if !digit.is_ascii_digit() {
            return None;
        }
        millis += i64::from(digit - b'0') * scale;
        scale /= 10;
    }
    Some(whole * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(10);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn first_scan_is_immediate() {
        let now = Instant::now();
        let mut scanner = Scanner::default();
        assert!(scanner.switch(true, now, None, INTERVAL));
        assert_eq!(scanner.deadline, Some(now + INTERVAL));
    }

    #[test]
    fn the_device_last_scan_delays_the_request() {
        let start = Instant::now();
        let mut scanner = Scanner::default();
        let last_scan = Some(start);
        assert!(!scanner.switch(true, start + secs(4), last_scan, INTERVAL));
        assert_eq!(scanner.deadline, Some(start + secs(10)));
        assert!(scanner.timeout(start + secs(10), last_scan, INTERVAL));
        assert_eq!(scanner.deadline, Some(start + secs(20)));
    }

    #[test]
    fn our_last_request_delays_the_next_one() {
        let start = Instant::now();
        let mut scanner = Scanner::default();
        assert!(scanner.switch(true, start, None, INTERVAL));
        assert!(!scanner.switch(false, start + secs(1), None, INTERVAL));
        assert_eq!(scanner.deadline, None);
        assert!(!scanner.switch(true, start + secs(3), None, INTERVAL));
        assert_eq!(scanner.deadline, Some(start + secs(10)));
        assert!(scanner.timeout(start + secs(10), None, INTERVAL));
    }

    #[test]
    fn a_completed_scan_pushes_the_timer_back() {
        let start = Instant::now();
        let mut scanner = Scanner::default();
        assert!(scanner.switch(true, start, None, INTERVAL));
        let last_scan = Some(start + secs(2));
        assert!(!scanner.timeout(start + secs(10), last_scan, INTERVAL));
        assert_eq!(scanner.deadline, Some(start + secs(12)));
        assert!(scanner.timeout(start + secs(12), last_scan, INTERVAL));
    }

    #[test]
    fn switching_to_the_same_state_does_nothing() {
        let now = Instant::now();
        let mut scanner = Scanner::default();
        assert!(!scanner.switch(false, now, None, INTERVAL));
        assert!(scanner.switch(true, now, None, INTERVAL));
        assert!(!scanner.switch(true, now + secs(30), None, INTERVAL));
    }

    #[test]
    fn last_scan_conversion() {
        let now = Instant::now() + secs(100);
        assert_eq!(last_scan_instant(-1, 50_000, now), None);
        assert_eq!(
            last_scan_instant(48_000, 50_000, now),
            Some(now - secs(2))
        );
        assert_eq!(
            last_scan_instant(51_000, 50_000, now),
            Some(now + secs(1))
        );
    }

    #[test]
    fn uptime_parsing() {
        assert_eq!(
            parse_uptime("12345.67 54321.00\n"),
            Some(12_345_670)
        );
        assert_eq!(parse_uptime("5.5 1.0"), Some(5_500));
        assert_eq!(parse_uptime("7 1"), Some(7_000));
        assert_eq!(parse_uptime("x.1 1"), None);
        assert!(boot_ms().is_some_and(|ms| ms > 0));
    }
}
