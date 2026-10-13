//! Wire protocol shared by the Client and Client Guard (ADR-0048).
//!
//! The transport is a one-way named pipe. These values deliberately use a
//! small typed binary protocol instead of free-form strings: the Client can
//! announce intent, but the Guard remains the sole judge of whether a death
//! was legitimate.

/// Reason supplied with a clean Client exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CleanExitReason {
    /// The server-authoritative expiry grace period completed.
    ExpiryPowerOff = 1,
    /// The cached Offline Grace countdown completed.
    OfflineShutdown = 2,
    /// The owner used the PIN-gated maintenance quit.
    OwnerQuit = 3,
}

/// Longest endpoint message the Guard will accept over the pipe. A pipe write
/// is the only input a standard user can reach, so the length is bounded here
/// rather than wherever it is later stored.
pub const MAX_ADMIN_ENDPOINT_BYTES: usize = 512;

/// A message sent from the Client to the Guard pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardMessage {
    /// The Client is about to exit intentionally.
    CleanExit(CleanExitReason),
    /// Windows is shutting down or restarting.
    OsShutdown,
    /// Where the Client reaches Admin, and the MAC it is known by there
    /// (ADR-0077). The Guard caches both so its own poll reaches the same
    /// Admin as the same PC.
    ///
    /// The MAC comes from the Client rather than being resolved again by the
    /// Guard: a Guard that picked a different adapter would poll as a
    /// different PC, or as none at all. It carries no authority — the Admin
    /// looks the PC up read-only and the Guard still decides everything local
    /// by itself.
    AdminEndpoint { url: String, mac: String },
}

impl GuardMessage {
    const CLEAN_EXIT_TAG: u8 = 0x01;
    const OS_SHUTDOWN_TAG: u8 = 0x02;
    const ADMIN_ENDPOINT_TAG: u8 = 0x03;
    /// Separates the two text fields of an endpoint message. Neither a URL
    /// nor a MAC may contain it, so the split is unambiguous.
    const FIELD_SEPARATOR: u8 = b'\n';

    /// Encode one complete pipe message.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::CleanExit(reason) => vec![Self::CLEAN_EXIT_TAG, *reason as u8],
            Self::OsShutdown => vec![Self::OS_SHUTDOWN_TAG],
            Self::AdminEndpoint { url, mac } => {
                let mut bytes = vec![Self::ADMIN_ENDPOINT_TAG];
                bytes.extend_from_slice(url.as_bytes());
                bytes.push(Self::FIELD_SEPARATOR);
                bytes.extend_from_slice(mac.as_bytes());
                bytes
            }
        }
    }

    /// Decode one complete pipe message. Unknown or truncated messages are
    /// rejected and therefore cannot disarm the Guard.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [Self::CLEAN_EXIT_TAG, reason] => Some(Self::CleanExit(match reason {
                1 => CleanExitReason::ExpiryPowerOff,
                2 => CleanExitReason::OfflineShutdown,
                3 => CleanExitReason::OwnerQuit,
                _ => return None,
            })),
            [Self::OS_SHUTDOWN_TAG] => Some(Self::OsShutdown),
            [Self::ADMIN_ENDPOINT_TAG, body @ ..] => {
                if body.is_empty() || body.len() > MAX_ADMIN_ENDPOINT_BYTES {
                    return None;
                }
                let body = std::str::from_utf8(body).ok()?;
                let (url, mac) = body.split_once('\n')?;
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return None;
                }
                if mac.is_empty() || !mac.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
                    return None;
                }
                Some(Self::AdminEndpoint {
                    url: url.to_owned(),
                    mac: mac.to_owned(),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_exit_messages_round_trip_with_their_typed_reason() {
        for reason in [
            CleanExitReason::ExpiryPowerOff,
            CleanExitReason::OfflineShutdown,
            CleanExitReason::OwnerQuit,
        ] {
            let encoded = GuardMessage::CleanExit(reason).encode();
            assert_eq!(
                GuardMessage::decode(&encoded),
                Some(GuardMessage::CleanExit(reason))
            );
        }
    }

    #[test]
    fn os_shutdown_message_round_trips() {
        let encoded = GuardMessage::OsShutdown.encode();
        assert_eq!(
            GuardMessage::decode(&encoded),
            Some(GuardMessage::OsShutdown)
        );
    }

    #[test]
    fn admin_endpoint_messages_round_trip() {
        let message = GuardMessage::AdminEndpoint {
            url: "http://192.168.1.2:3210".to_owned(),
            mac: "aa:bb:cc:dd:ee:ff".to_owned(),
        };
        assert_eq!(GuardMessage::decode(&message.encode()), Some(message));
    }

    #[test]
    fn admin_endpoint_rejects_anything_it_could_not_safely_poll() {
        // Truncated, and missing the MAC half entirely.
        assert_eq!(GuardMessage::decode(&[0x03]), None);
        assert_eq!(GuardMessage::decode(b"\x03http://a"), None);

        let oversized = GuardMessage::AdminEndpoint {
            url: format!("http://{}", "a".repeat(MAX_ADMIN_ENDPOINT_BYTES)),
            mac: "aa:bb:cc:dd:ee:ff".to_owned(),
        };
        assert_eq!(GuardMessage::decode(&oversized.encode()), None);

        let scheme_less = GuardMessage::AdminEndpoint {
            url: "192.168.1.2:3210".to_owned(),
            mac: "aa:bb:cc:dd:ee:ff".to_owned(),
        };
        assert_eq!(GuardMessage::decode(&scheme_less.encode()), None);

        let junk_mac = GuardMessage::AdminEndpoint {
            url: "http://192.168.1.2:3210".to_owned(),
            mac: "not a mac".to_owned(),
        };
        assert_eq!(GuardMessage::decode(&junk_mac.encode()), None);
    }

    #[test]
    fn malformed_messages_are_not_clean_exit_evidence() {
        assert_eq!(GuardMessage::decode(&[]), None);
        assert_eq!(GuardMessage::decode(&[0x01]), None);
        assert_eq!(GuardMessage::decode(&[0x01, 0xff]), None);
        assert_eq!(GuardMessage::decode(&[0x02, 0x00]), None);
    }
}
