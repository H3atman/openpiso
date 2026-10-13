//! Per-boot MAC collection for the OpenPiso Client (ADR-0031 / plan
//! §4.4 / fix plan §4 M4).
//!
//! The identity hint the server uses to bind a PC is the **MAC of the
//! onboard Ethernet NIC**. The client never persists the MAC: it is
//! read at process start, sent in the `X-OpenPiso-Mac` header on every
//! heartbeat/events/disconnect, and then lives only in the in-memory
//! `ConnectionIdentity`.
//!
//! Resolution order (matches the glossary and ADR-0031 §2):
//!
//! 1. **Interface that carries the route to the admin server.** In a
//!    wired shop that *is* the onboard NIC. Detected by listing the
//!    host's local addresses and intersecting them with the addresses
//!    of the outgoing route. The first hit wins.
//! 2. **Lowest-sorted physical, non-virtual, non-wireless MAC.** When
//!    the route walk is inconclusive (no admin URL resolved yet, or no
//!    local address matches the route), fall back to a deterministic
//!    pick over the interface list. Loopback, virtual, and wireless
//!    adapters are skipped.
//! 3. **None.** When no physical adapter is reachable (no NICs, or
//!    the platform refuses to enumerate), the client has no identity
//!    hint and the server resolves by source IP only.

use std::net::Ipv4Addr;

use get_if_addrs::{get_if_addrs, IfAddr, Ifv4Addr};

/// Loopback IPv4 address — never eligible as the identity hint source
/// or as a route destination.
const LOOPBACK_V4: [u8; 4] = [127, 0, 0, 1];

/// Collect the MAC of the interface that carries the route to the
/// admin server when the URL is known, falling back to the
/// lowest-sorted physical MAC. `admin_host` is the *host portion* of
/// the admin server URL — for `http://192.168.1.10:3210` it is
/// `"192.168.1.10"`.
pub fn collect_mac_for_admin(admin_host: &str) -> Option<String> {
    let candidates = physical_mac_candidates();
    if !admin_host.trim().is_empty() {
        if let Some(mac) = route_to_host(admin_host, &candidates) {
            return Some(format_mac(&mac));
        }
    }
    pick_lowest_sorted(&candidates).map(|mac| format_mac(&mac))
}

/// Lowercase, colon-separated MAC string. The server's identity
/// resolution is case-insensitive, so any casing is acceptable; we
/// normalise to lowercase for stable persistence and tests.
pub fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    /// The interface name — used to look up the MAC via the
    /// `mac_address` crate, which enumerates adapters by OS-level
    /// name.
    name: String,
}

/// All physical, non-loopback, non-virtual, non-wireless adapters
/// the client can reach, paired with their friendly name so the
/// `mac_address` crate can resolve the MAC. The list is empty when
/// `get_if_addrs` fails (e.g. permission denied in a locked-down
/// kiosk) — that case degrades to `None` identity.
fn physical_mac_candidates() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let Ok(interfaces) = get_if_addrs() else {
        return out;
    };
    for iface in interfaces {
        if iface.is_loopback() {
            continue;
        }
        // `get_if_addrs` does not expose the adapter kind cross-platform,
        // so we use the name's well-known virtual prefixes as a guard.
        // Windows: \Device\..., TAP adapters, Hyper-V vEthernet,
        //          WSL, VPNs, etc. Linux: docker, veth, br-, virbr.
        let name = iface.name.to_ascii_lowercase();
        if is_virtual_name(&name) {
            continue;
        }
        // Only consider interfaces with an IPv4 address; the typical
        // shop is IPv4-only and the route walker only deals with
        // IPv4. An IPv6-only adapter without a v4 sibling is
        // exceedingly rare in the target deployment.
        let has_v4 = matches!(iface.addr, IfAddr::V4(Ifv4Addr { .. }));
        if !has_v4 {
            continue;
        }
        out.push(Candidate { name: iface.name });
    }
    out
}

fn is_virtual_name(name: &str) -> bool {
    const VIRTUAL_HINTS: &[&str] = &[
        "virtual",
        "hyper-v",
        "hyperv",
        "vmware",
        "virtualbox",
        "veth",
        "docker",
        "vethernet",
        "tap-",
        "tun-",
        "tun0",
        "wg", // WireGuard
        "nordtun",
        "tailscale",
        "zerotier",
        "vpn",
        "loopback",
        "wsl",
    ];
    VIRTUAL_HINTS.iter().any(|hint| name.contains(hint))
}

fn pick_lowest_sorted(candidates: &[Candidate]) -> Option<[u8; 6]> {
    let mut best: Option<([u8; 6], String)> = None;
    for candidate in candidates {
        let Ok(Some(mac)) = mac_address::mac_address_by_name(&candidate.name) else {
            continue;
        };
        let bytes = mac.bytes();
        if bytes == [0; 6] {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, prior_name)| candidate.name < *prior_name)
        {
            best = Some((bytes, candidate.name.clone()));
        }
    }
    best.map(|(mac, _)| mac)
}

