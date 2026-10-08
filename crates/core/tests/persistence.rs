//! Saved tunnels across restarts and runtime settings toggles.

use lanlink_core::lan;
use lanlink_core::{Config, ConnState, Node, NodeId, Protocol, SavedTunnel, SecretKey, Service};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

fn set_test_dir() {
    // Keep test nodes away from the user's real config and latency log.
    let dir = std::env::temp_dir().join(format!("lanlink-test-{}", std::process::id()));
    std::env::set_var("LANLINK_CONFIG_DIR", &dir);
}

fn local_addr(node: &Node) -> iroh::EndpointAddr {
    let mut addr = iroh::EndpointAddr::new(node.id());
    for s in node.bound_sockets() {
        if s.is_ipv4() {
            addr = addr.with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, s.port())));
        }
    }
    addr
}

async fn udp_echo_server() -> anyhow::Result<u16> {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = sock.local_addr()?.port();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok((n, from)) = sock.recv_from(&mut buf).await {
            let _ = sock.send_to(&buf[..n], from).await;
        }
    });
    Ok(port)
}

fn config(allowed: &[NodeId], services: Vec<Service>) -> Config {
    Config {
        allowed_peers: allowed.iter().map(|id| id.to_string()).collect(),
        services,
        disable_lan_detection: true,
        ..Default::default()
    }
}

/// Send `msg` to the UDP tunnel at `addr` until it is echoed back or `secs` pass.
/// UDP is lossy and the first packets may only trigger the dial, so keep resending.
async fn udp_round_trip(addr: SocketAddr, msg: &[u8], secs: u64) -> anyhow::Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut buf = vec![0u8; 2048];
    while tokio::time::Instant::now() < deadline {
        sock.send_to(msg, addr).await?;
        if let Ok(Ok((n, _))) =
            tokio::time::timeout(Duration::from_millis(500), sock.recv_from(&mut buf)).await
        {
            if &buf[..n] == msg {
                return Ok(());
            }
        }
    }
    anyhow::bail!("no UDP echo through {addr} within {secs}s")
}

/// Poll `f` every 100 ms until it returns true or `secs` pass.
async fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    f()
}

fn connected(node: &Node, peer: NodeId) -> bool {
    node.peers().iter().any(|p| {
        p.id == peer
            && matches!(p.state, ConnState::Direct | ConnState::Relayed)
            && p.connected_since.is_some()
    })
}

#[test]
fn old_config_defaults() {
    let st: SavedTunnel =
        serde_json::from_str(r#"{"peer":"p","service":"mc","local_port":25565}"#).unwrap();
    assert_eq!(st.protocol, Protocol::Tcp);
    assert!(st.auto_open);
    let c: Config = serde_json::from_str(r#"{"allowed_peers":[]}"#).unwrap();
    assert!(!c.latency_log);
    assert!(!c.disable_lan_detection);
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_udp_tunnel_survives_restart_with_host_offline() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = udp_echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());
    let host_cfg = config(
        &[client_id],
        vec![Service::new("game", Protocol::Udp, echo_port)],
    );

    let host = Node::start_with_secret_key(host_cfg.clone(), host_key.clone()).await?;
    let client =
        Node::start_with_secret_key(config(&[host_id], vec![]), client_key.clone()).await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));
    tokio::time::timeout(Duration::from_secs(20), client.connect(host_id)).await??;

    let tunnel = client.save_tunnel(host_id, "game", 0, true).await?;
    let port = tunnel.local_addr.port();
    let saved = client.config().saved_tunnels;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].protocol, Protocol::Udp);
    assert_eq!(saved[0].local_port, port);
    udp_round_trip(tunnel.local_addr, b"before", 10).await?;

    // Restart the client while the host is down. The config file is shared by every test in
    // this process, so carry the client's config over by hand.
    let client_cfg = client.config();
    client.shutdown().await?;
    host.shutdown().await?;
    assert!(
        wait_for(5, || std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port))
            .is_ok())
        .await,
        "old tunnel socket still bound"
    );
    let client = Node::start_with_secret_key(client_cfg, client_key).await?;

    let info = client.tunnels_info();
    assert_eq!(info.len(), 1, "saved tunnel not reopened: {info:?}");
    assert_eq!(info[0].protocol, Protocol::Udp);
    assert_eq!(info[0].tunnel.local_addr.port(), port);
    assert!(
        std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_err(),
        "no UDP socket bound on the saved port"
    );
    assert!(
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
        "a TCP listener was bound for a UDP service"
    );

    // The host comes back: the first packets dial lazily and traffic flows.
    let host = Node::start_with_secret_key(host_cfg, host_key).await?;
    host.add_peer_addr(local_addr(&client));
    client.add_peer_addr(local_addr(&host));
    udp_round_trip(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), b"after", 45).await?;
    assert!(connected(&client, host_id));

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}

/// Announce a fake "Open to LAN" world the way Minecraft does (multicast, plus unicast to
/// loopback in case multicast is unavailable).
async fn announce(motd: &str, port: u16) {
    let msg = lan::format_announcement(motd, port);
    if let Ok(s) = lan::bind_announcer() {
        let _ = s.send_to(msg.as_bytes(), lan::group_addr()).await;
    }
    if let Ok(s) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await {
        let _ = s
            .send_to(msg.as_bytes(), (Ipv4Addr::LOCALHOST, lan::LAN_PORT))
            .await;
    }
}

/// Keep announcing until `node` sees the world or `secs` pass.
async fn sees_world(node: &Node, motd: &str, port: u16, secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        announce(motd, port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if node
            .lan_worlds()
            .iter()
            .any(|w| w.motd == motd && w.port == port)
        {
            return true;
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn lan_detection_toggles_at_runtime() -> anyhow::Result<()> {
    set_test_dir();
    let motd = format!("lanlink test world {}", std::process::id());
    let node = Node::start_with_secret_key(config(&[], vec![]), SecretKey::generate()).await?;
    assert!(node.config().disable_lan_detection);
    assert!(!sees_world(&node, &motd, 40001, 2).await);

    node.set_lan_detection(true).await?;
    assert!(!node.config().disable_lan_detection);
    assert!(
        sees_world(&node, &motd, 40001, 10).await,
        "enabled detection did not see the world: {:?}",
        node.lan_worlds()
    );

    node.set_lan_detection(false).await?;
    assert!(node.config().disable_lan_detection);
    assert!(node.lan_worlds().is_empty());
    assert!(!sees_world(&node, &motd, 40001, 2).await);

    // Turning it back on starts a fresh listener.
    node.set_lan_detection(true).await?;
    assert!(sees_world(&node, &motd, 40002, 10).await);

    node.shutdown().await?;
    Ok(())
}
