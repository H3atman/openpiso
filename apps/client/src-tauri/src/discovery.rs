//! Server Discovery (ADR-0090 §2): one broadcast round that finds Admins on
//! the LAN for a Client with no saved server URL. The `discover_servers`
//! command refuses once a server URL is saved, so a configured Client never
//! follows a broadcast answer.

use std::net::SocketAddr;
use std::time::Duration;

use openpiso_core::discovery::{self, DISCOVERY_PORT, MAX_DATAGRAM_BYTES};
use serde::Serialize;
use tokio::net::UdpSocket;

use crate::state::SharedState;

/// Limited broadcast: reaches the LAN of the default route's interface.
pub(crate) const BROADCAST_TARGET: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST),
    DISCOVERY_PORT,
);

/// How long one round collects replies.
pub(crate) const DISCOVERY_WINDOW: Duration = Duration::from_millis(1500);

pub(crate) const ALREADY_CONFIGURED: &str =
    "this PC already has a saved admin server; discovery is only for first setup";

/// An Admin that answered. `url` is built from the reply's source IP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredServer {
    pub url: String,
    pub shop_name: String,
}

/// Run one round unless this Client already has a saved server URL.
pub(crate) async fn discover_unless_configured(
    shared: &SharedState,
    target: SocketAddr,
    window: Duration,
) -> Result<Vec<DiscoveredServer>, String> {
    if *shared.configured.read().await {
        return Err(ALREADY_CONFIGURED.to_owned());
    }
    discover(target, window)
        .await
        .map_err(|error| format!("server discovery failed: {error}"))
}

