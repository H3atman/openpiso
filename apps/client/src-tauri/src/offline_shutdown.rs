use std::time::{Duration, Instant};

pub const EXPIRED_REWARNING_SECONDS: u64 = 10;

/// Whether a just-observed connection state is the edge that arms Offline
/// Shutdown Countdown. A previously connected Client is the only state that
/// may arm it; setup, connecting, unregistered, and never-connected states do
/// not.
pub fn should_arm(previously_connected: bool, next_connection_status: &str) -> bool {
    previously_connected && next_connection_status == "disconnected"
}

/// Produce the monotonic deadline for an arm decision. The caller owns the
/// persisted/in-memory state transition; this function only decides the
/// deadline value.
pub fn arm_deadline(
    previously_connected: bool,
    next_connection_status: &str,
    now: Instant,
    grace_seconds: i64,
) -> Option<Instant> {
    should_arm(previously_connected, next_connection_status)
        .then(|| now + Duration::from_secs(grace_seconds.max(1) as u64))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeDecision {
    pub deadline: Instant,
    pub expired_on_launch: bool,
}

/// Resume a persisted wall-clock deadline against a fresh monotonic clock.
/// A deadline that elapsed while the app was not running gets a short,
/// explicit re-warning instead of firing shutdown immediately on launch.
pub fn resume_deadline(
    persisted_deadline_unix_ms: i64,
    now_unix_ms: i64,
    now: Instant,
) -> ResumeDecision {
    let remaining_ms = persisted_deadline_unix_ms.saturating_sub(now_unix_ms);
    if remaining_ms <= 0 {
        ResumeDecision {
            deadline: now + Duration::from_secs(EXPIRED_REWARNING_SECONDS),
            expired_on_launch: true,
        }
    } else {
        ResumeDecision {
            deadline: now + Duration::from_millis(remaining_ms as u64),
            expired_on_launch: false,
        }
    }
}

/// Convert a monotonic deadline to an absolute wall-clock deadline for the
/// persisted file. The remaining duration is what survives a wall-clock jump
/// while the app is running.
pub fn wall_clock_deadline(
    deadline: Option<Instant>,
    now: Instant,
    now_unix_ms: i64,
) -> Option<i64> {
    deadline.map(|deadline| {
        let remaining_ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i64::MAX as u128) as i64;
        now_unix_ms.saturating_add(remaining_ms)
    })
}

/// Start an already-open owner settings pause when a disconnect arms the
/// countdown.
pub fn pause_at_arm(owner_unlocked: bool, now: Instant) -> Option<Instant> {
    owner_unlocked.then_some(now)
}

/// Whether an owner-unlocked settings panel should pause the countdown.
pub fn should_pause(deadline: Option<Instant>, paused_at: Option<Instant>) -> bool {
    deadline.is_some() && paused_at.is_none()
}

/// Extend the monotonic deadline by the time spent in owner-unlocked settings.
pub fn extend_for_pause(
    deadline: Option<Instant>,
    paused_at: Option<Instant>,
    resumed_at: Instant,
) -> Option<Instant> {
    match (deadline, paused_at) {
        (Some(deadline), Some(paused_at)) => {
            Some(deadline + resumed_at.saturating_duration_since(paused_at))
        }
        _ => deadline,
    }
}

/// A server response that proves the admin is reachable cancels the offline
/// countdown. An unregistered response is reachable too, but retains its own
/// Lock surface instead of continuing an offline power-off race.
pub fn should_cancel(next_connection_status: &str) -> bool {
    matches!(next_connection_status, "connected" | "unregistered")
}

