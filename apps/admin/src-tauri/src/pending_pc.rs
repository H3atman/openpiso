//! Pending PCs (ADR-0090 §1): unknown Clients the owner may adopt.
//!
//! A heartbeat whose MAC resolves to no `pcs` row is still rejected with
//! `pc_unregistered`, but the `:3210` handler records it here so the `:8080`
//! owner surface can list and approve it. State is in memory only, capped,
//! and expires shortly after the Client stops knocking, so the
//! unauthenticated endpoint can never grow anything durable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Most Pending PCs remembered at once; a new MAC beyond this is not recorded.
const PENDING_PC_CAP: usize = 64;
/// A Pending PC is forgotten this long after its last knock.
const PENDING_PC_TTL: Duration = Duration::from_secs(60);

/// One Pending PC as the heartbeat observed it. Every field is server-observed
/// (peer address) or display-only (hostname, version); none is authority.
#[derive(Debug, Clone)]
pub struct PendingPc {
    pub mac: String,
    pub source_ip: String,
    pub hostname: String,
    pub client_version: String,
    pub last_seen: Instant,
}

/// The owner-facing row for `GET /web-admin/pcs/pending`.
#[derive(Debug, Serialize)]
pub struct PendingPcView {
    pub mac: String,
    pub approval_code: String,
    pub source_ip: String,
    pub hostname: String,
    pub client_version: String,
    pub last_seen_seconds_ago: u64,
}

/// The canonical MAC key, or `None` for anything but six colon-separated hex
/// octets — the shape the Client's `format_mac` sends. Lowercased, matching
/// how `db::pc_id_by_mac` compares stored MACs.
pub fn normalize_mac(mac: &str) -> Option<String> {
    let mac = mac.trim().to_lowercase();
    let octets: Vec<&str> = mac.split(':').collect();
    let well_formed = octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.chars().all(|c| c.is_ascii_hexdigit()));
    well_formed.then_some(mac)
}

/// The Approval Code the Client's Lock and the Web Admin both show: the last
/// two octets of a normalized MAC, uppercase. Derived, so it needs no storage.
pub fn approval_code(mac: &str) -> String {
    mac[mac.len() - 5..].replace(':', "").to_uppercase()
}

/// Shared Pending PC state, keyed by normalized MAC.
#[derive(Clone, Default)]
pub struct PendingPcRegistry(Arc<Mutex<HashMap<String, PendingPc>>>);

impl PendingPcRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self, now: Instant) -> std::sync::MutexGuard<'_, HashMap<String, PendingPc>> {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.retain(|_, entry| now.duration_since(entry.last_seen) < PENDING_PC_TTL);
        guard
    }

    /// Record or refresh a knock and return its Approval Code. A malformed or
    /// empty MAC is never pending and gets no code; a new MAC is dropped (but
    /// still gets its code) while the store is full.
    pub fn record(
        &self,
        mac: &str,
        source_ip: &str,
        hostname: &str,
        client_version: &str,
        now: Instant,
    ) -> Option<String> {
        let mac = normalize_mac(mac)?;
        let code = approval_code(&mac);
        let mut guard = self.locked(now);
        if !guard.contains_key(&mac) && guard.len() >= PENDING_PC_CAP {
            return Some(code);
        }
        guard.insert(
            mac.clone(),
            PendingPc {
                mac,
                source_ip: source_ip.to_owned(),
                hostname: hostname.trim().to_owned(),
                client_version: client_version.trim().to_owned(),
                last_seen: now,
            },
        );
        Some(code)
    }

    /// Unexpired Pending PCs, most recently seen first.
    pub fn list(&self, now: Instant) -> Vec<PendingPcView> {
        let mut entries: Vec<PendingPc> = self.locked(now).values().cloned().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_seen));
        entries
            .into_iter()
            .map(|entry| PendingPcView {
                approval_code: approval_code(&entry.mac),
                last_seen_seconds_ago: now.duration_since(entry.last_seen).as_secs(),
                mac: entry.mac,
                source_ip: entry.source_ip,
                hostname: entry.hostname,
                client_version: entry.client_version,
            })
            .collect()
    }

    pub fn get(&self, mac: &str, now: Instant) -> Option<PendingPc> {
        self.locked(now).get(&normalize_mac(mac)?).cloned()
    }

    /// Forget an entry; `mac` is a normalized key from [`PendingPc::mac`].
    pub fn remove(&self, mac: &str) {
        self.locked(Instant::now()).remove(mac);
    }
}