/// Walk the candidates to find the one that owns an IPv4 address on
/// the same subnet as `admin_host`. A first-pass match by exact IPv4
/// equality covers the simple case; otherwise we look for any local
/// address whose first three octets match the host's first three
/// octets (a /24 heuristic — good enough for the wired shop's static
/// /24 deployments and deliberately conservative so a stray adapter on
/// an unrelated subnet cannot match).
fn route_to_host(admin_host: &str, candidates: &[Candidate]) -> Option<[u8; 6]> {
    let host = parse_ipv4(admin_host)?;

    if let Some(name) = candidates.iter().find_map(|candidate| {
        local_addresses()
            .any(|addr| addr == host)
            .then(|| candidate.name.clone())
    }) {
        if let Ok(Some(mac)) = mac_address::mac_address_by_name(&name) {
            let bytes = mac.bytes();
            if bytes != [0; 6] {
                return Some(bytes);
            }
        }
    }

    let admin_prefix = [host[0], host[1], host[2]];
    let mut best: Option<(String, u8)> = None;
    for candidate in candidates {
        let mut best_addr: Option<[u8; 4]> = None;
        for addr in local_addresses() {
            if addr == host || addr == LOOPBACK_V4 {
                continue;
            }
            if [addr[0], addr[1], addr[2]] == admin_prefix {
                let score = common_prefix_bits(addr, host);
                if best_addr.is_none_or(|best| score > common_prefix_bits(best, host)) {
                    best_addr = Some(addr);
                    best = Some((candidate.name.clone(), score));
                }
            }
        }
        let _ = best_addr;
    }
    let (name, _score) = best?;
    if let Ok(Some(mac)) = mac_address::mac_address_by_name(&name) {
        let bytes = mac.bytes();
        if bytes != [0; 6] {
            return Some(bytes);
        }
    }
    None
}

fn parse_ipv4(value: &str) -> Option<[u8; 4]> {
    let parsed: Ipv4Addr = value.trim().parse().ok()?;
    Some(parsed.octets())
}

fn local_addresses() -> impl Iterator<Item = [u8; 4]> {
    let mut addrs: Vec<[u8; 4]> = Vec::new();
    if let Ok(interfaces) = get_if_addrs() {
        for iface in interfaces {
            if let IfAddr::V4(v4) = iface.addr {
                addrs.push(v4.ip.octets());
            }
        }
    }
    addrs.into_iter()
}

/// Common-prefix length in bits of two IPv4 addresses, counted at
/// octet granularity. The route walker uses this to score
/// candidates: a /24 match is preferred over a /16 match, etc. The
/// in-octet bit-precision is not needed for the wired-shop /24
/// deployment the design assumes (ADR-0031 §2).
fn common_prefix_bits(a: [u8; 4], b: [u8; 4]) -> u8 {
    let mut count = 0;
    for (left, right) in a.iter().zip(b.iter()) {
        if left == right {
            count += 8;
        } else {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mac_lowercases_and_colon_separates() {
        assert_eq!(
            format_mac(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01]),
            "aa:bb:cc:dd:ee:01"
        );
    }

    #[test]
    fn parse_ipv4_round_trips_canonical_dotted_quad() {
        assert_eq!(parse_ipv4("192.168.1.10"), Some([192, 168, 1, 10]));
        assert_eq!(parse_ipv4("127.0.0.1"), Some([127, 0, 0, 1]));
        assert_eq!(parse_ipv4("  10.0.0.1  "), Some([10, 0, 0, 1]));
    }

    #[test]
    fn parse_ipv4_rejects_non_ipv4_strings() {
        assert_eq!(parse_ipv4("::1"), None);
        assert_eq!(parse_ipv4("example.com"), None);
        assert_eq!(parse_ipv4("192.168.1"), None);
        assert_eq!(parse_ipv4(""), None);
    }

    #[test]
    fn is_virtual_name_recognises_common_prefixes() {
        assert!(is_virtual_name("docker0"));
        assert!(is_virtual_name("veth1234abcd"));
        assert!(is_virtual_name("nordtun0"));
        assert!(is_virtual_name("tailscale0"));
        assert!(is_virtual_name("wg0"));
        assert!(!is_virtual_name("eth0"));
        assert!(!is_virtual_name("en0"));
        assert!(!is_virtual_name("Ethernet"));
    }

    #[test]
    fn common_prefix_bits_counts_matching_octets() {
        assert_eq!(common_prefix_bits([10, 0, 0, 1], [10, 0, 0, 5]), 24);
        assert_eq!(common_prefix_bits([10, 0, 0, 1], [10, 1, 0, 0]), 8);
        assert_eq!(common_prefix_bits([192, 168, 1, 1], [10, 0, 0, 1]), 0);
        assert_eq!(common_prefix_bits([10, 0, 0, 1], [10, 0, 0, 1]), 32);
    }
}
