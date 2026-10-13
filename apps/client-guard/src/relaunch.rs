//! Pure-function relaunch policy (ADR-0048).
//!
//! Several public items are only consumed by tests at this stage (the
//! coalescing window helpers); that is expected — they complete the pure
//! surface required by the ADR.
#![allow(dead_code)]
//!
//! The backoff schedule, coalescing window, and coalesced-attempt-count logic
//! are pure functions tested on every platform — mirroring the
//! `shutdown.rs::should_execute` / `kiosk_hardening.rs::should_block_key`
//! precedent.

use std::time::Duration;

/// Initial back-off (first repeat after a failed relaunch).
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Maximum back-off between relaunch attempts.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Coalescing window: deaths inside this wall-clock gap (measured from the
/// *first* death of the group) produce one pending report with an incrementing
/// attempt count rather than a new report per death.
pub const COALESCE_WINDOW: Duration = Duration::from_secs(5);

/// Back-off multiplier (exponential).
const BACKOFF_MULTIPLIER: u32 = 2;

/// State of the exact process handle retained after a successful relaunch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackedProcessState {
    Absent,
    Running,
    Exited,
    StatusUnavailable,
}

/// How often the Guard checks that a Client is actually there (ADR-0077).
///
/// The pipe's disconnect event stays the primary trigger — it is instant, and
/// instant is what makes a Tamper Kill feel answered. This is the floor
/// underneath it, for the case ADR-0048 named and left open: a Client that
/// never connected produces no disconnect, so nothing edge-triggered ever
/// fires and the PC sits at a bare desktop indefinitely.
pub const LEVEL_CHECK_INTERVAL: Duration = Duration::from_secs(15);

/// Pure decision: should the level check relaunch now?
///
/// It defers to everything already in flight. A pending or backed-off relaunch
/// owns the recovery, and a stood-down Guard is the owner's maintenance
/// window, which this must never interrupt.
pub const fn should_level_relaunch(
    client_connected: bool,
    armed: bool,
    relaunch_in_flight: bool,
) -> bool {
    armed && !client_connected && !relaunch_in_flight
}

/// Relaunch only when no tracked process exists or that exact process exited.
/// An unknown state fails closed against duplicate process creation.
pub const fn should_attempt_relaunch(state: TrackedProcessState) -> bool {
    matches!(
        state,
        TrackedProcessState::Absent | TrackedProcessState::Exited
    )
}

/// Returns the delay before the *next* relaunch given how many consecutive
/// deaths have occurred so far (including the most recent one).
///
/// - 1st death (consecutive == 1): immediate — `Duration::ZERO`.
/// - 2nd death (consecutive == 2): ~1 s.
/// - 3rd: ~2 s.
/// - 4th: ~4 s.
/// - 5th: ~8 s.
/// - 6th: ~16 s.
/// - 7th+: capped at 30 s.
///
/// `consecutive` is the total number of *consecutive* deaths since the last
/// successful period, counting the current one. It must be >= 1.
///
/// This is intentionally not the same as the index into the attempt-count
/// from the coalesced report, because the first relaunch has zero delay even
/// though its attempt-index inside the coalesced report may be > 1.
pub fn next_relaunch_delay(consecutive: u32) -> Duration {
    if consecutive <= 1 {
        return Duration::ZERO;
    }
    let exponent = consecutive.saturating_sub(2);
    // 1 << exponent would overflow at exponent >= 31, but our cap is well
    // below that, so a manual saturating shift is fine.
    let secs = match exponent {
        0 => 1u64,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        _ => 30, // cap — 5th repeat onward
    };
    Duration::from_secs(secs.min(MAX_BACKOFF.as_secs()))
}

/// Whether a death that happened at `death_time` falls inside the coalescing
/// window opened by `window_start` (the timestamp of the first death in the
/// current pending group).
///
/// When this returns `true`, the pending report's attempt count should be
/// incremented *instead of* flushing the report and starting a new one.
pub fn inside_coalesce_window(death_time: u64, window_start: u64) -> bool {
    death_time >= window_start && (death_time - window_start) < COALESCE_WINDOW.as_secs()
}

/// Start a fresh coalescing window. Returns the `window_start` timestamp for
/// the new group.
pub fn start_coalesce_window(death_time: u64) -> u64 {
    death_time
}

