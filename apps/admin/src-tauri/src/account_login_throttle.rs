//! In-memory, per-Username brute-force throttle for Account login (ADR-0061).
//!
//! Account login deliberately has a separate flat policy from the escalating
//! Web Admin PIN throttle. The pure state takes an injected clock so its
//! lockout window can be tested without sleeps; the concurrent wrapper is
//! shared by the client API requests.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::Mutex;

use crate::login_throttle::Decision;

/// Consecutive failed Account logins tolerated before a Username is locked.
pub const FAILURE_THRESHOLD: u32 = 10;

/// Every Account login lockout lasts exactly one minute.
pub const LOCKOUT_SECONDS: i64 = 60;

/// Pure per-Username Account login attempt state.
#[derive(Debug, Clone, Default)]
pub struct AccountLoginAttemptState {
    consecutive_failures: u32,
    lockout_until: Option<DateTime<Utc>>,
}

impl AccountLoginAttemptState {
    /// Whether this Username may attempt a login at `now`.
    pub fn decision(&self, now: DateTime<Utc>) -> Decision {
        match self.lockout_until {
            Some(until) if until > now => Decision::Locked {
                retry_after_secs: (until - now).num_seconds().max(1),
            },
            _ => Decision::Allow,
        }
    }

    /// Record a failed credential attempt. Returns the lockout duration when
    /// this failure starts a new lockout.
    pub fn record_failure(&mut self, now: DateTime<Utc>) -> Option<i64> {
        if self.decision(now) != Decision::Allow {
            return None;
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures < FAILURE_THRESHOLD {
            return None;
        }
        self.lockout_until = Some(now + Duration::seconds(LOCKOUT_SECONDS));
        self.consecutive_failures = 0;
        Some(LOCKOUT_SECONDS)
    }

    /// Clear both consecutive failures and any active lockout.
    pub fn record_success(&mut self) {
        *self = Self::default();
    }
}

/// Concurrent per-normalized-Username Account login throttle shared across
/// client API requests. State is ephemeral and has no database representation.
#[derive(Clone, Default)]
pub struct AccountLoginThrottle {
    states: Arc<Mutex<HashMap<String, AccountLoginAttemptState>>>,
}

impl AccountLoginThrottle {
    /// Check a normalized Username without creating a bucket for it.
    pub async fn check(&self, username_normalized: &str, now: DateTime<Utc>) -> Decision {
        self.states
            .lock()
            .await
            .get(username_normalized)
            .map_or(Decision::Allow, |state| state.decision(now))
    }

    /// Record a failed login for an existing, normalized Username.
    pub async fn record_failure(
        &self,
        username_normalized: &str,
        now: DateTime<Utc>,
    ) -> Option<i64> {
        self.states
            .lock()
            .await
            .entry(username_normalized.to_owned())
            .or_default()
            .record_failure(now)
    }

    /// Clear a Username's failure state after successful login or Password
    /// Reset. A missing Username does not create a bucket.
    pub async fn record_success(&self, username_normalized: &str) {
        if let Some(state) = self.states.lock().await.get_mut(username_normalized) {
            state.record_success();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    #[test]
    fn expires_exactly_at_the_end_of_the_window() {
        let now = t0();
        let mut state = AccountLoginAttemptState::default();
        for _ in 0..FAILURE_THRESHOLD {
            state.record_failure(now);
        }

        assert!(matches!(
            state.decision(now + Duration::seconds(LOCKOUT_SECONDS - 1)),
            Decision::Locked { .. }
        ));
        assert_eq!(
            state.decision(now + Duration::seconds(LOCKOUT_SECONDS)),
            Decision::Allow
        );
    }

    #[test]
    fn second_and_eighth_lockouts_are_the_same_length() {
        let mut state = AccountLoginAttemptState::default();
        let mut now = t0();

        for _ in 0..8 {
            let mut lockout = None;
            for _ in 0..FAILURE_THRESHOLD {
                lockout = state.record_failure(now);
            }
            assert_eq!(lockout, Some(LOCKOUT_SECONDS));
            now += Duration::seconds(LOCKOUT_SECONDS);
            assert_eq!(state.decision(now), Decision::Allow);
            now += Duration::seconds(1);
        }
    }

    #[test]
    fn failures_during_lockout_do_not_start_another_transition() {
        let now = t0();
        let mut state = AccountLoginAttemptState::default();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(state.record_failure(now), None);
        }
        assert_eq!(state.record_failure(now), Some(LOCKOUT_SECONDS));

        for _ in 0..FAILURE_THRESHOLD {
            assert_eq!(state.record_failure(now + Duration::seconds(1)), None);
        }
        assert!(matches!(
            state.decision(now + Duration::seconds(1)),
            Decision::Locked {
                retry_after_secs: 59
            }
        ));
    }

    #[test]
    fn success_resets_failures_and_a_pending_lockout() {
        let now = t0();
        let mut state = AccountLoginAttemptState::default();
        for _ in 0..FAILURE_THRESHOLD {
            state.record_failure(now);
        }
        state.record_success();
        assert_eq!(state.decision(now), Decision::Allow);

        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(state.record_failure(now), None);
        }
        assert_eq!(state.record_failure(now), Some(LOCKOUT_SECONDS));
    }

    #[tokio::test]
    async fn different_usernames_have_independent_state() {
        let now = t0();
        let throttle = AccountLoginThrottle::default();
        for _ in 0..FAILURE_THRESHOLD {
            throttle.record_failure("first", now).await;
        }

        assert!(matches!(
            throttle.check("first", now).await,
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.check("second", now).await, Decision::Allow);
    }
}
