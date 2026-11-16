//! pam_faillock status from `faillock.conf` and the tally file, following Linux-PAM 1.7.3.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

pub const CONFIG_PATH: &str = "/etc/security/faillock.conf";
pub const DEFAULT_DIR: &str = "/run/faillock";

const MAX_TIME_INTERVAL: u64 = 604_800;
const RECORD_SIZE: usize = 64;
// `read_tally()` in faillock.c reads at most this many records.
const MAX_RECORDS: usize = 1024;
const STATUS_VALID: u16 = 0x1;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Config {
    pub deny: u32,
    pub fail_interval: u64,
    pub unlock_time: u64,
    pub dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self { deny: 3, fail_interval: 900, unlock_time: 600, dir: PathBuf::from(DEFAULT_DIR) }
    }
}

impl Config {
    /// Follows `read_config_file()`: a bad value, an interval over 7 days or a relative `dir`
    /// keeps the previous value, and numbers parse like `sscanf("%u")`.
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        for line in text.split('\n') {
            if let Some((name, value)) = split_line(line) {
                config.set(name, value);
            }
        }
        config
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => Self::default(),
        }
    }

    fn set(&mut self, name: &str, value: &str) {
        match name {
            "dir" => {
                if value.starts_with('/') {
                    self.dir = PathBuf::from(value);
                }
            }
            "deny" if let Some(deny) = scan_unsigned(value) => self.deny = u32::from(deny as u16),
            "fail_interval" if let Some(seconds) = scan_interval(value) => {
                self.fail_interval = seconds;
            }
            "unlock_time" => {
                if value == "never" {
                    self.unlock_time = 0;
                } else if let Some(seconds) = scan_interval(value) {
                    self.unlock_time = seconds;
                }
            }
            _ => {}
        }
    }
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line
        .split('#')
        .next()
        .unwrap_or_default();
    let bytes = line.as_bytes();
    let end = bytes
        .iter()
        .rposition(|&b| !is_c_space(b))?
        + 1;
    let start = bytes
        .iter()
        .position(|&b| !is_c_space(b))?;
    let bytes = &bytes[..end];

    let mut i = start;
    while i < end && !is_c_space(bytes[i]) && bytes[i] != b'=' {
        i += 1;
    }
    let name = &line[start..i];
    if i == end {
        return Some((name, ""));
    }
    let mut eq = bytes[i] == b'=';
    i += 1;
    while i < end {
        if bytes[i] == b'=' && !eq {
            eq = true;
        } else if !is_c_space(bytes[i]) {
            break;
        }
        i += 1;
    }
    Some((name, &line[i..end]))
}

/// `sscanf("%u")` through `strtoul`: overflow saturates and a minus sign negates modulo 2^64.
fn scan_unsigned(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    let mut i = bytes
        .iter()
        .position(|&b| !is_c_space(b))
        .unwrap_or(bytes.len());
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits = bytes[i..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let mut saturated = false;
    let mut n: u64 = 0;
    for &b in &bytes[i..i + digits] {
        match n
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(b - b'0')))
        {
            Some(next) => n = next,
            None => {
                saturated = true;
                n = u64::MAX;
                break;
            }
        }
    }
    Some(if negative && !saturated { n.wrapping_neg() } else { n })
}

fn scan_interval(value: &str) -> Option<u64> {
    let seconds = u64::from(scan_unsigned(value)? as u32);
    (seconds <= MAX_TIME_INTERVAL).then_some(seconds)
}

