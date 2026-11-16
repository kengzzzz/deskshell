//! The bar clock: `HH:MM` in local time, redrawn at each minute boundary.

use std::fmt::Write as _;
use std::time::Duration;

use iced::futures::Stream;
use iced::futures::stream;
use jiff::Zoned;

// Wakes a little after the boundary, so the new minute has begun when the label is read.
const MARGIN: Duration = Duration::from_millis(5);

pub fn format(now: &Zoned, out: &mut String) {
    out.clear();
    let _ = write!(out, "{:02}:{:02}", now.hour(), now.minute());
}

pub fn until_next_minute(now: &Zoned) -> Duration {
    let into_minute = Duration::new(
        u64::from(now.second().unsigned_abs()),
        now.subsec_nanosecond().unsigned_abs(),
    );
    Duration::from_secs(60).saturating_sub(into_minute) + MARGIN
}

/// A sleep runs on the monotonic clock, so after a suspend the first tick can come late.
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
    fn formats_hours_and_minutes() {
        let mut out = String::from("stale");
        format(
            &at("2026-11-12T07:05:59.9+07:00[Asia/Bangkok]"),
            &mut out,
        );
        assert_eq!(out, "07:05");
        format(
            &at("2026-11-12T23:59:00+07:00[Asia/Bangkok]"),
            &mut out,
        );
        assert_eq!(out, "23:59");
    }

    #[test]
    fn sleeps_until_the_next_minute() {
        assert_eq!(
            until_next_minute(&at("2026-11-12T12:34:56.5+07:00[Asia/Bangkok]")),
            Duration::from_millis(3505)
        );
        assert_eq!(
            until_next_minute(&at("2026-11-12T12:34:00+07:00[Asia/Bangkok]")),
            Duration::from_millis(60_005)
        );
    }
}
