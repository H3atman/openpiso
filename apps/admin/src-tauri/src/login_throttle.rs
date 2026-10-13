//! In-memory, per-IP brute-force throttle for the Web Admin PIN login
//! (`POST /web-admin/login`, ADR-0024). The Web Admin PIN is a short numeric
//! convenience gate, so an unthrottled login is trivially brute-forceable on a
//! LAN. This adds an exponential-backoff speed bump matched to that threat: it
//! never permanently locks the owner out, and its state is ephemeral (a process
//! restart clears it, which is acceptable because a LAN attacker cannot force a
//! remote restart).
//!
//! The time-dependent decision logic lives in the pure [`AttemptState`], whose
//! methods take an injected `now`, so backoff/cap/decay/reset are tested
//! deterministically without real sleeps. [`LoginThrottle`] is the concurrent
//! per-IP wrapper the server shares across requests.

use std::{collections::HashMap, net::IpAddr, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::Mutex;

/// Consecutive failures tolerated before an IP is locked out. Enough headroom
/// for honest fat-fingering (user story 6).
pub const FAILURE_THRESHOLD: u32 = 5;

/// First lockout duration in seconds; doubles per further lockout burst.
const BASE_LOCKOUT_SECONDS: i64 = 60;

/// Upper bound on any single lockout window (~15 min). Guarantees a lockout
/// always decays and is never permanent (user story 5).
const MAX_LOCKOUT_SECONDS: i64 = 900;

/// Outcome of checking whether an IP may attempt a login right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The IP is allowed to attempt; verify the PIN.
    Allow,
    /// The IP is within a lockout window; do not verify the PIN.
    Locked { retry_after_secs: i64 },
}

/// Per-IP attempt bookkeeping. Pure: every time-dependent method takes an
/// injected `now` and never reads the wall clock itself.
#[derive(Debug, Clone, Default)]
pub struct AttemptState {
    /// Failures since the last lockout or success.
    consecutive_failures: u32,
    /// How many lockouts this IP has triggered — the backoff exponent.
    lockout_count: u32,
    /// When the current lockout ends, if any.
    lockout_until: Option<DateTime<Utc>>,
}

impl AttemptState {
    /// Whether this IP may attempt a login at `now`. A lockout whose window has
    /// elapsed reads as [`Decision::Allow`] (lockouts always decay).
    pub fn decision(&self, now: DateTime<Utc>) -> Decision {
        match self.lockout_until {
            Some(until) if until > now => Decision::Locked {
                // At least 1s so a live countdown never shows 0 while locked.
                retry_after_secs: (until - now).num_seconds().max(1),
            },
            _ => Decision::Allow,
        }
    }

    /// Record a failed attempt. Returns `Some(lockout_seconds)` exactly when
    /// this failure transitions the IP into a *new* lockout — the single point
    /// at which the throttle audit event should be emitted.
    pub fn record_failure(&mut self, now: DateTime<Utc>) -> Option<i64> {
        self.consecutive_failures += 1;
        if self.consecutive_failures < FAILURE_THRESHOLD {
            return None;
        }
        let seconds = lockout_seconds(self.lockout_count);
        self.lockout_until = Some(now + Duration::seconds(seconds));
        self.lockout_count = self.lockout_count.saturating_add(1);
        self.consecutive_failures = 0;
        Some(seconds)
    }

    /// A successful login clears all failure state for this IP (user story 4).
    pub fn record_success(&mut self) {
        *self = Self::default();
    }
}

/// Lockout window for the `n`-th lockout (0-based): `60 << n`, capped at
/// [`MAX_LOCKOUT_SECONDS`]. 60s → 120s → 240s → 480s → 900s → 900s …
fn lockout_seconds(lockout_count: u32) -> i64 {
    BASE_LOCKOUT_SECONDS
        .checked_shl(lockout_count.min(u32::BITS - 1))
        .filter(|&secs| secs > 0)
        .unwrap_or(MAX_LOCKOUT_SECONDS)
        .min(MAX_LOCKOUT_SECONDS)
}

/// Concurrent per-IP throttle shared (via `Arc`) across owner-server requests.
/// State is in-memory only — no SQLite table, no migration — and cleared on
/// process restart by design.
#[derive(Clone, Default)]
pub struct LoginThrottle {
    states: Arc<Mutex<HashMap<IpAddr, AttemptState>>>,
}