/// Compute the display-friendly back-off label for a given delay.
///
/// This is a pure helper for logging / diagnostics.
pub fn delay_label(delay: Duration) -> &'static str {
    if delay.is_zero() {
        "immediate"
    } else if delay.as_secs() <= 5 {
        "short"
    } else {
        "long"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_process_blocks_duplicate_launch_until_it_exits() {
        assert!(should_attempt_relaunch(TrackedProcessState::Absent));
        assert!(!should_attempt_relaunch(TrackedProcessState::Running));
        assert!(should_attempt_relaunch(TrackedProcessState::Exited));
        assert!(!should_attempt_relaunch(
            TrackedProcessState::StatusUnavailable
        ));
    }

    // ── next_relaunch_delay ──────────────────────────────────────────

    #[test]
    fn first_death_is_immediate() {
        assert_eq!(next_relaunch_delay(1), Duration::ZERO);
    }

    #[test]
    fn second_death_backs_off_one_second() {
        assert_eq!(next_relaunch_delay(2), Duration::from_secs(1));
    }

    #[test]
    fn third_death_is_two_seconds() {
        assert_eq!(next_relaunch_delay(3), Duration::from_secs(2));
    }

    #[test]
    fn fourth_death_is_four_seconds() {
        assert_eq!(next_relaunch_delay(4), Duration::from_secs(4));
    }

    #[test]
    fn fifth_death_is_eight_seconds() {
        assert_eq!(next_relaunch_delay(5), Duration::from_secs(8));
    }

    #[test]
    fn sixth_is_sixteen_seconds() {
        assert_eq!(next_relaunch_delay(6), Duration::from_secs(16));
    }

    #[test]
    fn seventh_and_beyond_cap_at_thirty_seconds() {
        for consecutive in 7..=100 {
            let delay = next_relaunch_delay(consecutive);
            assert!(
                delay <= MAX_BACKOFF,
                "consecutive={consecutive} → delay={delay:?} exceeds cap"
            );
            assert_eq!(delay, MAX_BACKOFF);
        }
    }

    #[test]
    fn backoff_is_non_decreasing() {
        let mut prev = Duration::ZERO;
        for i in 1..=20 {
            let delay = next_relaunch_delay(i);
            assert!(delay >= prev, "delay regressed at consecutive={i}");
            prev = delay;
        }
    }

    // ── inside_coalesce_window ───────────────────────────────────────

    #[test]
    fn death_exactly_at_window_start_is_inside() {
        assert!(inside_coalesce_window(1000, 1000));
    }

    #[test]
    fn death_just_before_window_end_is_inside() {
        // window starts at 1000, ends at 1000 + 5 = 1005
        assert!(inside_coalesce_window(1004, 1000));
    }

    #[test]
    fn death_at_window_boundary_is_outside() {
        // death at 1000 + 5 = 1005 is >= 1000 + 5, so outside
        assert!(!inside_coalesce_window(1005, 1000));
    }

    #[test]
    fn death_before_window_start_is_outside() {
        assert!(!inside_coalesce_window(999, 1000));
    }

    #[test]
    fn death_well_after_window_is_outside() {
        assert!(!inside_coalesce_window(9999, 1000));
    }

    #[test]
    fn coalesce_window_works_with_zero() {
        // window starts at t=0
        assert!(inside_coalesce_window(0, 0));
        assert!(inside_coalesce_window(4, 0));
        assert!(!inside_coalesce_window(5, 0));
    }

    // ── delay_label helpers ──────────────────────────────────────────

    #[test]
    fn delay_label_matches_duration() {
        assert_eq!(delay_label(Duration::ZERO), "immediate");
        assert_eq!(delay_label(Duration::from_secs(1)), "short");
        assert_eq!(delay_label(Duration::from_secs(5)), "short");
        assert_eq!(delay_label(Duration::from_secs(6)), "long");
        assert_eq!(delay_label(Duration::from_secs(30)), "long");
    }

    // ── start_coalesce_window ────────────────────────────────────────

    #[test]
    fn fresh_window_uses_death_time() {
        assert_eq!(start_coalesce_window(42), 42);
        assert_eq!(start_coalesce_window(0), 0);
        assert_eq!(start_coalesce_window(u64::MAX), u64::MAX);
    }

    #[test]
    fn the_level_check_relaunches_a_client_that_never_connected() {
        assert!(should_level_relaunch(false, true, false));
    }

    #[test]
    fn the_level_check_defers_to_a_live_client_a_stand_down_and_work_in_flight() {
        // A Client is there: nothing to do.
        assert!(!should_level_relaunch(true, true, false));
        // Owner maintenance: the Guard stood down on purpose.
        assert!(!should_level_relaunch(false, false, false));
        // The edge trigger already owns this recovery.
        assert!(!should_level_relaunch(false, true, true));
    }
}
