//! Importing a config into a running node.

use lanlink_core::export::{self, ImportMode};
use lanlink_core::{Config, ConnState, Node, NodeId, Protocol, SavedTunnel, SecretKey, Service};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

async fn echo_server() -> anyhow::Result<u16> {
    let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = echo.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
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

async fn round_trip(addr: SocketAddr, msg: &[u8]) -> anyhow::Result<()> {
    let mut sock = TcpStream::connect(addr).await?;
    sock.write_all(msg).await?;
    let mut buf = vec![0u8; msg.len()];
    tokio::time::timeout(Duration::from_secs(10), sock.read_exact(&mut buf)).await??;
    assert_eq!(buf, msg);
    Ok(())
}

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

fn free_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread")]
async fn import_applies_to_running_node() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());

    // The host starts knowing nobody and sharing nothing.
    let host = Node::start_with_secret_key(config(&[], vec![]), host_key).await?;
    let client = Node::start_with_secret_key(config(&[host_id], vec![]), client_key).await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));
    assert!(!wait_for(2, || connected(&client, host_id)).await);

    // Import (merge) a file that allows the client and shares the echo service.
    let mut file = config(
        &[client_id],
        vec![Service::new("echo", Protocol::Tcp, echo_port)],
    );
    file.peer_names
        .insert(client_id.to_string(), "Imported client".into());
    let imported = export::parse(export::export(&file).as_bytes())?;
    let before = host.config();
    let outcome = host
        .apply_config(imported.resolve(&before, ImportMode::Merge))
        .await?;
    assert!(!outcome.restart_required);
    let backup: Config = serde_json::from_slice(&std::fs::read(&outcome.backup)?)?;
    assert!(backup.allowed_peers.is_empty() && backup.services.is_empty());
    assert_eq!(host.config().allowed_peers, vec![client_id.to_string()]);

    // The client's auto-reconnect gets through and sees the new service.
    assert!(
        wait_for(40, || connected(&client, host_id)).await,
        "client did not connect after the import: {:?}",
        client.peers()
    );
    assert!(
        wait_for(10, || client.peers().iter().any(
            |p| p.id == host_id && p.services.iter().any(|s| s.name == "echo")
        ))
        .await,
        "imported service not advertised: {:?}",
        client.peers()
    );
    assert!(
        wait_for(5, || host.peers().iter().any(
            |p| p.id == client_id && p.name.as_deref() == Some("Imported client")
        ))
        .await
    );

    // The client imports a saved tunnel to it, which opens right away.
    let port = free_port();
    let mut client_file = client.config();
    client_file.saved_tunnels.push(SavedTunnel {
        peer: host_id.to_string(),
        service: "echo".into(),
        protocol: Protocol::Tcp,
        local_port: port,
        auto_open: true,
    });
    client.apply_config(client_file).await?;
    let tunnels = client.tunnels();
    assert_eq!(tunnels.len(), 1, "{tunnels:?}");
    assert_eq!(tunnels[0].local_addr.port(), port);
    round_trip(tunnels[0].local_addr, b"imported tunnel").await?;

    // Replacing the client's config with one that has no saved tunnels closes it.
    client.apply_config(config(&[host_id], vec![])).await?;
    assert!(client.tunnels().is_empty());

    // Replacing the host's config with an empty one drops the client and the service.
    let empty = export::parse(export::export(&config(&[], vec![])).as_bytes())?;
    host.apply_config(empty.resolve(&host.config(), ImportMode::Replace))
        .await?;
    assert!(host.services_status().is_empty());
    assert!(
        wait_for(10, || !connected(&client, host_id)).await,
        "client still connected after being removed"
    );

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}
