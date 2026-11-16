//! When the worker locks its memory.

pub const HEADROOM: u64 = 4 << 20;

// Under MCL_FUTURE a mapping past RLIMIT_MEMLOCK fails. A module whose allocation fails
// fails the attempt and pam_faillock counts it, so lock only with room to spare.
pub fn lock_fits(mapped: Option<u64>, limit: Option<u64>) -> bool {
    match (mapped, limit) {
        (_, None) => true,
        (Some(mapped), Some(limit)) => mapped.saturating_add(HEADROOM) <= limit,
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn locks_only_with_room_to_spare() {
        assert!(lock_fits(Some(3 * MIB), Some(8 * MIB)));
        assert!(lock_fits(Some(4 * MIB), Some(8 * MIB)));
        assert!(!lock_fits(Some(5 * MIB), Some(8 * MIB)));
        assert!(
            !lock_fits(None, Some(8 * MIB)),
            "an unknown size is not locked"
        );
        assert!(lock_fits(None, None), "no limit, no risk");
        assert!(!lock_fits(Some(u64::MAX), Some(8 * MIB)));
    }
}