pub fn parse_tally(bytes: &[u8]) -> impl Iterator<Item = u64> {
    let (records, _partial) = bytes.as_chunks::<RECORD_SIZE>();
    records
        .iter()
        .take(MAX_RECORDS)
        .filter_map(|record| {
            let status = u16::from_ne_bytes([record[54], record[55]]);
            let mut time = [0; 8];
            time.copy_from_slice(&record[56..64]);
            (status & STATUS_VALID != 0).then_some(u64::from_ne_bytes(time))
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tally {
    Unknown,
    Known(Vec<u64>),
}

/// A name that is not a plain file name is unknown, so it cannot leave `dir`.
/// The read takes no lock, so a read that races a write can be off until the next one.
pub fn read_tally(dir: &Path, user: &str) -> Tally {
    if user.is_empty() || user.contains('/') {
        return Tally::Unknown;
    }
    let file = match File::open(dir.join(user)) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Tally::Known(Vec::new()),
        Err(_) => return Tally::Unknown,
    };
    let mut bytes = Vec::new();
    match file
        .take((RECORD_SIZE * MAX_RECORDS) as u64)
        .read_to_end(&mut bytes)
    {
        Ok(_) => Tally::Known(parse_tally(&bytes).collect()),
        Err(_) => Tally::Unknown,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Status {
    pub known: bool,
    pub failures: u32,
    pub attempts_left: Option<u32>,
    pub locked: Option<Locked>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Locked {
    /// The first unix second at which the account is free.
    Until(u64),
    Permanent,
}

pub fn evaluate(config: &Config, tally: &Tally, now: u64) -> Status {
    let Tally::Known(times) = tally else {
        return Status::default();
    };
    let latest = times.iter().copied().max().unwrap_or(0);
    let failures = times
        .iter()
        .filter(|&&t| latest.wrapping_sub(t) < config.fail_interval)
        .count();
    let failures = u32::try_from(failures).unwrap_or(u32::MAX);
    let over = config.deny > 0 && failures >= config.deny;
    // check_tally() lifts a lock only when latest + unlock_time < now, and never when
    // unlock_time is 0 (`never`).
    let expired = over && config.unlock_time > 0 && latest.wrapping_add(config.unlock_time) < now;
    let locked = (over && !expired).then(|| {
        if config.unlock_time == 0 {
            Locked::Permanent
        } else {
            Locked::Until(
                latest
                    .saturating_add(config.unlock_time)
                    .saturating_add(1),
            )
        }
    });
    let attempts_left = if config.deny == 0 {
        None
    } else if locked.is_some() {
        Some(0)
    } else if expired {
        // After an expired lock the next failure clears every old record.
        Some(config.deny)
    } else {
        // write_tally() drops records older than fail_interval before it adds a new one.
        let recent = times
            .iter()
            .filter(|&&t| now.wrapping_sub(t) < config.fail_interval)
            .count();
        Some(
            config
                .deny
                .saturating_sub(u32::try_from(recent).unwrap_or(u32::MAX)),
        )
    };
    Status { known: true, failures, attempts_left, locked }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(valid: bool, time: u64) -> [u8; RECORD_SIZE] {
        let mut r = [0u8; RECORD_SIZE];
        r[..4].copy_from_slice(b"tty1");
        let status: u16 = if valid { STATUS_VALID | 0x4 } else { 0x4 };
        r[54..56].copy_from_slice(&status.to_ne_bytes());
        r[56..].copy_from_slice(&time.to_ne_bytes());
        r
    }

    fn known(times: &[u64]) -> Tally {
        Tally::Known(times.to_vec())
    }

    fn config(deny: u32, fail_interval: u64, unlock_time: u64) -> Config {
        Config { deny, fail_interval, unlock_time, ..Config::default() }
    }

    #[test]
    fn config_defaults() {
        assert_eq!(Config::parse(""), Config::default());
        assert_eq!(
            Config::default(),
            Config {
                deny: 3,
                fail_interval: 900,
                unlock_time: 600,
                dir: PathBuf::from("/run/faillock")
            }
        );
    }

    #[test]
    fn config_comments_and_spacing() {
        let text = "# deny = 9\n  deny = 5 # trailing\nfail_interval=60\n\tunlock_time\t120\r\n";
        let c = Config::parse(text);
        assert_eq!(
            (c.deny, c.fail_interval, c.unlock_time),
            (5, 60, 120)
        );
    }

    #[test]
    fn config_hash_cuts_values() {
        assert_eq!(Config::parse("deny = 4#5").deny, 4);
        assert_eq!(
            Config::parse("dir = /tmp/a#b").dir,
            PathBuf::from("/tmp/a")
        );
    }

    #[test]
    fn config_flags_and_ignored_keys() {
        let text = "silent\naudit\neven_deny_root\nlocal_users_only\nnodelay\nno_log_info\n\
                    admin_group = wheel\nroot_unlock_time = 5\nbogus = 1\ndeny = 2\n";
        let c = Config::parse(text);
        assert_eq!(c, Config { deny: 2, ..Config::default() });
    }

    #[test]
    fn config_never() {
        assert_eq!(
            Config::parse("unlock_time = never").unlock_time,
            0
        );
        assert_eq!(Config::parse("unlock_time = 0").unlock_time, 0);
        assert_eq!(
            Config::parse("unlock_time = Never").unlock_time,
            600
        );
    }

    #[test]
    fn config_junk_numbers() {
        assert_eq!(Config::parse("deny = abc").deny, 3);
        assert_eq!(Config::parse("deny =").deny, 3);
        assert_eq!(Config::parse("deny = 7x").deny, 7);
        assert_eq!(Config::parse("deny = +4").deny, 4);
        assert_eq!(Config::parse("deny = -1").deny, 65535);
        assert_eq!(Config::parse("deny = 65537").deny, 1);
        assert_eq!(
            Config::parse("fail_interval = 604800").fail_interval,
            604_800
        );
        assert_eq!(
            Config::parse("fail_interval = 604801").fail_interval,
            900
        );
        assert_eq!(
            Config::parse("fail_interval = -5").fail_interval,
            900
        );
        assert_eq!(
            Config::parse("unlock_time = 99999999999999999999999").unlock_time,
            600
        );
        assert_eq!(
            Config::parse("unlock_time = 4294967297").unlock_time,
            1
        );
    }

    #[test]
    fn config_value_forms() {
        assert_eq!(Config::parse("deny 4").deny, 4);
        assert_eq!(Config::parse("deny=4").deny, 4);
        assert_eq!(Config::parse("deny = = 4").deny, 3);
        assert_eq!(
            Config::parse("dir = /var/lib/fl ock ").dir,
            PathBuf::from("/var/lib/fl ock")
        );
        assert_eq!(
            Config::parse("dir = relative").dir,
            PathBuf::from(DEFAULT_DIR)
        );
    }

    #[test]
    fn config_later_lines_win() {
        assert_eq!(
            Config::parse("deny = 4\ndeny = 6\ndeny = x").deny,
            6
        );
    }

    #[test]
    fn tally_valid_bit() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&record(true, 100));
        bytes.extend_from_slice(&record(false, 200));
        bytes.extend_from_slice(&record(true, 300));
        assert_eq!(
            parse_tally(&bytes).collect::<Vec<_>>(),
            [100, 300]
        );
    }

    #[test]
    fn tally_partial_record() {
        let mut bytes = record(true, 100).to_vec();
        bytes.extend_from_slice(&record(true, 200)[..63]);
        assert_eq!(parse_tally(&bytes).collect::<Vec<_>>(), [100]);
    }

    #[test]
    fn tally_empty() {
        assert_eq!(parse_tally(&[]).count(), 0);
        assert_eq!(parse_tally(&[0; 63]).count(), 0);
    }

    #[test]
    fn tally_stops_at_1024_records() {
        let bytes: Vec<u8> = (0..1100)
            .flat_map(|t| record(true, t))
            .collect();
        assert_eq!(parse_tally(&bytes).count(), 1024);
    }

    #[test]
    fn read_tally_files() {
        let dir = crate::tests_support::TempDir::new("tally");
        assert_eq!(
            read_tally(dir.path(), "missing"),
            Tally::Known(Vec::new())
        );
        let mut bytes = record(true, 5).to_vec();
        bytes.extend_from_slice(&record(false, 6));
        std::fs::write(dir.path().join("someone"), &bytes).unwrap();
        assert_eq!(read_tally(dir.path(), "someone"), known(&[5]));
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        assert_eq!(read_tally(dir.path(), "adir"), Tally::Unknown);
        assert_eq!(read_tally(dir.path(), "../x"), Tally::Unknown);
        assert_eq!(read_tally(dir.path(), ""), Tally::Unknown);
        assert_eq!(
            read_tally(&dir.path().join("nodir"), "someone"),
            Tally::Known(Vec::new())
        );
    }

    #[test]
    fn unknown_never_locks() {
        let s = evaluate(&Config::default(), &Tally::Unknown, 1000);
        assert_eq!(
            s,
            Status { known: false, failures: 0, attempts_left: None, locked: None }
        );
    }

    #[test]
    fn empty_tally() {
        let s = evaluate(&Config::default(), &known(&[]), 1000);
        assert_eq!(
            s,
            Status { known: true, failures: 0, attempts_left: Some(3), locked: None }
        );
    }

    #[test]
    fn counts_down() {
        let c = config(3, 900, 600);
        assert_eq!(
            evaluate(&c, &known(&[1000]), 1001).attempts_left,
            Some(2)
        );
        assert_eq!(
            evaluate(&c, &known(&[1000, 1001]), 1002).attempts_left,
            Some(1)
        );
    }

    #[test]
    fn lock_boundary_second() {
        let c = config(3, 900, 600);
        let t = known(&[1000, 1001, 1002]);
        let locked = evaluate(&c, &t, 1002);
        assert_eq!(locked.locked, Some(Locked::Until(1603)));
        assert_eq!(locked.failures, 3);
        assert_eq!(locked.attempts_left, Some(0));
        assert_eq!(
            evaluate(&c, &t, 1602).locked,
            Some(Locked::Until(1603))
        );
        let free = evaluate(&c, &t, 1603);
        assert_eq!(free.locked, None);
        assert_eq!(free.attempts_left, Some(3));
        assert_eq!(free.failures, 3);
    }

    #[test]
    fn never_and_zero_are_permanent() {
        let t = known(&[1000, 1001, 1002]);
        for c in [Config::parse("unlock_time = never"), Config::parse("unlock_time = 0")] {
            let s = evaluate(&c, &t, 1_000_000);
            assert_eq!(s.locked, Some(Locked::Permanent));
            assert_eq!(s.attempts_left, Some(0));
        }
    }

    #[test]
    fn deny_zero_never_locks() {
        let s = evaluate(
            &config(0, 900, 600),
            &known(&[1000, 1001, 1002, 1003]),
            1004,
        );
        assert_eq!(s.locked, None);
        assert_eq!(s.attempts_left, None);
        assert_eq!(s.failures, 4);
    }

    #[test]
    fn lock_window_is_relative_to_latest() {
        let c = config(3, 900, 600);
        let s = evaluate(&c, &known(&[100, 900, 1000]), 1000);
        assert_eq!(s.failures, 2);
        assert_eq!(s.locked, None);
        let s = evaluate(&c, &known(&[101, 900, 1000]), 1000);
        assert_eq!(s.failures, 3);
        assert_eq!(s.locked, Some(Locked::Until(1601)));
    }

    #[test]
    fn attempts_are_relative_to_now() {
        let c = config(3, 900, 600);
        let s = evaluate(&c, &known(&[100, 200]), 1050);
        assert_eq!(s.failures, 2);
        assert_eq!(s.attempts_left, Some(2));
        assert_eq!(
            evaluate(&c, &known(&[100, 200]), 1100).attempts_left,
            Some(3)
        );
        assert_eq!(
            evaluate(&c, &known(&[100, 200]), 1099).attempts_left,
            Some(2)
        );
    }

    #[test]
    fn locked_with_short_interval() {
        let c = config(3, 60, 600);
        let s = evaluate(&c, &known(&[1000, 1010, 1020]), 1500);
        assert_eq!(s.locked, Some(Locked::Until(1621)));
        assert_eq!(s.attempts_left, Some(0));
    }

    #[test]
    fn invalid_records_ignored_by_parser_then_evaluated() {
        let mut bytes = Vec::new();
        for (valid, time) in [(true, 1000), (false, 1001), (true, 1002)] {
            bytes.extend_from_slice(&record(valid, time));
        }
        let t = Tally::Known(parse_tally(&bytes).collect());
        let s = evaluate(&Config::default(), &t, 1003);
        assert_eq!(s.failures, 2);
        assert_eq!(s.attempts_left, Some(1));
    }

    #[test]
    fn future_records_do_not_panic() {
        let c = Config::default();
        // latest + unlock_time wraps in C as well, which reads as an expired lock.
        let s = evaluate(
            &c,
            &known(&[u64::MAX, u64::MAX - 1, u64::MAX - 2]),
            1_700_000_000,
        );
        assert_eq!(s.failures, 3);
        assert_eq!(s.locked, None);
        assert_eq!(s.attempts_left, Some(3));
        let s = evaluate(&c, &known(&[2000, 2001, 2002]), 1000);
        assert_eq!(s.locked, Some(Locked::Until(2603)));
        let s = evaluate(&c, &known(&[2000]), 1000);
        assert_eq!(s.attempts_left, Some(3));
    }

    #[test]
    fn load_missing_is_default() {
        let dir = crate::tests_support::TempDir::new("conf");
        assert_eq!(
            Config::load(&dir.path().join("nope")),
            Config::default()
        );
        std::fs::write(dir.path().join("faillock.conf"), "deny = 5\n").unwrap();
        assert_eq!(
            Config::load(&dir.path().join("faillock.conf")).deny,
            5
        );
    }
}
