//! The Diagnostic Request (ADR-0078, #364).
//!
//! The top-up for a PC that is still alive: when the forwarded allowlist did
//! not carry enough, the owner asks that Client for more. Admin places a
//! single-use nonce on the Heartbeat response; the Client acts once per nonce
//! and uploads one windowed, byte-capped slice of its own log through the
//! batched Forwarded Diagnostics endpoint, echoing the nonce so Admin knows
//! the request was fulfilled.
//!
//! State is deliberately in-memory, mirroring [`crate::guard_poll`]: the
//! request describes an *ask right now*, it dies with the process that made
//! it, and a restart must not resurrect a stale ask. A PC that is powered off
//! or unreachable simply lets the request expire — that is the whole failure
//! mode, and it produces no owner-facing error beyond the request going
//! unfulfilled.
//!
//! Nothing here touches money, Remaining Time, Points, sessions, or routing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

/// How long an armed request is delivered on Heartbeat responses before it
/// goes unfulfilled. A live Client answers within one Heartbeat interval;
/// the TTL only needs to cover a PC that comes back online while the owner
/// is still looking, not forever.
pub const DIAGNOSTIC_REQUEST_TTL_SECS: i64 = 600;

/// One armed request for one PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedDiagnosticRequest {
    /// Idempotence key. The Client acts once per nonce, so a repeated
    /// Heartbeat response cannot cause a repeated upload.
    pub nonce: String,
    pub armed_at: DateTime<Utc>,
}

impl ArmedDiagnosticRequest {
    fn expired(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.armed_at).num_seconds() >= DIAGNOSTIC_REQUEST_TTL_SECS
    }
}

/// Shared, in-memory Diagnostic Request state, keyed by PC id.
#[derive(Clone, Default)]
pub struct DiagnosticRequestRegistry(Arc<Mutex<HashMap<String, ArmedDiagnosticRequest>>>);

impl DiagnosticRequestRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place (or re-state) the owner's ask for one PC. Re-arming an
    /// unexpired request keeps its nonce, so an owner double-click cannot
    /// cause two uploads; an expired request is replaced with a fresh one.
    pub fn arm(&self, pc_id: &str, now: DateTime<Utc>) -> ArmedDiagnosticRequest {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard
            .entry(pc_id.to_owned())
            .or_insert_with(|| ArmedDiagnosticRequest {
                nonce: uuid::Uuid::new_v4().to_string(),
                armed_at: now,
            });
        if entry.expired(now) {
            *entry = ArmedDiagnosticRequest {
                nonce: uuid::Uuid::new_v4().to_string(),
                armed_at: now,
            };
        }
        entry.clone()
    }

    /// The nonce the next Heartbeat response for this PC should carry, or
    /// `None` when nothing is armed. An expired request is dropped lazily
    /// here, so an ask never outlives the owner's attention. Deliberately
    /// does *not* consume: the same response may repeat until the Client's
    /// answer arrives.
    pub fn pending(&self, pc_id: &str, now: DateTime<Utc>) -> Option<String> {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = guard.get(pc_id).is_some_and(|request| request.expired(now));
        if expired {
            guard.remove(pc_id);
            return None;
        }
        guard.get(pc_id).map(|request| request.nonce.clone())
    }

    /// The Client's answer arrived: stop carrying the nonce on Heartbeat
    /// responses. Only an exact nonce match consumes — a stale or regular
    /// forwarded batch must not silence a request that was never answered.
    pub fn consume(&self, pc_id: &str, nonce: &str) -> bool {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let matches = guard
            .get(pc_id)
            .is_some_and(|request| request.nonce == nonce);
        if matches {
            guard.remove(pc_id);
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn an_armed_request_keeps_its_nonce_across_repeated_arming() {
        let registry = DiagnosticRequestRegistry::new();
        let first = registry.arm("pc-01", now());
        let second = registry.arm("pc-01", now());
        assert_eq!(first, second, "an unexpired ask is re-stated, not stacked");
        assert_eq!(registry.pending("pc-01", now()), Some(first.nonce));
    }

    #[test]
    fn pending_does_not_consume_the_request() {
        let registry = DiagnosticRequestRegistry::new();
        let armed = registry.arm("pc-01", now());
        for _ in 0..3 {
            assert_eq!(
                registry.pending("pc-01", now()),
                Some(armed.nonce.clone()),
                "the same response may repeat until the Client answers"
            );
        }
    }

    #[test]
    fn a_client_answer_consumes_only_its_own_nonce() {
        let registry = DiagnosticRequestRegistry::new();
        registry.arm("pc-01", now());
        assert!(!registry.consume("pc-01", "some-other-nonce"));
        assert!(
            registry.pending("pc-01", now()).is_some(),
            "a foreign nonce must not silence the ask"
        );
        let nonce = registry.pending("pc-01", now()).expect("armed");
        assert!(registry.consume("pc-01", &nonce));
        assert_eq!(registry.pending("pc-01", now()), None);
        assert!(!registry.consume("pc-01", &nonce), "single use");
    }

    #[test]
    fn an_expired_request_goes_unfulfilled_without_error() {
        let registry = DiagnosticRequestRegistry::new();
        let armed_at = now() - chrono::Duration::seconds(1);
        let expired_at = armed_at + chrono::Duration::seconds(DIAGNOSTIC_REQUEST_TTL_SECS);
        registry.arm("pc-01", armed_at);
        assert_eq!(registry.pending("pc-02", armed_at), None, "other PC");
        assert!(registry.pending("pc-01", expired_at).is_none());
        // A later arm starts a fresh ask rather than resurrecting the old one.
        let re_armed = registry.arm("pc-01", expired_at);
        assert_eq!(registry.pending("pc-01", expired_at), Some(re_armed.nonce));
    }

    #[test]
    fn a_consumed_request_is_not_resurrected_by_expiry_bookkeeping() {
        let registry = DiagnosticRequestRegistry::new();
        let armed = registry.arm("pc-01", now());
        assert!(registry.consume("pc-01", &armed.nonce));
        assert_eq!(registry.pending("pc-01", now()), None);
    }
}
