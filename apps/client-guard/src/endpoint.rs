//! Where the Guard polls, and what it has already acted on (ADR-0077).
//!
//! The Guard has no configuration of its own and cannot read the Client's:
//! the Client stores its config under a per-user `%APPDATA%`, and the Guard is
//! a SYSTEM service. So the Client hands its Admin URL and its own MAC down
//! the pipe, and the Guard caches them beside its logs in `%ProgramData%`.
//!
//! Two consequences are deliberate. A Guard that has never seen a Client
//! connect has no endpoint and therefore never polls — which is exactly the
//! case the level-triggered relaunch handles without any server. And a
//! re-pointed Admin propagates on the next Client connection, with no
//! reinstall and no second place to edit.
//!
//! The last acted nonce lives here too. It is the whole of the Guard's
//! idempotence: an order is a desired state the Guard may read many times, and
//! acting once per nonce is what stops a re-read from powering a PC off twice.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Cached poll target and action history. Absent fields simply mean "not known
/// yet"; nothing here is authority.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardEndpoint {
    /// Admin base URL, e.g. `http://192.168.1.2:3210`.
    #[serde(default)]
    pub admin_url: Option<String>,
    /// The MAC the Admin knows this PC by, as resolved by the Client.
    #[serde(default)]
    pub mac: Option<String>,
    /// The last power-off nonce this Guard acted on.
    #[serde(default)]
    pub acted_nonce: Option<String>,
}

impl GuardEndpoint {
    /// Whether this endpoint is complete enough to poll with.
    pub fn is_pollable(&self) -> bool {
        self.admin_url.is_some() && self.mac.is_some()
    }
}

/// Machine-wide state directory, shared with the Guard's rotating logs.
pub fn state_dir() -> PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .unwrap_or_else(|| std::ffi::OsString::from(r"C:\ProgramData"));
    Path::new(&program_data).join("OpenPiso")
}

fn state_path() -> PathBuf {
    state_dir().join("client-guard-endpoint.json")
}

/// Read the cached endpoint. Every failure — missing file, unreadable
/// directory, malformed JSON — reads as "nothing cached" rather than an error:
/// a Guard that cannot read its cache must still relaunch Clients, and a
/// corrupt cache must never stop it from doing so.
pub fn load() -> GuardEndpoint {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist the endpoint. Returns the OS error text on failure so the caller
/// can log it; a failed write costs the next boot's cache, never this boot's
/// protection.
pub fn save(endpoint: &GuardEndpoint) -> Result<(), String> {
    let directory = state_dir();
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let serialized = serde_json::to_string_pretty(endpoint).map_err(|error| error.to_string())?;
    fs::write(state_path(), serialized).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_pollable_only_with_both_halves() {
        let mut endpoint = GuardEndpoint::default();
        assert!(!endpoint.is_pollable());
        endpoint.admin_url = Some("http://192.168.1.2:3210".to_owned());
        assert!(!endpoint.is_pollable());
        endpoint.mac = Some("aa:bb:cc:dd:ee:ff".to_owned());
        assert!(endpoint.is_pollable());
    }

    #[test]
    fn a_missing_or_corrupt_cache_reads_as_nothing_known() {
        // `load` must not panic or error on a machine with no cache at all;
        // an empty endpoint is the correct answer.
        let parsed: Option<GuardEndpoint> = serde_json::from_str("{ not json ").ok();
        assert_eq!(parsed, None);
        assert!(!GuardEndpoint::default().is_pollable());
    }

    #[test]
    fn an_endpoint_round_trips_through_its_stored_form() {
        let endpoint = GuardEndpoint {
            admin_url: Some("http://192.168.1.2:3210".to_owned()),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
            acted_nonce: Some("nonce".to_owned()),
        };
        let raw = serde_json::to_string(&endpoint).expect("serializes");
        assert_eq!(
            serde_json::from_str::<GuardEndpoint>(&raw).expect("parses"),
            endpoint
        );
    }
}
