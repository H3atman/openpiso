//! State the guard loop observes and the poll thread reports (ADR-0077).
//!
//! Three facts and one cached endpoint, shared by exactly two threads. The
//! facts are atomics rather than a lock because the poll thread only ever
//! reads them and must never be able to stall the loop that relaunches
//! Clients.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;

use crate::endpoint::GuardEndpoint;
use crate::poll::GuardReport;

#[derive(Debug)]
pub struct GuardShared {
    /// Whether a verified Client currently holds the pipe.
    client_connected: AtomicBool,
    /// Whether this boot's Guard stood down after a PIN-gated owner quit.
    disarmed: AtomicBool,
    /// Relaunch attempts spent on the current absence. Reset the moment a
    /// Client connects, because the count is evidence about *this* failure and
    /// a recovered PC has none.
    relaunch_attempts: AtomicI64,
    /// Cached poll target, learned from the Client over the pipe.
    endpoint: Mutex<GuardEndpoint>,
}

impl GuardShared {
    pub fn new(endpoint: GuardEndpoint) -> Self {
        Self {
            client_connected: AtomicBool::new(false),
            disarmed: AtomicBool::new(false),
            relaunch_attempts: AtomicI64::new(0),
            endpoint: Mutex::new(endpoint),
        }
    }

    pub fn set_client_connected(&self, connected: bool) {
        self.client_connected.store(connected, Ordering::Relaxed);
        if connected {
            self.relaunch_attempts.store(0, Ordering::Relaxed);
        }
    }

    pub fn client_connected(&self) -> bool {
        self.client_connected.load(Ordering::Relaxed)
    }

    pub fn set_disarmed(&self, disarmed: bool) {
        self.disarmed.store(disarmed, Ordering::Relaxed);
    }

    pub fn record_relaunch_attempt(&self) {
        self.relaunch_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the facts for one poll.
    pub fn report(&self) -> GuardReport {
        GuardReport {
            client_connected: self.client_connected.load(Ordering::Relaxed),
            disarmed: self.disarmed.load(Ordering::Relaxed),
            relaunch_attempts: self.relaunch_attempts.load(Ordering::Relaxed),
        }
    }

    /// The cached endpoint, or `None` when the lock is poisoned — treated as
    /// "cannot poll" so a broken lock leaves PCs on rather than off.
    pub fn endpoint(&self) -> Option<GuardEndpoint> {
        self.endpoint.lock().ok().map(|endpoint| endpoint.clone())
    }

    /// The `(url, mac)` to poll with, or `None` when nothing cached is
    /// complete enough to poll with. A Guard without a poll target makes no
    /// request at all — and so forwards nothing, and logs nothing about
    /// forwarding.
    pub fn poll_target(&self) -> Option<(String, String)> {
        let cached = self.endpoint().filter(GuardEndpoint::is_pollable)?;
        let url = cached.admin_url.clone()?;
        let mac = cached.mac.clone()?;
        Some((url, mac))
    }

    /// Record the Admin URL and MAC the Client just reported. Returns the
    /// endpoint to persist when it actually changed, so an unchanged
    /// reconnection does not rewrite the file on every Client start.
    pub fn learn_endpoint(&self, url: &str, mac: &str) -> Option<GuardEndpoint> {
        let mut endpoint = self.endpoint.lock().ok()?;
        if endpoint.admin_url.as_deref() == Some(url) && endpoint.mac.as_deref() == Some(mac) {
            return None;
        }
        endpoint.admin_url = Some(url.to_owned());
        endpoint.mac = Some(mac.to_owned());
        Some(endpoint.clone())
    }

    /// Record that this Guard acted on `nonce`. Returns the endpoint to
    /// persist, so the action survives a service restart and cannot be
    /// repeated.
    pub fn record_acted_nonce(&self, nonce: &str) -> Option<GuardEndpoint> {
        let mut endpoint = self.endpoint.lock().ok()?;
        endpoint.acted_nonce = Some(nonce.to_owned());
        Some(endpoint.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connected_client_clears_the_relaunch_evidence() {
        let shared = GuardShared::new(GuardEndpoint::default());
        shared.record_relaunch_attempt();
        shared.record_relaunch_attempt();
        assert_eq!(shared.report().relaunch_attempts, 2);

        shared.set_client_connected(true);
        let report = shared.report();
        assert!(report.client_connected);
        assert_eq!(report.relaunch_attempts, 0);
    }

    #[test]
    fn losing_the_client_does_not_erase_the_attempts_it_cost() {
        let shared = GuardShared::new(GuardEndpoint::default());
        shared.set_client_connected(true);
        shared.set_client_connected(false);
        shared.record_relaunch_attempt();
        assert_eq!(shared.report().relaunch_attempts, 1);
        assert!(!shared.report().client_connected);
    }

    #[test]
    fn an_unchanged_endpoint_is_not_rewritten() {
        let shared = GuardShared::new(GuardEndpoint::default());
        assert!(shared
            .learn_endpoint("http://a:3210", "aa:bb:cc:dd:ee:ff")
            .is_some());
        assert!(shared
            .learn_endpoint("http://a:3210", "aa:bb:cc:dd:ee:ff")
            .is_none());
        assert!(shared
            .learn_endpoint("http://b:3210", "aa:bb:cc:dd:ee:ff")
            .is_some());
    }

    #[test]
    fn learning_an_endpoint_preserves_the_acted_nonce() {
        let shared = GuardShared::new(GuardEndpoint {
            acted_nonce: Some("abc".to_owned()),
            ..GuardEndpoint::default()
        });
        let learned = shared
            .learn_endpoint("http://a:3210", "aa:bb:cc:dd:ee:ff")
            .expect("changed");
        assert_eq!(learned.acted_nonce.as_deref(), Some("abc"));
        assert!(learned.is_pollable());
    }

    #[test]
    fn a_disarmed_guard_reports_it() {
        let shared = GuardShared::new(GuardEndpoint::default());
        assert!(!shared.report().disarmed);
        shared.set_disarmed(true);
        assert!(shared.report().disarmed);
    }

    #[test]
    fn a_poll_target_needs_both_halves_of_the_endpoint() {
        let shared = GuardShared::new(GuardEndpoint::default());
        assert_eq!(shared.poll_target(), None, "nothing cached, no target");
        shared.learn_endpoint("http://a:3210", "aa:bb:cc:dd:ee:ff");
        assert_eq!(
            shared.poll_target(),
            Some(("http://a:3210".to_owned(), "aa:bb:cc:dd:ee:ff".to_owned()))
        );
    }
}
