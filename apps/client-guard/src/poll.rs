//! The Guard Poll: one outbound HTTP request, and nothing else (ADR-0077).
//!
//! ADR-0048 gave the Guard no network client on the grounds that a second one
//! would duplicate identity and config for no gain. ADR-0077 reverses that on
//! one narrow point — there is no other way for the Admin to reach a Guard —
//! and keeps the reversal as small as the reason for it.
//!
//! So this is a hand-written HTTP/1.1 POST over `TcpStream` rather than a
//! client library: the request is one short JSON body to a plain-HTTP LAN
//! address, and adding an HTTP stack plus TLS to a SYSTEM service to send it
//! would be the larger risk. The Guard opens no listener and accepts no
//! inbound connection; this is the entire network surface it has.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use openpiso_core::forwarding::ForwardedDiagnostic;
use serde::{Deserialize, Serialize};

/// Connect and read timeouts. A slow or absent Admin must cost one poll
/// interval, never a stalled Guard thread.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(5);

/// Refuse to read an unbounded response body. The real one is a few hundred
/// bytes; anything larger is not an Admin answering us.
const MAX_RESPONSE_BYTES: usize = 8 * 1024;

/// The facts the Guard reports about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GuardReport {
    pub client_connected: bool,
    pub disarmed: bool,
    pub relaunch_attempts: i64,
}

/// The desired state the Admin answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GuardState {
    #[serde(default)]
    pub power_off: bool,
    #[serde(default)]
    pub nonce: Option<String>,
}

/// The widened poll body (ADR-0078): the Guard's own facts, flattened exactly
/// as before, plus at most one optional bounded batch of Forwarded
/// Diagnostics riding the same request. No second network call exists —
/// ADR-0077 chose one hand-written request per poll interval precisely to
/// keep the SYSTEM service small.
#[derive(Debug, Serialize)]
pub struct GuardPollBody<'a> {
    #[serde(flatten)]
    pub report: GuardReport,
    /// The batch this poll carries. Absent when the buffer is empty or
    /// forwarding is gated off, so the body stays byte-identical to the
    /// pre-forwarding shape whenever there is nothing to send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<ForwardedPollBatch<'a>>,
}

/// The wire form of one forwarded batch. The PC identity rides the MAC
/// header and the source is the poll itself, so the body carries only lines.
/// `new` yields `None` for an empty slice, so a poll with nothing to send
/// carries no `forwarded` field at all.
#[derive(Debug, PartialEq, Serialize)]
pub struct ForwardedPollBatch<'a> {
    pub lines: &'a [ForwardedDiagnostic],
}

impl<'a> ForwardedPollBatch<'a> {
    pub fn new(lines: &'a [ForwardedDiagnostic]) -> Option<Self> {
        (!lines.is_empty()).then_some(Self { lines })
    }
}

/// Split an Admin base URL into `(host, port, path_prefix)`.
///
/// Deliberately minimal, and deliberately strict: an unparseable or
/// non-`http://` URL is refused rather than guessed at, because the only thing
/// this connection is used for is an instruction to power a PC off.
pub fn parse_base_url(base_url: &str) -> Option<(String, u16)> {
    let rest = base_url.trim().strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    match authority.split_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().ok()?;
            if host.is_empty() || port == 0 {
                return None;
            }
            Some((host.to_owned(), port))
        }
        None => Some((authority.to_owned(), 80)),
    }
}

/// Build the raw request. Kept pure so the wire format is testable without a
/// socket: the MAC header is what identifies this PC, exactly as it does on
/// the Client heartbeat.
pub fn build_request(host: &str, port: u16, mac: &str, body: &str) -> String {
    format!(
        "POST /api/guard/state HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         X-OpenPiso-Mac: {mac}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        length = body.len()
    )
}

/// Pull the JSON body out of an HTTP response, given a `200`. Any other status
/// is treated as no answer at all: a Guard must never infer an order from an
/// error page.
pub fn parse_response(raw: &str) -> Option<GuardState> {
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.lines().next()?;
    if !status.contains(" 200") {
        return None;
    }
    serde_json::from_str(body.trim()).ok()
}

