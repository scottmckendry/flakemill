//! Shared wall-clock helpers.
//!
//! Every module that needs "time since the Unix epoch" goes through here so
//! the `SystemTime`/`UNIX_EPOCH` dance lives in exactly one place.

use std::time::{SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch. Returns 0 if the clock is before the
/// epoch (which `unwrap_or_default` already guarded for us everywhere).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Nanoseconds since the Unix epoch. Used for collision-resistant names
/// (build-log files, temp directories) where second granularity is too coarse.
pub fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_secs_is_a_plausible_recent_timestamp() {
        // 2023-11-14, the rough vintage of this project; guards against a
        // mis-wired epoch or a zero from the fallback path.
        assert!(now_secs() > 1_700_000_000);
    }

    #[test]
    fn now_nanos_is_consistent_with_now_secs() {
        // Both read the same wall clock, so nanos / 1e9 should sit within a
        // couple of seconds of the seconds value.
        let secs = now_secs();
        let nanos_as_secs = now_nanos() / 1_000_000_000;
        assert!(
            nanos_as_secs.abs_diff(secs as u128) <= 2,
            "nanos {nanos_as_secs}s vs secs {secs}s"
        );
    }

    #[test]
    fn now_nanos_does_not_go_backwards() {
        let first = now_nanos();
        let second = now_nanos();
        assert!(second >= first);
    }
}
