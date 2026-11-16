//! Faillock config and tally reads for the auth sheets, and the lock texts drawn from them.

use std::path::PathBuf;

use deskshell_auth::faillock::{self, Config, Locked, Status, Tally};
use jiff::Timestamp;
use jiff::tz::TimeZone;

const PERMANENT_TEXT: &str = "Too many failed attempts. Run faillock --reset to unlock.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub config: PathBuf,
    pub user: String,
}

impl Source {
    pub fn new(user: String) -> Self {
        Self { config: PathBuf::from(faillock::CONFIG_PATH), user }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub config: Config,
    pub tally: Tally,
}

pub fn read(source: &Source) -> Read {
    let config = Config::load(&source.config);
    let tally = faillock::read_tally(&config.dir, &source.user);
    Read { config, tally }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockout {
    pub status: Status,
    pub until: String,
    pub remaining: String,
    pub text: String,
    pub placeholder: String,
}

impl Lockout {
    pub fn locked(&self) -> bool {
        self.status.locked.is_some()
    }

    pub fn new(status: Status, now: u64, zone: &TimeZone) -> Self {
        let mut lockout = Self { status, ..Self::default() };
        match lockout.status.locked {
            None => {}
            Some(Locked::Permanent) => {
                lockout.text = PERMANENT_TEXT.to_owned();
                lockout.placeholder = String::from("Locked");
            }
            Some(Locked::Until(free)) => {
                lockout.until = clock_text(free, zone);
                lockout.remaining = remaining_text(free.saturating_sub(now));
                lockout.text = format!(
                    "Too many failed attempts. Try again at {} ({}).",
                    lockout.until, lockout.remaining
                );
                lockout.placeholder = format!("Locked until {}", lockout.until);
            }
        }
        lockout
    }
}

fn clock_text(second: u64, zone: &TimeZone) -> String {
    let Ok(stamp) = Timestamp::from_second(i64::try_from(second).unwrap_or(i64::MAX)) else {
        return String::new();
    };
    stamp
        .to_zoned(zone.clone())
        .strftime("%H:%M")
        .to_string()
}

fn remaining_text(seconds: u64) -> String {
    if seconds < 60 {
        format!("in {seconds} s")
    } else {
        format!("in {} min", seconds.div_ceil(60))
    }
}

#[derive(Debug)]
pub struct Faillock {
    read: Option<Read>,
    lockout: Lockout,
    zone: TimeZone,
}

impl Default for Faillock {
    fn default() -> Self {
        Self::new(TimeZone::system())
    }
}

impl Faillock {
    pub fn new(zone: TimeZone) -> Self {
        Self { read: None, lockout: Lockout::default(), zone }
    }

    pub fn lockout(&self) -> &Lockout {
        &self.lockout
    }

    pub fn locked(&self) -> bool {
        self.lockout.locked()
    }

    pub fn counting_down(&self) -> bool {
        matches!(self.lockout.status.locked, Some(Locked::Until(_)))
    }

    pub fn apply(&mut self, read: Read, now: u64) -> bool {
        self.read = Some(read);
        self.evaluate(now)
    }

    pub fn tick(&mut self, now: u64) -> bool {
        self.evaluate(now)
    }

    fn evaluate(&mut self, now: u64) -> bool {
        let Some(read) = &self.read else {
            return false;
        };
        let status = faillock::evaluate(&read.config, &read.tally, now);
        let lockout = Lockout::new(status, now, &self.zone);
        if lockout == self.lockout {
            return false;
        }
        self.lockout = lockout;
        true
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const RECORD: usize = 64;

    fn test_config(dir: &Path, extra: &str) -> PathBuf {
        let path = dir.join("faillock.conf");
        let text = format!("dir = {}\n{extra}", dir.display());
        std::fs::write(&path, text).unwrap();
        path
    }

    fn tally_file(dir: &Path, user: &str, times: &[u64]) {
        let mut bytes = Vec::with_capacity(times.len() * RECORD);
        for time in times {
            let mut record = [0u8; RECORD];
            record[..4].copy_from_slice(b"tty1");
            record[54..56].copy_from_slice(&1u16.to_ne_bytes());
            record[56..].copy_from_slice(&time.to_ne_bytes());
            bytes.extend_from_slice(&record);
        }
        std::fs::write(dir.join(user), bytes).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-faillock-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_config_and_the_tally_from_its_dir() {
        let dir = temp_dir("read");
        let config = test_config(&dir, "deny = 3\nunlock_time = 600\n");
        let source = Source { config, user: String::from("tester") };
        let empty = read(&source);
        assert_eq!(
            empty.tally,
            Tally::Known(Vec::new()),
            "a missing file is no failure"
        );
        assert_eq!(empty.config.dir, dir);
        tally_file(&dir, "tester", &[1000, 1010, 1020]);
        let mut faillock = Faillock::new(TimeZone::UTC);
        assert!(faillock.apply(read(&source), 1030));
        assert!(faillock.locked());
        assert_eq!(
            faillock.lockout().until,
            "00:27",
            "1020 + 600 + 1 is 00:27:01 UTC"
        );
        assert_eq!(faillock.lockout().remaining, "in 10 min");
        assert_eq!(
            faillock.lockout().text,
            "Too many failed attempts. Try again at 00:27 (in 10 min)."
        );
        assert_eq!(
            faillock.lockout().placeholder,
            "Locked until 00:27"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_clock_counts_down_and_lifts_the_lock() {
        let dir = temp_dir("tick");
        let config = test_config(&dir, "deny = 3\nunlock_time = 600\n");
        tally_file(&dir, "tester", &[1000, 1010, 1020]);
        let source = Source { config, user: String::from("tester") };
        let mut faillock = Faillock::new(TimeZone::UTC);
        faillock.apply(read(&source), 1561);
        assert_eq!(
            faillock.lockout().remaining,
            "in 1 min",
            "60 s is a minute"
        );
        assert!(faillock.counting_down());
        assert!(faillock.tick(1562));
        assert_eq!(faillock.lockout().remaining, "in 59 s");
        assert!(
            !faillock.tick(1562),
            "the same second changes nothing"
        );
        assert!(faillock.tick(1620));
        assert_eq!(faillock.lockout().remaining, "in 1 s");
        assert!(
            faillock.tick(1621),
            "1621 is the first free second"
        );
        assert!(!faillock.locked());
        assert!(faillock.lockout().text.is_empty());
        assert!(!faillock.counting_down());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn permanent_and_unreadable() {
        let mut faillock = Faillock::new(TimeZone::UTC);
        assert!(!faillock.tick(5), "nothing read yet");
        let config = Config { unlock_time: 0, ..Config::default() };
        let read = Read { config, tally: Tally::Known(vec![10, 20, 30]) };
        assert!(faillock.apply(read, 100_000));
        assert_eq!(faillock.lockout().text, PERMANENT_TEXT);
        assert_eq!(faillock.lockout().placeholder, "Locked");
        assert!(faillock.lockout().until.is_empty());
        assert!(!faillock.counting_down());
        let unknown = Read { config: Config::default(), tally: Tally::Unknown };
        assert!(faillock.apply(unknown, 100_000));
        assert!(!faillock.locked(), "an unknown tally never locks");
    }

    #[test]
    fn remaining_rounds_minutes_up() {
        assert_eq!(remaining_text(0), "in 0 s");
        assert_eq!(remaining_text(59), "in 59 s");
        assert_eq!(remaining_text(60), "in 1 min");
        assert_eq!(remaining_text(61), "in 2 min");
        assert_eq!(remaining_text(600), "in 10 min");
    }
}
