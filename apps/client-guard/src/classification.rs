//! Pure Client Guard death classification (ADR-0048).
//!
//! This module is platform-independent so every platform tests the Guard's
//! central decision. Transport and service APIs are deliberately outside it;
//! the end-session grace wait takes its clock and pause as parameters.

use openpiso_core::guard::{CleanExitReason, GuardMessage};
use std::time::{Duration, Instant};

/// How long the Guard waits after an unexplained pipe drop for Windows to say
/// the session is ending. Windows ends the interactive user's processes before
/// it notifies services of a shutdown, and a sign-out sends no shutdown control
/// at all, so the notice routinely trails the death it explains.
pub const END_SESSION_GRACE: Duration = Duration::from_secs(3);

/// How often the grace wait re-reads the end-session notice.
pub const END_SESSION_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathClassification {
    CleanExit(CleanExitReason),
    OsShutdown,
    TamperKill,
}

/// Decide whether the Guard should treat a disconnected Client as a Tamper
/// Kill. The Client never supplies this classification; it only supplies
/// evidence that the Guard may accept or reject.
///
/// Crashes are intentionally Tamper Kills: the Guard cannot distinguish a
/// crash from Task Manager's "End task", and allowing unexplained deaths to
/// pass would provide a reliable loophole around the protection.
pub fn classify(message: Option<GuardMessage>, os_shutdown_notified: bool) -> DeathClassification {
    if os_shutdown_notified || message == Some(GuardMessage::OsShutdown) {
        DeathClassification::OsShutdown
    } else if let Some(GuardMessage::CleanExit(reason)) = message {
        DeathClassification::CleanExit(reason)
    } else {
        DeathClassification::TamperKill
    }
}

/// A disconnect waits out the end-session grace only when nothing explains it
/// yet: a typed message is final and is never delayed, and a notice already
/// seen needs no waiting.
pub const fn needs_end_session_grace(message: &Option<GuardMessage>, notified: bool) -> bool {
    message.is_none() && !notified
}

/// Whether an end-session notice explains a disconnect at `disconnected_at`.
/// A notice that trails the death is the expected Windows ordering; one that
/// precedes it by more than the grace is stale — a sign-out elsewhere while
/// the Client stayed connected must not excuse a later, unrelated kill.
/// Both instants are offsets on the same monotonic clock.
pub fn notice_is_current(notice_at: Option<Duration>, disconnected_at: Duration) -> bool {
    notice_at.is_some_and(|at| at >= disconnected_at.saturating_sub(END_SESSION_GRACE))
}

/// Wait up to `grace` for an end-session notice, re-reading `notified` every
/// [`END_SESSION_POLL_INTERVAL`]. Returns whether a notice arrived by the
/// deadline; a notice exactly at the deadline counts.
pub fn wait_for_end_session_notice(
    mut notified: impl FnMut() -> bool,
    grace: Duration,
    mut now: impl FnMut() -> Instant,
    mut pause: impl FnMut(Duration),
) -> bool {
    let deadline = now() + grace;
    loop {
        let current = now();
        if current >= deadline {
            return notified();
        }
        if notified() {
            return true;
        }
        pause(END_SESSION_POLL_INTERVAL.min(deadline - current));
    }
}

