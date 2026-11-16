//! One byte budget for every selection in flight. A selection that does not fit is dropped.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What every selection costs on top of its bytes, so a flood of empty ones is bounded too.
pub const EVENT_OVERHEAD: usize = 4 << 10;

#[derive(Debug)]
pub struct Budget {
    cap: usize,
    used: AtomicUsize,
}

impl Budget {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self { cap, used: AtomicUsize::new(0) })
    }

    /// Takes nothing when `bytes` do not fit. It never waits.
    pub fn try_reserve(self: &Arc<Self>, bytes: usize) -> Option<Reservation> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|after| *after <= self.cap)
            })
            .ok()?;
        Some(Reservation { budget: Arc::clone(self), bytes })
    }

    pub fn in_flight(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
}

/// Dropping it gives the bytes back.
pub struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}

impl fmt::Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reservation({})", self.bytes)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget
            .used
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_fit_the_cap_and_come_back() {
        let budget = Budget::new(100);
        let first = budget.try_reserve(60).unwrap();
        assert!(budget.try_reserve(41).is_none(), "over the cap");
        let second = budget.try_reserve(40).unwrap();
        assert_eq!(budget.in_flight(), 100);
        drop(first);
        assert_eq!(budget.in_flight(), 40);
        assert!(budget.try_reserve(60).is_some());
        drop(second);
        assert_eq!(budget.in_flight(), 0);
        assert!(budget.try_reserve(usize::MAX).is_none());
    }
}
