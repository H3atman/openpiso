//! Guard Poll facts and the Client Recovery Escalation predicate (ADR-0077).
//!
//! The Client Guard is the only party that can see whether a Client process is
//! actually holding its pipe. ADR-0076 could learn that only from the Client
//! heartbeat, which is exactly what is missing during a **Client Reachability
//! Mismatch**, so a mismatching PC whose Guard was dead looked identical to one
//! whose Guard was alive and losing. The Guard Poll closes that gap: the Guard
//! reports its own facts on the way in and reads back the desired power state
//! on the way out.
//!
//! Facts are deliberately in-memory. They describe *right now*, they are
//! rebuilt within one poll interval of an Admin restart, and a restart must
//! restart the escalation clock rather than resume it. The only durable state
//! this feature writes is the settings, the Audit event, the incident closure,
//! and the cooldown.
//!
//! Nothing here touches money, Remaining Time, Points, sessions, or routing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

/// A Guard whose last poll is older than this is not polling. Three missed
/// ten-second polls: long enough to ride out one slow round, short enough that
/// a stopped service stops counting as evidence within the escalation delay.
pub const GUARD_POLL_STALE_AFTER_SECS: i64 = 35;

/// What one Guard reported about itself on its last poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardFacts {
    /// When Admin received the poll. Freshness is measured on the Admin clock;
    /// the Guard never supplies a time.
    pub received_at: DateTime<Utc>,
    /// Whether a verified Client currently holds the Guard pipe.
    pub client_connected: bool,
    /// Whether the Guard stood down for this boot after a PIN-gated owner quit.
    pub disarmed: bool,
    /// Relaunch attempts spent on the current absence. Zero means the Guard has
    /// not yet tried, and a PC whose Guard has not tried is never escalated.
    pub relaunch_attempts: i64,
}

/// The desired power state one PC's Guard reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerOffOrder {
    /// Idempotence key. The Guard acts once per nonce, so a repeated read of
    /// the same desired state cannot power a PC off twice.
    pub nonce: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct Entry {
    facts: Option<GuardFacts>,
    order: Option<PowerOffOrder>,
}

/// Shared, in-memory Guard Poll state, keyed by PC id.
#[derive(Clone, Default)]
pub struct GuardRegistry(Arc<Mutex<HashMap<String, Entry>>>);

impl GuardRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one poll and hand back whatever this PC is currently ordered to
    /// do. A poisoned lock is treated as "no order": the failure mode of this
    /// registry must be a PC that stays on.
    pub fn record_poll(&self, pc_id: &str, facts: GuardFacts) -> Option<PowerOffOrder> {
        let mut guard = self.0.lock().ok()?;
        let entry = guard.entry(pc_id.to_owned()).or_default();
        entry.facts = Some(facts);
        entry.order.clone()
    }

    /// The last facts reported by this PC's Guard, or `None` when it has never
    /// polled.
    pub fn facts(&self, pc_id: &str) -> Option<GuardFacts> {
        self.0.lock().ok()?.get(pc_id)?.facts.clone()
    }

    /// Arm an Escalated Power Off for one PC. Re-arming an already-ordered PC
    /// keeps the original nonce so a Guard that already acted does not act
    /// again.
    pub fn arm_power_off(&self, pc_id: &str, nonce: String, decided_at: DateTime<Utc>) {
        let Ok(mut guard) = self.0.lock() else {
            return;
        };
        let entry = guard.entry(pc_id.to_owned()).or_default();
        if entry.order.is_none() {
            entry.order = Some(PowerOffOrder { nonce, decided_at });
        }
    }

    /// Whether this PC currently carries an order.
    pub fn is_armed(&self, pc_id: &str) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|guard| guard.get(pc_id).map(|entry| entry.order.is_some()))
            .unwrap_or(false)
    }

    /// Withdraw any order for one PC. Called as soon as the PC reports Client
    /// Reachability again, so a machine that recovers on its own is never
    /// powered off by a stale decision.
    pub fn clear_power_off(&self, pc_id: &str) {
        if let Ok(mut guard) = self.0.lock() {
            if let Some(entry) = guard.get_mut(pc_id) {
                entry.order = None;
            }
        }
    }
}

/// Everything the escalation decision reads for one PC in one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationInputs {
    pub host_reachable: bool,
    pub client_reachable: bool,
    pub suppressed: bool,
    /// Absent when this PC's Guard has never polled.
    pub guard: Option<GuardFacts>,
}

