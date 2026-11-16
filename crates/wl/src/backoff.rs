//! Reconnect delays: 100 ms doubling up to 5 s, reset after a connection that stayed up.

use std::time::Duration;

pub(crate) const FIRST: Duration = Duration::from_millis(100);
pub(crate) const MAX: Duration = Duration::from_secs(5);
pub(crate) const HEALTHY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: FIRST }
    }
}

impl Backoff {
    /// `uptime` is how long the last connection lived, or zero when connecting failed.
    pub fn after(&mut self, uptime: Duration) -> Duration {
        if uptime >= HEALTHY {
            self.next = FIRST;
        }
        let delay = self.next;
        self.next = (self.next * 2).min(MAX);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_five_seconds() {
        let mut backoff = Backoff::default();
        let delays: Vec<u64> = (0..9)
            .map(|_| {
                backoff
                    .after(Duration::ZERO)
                    .as_millis() as u64
            })
            .collect();
        assert_eq!(
            delays,
            [100, 200, 400, 800, 1600, 3200, 5000, 5000, 5000]
        );
    }

    #[test]
    fn a_healthy_connection_resets() {
        let mut backoff = Backoff::default();
        backoff.after(Duration::ZERO);
        backoff.after(Duration::from_secs(1));
        assert_eq!(
            backoff.after(Duration::from_secs(1)),
            Duration::from_millis(400)
        );
        assert_eq!(backoff.after(HEALTHY), FIRST);
        assert_eq!(
            backoff.after(Duration::ZERO),
            Duration::from_millis(200)
        );
    }
}
