//! Server Discovery responder (ADR-0090 §2). Answers each valid discovery
//! request with this Admin's device API port and current shop name, unicast
//! to the sender. Discovery is a convenience: a failed bind is logged by the
//! caller and the Admin keeps running.

use openpiso_core::discovery::{self, DiscoveryReply, DEVICE_API_PORT, MAX_DATAGRAM_BYTES};
use sqlx::SqlitePool;
use tokio::net::UdpSocket;

use crate::db;

/// Answer discovery requests on `socket` forever. Production binds
/// `0.0.0.0:3210`; tests bind an ephemeral loopback port. Malformed datagrams
/// and per-datagram errors are skipped so one bad packet never stops the loop.
pub async fn serve(socket: UdpSocket, pool: SqlitePool) {
    let mut buffer = [0_u8; MAX_DATAGRAM_BYTES];
    loop {
        let (length, peer) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            // Windows reports an ICMP port-unreachable for an earlier reply as
            // a receive error (WSAECONNRESET); it says nothing about this socket.
            Err(error) => {
                tracing::debug!(target: "openpiso_admin::discovery", "discovery receive failed: {error}");
                continue;
            }
        };
        if !discovery::is_request(&buffer[..length]) {
            continue;
        }
        let shop_name = match db::shop_name(&pool).await {
            Ok(name) => name,
            Err(error) => {
                tracing::warn!(target: "openpiso_admin::discovery", "could not read shop name for discovery reply: {error}");
                continue;
            }
        };
        let reply = discovery::encode_reply(&DiscoveryReply {
            port: DEVICE_API_PORT,
            shop_name,
        });
        if let Err(error) = socket.send_to(&reply, peer).await {
            tracing::debug!(target: "openpiso_admin::discovery", "discovery reply to {peer} failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use std::net::SocketAddr;

    use super::*;
    use crate::test_utils;

    async fn spawn_responder(pool: SqlitePool) -> SocketAddr {
        let socket = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("responder binds");
        let address = socket.local_addr().expect("responder address");
        tokio::spawn(serve(socket, pool));
        address
    }

    async fn client_socket() -> UdpSocket {
        UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("client binds")
    }

    async fn receive(socket: &UdpSocket, wait: Duration) -> Option<Vec<u8>> {
        let mut buffer = [0_u8; MAX_DATAGRAM_BYTES];
        match tokio::time::timeout(wait, socket.recv_from(&mut buffer)).await {
            Ok(Ok((length, _))) => Some(buffer[..length].to_vec()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn valid_request_gets_port_and_current_shop_name() {
        let pool = test_utils::mem_test_pool().await;
        db::set_shop_name(&pool, "Juan's Pisonet")
            .await
            .expect("shop name saves");
        let responder = spawn_responder(pool).await;
        let client = client_socket().await;

        client
            .send_to(&discovery::encode_request(), responder)
            .await
            .expect("request sends");
        let reply = receive(&client, Duration::from_secs(2))
            .await
            .expect("responder replies");

        assert_eq!(
            discovery::decode_reply(&reply),
            Some(DiscoveryReply {
                port: DEVICE_API_PORT,
                shop_name: "Juan's Pisonet".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn malformed_datagrams_get_no_reply_and_serving_continues() {
        let pool = test_utils::mem_test_pool().await;
        let responder = spawn_responder(pool).await;
        let client = client_socket().await;

        for junk in [
            &b"garbage"[..],
            br#"{"openpiso":"other","v":1}"#,
            br#"{"openpiso":"discover","v":9}"#,
            br#"{"openpiso":"admin","v":1,"port":3210,"shop_name":"spoof"}"#,
        ] {
            client.send_to(junk, responder).await.expect("junk sends");
        }
        assert_eq!(receive(&client, Duration::from_millis(300)).await, None);

        client
            .send_to(&discovery::encode_request(), responder)
            .await
            .expect("request sends");
        let reply = receive(&client, Duration::from_secs(2))
            .await
            .expect("responder still serves after junk");
        assert_eq!(
            discovery::decode_reply(&reply).map(|reply| reply.port),
            Some(DEVICE_API_PORT)
        );
    }
}
