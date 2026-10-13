//! Pure identity resolution for the server-side PC identity model
//! (ADR-0031 / plan §4.2 / fix plan §4 M2).
//!
//! The admin server owns which `pcs` row a booting machine is. Identity is
//! resolved per heartbeat/SSE/disconnect by (a) the MAC the client reports
//! in the `X-OpenPiso-Mac` header and (b) the source IP captured from the
//! connection peer address. The MAC is the durable key; the source IP only
//! bootstraps the mapping once when the row has not yet bound a NIC.
//!
//! The rule is intentionally a **pure free function** so it unit-tests
//! without a DB or a socket. The four branches are:
//!
//! 1. **MAC matches an existing row** → `Identify` it. Steady-state path
//!    on every reboot — the PC is auto-recognized with no re-setup.
//! 2. **No MAC match, but the source IP matches a row whose `mac_address`
//!    is `NULL`** → `Bind` that row's `mac_address`. One-time onboarding
//!    bind; static IPs make it dependable. A row that is already bound
//!    (`mac_address IS NOT NULL`) is **never** re-bound by IP, even if the
//!    source IP happens to match — a fresh MAC from a known IP is
//!    hardware-replacement territory and is rejected until the owner
//!    clears the binding.
//! 3. **No MAC and no IP match** → `Reject`. The unauthenticated endpoint
//!    never auto-creates a row.
//!
//! `127.0.0.1` still matches a loopback row in development, so local
//! tests against an in-memory server keep working.

use serde::{Deserialize, Serialize};

/// A trimmed view of a `pcs` row that the resolution function needs to
/// pick the right identity. Anything more than the durable identity fields
/// only widens the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcIdentityRow {
    pub id: String,
    pub mac_address: Option<String>,
    pub registered_ip: Option<String>,
}

/// The pure resolution decision the server applies. Returned to the
/// call-site; never serialized to the LAN client (rejections carry a
/// typed `pc_unregistered` body instead, see `server.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// The MAC matched an existing row. The client adopts `pc_id`.
    Identify { pc_id: String },
    /// The source IP matched a row whose `mac_address` is `NULL`; the
    /// server binds the row's `mac_address` to the supplied MAC and
    /// adopts `pc_id`. The bind is applied by the caller inside the
    /// same SQLite transaction that resolved the row, so identify-bind
    /// is atomic.
    Bind { pc_id: String },
    /// The machine is unregistered. The client shows the "ask the
    /// owner" Lock copy.
    Reject,
}

/// Resolve a `(mac, source_ip)` pair against the candidate rows.
///
/// `mac` is the trimmed MAC string the client reported (or `None` if the
/// header was missing/malformed). `source_ip` is the connection peer
/// address as a string (e.g. `"192.168.1.42"`, `"127.0.0.1"`,
/// `"::1"`); comparison is exact and case-insensitive. `rows` is the
/// candidate PC set (typically `SELECT * FROM pcs`); a `LIMIT`/`WHERE`
/// belongs at the call-site if it ever matters.
pub fn resolve(mac: Option<&str>, source_ip: &str, rows: &[PcIdentityRow]) -> Decision {
    let mac = mac.map(str::trim).filter(|value| !value.is_empty());
    let source_ip = source_ip.trim();

    // Step 1: MAC match wins. A row with this MAC is *the* PC, full stop.
    if let Some(mac) = mac {
        if let Some(row) = rows.iter().find(|row| {
            row.mac_address
                .as_deref()
                .is_some_and(|stored| stored.eq_ignore_ascii_case(mac))
        }) {
            return Decision::Identify {
                pc_id: row.id.clone(),
            };
        }
    }

    // Step 2: IP bootstrap, but only against rows that have not yet
    // bound a MAC. A row with a non-NULL `mac_address` is locked to
    // its existing binding — a fresh MAC from the same IP is
    // hardware-replacement territory and is rejected here.
    if let Some(row) = rows.iter().find(|row| {
        row.mac_address.is_none()
            && row
                .registered_ip
                .as_deref()
                .is_some_and(|registered| registered.eq_ignore_ascii_case(source_ip))
    }) {
        return Decision::Bind {
            pc_id: row.id.clone(),
        };
    }

    Decision::Reject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, mac: Option<&str>, ip: Option<&str>) -> PcIdentityRow {
        PcIdentityRow {
            id: id.to_owned(),
            mac_address: mac.map(str::to_owned),
            registered_ip: ip.map(str::to_owned),
        }
    }

    #[test]
    fn mac_match_identifies_existing_pc() {
        let rows = vec![row("pc-01", Some("aa:bb:cc:dd:ee:01"), None)];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:01"), "192.168.1.10", &rows),
            Decision::Identify {
                pc_id: "pc-01".to_owned(),
            }
        );
    }

    #[test]
    fn mac_match_is_case_insensitive() {
        let rows = vec![row("pc-01", Some("AA:BB:CC:DD:EE:01"), None)];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:01"), "192.168.1.10", &rows),
            Decision::Identify {
                pc_id: "pc-01".to_owned(),
            }
        );
    }

    #[test]
    fn ip_match_with_null_mac_binds_a_preregistered_row() {
        let rows = vec![row("pc-01", None, Some("192.168.1.10"))];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:99"), "192.168.1.10", &rows),
            Decision::Bind {
                pc_id: "pc-01".to_owned(),
            }
        );
    }

    #[test]
    fn ip_match_with_already_bound_mac_is_rejected() {
        // A new MAC from the same IP against an already-bound row is
        // hardware-replacement territory — the owner must clear the
        // binding first.
        let rows = vec![row(
            "pc-01",
            Some("aa:bb:cc:dd:ee:01"),
            Some("192.168.1.10"),
        )];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:99"), "192.168.1.10", &rows),
            Decision::Reject,
        );
    }

    #[test]
    fn unknown_mac_and_unknown_ip_rejects() {
        let rows = vec![
            row("pc-01", Some("aa:bb:cc:dd:ee:01"), Some("192.168.1.10")),
            row("pc-02", None, Some("192.168.1.11")),
        ];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:99"), "192.168.1.42", &rows),
            Decision::Reject,
        );
    }

    #[test]
    fn empty_mac_header_treats_request_as_unbound() {
        let rows = vec![row("pc-01", None, Some("192.168.1.10"))];
        assert_eq!(
            resolve(Some(""), "192.168.1.10", &rows),
            Decision::Bind {
                pc_id: "pc-01".to_owned(),
            }
        );
    }

    #[test]
    fn loopback_ip_still_matches_a_loopback_row_in_development() {
        let rows = vec![row("pc-01", None, Some("127.0.0.1"))];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:01"), "127.0.0.1", &rows),
            Decision::Bind {
                pc_id: "pc-01".to_owned(),
            }
        );
    }

    #[test]
    fn mac_match_wins_over_ip_match() {
        // The MAC of a different row is the identity; the source IP
        // matching a third row is irrelevant.
        let rows = vec![
            row("pc-01", Some("aa:bb:cc:dd:ee:01"), Some("192.168.1.10")),
            row("pc-02", None, Some("192.168.1.20")),
        ];
        assert_eq!(
            resolve(Some("aa:bb:cc:dd:ee:01"), "192.168.1.20", &rows),
            Decision::Identify {
                pc_id: "pc-01".to_owned(),
            }
        );
    }
}