/// Classify a disconnect, waiting out the end-session grace when nothing
/// explains it yet. Returns the classification and whether the grace wait
/// was the thing that excused it.
pub fn classify_disconnect(
    message: Option<GuardMessage>,
    mut notified: impl FnMut() -> bool,
    now: impl FnMut() -> Instant,
    pause: impl FnMut(Duration),
) -> (DeathClassification, bool) {
    let already_notified = notified();
    let mut excused_by_grace = false;
    let notified_by_deadline = if needs_end_session_grace(&message, already_notified) {
        excused_by_grace = wait_for_end_session_notice(notified, END_SESSION_GRACE, now, pause);
        excused_by_grace
    } else {
        already_notified
    };
    (classify(message, notified_by_deadline), excused_by_grace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpiso_core::guard::{CleanExitReason, GuardMessage};
    use std::cell::Cell;
    use std::time::Instant;

    #[test]
    fn expiry_power_off_is_clean() {
        assert_eq!(
            classify(
                Some(GuardMessage::CleanExit(CleanExitReason::ExpiryPowerOff)),
                false,
            ),
            DeathClassification::CleanExit(CleanExitReason::ExpiryPowerOff)
        );
    }

    #[test]
    fn offline_shutdown_is_clean() {
        assert_eq!(
            classify(
                Some(GuardMessage::CleanExit(CleanExitReason::OfflineShutdown)),
                false,
            ),
            DeathClassification::CleanExit(CleanExitReason::OfflineShutdown)
        );
    }

    #[test]
    fn owner_quit_is_a_clean_exit_with_its_reason() {
        assert_eq!(
            classify(
                Some(GuardMessage::CleanExit(CleanExitReason::OwnerQuit)),
                false,
            ),
            DeathClassification::CleanExit(CleanExitReason::OwnerQuit)
        );
    }

    #[test]
    fn os_shutdown_is_never_a_tamper_kill() {
        assert_eq!(classify(None, true), DeathClassification::OsShutdown);
        assert_eq!(
            classify(Some(GuardMessage::OsShutdown), false),
            DeathClassification::OsShutdown
        );
    }

    #[test]
    fn missing_clean_exit_is_a_tamper_kill() {
        assert_eq!(classify(None, false), DeathClassification::TamperKill);
    }

    #[test]
    fn malformed_transport_is_not_clean_exit_evidence() {
        assert_eq!(GuardMessage::decode(&[0xff]), None);
        assert_eq!(classify(None, false), DeathClassification::TamperKill);
    }

    /// A fake clock advanced only by the injected pause, so a grace wait runs
    /// instantly and deterministically. `notice_at` is when Windows delivers
    /// its end-session notice, relative to the disconnect.
    fn disconnect_with_notice_at(
        message: Option<GuardMessage>,
        notice_at: Option<Duration>,
    ) -> (DeathClassification, Duration) {
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let (classification, _) = super::classify_disconnect(
            message,
            || notice_at.is_some_and(|at| elapsed.get() >= at),
            || start + elapsed.get(),
            |pause| elapsed.set(elapsed.get() + pause),
        );
        (classification, elapsed.get())
    }

    #[test]
    fn a_notice_arriving_during_the_grace_is_an_os_shutdown() {
        let (classification, waited) =
            disconnect_with_notice_at(None, Some(Duration::from_secs(1)));
        assert_eq!(classification, DeathClassification::OsShutdown);
        assert!(waited >= Duration::from_secs(1));
        assert!(waited < END_SESSION_GRACE);
    }

    #[test]
    fn no_notice_within_the_grace_is_a_tamper_kill() {
        let (classification, waited) = disconnect_with_notice_at(None, None);
        assert_eq!(classification, DeathClassification::TamperKill);
        assert_eq!(waited, END_SESSION_GRACE);
    }

    #[test]
    fn a_notice_after_the_grace_does_not_excuse_the_death() {
        let late = END_SESSION_GRACE + END_SESSION_POLL_INTERVAL;
        let (classification, _) = disconnect_with_notice_at(None, Some(late));
        assert_eq!(classification, DeathClassification::TamperKill);
    }

    #[test]
    fn a_notice_already_seen_at_disconnect_needs_no_grace() {
        assert!(!needs_end_session_grace(&None, true));
        let (classification, waited) = disconnect_with_notice_at(None, Some(Duration::ZERO));
        assert_eq!(classification, DeathClassification::OsShutdown);
        assert_eq!(waited, Duration::ZERO);
    }

    #[test]
    fn a_typed_clean_exit_needs_no_grace_and_stays_clean() {
        let message = Some(GuardMessage::CleanExit(CleanExitReason::ExpiryPowerOff));
        assert!(!needs_end_session_grace(&message, false));
        let (classification, waited) = disconnect_with_notice_at(message, None);
        assert_eq!(
            classification,
            DeathClassification::CleanExit(CleanExitReason::ExpiryPowerOff)
        );
        assert_eq!(waited, Duration::ZERO);
    }

    #[test]
    fn a_notice_exactly_at_the_deadline_counts() {
        let (classification, _) = disconnect_with_notice_at(None, Some(END_SESSION_GRACE));
        assert_eq!(classification, DeathClassification::OsShutdown);
    }

    #[test]
    fn an_off_interval_notice_at_the_deadline_still_counts() {
        // A clock that jumps past the deadline between polls must still get
        // the final check rather than giving up early.
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let notified = wait_for_end_session_notice(
            || elapsed.get() >= END_SESSION_GRACE,
            END_SESSION_GRACE,
            || start + elapsed.get(),
            |pause| elapsed.set(elapsed.get() + pause + Duration::from_millis(37)),
        );
        assert!(notified);
    }

    #[test]
    fn a_notice_long_before_the_disconnect_is_stale() {
        let disconnected = Duration::from_secs(600);
        assert!(!notice_is_current(
            Some(Duration::from_secs(10)),
            disconnected
        ));
        assert!(!notice_is_current(None, disconnected));
    }

    #[test]
    fn a_notice_just_before_or_after_the_disconnect_is_current() {
        let disconnected = Duration::from_secs(600);
        assert!(notice_is_current(
            Some(disconnected - END_SESSION_GRACE),
            disconnected
        ));
        assert!(notice_is_current(
            Some(disconnected - Duration::from_millis(500)),
            disconnected
        ));
        assert!(notice_is_current(
            Some(disconnected + Duration::from_secs(2)),
            disconnected
        ));
    }
}
