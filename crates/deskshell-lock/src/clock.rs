//! Local time for the clock, the date and "Locked until", through jiff.

use std::fmt::Write as _;
use std::time::Duration;

use iced::futures::Stream;
use iced::futures::stream;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Wakes a little after the boundary, so the new minute has begun when the clock is read.
const MARGIN: Duration = Duration::from_millis(5);

/// `dddd, d MMMM` and `HH:mm`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Clock {
    pub date: String,
    pub time: String,
}

impl Clock {
    pub fn now() -> Self {
        let mut clock = Self::default();
        clock.set(&Zoned::now());
        clock
    }

    pub fn set(&mut self, now: &Zoned) {
        self.date.clear();
        let _ = write!(self.date, "{}", now.strftime("%A, %-d %B"));
        self.time.clear();
        let _ = write!(self.time, "{:02}:{:02}", now.hour(), now.minute());
    }
}

pub fn time_of(second: u64) -> String {
    let Ok(second) = i64::try_from(second) else {
        return String::new();
    };
    match Timestamp::from_second(second) {
        Ok(timestamp) => {
            let zoned = timestamp.to_zoned(TimeZone::system());
            format!("{:02}:{:02}", zoned.hour(), zoned.minute())
        }
        Err(_) => String::new(),
    }
}

pub fn until_next_minute(now: &Zoned) -> Duration {
    let into_minute = Duration::new(
        u64::from(now.second().unsigned_abs()),
        now.subsec_nanosecond().unsigned_abs(),
    );
    Duration::from_secs(60).saturating_sub(into_minute) + MARGIN
}

pub fn minutes() -> impl Stream<Item = ()> {
    stream::unfold((), |()| async {
        tokio::time::sleep(until_next_minute(&Zoned::now())).await;
        Some(((), ()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn formats_like_qt() {
        let mut clock = Clock::default();
        clock.set(&at("2026-11-13T07:05:59+07:00[Asia/Bangkok]"));
        assert_eq!(clock.date, "Friday, 13 November");
        assert_eq!(clock.time, "07:05");
        clock.set(&at("2026-11-01T23:59:00+07:00[Asia/Bangkok]"));
        assert_eq!(clock.date, "Sunday, 1 November");
        assert_eq!(clock.time, "23:59");
    }

    #[test]
    fn sleeps_until_the_next_minute() {
        assert_eq!(
            until_next_minute(&at("2026-11-12T12:34:56.5+07:00[Asia/Bangkok]")),
            Duration::from_millis(3505)
        );
    }
}