/// The seven conditions of ADR-0077, as one pure predicate.
///
/// Every one of them is a reason *not* to power a PC off, which is why they are
/// evaluated together and why any single false clears the clock. In particular
/// a Guard that is not polling fails this: without a poll there is no channel
/// to carry the order anyway, and a dead Guard is a Client Guard Coverage
/// problem for the owner rather than a second enforcement path.
pub fn escalation_condition_holds(inputs: &EscalationInputs, now: DateTime<Utc>) -> bool {
    if !inputs.host_reachable || inputs.client_reachable || inputs.suppressed {
        return false;
    }
    let Some(guard) = &inputs.guard else {
        return false;
    };
    let age = now.signed_duration_since(guard.received_at).num_seconds();
    if !(0..=GUARD_POLL_STALE_AFTER_SECS).contains(&age) {
        return false;
    }
    !guard.client_connected && !guard.disarmed && guard.relaunch_attempts >= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts(now: DateTime<Utc>) -> GuardFacts {
        GuardFacts {
            received_at: now,
            client_connected: false,
            disarmed: false,
            relaunch_attempts: 1,
        }
    }

    fn inputs(now: DateTime<Utc>) -> EscalationInputs {
        EscalationInputs {
            host_reachable: true,
            client_reachable: false,
            suppressed: false,
            guard: Some(base_facts(now)),
        }
    }

    #[test]
    fn the_full_condition_holds_only_when_every_part_does() {
        let now = Utc::now();
        assert!(escalation_condition_holds(&inputs(now), now));
    }

    #[test]
    fn every_single_missing_condition_refuses_escalation() {
        let now = Utc::now();
        let cases: Vec<(&str, EscalationInputs)> = vec![
            ("host unreachable", {
                let mut i = inputs(now);
                i.host_reachable = false;
                i
            }),
            ("client recovered", {
                let mut i = inputs(now);
                i.client_reachable = true;
                i
            }),
            ("suppressed", {
                let mut i = inputs(now);
                i.suppressed = true;
                i
            }),
            ("guard never polled", {
                let mut i = inputs(now);
                i.guard = None;
                i
            }),
            ("guard reports a connected Client", {
                let mut i = inputs(now);
                i.guard = Some(GuardFacts {
                    client_connected: true,
                    ..base_facts(now)
                });
                i
            }),
            ("guard disarmed for owner maintenance", {
                let mut i = inputs(now);
                i.guard = Some(GuardFacts {
                    disarmed: true,
                    ..base_facts(now)
                });
                i
            }),
            ("guard has not tried a relaunch", {
                let mut i = inputs(now);
                i.guard = Some(GuardFacts {
                    relaunch_attempts: 0,
                    ..base_facts(now)
                });
                i
            }),
        ];
        for (name, case) in cases {
            assert!(
                !escalation_condition_holds(&case, now),
                "{name} must not escalate"
            );
        }
    }

    #[test]
    fn a_guard_that_stopped_polling_is_no_longer_evidence() {
        let now = Utc::now();
        let stale = EscalationInputs {
            guard: Some(GuardFacts {
                received_at: now - chrono::Duration::seconds(GUARD_POLL_STALE_AFTER_SECS + 1),
                ..base_facts(now)
            }),
            ..inputs(now)
        };
        assert!(!escalation_condition_holds(&stale, now));
    }

    #[test]
    fn an_order_keeps_its_nonce_across_repeated_arming() {
        let registry = GuardRegistry::new();
        let now = Utc::now();
        registry.arm_power_off("pc-01", "first".to_owned(), now);
        registry.arm_power_off("pc-01", "second".to_owned(), now);
        let order = registry
            .record_poll("pc-01", base_facts(now))
            .expect("armed PC returns its order");
        assert_eq!(order.nonce, "first");
    }

    #[test]
    fn a_recovered_pc_loses_its_order() {
        let registry = GuardRegistry::new();
        let now = Utc::now();
        registry.arm_power_off("pc-01", "nonce".to_owned(), now);
        assert!(registry.is_armed("pc-01"));
        registry.clear_power_off("pc-01");
        assert!(!registry.is_armed("pc-01"));
        assert_eq!(registry.record_poll("pc-01", base_facts(now)), None);
    }

    #[test]
    fn polls_are_recorded_for_later_reads() {
        let registry = GuardRegistry::new();
        let now = Utc::now();
        assert_eq!(registry.facts("pc-01"), None);
        registry.record_poll("pc-01", base_facts(now));
        assert_eq!(registry.facts("pc-01"), Some(base_facts(now)));
    }
}