/// Whether the one-shot offline OS shutdown should fire now while the Client
/// remains disconnected or is retrying a connection.
pub fn should_execute(
    connection_status: &str,
    deadline: Option<Instant>,
    issued_at: Option<Instant>,
    now: Instant,
) -> bool {
    matches!(connection_status, "disconnected" | "connecting")
        && deadline.is_some_and(|deadline| now >= deadline)
        && crate::shutdown::retry_is_due(issued_at, now)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn arms_only_on_connected_to_disconnected_transition() {
        let now = Instant::now();
        assert_eq!(
            arm_deadline(true, "disconnected", now, 120),
            Some(now + Duration::from_secs(120))
        );
        for status in ["setup", "connecting", "unregistered", "connected"] {
            assert!(arm_deadline(false, status, now, 120).is_none());
            assert!(arm_deadline(true, status, now, 120).is_none());
        }
    }

    #[test]
    fn grace_duration_is_positive_even_if_cached_config_is_invalid() {
        let now = Instant::now();
        assert_eq!(
            arm_deadline(true, "disconnected", now, 0),
            Some(now + Duration::from_secs(1))
        );
        assert_eq!(
            arm_deadline(true, "disconnected", now, -20),
            Some(now + Duration::from_secs(1))
        );
    }

    #[test]
    fn resumes_a_future_deadline_with_remaining_wall_time() {
        let now = Instant::now();
        let decision = resume_deadline(10_500, 10_000, now);

        assert!(!decision.expired_on_launch);
        assert_eq!(decision.deadline, now + Duration::from_millis(500));
    }

    #[test]
    fn expired_deadline_gets_a_ten_second_rewarning() {
        let now = Instant::now();
        let decision = resume_deadline(9_999, 10_000, now);

        assert!(decision.expired_on_launch);
        assert_eq!(
            decision.deadline,
            now + Duration::from_secs(EXPIRED_REWARNING_SECONDS)
        );
    }

    #[test]
    fn wall_clock_deadline_preserves_monotonic_remaining_time() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(12);
        assert_eq!(
            wall_clock_deadline(Some(deadline), now, 50_000),
            Some(62_000)
        );
        assert_eq!(wall_clock_deadline(None, now, 50_000), None);
    }

    #[test]
    fn an_already_open_owner_panel_pauses_a_new_countdown() {
        let now = Instant::now();
        assert_eq!(pause_at_arm(true, now), Some(now));
        assert_eq!(pause_at_arm(false, now), None);
    }

    #[test]
    fn pause_only_starts_for_an_active_unpaused_countdown() {
        let now = Instant::now();
        assert!(should_pause(Some(now), None));
        assert!(!should_pause(Some(now), Some(now)));
        assert!(!should_pause(None, None));
    }

    #[test]
    fn pause_extends_deadline_by_the_owner_intervention_duration() {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(10);
        let resumed = started + Duration::from_secs(4);

        assert_eq!(
            extend_for_pause(Some(deadline), Some(started), resumed),
            Some(started + Duration::from_secs(14))
        );
        assert_eq!(extend_for_pause(None, Some(started), resumed), None);
        assert_eq!(
            extend_for_pause(Some(deadline), None, resumed),
            Some(deadline)
        );
    }

    #[test]
    fn reconnect_cancels_but_other_states_do_not_claim_reconnect() {
        assert!(should_cancel("connected"));
        assert!(should_cancel("unregistered"));
        assert!(!should_cancel("disconnected"));
        assert!(!should_cancel("connecting"));
    }

    #[test]
    fn fires_once_only_after_deadline_while_disconnected() {
        let now = Instant::now();
        let deadline = now - Duration::from_secs(1);
        assert!(should_execute("disconnected", Some(deadline), None, now));
        assert!(!should_execute(
            "disconnected",
            Some(deadline),
            Some(now),
            now
        ));
        assert!(!should_execute("connected", Some(deadline), None, now));
        assert!(should_execute("connecting", Some(deadline), None, now));
        // A power-off that left the machine running is issued again.
        assert!(should_execute(
            "disconnected",
            Some(deadline),
            Some(now - Duration::from_secs(crate::shutdown::SHUTDOWN_RETRY_SECONDS)),
            now
        ));
        assert!(!should_execute(
            "disconnected",
            Some(now + Duration::from_secs(1)),
            None,
            now
        ));
        assert!(!should_execute("disconnected", None, None, now));
    }
}