impl LoginThrottle {
    /// Decision for `ip` at `now`. An IP that has never failed is allowed.
    pub async fn check(&self, ip: IpAddr, now: DateTime<Utc>) -> Decision {
        self.states
            .lock()
            .await
            .get(&ip)
            .map_or(Decision::Allow, |state| state.decision(now))
    }

    /// Record a failure for `ip`. Returns `Some(lockout_seconds)` only on the
    /// transition into a new lockout.
    pub async fn record_failure(&self, ip: IpAddr, now: DateTime<Utc>) -> Option<i64> {
        self.states
            .lock()
            .await
            .entry(ip)
            .or_default()
            .record_failure(now)
    }

    /// Clear `ip`'s failure state after a successful login. A no-op for an IP
    /// that has no recorded failures, so it never grows the map.
    pub async fn record_success(&self, ip: IpAddr) {
        if let Some(state) = self.states.lock().await.get_mut(&ip) {
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
    fn allows_until_threshold_then_locks() {
        let now = t0();
        let mut state = AttemptState::default();
        // The first THRESHOLD-1 failures do not lock.
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(state.record_failure(now), None);
            assert_eq!(state.decision(now), Decision::Allow);
        }
        // The THRESHOLD-th failure locks with the base window.
        assert_eq!(state.record_failure(now), Some(BASE_LOCKOUT_SECONDS));
        assert_eq!(
            state.decision(now),
            Decision::Locked {
                retry_after_secs: BASE_LOCKOUT_SECONDS
            }
        );
    }

    #[test]
    fn lockout_decays_after_its_window() {
        let now = t0();
        let mut state = AttemptState::default();
        for _ in 0..FAILURE_THRESHOLD {
            state.record_failure(now);
        }
        // Still locked just before the window ends.
        let almost = now + Duration::seconds(BASE_LOCKOUT_SECONDS - 1);
        assert!(matches!(state.decision(almost), Decision::Locked { .. }));
        // Allowed once the window has elapsed.
        let after = now + Duration::seconds(BASE_LOCKOUT_SECONDS);
        assert_eq!(state.decision(after), Decision::Allow);
    }

    #[test]
    fn backoff_doubles_on_successive_lockouts_and_caps() {
        let mut state = AttemptState::default();
        let mut now = t0();
        let mut expected = BASE_LOCKOUT_SECONDS;
        for _ in 0..8 {
            let mut triggered = None;
            for _ in 0..FAILURE_THRESHOLD {
                triggered = state.record_failure(now);
            }
            let capped = expected.min(MAX_LOCKOUT_SECONDS);
            assert_eq!(triggered, Some(capped));
            // Move past this lockout to allow the next burst.
            now += Duration::seconds(capped + 1);
            expected = (expected * 2).min(MAX_LOCKOUT_SECONDS);
        }
        // Backoff has reached and holds the cap.
        assert_eq!(expected, MAX_LOCKOUT_SECONDS);
    }

    #[test]
    fn success_resets_failures_and_backoff() {
        let now = t0();
        let mut state = AttemptState::default();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            state.record_failure(now);
        }
        state.record_success();
        // Back to a clean slate: it takes a full THRESHOLD again to lock, and
        // the window is the base (backoff exponent reset).
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert_eq!(state.record_failure(now), None);
        }
        assert_eq!(state.record_failure(now), Some(BASE_LOCKOUT_SECONDS));
    }

    #[tokio::test]
    async fn throttle_is_per_ip() {
        let now = t0();
        let throttle = LoginThrottle::default();
        let attacker: IpAddr = "10.0.0.9".parse().unwrap();
        let owner: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..FAILURE_THRESHOLD {
            throttle.record_failure(attacker, now).await;
        }
        assert!(matches!(
            throttle.check(attacker, now).await,
            Decision::Locked { .. }
        ));
        // A different machine is unaffected.
        assert_eq!(throttle.check(owner, now).await, Decision::Allow);
        // A success clears the attacker's lockout state.
        throttle.record_success(attacker).await;
        assert_eq!(throttle.check(attacker, now).await, Decision::Allow);
    }
}
