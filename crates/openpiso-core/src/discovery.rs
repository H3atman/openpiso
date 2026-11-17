//! Server Discovery wire contract (ADR-0090 §2). An unconfigured Client
//! broadcasts a request datagram on UDP [`DISCOVERY_PORT`]; each Admin answers
//! the sender with its device API port and shop name. Both datagrams are JSON.
//! Anything that does not decode as the expected message (wrong tag, wrong
//! version, garbage) is ignored by both sides.
//!
//! The reply carries no host: the Client builds the server URL from the
//! datagram's source IP, so a reply can only point at the machine that sent it.

use serde::{Deserialize, Serialize};

/// UDP port the Admin listens on for discovery requests. Same number as the
/// TCP device API, so one firewall port pair covers both.
pub const DISCOVERY_PORT: u16 = 3210;

/// TCP port of the Admin's client/device API, advertised in replies.
pub const DEVICE_API_PORT: u16 = 3210;

const PROTOCOL_VERSION: u32 = 1;

/// Datagrams larger than this are not discovery messages.
pub const MAX_DATAGRAM_BYTES: usize = 1024;

/// What an Admin advertises in answer to a discovery request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryReply {
    pub port: u16,
    pub shop_name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "openpiso", rename_all = "snake_case")]
enum Message {
    Discover {
        v: u32,
    },
    Admin {
        v: u32,
        port: u16,
        shop_name: String,
    },
}

fn decode(bytes: &[u8]) -> Option<Message> {
    serde_json::from_slice(bytes).ok()
}

/// `{"openpiso":"discover","v":1}`
pub fn encode_request() -> Vec<u8> {
    encode(&Message::Discover {
        v: PROTOCOL_VERSION,
    })
}

/// Whether `bytes` is a discovery request this Admin should answer.
pub fn is_request(bytes: &[u8]) -> bool {
    matches!(
        decode(bytes),
        Some(Message::Discover {
            v: PROTOCOL_VERSION
        })
    )
}

/// `{"openpiso":"admin","v":1,"port":…,"shop_name":"…"}`
pub fn encode_reply(reply: &DiscoveryReply) -> Vec<u8> {
    encode(&Message::Admin {
        v: PROTOCOL_VERSION,
        port: reply.port,
        shop_name: reply.shop_name.clone(),
    })
}

/// Decode an Admin reply; `None` for anything else.
pub fn decode_reply(bytes: &[u8]) -> Option<DiscoveryReply> {
    match decode(bytes)? {
        Message::Admin {
            v: PROTOCOL_VERSION,
            port,
            shop_name,
        } if port != 0 => Some(DiscoveryReply { port, shop_name }),
        _ => None,
    }
}

fn encode(message: &Message) -> Vec<u8> {
    // Serializing a plain struct of strings and integers cannot fail.
    serde_json::to_vec(message).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_the_documented_shape_and_round_trips() {
        let bytes = encode_request();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({ "openpiso": "discover", "v": 1 })
        );
        assert!(is_request(&bytes));
    }

    #[test]
    fn reply_has_the_documented_shape_and_round_trips() {
        let reply = DiscoveryReply {
            port: 3210,
            shop_name: "Juan's Pisonet".to_owned(),
        };
        let bytes = encode_reply(&reply);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({ "openpiso": "admin", "v": 1, "port": 3210, "shop_name": "Juan's Pisonet" })
        );
        assert_eq!(decode_reply(&bytes), Some(reply));
    }

    #[test]
    fn anything_else_is_ignored() {
        for junk in [
            &b""[..],
            b"\xff\xfe",
            b"hello",
            br#"{"openpiso":"discover","v":2}"#,
            br#"{"openpiso":"discover"}"#,
            br#"{"openpiso":"other","v":1}"#,
            br#"{"v":1}"#,
            br#"{"openpiso":"admin","v":2,"port":3210,"shop_name":"x"}"#,
            br#"{"openpiso":"admin","v":1,"port":0,"shop_name":"x"}"#,
            br#"{"openpiso":"admin","v":1,"port":70000,"shop_name":"x"}"#,
            br#"{"openpiso":"admin","v":1,"shop_name":"x"}"#,
        ] {
            assert!(!is_request(junk), "{junk:?} accepted as request");
            assert_eq!(decode_reply(junk), None, "{junk:?} accepted as reply");
        }
        // Each side ignores the other's message.
        assert_eq!(decode_reply(&encode_request()), None);
        assert!(!is_request(&encode_reply(&DiscoveryReply {
            port: 3210,
            shop_name: "x".to_owned()
        })));
    }
}