/// Send one request to `target` and collect distinct replies for `window`.
/// No reply is an empty list, not an error.
pub(crate) async fn discover(
    target: SocketAddr,
    window: Duration,
) -> std::io::Result<Vec<DiscoveredServer>> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
    socket.set_broadcast(true)?;
    socket.send_to(&discovery::encode_request(), target).await?;

    let deadline = tokio::time::Instant::now() + window;
    let mut buffer = [0_u8; MAX_DATAGRAM_BYTES];
    let mut servers: Vec<DiscoveredServer> = Vec::new();
    loop {
        let (length, source) =
            match tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await {
                Err(_elapsed) => return Ok(servers),
                Ok(Ok(received)) => received,
                // Windows surfaces ICMP port-unreachable as a receive error;
                // keep listening for the rest of the window.
                Ok(Err(_)) => continue,
            };
        let Some(reply) = discovery::decode_reply(&buffer[..length]) else {
            continue;
        };
        let url = format!("http://{}", SocketAddr::new(source.ip(), reply.port));
        if !servers.iter().any(|server| server.url == url) {
            servers.push(DiscoveredServer {
                url,
                shop_name: reply.shop_name,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::state::AppState;
    use openpiso_core::discovery::DiscoveryReply;

    const WINDOW: Duration = Duration::from_millis(400);

    /// A minimal Admin stand-in bound to `ip`: answers every datagram with
    /// each of `bodies`, built from the core types.
    async fn responder(ip: Ipv4Addr, bodies: Vec<Vec<u8>>) -> SocketAddr {
        let socket = UdpSocket::bind((ip, 0)).await.expect("responder binds");
        let address = socket.local_addr().expect("responder address");
        tokio::spawn(async move {
            let mut buffer = [0_u8; MAX_DATAGRAM_BYTES];
            while let Ok((length, peer)) = socket.recv_from(&mut buffer).await {
                if !discovery::is_request(&buffer[..length]) {
                    continue;
                }
                for body in &bodies {
                    let _ = socket.send_to(body, peer).await;
                }
            }
        });
        address
    }

    fn reply(port: u16, shop_name: &str) -> Vec<u8> {
        discovery::encode_reply(&DiscoveryReply {
            port,
            shop_name: shop_name.to_owned(),
        })
    }

    #[tokio::test]
    async fn one_responder_gives_its_source_ip_and_replied_port() {
        let target = responder(Ipv4Addr::LOCALHOST, vec![reply(4321, "Shop A")]).await;
        let servers = discover(target, WINDOW).await.expect("discovery runs");
        assert_eq!(
            servers,
            vec![DiscoveredServer {
                url: "http://127.0.0.1:4321".to_owned(),
                shop_name: "Shop A".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn two_responders_give_two_results() {
        // Stands in for a broadcast reaching two Admins: the request lands on
        // `target`, and Admins on two loopback addresses each answer the
        // client from their own socket (Linux and Windows route all of 127/8).
        let target_socket = UdpSocket::bind(("127.0.0.1", 0)).await.expect("binds");
        let target = target_socket.local_addr().expect("address");
        let mut admins = Vec::new();
        for ip in [Ipv4Addr::new(127, 0, 0, 1), Ipv4Addr::new(127, 0, 0, 2)] {
            admins.push(UdpSocket::bind((ip, 0)).await.expect("admin binds"));
        }
        tokio::spawn(async move {
            let mut buffer = [0_u8; MAX_DATAGRAM_BYTES];
            let Ok((length, client)) = target_socket.recv_from(&mut buffer).await else {
                return;
            };
            if !discovery::is_request(&buffer[..length]) {
                return;
            }
            for (index, admin) in admins.iter().enumerate() {
                let _ = admin
                    .send_to(&reply(3210, &format!("Shop {index}")), client)
                    .await;
            }
        });

        let servers = discover(target, WINDOW).await.expect("discovery runs");
        let mut urls: Vec<_> = servers.iter().map(|server| server.url.as_str()).collect();
        urls.sort_unstable();
        assert_eq!(urls, ["http://127.0.0.1:3210", "http://127.0.0.2:3210"]);
    }

    #[tokio::test]
    async fn duplicate_replies_from_one_responder_give_one_result() {
        let target = responder(
            Ipv4Addr::LOCALHOST,
            vec![reply(3210, "Shop A"), reply(3210, "Shop A")],
        )
        .await;
        let servers = discover(target, WINDOW).await.expect("discovery runs");
        assert_eq!(servers.len(), 1);
    }

    #[tokio::test]
    async fn garbage_replies_are_ignored() {
        let target = responder(
            Ipv4Addr::LOCALHOST,
            vec![
                b"garbage".to_vec(),
                br#"{"openpiso":"admin","v":1,"port":0,"shop_name":"x"}"#.to_vec(),
                reply(3210, "Shop A"),
            ],
        )
        .await;
        let servers = discover(target, WINDOW).await.expect("discovery runs");
        assert_eq!(
            servers
                .iter()
                .map(|server| server.url.as_str())
                .collect::<Vec<_>>(),
            ["http://127.0.0.1:3210"]
        );
    }

    #[tokio::test]
    async fn no_responder_is_an_empty_list_after_the_window() {
        // A bound-but-silent socket: nothing answers.
        let silent = UdpSocket::bind(("127.0.0.1", 0)).await.expect("binds");
        let target = silent.local_addr().expect("address");
        let started = std::time::Instant::now();
        let servers = discover(target, WINDOW).await.expect("discovery runs");
        assert!(servers.is_empty());
        assert!(started.elapsed() >= WINDOW);
    }

    #[tokio::test]
    async fn refuses_when_a_server_url_is_already_saved() {
        let target = responder(Ipv4Addr::LOCALHOST, vec![reply(3210, "Rogue")]).await;
        let state = AppState::default();
        *state.shared.configured.write().await = true;
        assert_eq!(
            discover_unless_configured(&state.shared, target, WINDOW).await,
            Err(ALREADY_CONFIGURED.to_owned())
        );

        *state.shared.configured.write().await = false;
        assert_eq!(
            discover_unless_configured(&state.shared, target, WINDOW)
                .await
                .map(|servers| servers.len()),
            Ok(1)
        );
    }
}