/// Perform one poll, optionally carrying a bounded batch of Forwarded
/// Diagnostics (ADR-0078) in the body of the request the Guard already
/// makes. Every failure is `None` — unreachable Admin, refused connection,
/// timeout, malformed answer — and `None` always means "no order", so a
/// broken network can only ever leave a PC on. The caller treats `None` the
/// same way for a carried batch: not acknowledged, so its lines wait for a
/// later poll. A `Some` answer means Admin accepted the whole request,
/// including the batch.
pub fn poll(
    base_url: &str,
    mac: &str,
    report: GuardReport,
    forwarded: Option<&[ForwardedDiagnostic]>,
) -> Option<GuardState> {
    let (host, port) = parse_base_url(base_url)?;
    let body = serde_json::to_string(&GuardPollBody {
        report,
        forwarded: forwarded.and_then(ForwardedPollBatch::new),
    })
    .ok()?;
    let request = build_request(&host, port, mac, &body);

    let address = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&address).ok()?;
    stream.set_read_timeout(Some(NETWORK_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(NETWORK_TIMEOUT)).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut raw = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                raw.extend_from_slice(&buffer[..read]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    parse_response(&String::from_utf8_lossy(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_parse_into_host_and_port() {
        assert_eq!(
            parse_base_url("http://192.168.1.2:3210"),
            Some(("192.168.1.2".to_owned(), 3210))
        );
        assert_eq!(
            parse_base_url("http://admin-pc/"),
            Some(("admin-pc".to_owned(), 80))
        );
    }

    #[test]
    fn unusable_base_urls_are_refused_rather_than_guessed() {
        for value in [
            "",
            "192.168.1.2:3210",
            "https://192.168.1.2:3210",
            "http://",
            "http://:3210",
            "http://host:0",
            "http://host:not-a-port",
        ] {
            assert_eq!(parse_base_url(value), None, "{value} must not parse");
        }
    }

    #[test]
    fn the_request_carries_the_mac_header_and_an_exact_length() {
        let body = "{\"client_connected\":false}";
        let request = build_request("192.168.1.2", 3210, "aa:bb:cc:dd:ee:ff", body);
        assert!(request.starts_with("POST /api/guard/state HTTP/1.1\r\n"));
        assert!(request.contains("X-OpenPiso-Mac: aa:bb:cc:dd:ee:ff\r\n"));
        assert!(request.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(request.ends_with(body));
    }

    #[test]
    fn a_two_hundred_response_yields_the_order() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
                   {\"power_off\":true,\"nonce\":\"abc\",\"poll_interval_secs\":10}";
        assert_eq!(
            parse_response(raw),
            Some(GuardState {
                power_off: true,
                nonce: Some("abc".to_owned()),
            })
        );
    }

    #[test]
    fn nothing_but_a_two_hundred_can_order_a_power_off() {
        for raw in [
            "HTTP/1.1 500 Internal Server Error\r\n\r\n{\"power_off\":true}",
            "HTTP/1.1 404 Not Found\r\n\r\n{\"power_off\":true}",
            "HTTP/1.1 200 OK\r\n\r\nnot json",
            "no headers at all",
        ] {
            assert_eq!(parse_response(raw), None, "{raw} must not carry an order");
        }
    }

    #[test]
    fn a_healthy_pc_reads_as_no_order() {
        let raw = "HTTP/1.1 200 OK\r\n\r\n{\"power_off\":false,\"poll_interval_secs\":10}";
        assert_eq!(
            parse_response(raw),
            Some(GuardState {
                power_off: false,
                nonce: None,
            })
        );
    }

    fn report() -> GuardReport {
        GuardReport {
            client_connected: false,
            disarmed: false,
            relaunch_attempts: 2,
        }
    }

    fn sample_line(message: &str) -> ForwardedDiagnostic {
        ForwardedDiagnostic::new(
            openpiso_core::forwarding::ForwardedLevel::Warn,
            openpiso_core::forwarding::GUARD_PIPE_TARGET,
            message,
            1_760_000_000_000,
        )
    }

    #[test]
    fn a_poll_without_a_batch_serializes_its_pre_forwarding_shape() {
        let (host, port) = parse_base_url("http://192.168.1.2:3210").expect("parses");
        let body = serde_json::to_string(&GuardPollBody {
            report: report(),
            forwarded: None,
        })
        .expect("serializes");
        let request = build_request(&host, port, "aa:bb:cc:dd:ee:ff", &body);
        assert!(body.starts_with("{\"client_connected\":false"));
        assert!(!body.contains("forwarded"));
        assert!(request.contains("X-OpenPiso-Mac: aa:bb:cc:dd:ee:ff\r\n"));
        assert!(request.ends_with(&body));
    }

    #[test]
    fn a_poll_body_widens_with_its_batch_without_new_fields_of_its_own() {
        let lines = vec![sample_line("rejecting foreign pipe client")];
        let body = serde_json::to_string(&GuardPollBody {
            report: report(),
            forwarded: Some(ForwardedPollBatch { lines: &lines }),
        })
        .expect("serializes");
        assert_eq!(
            body,
            "{\"client_connected\":false,\"disarmed\":false,\"relaunch_attempts\":2,\
             \"forwarded\":{\"lines\":[{\
             \"level\":\"warn\",\
             \"target\":\"openpiso_client_guard::pipe\",\
             \"message\":\"rejecting foreign pipe client\",\
             \"timestamp_unix_ms\":1760000000000}]}}"
        );
        // An empty slice rides as no batch: the body carries no forwarded
        // field at all.
        assert_eq!(ForwardedPollBatch::new(&[]), None);
    }
}
