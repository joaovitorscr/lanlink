use lanlink_core::{Config, Node, Protocol, SecretKey, Service};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn local_addr(node: &Node) -> iroh::EndpointAddr {
    let mut addr = iroh::EndpointAddr::new(node.id());
    for s in node.bound_sockets() {
        if s.is_ipv4() {
            addr = addr.with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, s.port())));
        }
    }
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_round_trip() -> anyhow::Result<()> {
    // TCP echo server.
    let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let echo_port = echo.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });

    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let host_id = host_key.public();
    let client_id = client_key.public();

    let host = Node::start_with_secret_key(
        Config {
            allowed_peers: vec![client_id.to_string()],
            services: vec![Service {
                name: "echo".into(),
                protocol: Protocol::Tcp,
                port: echo_port,
            }],
            ..Default::default()
        },
        host_key,
    )
    .await?;
    let client = Node::start_with_secret_key(
        Config {
            allowed_peers: vec![host_id.to_string()],
            ..Default::default()
        },
        client_key,
    )
    .await?;
    client.add_peer_addr(local_addr(&host));

    let info = tokio::time::timeout(Duration::from_secs(20), client.connect(host_id)).await??;
    assert!(info.services.iter().any(|s| s.name == "echo"));

    let tunnel = client
        .open_tunnel(host_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await?;
    let mut sock = TcpStream::connect(tunnel.local_addr).await?;
    let msg = b"hello lanlink";
    for _ in 0..3 {
        sock.write_all(msg).await?;
        let mut buf = [0u8; 13];
        tokio::time::timeout(Duration::from_secs(10), sock.read_exact(&mut buf)).await??;
        assert_eq!(&buf, msg);
    }

    client.close_tunnel(&tunnel).await?;
    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}
