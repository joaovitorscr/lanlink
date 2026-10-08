//! UDP tunnels: small and fragmented packets, and several local senders on one tunnel.

use lanlink_core::{Config, Node, NodeId, Protocol, SecretKey, Service};
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

fn config(allowed: &[NodeId], services: Vec<Service>) -> Config {
    Config {
        allowed_peers: allowed.iter().map(|id| id.to_string()).collect(),
        services,
        disable_lan_detection: true,
        ..Default::default()
    }
}

/// UDP server that echoes every packet back to its sender.
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

/// Host with a UDP echo service, client connected to it with a tunnel open.
async fn setup() -> anyhow::Result<(Node, Node, SocketAddr)> {
    set_test_dir();
    let echo_port = udp_echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());
    let host = Node::start_with_secret_key(
        config(
            &[client_id],
            vec![Service::new("echo", Protocol::Udp, echo_port)],
        ),
        host_key,
    )
    .await?;
    let client = Node::start_with_secret_key(config(&[host_id], vec![]), client_key).await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));
    tokio::time::timeout(Duration::from_secs(20), client.connect(host_id)).await??;
    let tunnel = client
        .open_tunnel(host_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await?;
    Ok((host, client, tunnel.local_addr))
}

/// Send `msg` from `sock` to the tunnel until the echo comes back. UDP may drop packets, so
/// retry a few times.
async fn echo(sock: &UdpSocket, tunnel: SocketAddr, msg: &[u8]) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    for _ in 0..20 {
        sock.send_to(msg, tunnel).await?;
        if let Ok(res) =
            tokio::time::timeout(Duration::from_millis(500), sock.recv_from(&mut buf)).await
        {
            let (n, from) = res?;
            assert_eq!(from, tunnel);
            assert_eq!(&buf[..n], msg, "wrong reply for this sender");
            return Ok(());
        }
    }
    anyhow::bail!("no echo for a {} byte packet", msg.len())
}

#[tokio::test(flavor = "multi_thread")]
async fn udp_small_and_large_round_trip() -> anyhow::Result<()> {
    let (host, client, tunnel) = setup().await?;
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;

    echo(&sock, tunnel, b"small ping").await?;
    // Well above the QUIC datagram limit, so it must be fragmented.
    let big: Vec<u8> = (0..8 * 1024u32).map(|i| (i * 7) as u8).collect();
    echo(&sock, tunnel, &big).await?;

    let t = &client.tunnels_info()[0];
    assert_eq!(t.connections, 1, "one local sender, one flow");
    assert!(t.bytes_up >= 8 * 1024 && t.bytes_down >= 8 * 1024, "{t:?}");

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn udp_two_local_senders_get_their_own_replies() -> anyhow::Result<()> {
    let (host, client, tunnel) = setup().await?;
    let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;

    echo(&a, tunnel, b"from a").await?;
    echo(&b, tunnel, b"from b").await?;
    // Interleaved: replies must not follow whoever sent last.
    for i in 0..5 {
        a.send_to(format!("a{i}").as_bytes(), tunnel).await?;
        b.send_to(format!("b{i}").as_bytes(), tunnel).await?;
        let mut buf = [0u8; 64];
        for (sock, want) in [(&a, format!("a{i}")), (&b, format!("b{i}"))] {
            let (n, _) =
                tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await??;
            assert_eq!(&buf[..n], want.as_bytes());
        }
    }

    assert_eq!(client.tunnels_info()[0].connections, 2);
    assert_eq!(host.services_status()[0].connections, 2);

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}
