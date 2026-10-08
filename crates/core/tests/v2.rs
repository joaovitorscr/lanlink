//! v2 management API: connection requests, disabled services, byte counters, auto-reconnect.

use lanlink_core::{Config, ConnState, Node, NodeEvent, NodeId, Protocol, SecretKey, Service};
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

/// Echo `msg` through the tunnel at `addr`.
async fn round_trip(addr: SocketAddr, msg: &[u8]) -> anyhow::Result<()> {
    let mut sock = TcpStream::connect(addr).await?;
    sock.write_all(msg).await?;
    let mut buf = vec![0u8; msg.len()];
    tokio::time::timeout(Duration::from_secs(10), sock.read_exact(&mut buf)).await??;
    assert_eq!(buf, msg);
    Ok(())
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

#[tokio::test(flavor = "multi_thread")]
async fn unknown_peer_becomes_request_then_allowed() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());

    // Host does not know the client yet.
    let host = Node::start_with_secret_key(
        config(&[], vec![Service::new("echo", Protocol::Tcp, echo_port)]),
        host_key,
    )
    .await?;
    let mut host_events = host.subscribe();
    let client = Node::start_with_secret_key(
        Config {
            display_name: Some("Tester".into()),
            ..config(&[host_id], vec![])
        },
        client_key,
    )
    .await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));

    // The client cannot connect and gets no services.
    let res = tokio::time::timeout(Duration::from_secs(20), client.connect(host_id)).await?;
    assert!(res.is_err(), "unknown peer must not connect");
    assert!(client
        .open_tunnel(host_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await
        .is_err());
    let p = client
        .peers()
        .into_iter()
        .find(|p| p.id == host_id)
        .unwrap();
    assert!(p.services.is_empty());
    let err = p.last_error.unwrap_or_default();
    assert!(err.contains("accept your request"), "last_error = {err:?}");

    // The host saw a request with the client's name.
    let req = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(NodeEvent::PeerRequest(r)) = host_events.recv().await {
                return r;
            }
        }
    })
    .await?;
    assert_eq!(req.id, client_id);
    assert_eq!(req.name.as_deref(), Some("Tester"));
    let pending = host.pending_requests();
    assert_eq!(pending.len(), 1);
    assert_eq!(host.services_status()[0].connections, 0);

    // Allow: the client's auto-reconnect gets through without calling connect again.
    host.respond_request(client_id, true, None).await?;
    assert!(host.pending_requests().is_empty());
    assert!(host.config().allowed_peers.contains(&client_id.to_string()));
    assert!(
        wait_for(40, || connected(&client, host_id)).await,
        "client did not reconnect after being allowed"
    );
    let p = client
        .peers()
        .into_iter()
        .find(|p| p.id == host_id)
        .unwrap();
    assert!(p.last_error.is_none());

    let tunnel = client
        .open_tunnel(host_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await?;
    round_trip(tunnel.local_addr, b"allowed now").await?;

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_service_refuses_streams_and_counters_count() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());

    let host = Node::start_with_secret_key(
        config(
            &[client_id],
            vec![Service::new("echo", Protocol::Tcp, echo_port)],
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

    // (c) Byte counters grow after an echo round trip.
    let msg = vec![7u8; 4096];
    round_trip(tunnel.local_addr, &msg).await?;
    assert!(
        wait_for(5, || {
            let t = &client.tunnels_info()[0];
            t.bytes_up >= 4096 && t.bytes_down >= 4096
        })
        .await,
        "tunnel counters: {:?}",
        client.tunnels_info()
    );
    let p = client
        .peers()
        .into_iter()
        .find(|p| p.id == host_id)
        .unwrap();
    assert!(p.bytes_sent >= 4096 && p.bytes_received >= 4096);
    let h = host
        .peers()
        .into_iter()
        .find(|p| p.id == client_id)
        .unwrap();
    assert!(h.bytes_sent >= 4096 && h.bytes_received >= 4096);
    assert!(wait_for(5, || host.services_status()[0].reachable == Some(true)).await);

    // (b) Disabled: no longer advertised, and new streams are refused.
    host.set_service_enabled("echo", false).await?;
    assert!(
        wait_for(5, || {
            client
                .peers()
                .iter()
                .any(|p| p.id == host_id && p.services.is_empty())
        })
        .await,
        "disabled service still advertised"
    );
    let mut sock = TcpStream::connect(tunnel.local_addr).await?;
    let _ = sock.write_all(b"refused?").await;
    let mut buf = [0u8; 8];
    let res = tokio::time::timeout(Duration::from_secs(10), sock.read_exact(&mut buf)).await?;
    assert!(res.is_err(), "disabled service must refuse the stream");

    // Enabled again: works.
    host.set_service_enabled("echo", true).await?;
    round_trip(tunnel.local_addr, b"back on").await?;
    assert!(wait_for(5, || host.services_status()[0].connections == 0).await);

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn client_reconnects_after_host_restart() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());
    let host_cfg = config(
        &[client_id],
        vec![Service::new("echo", Protocol::Tcp, echo_port)],
    );

    let host = Node::start_with_secret_key(host_cfg.clone(), host_key.clone()).await?;
    let client = Node::start_with_secret_key(config(&[host_id], vec![]), client_key).await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));
    tokio::time::timeout(Duration::from_secs(20), client.connect(host_id)).await??;
    let tunnel = client
        .open_tunnel(host_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await?;
    round_trip(tunnel.local_addr, b"before").await?;

    host.shutdown().await?;
    assert!(wait_for(10, || !connected(&client, host_id)).await);

    let host = Node::start_with_secret_key(host_cfg, host_key).await?;
    host.add_peer_addr(local_addr(&client));
    client.add_peer_addr(local_addr(&host));
    assert!(
        wait_for(45, || connected(&client, host_id)).await,
        "client did not reconnect: {:?}",
        client.peers()
    );
    round_trip(tunnel.local_addr, b"after").await?;

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_listening_is_reported_once() -> anyhow::Result<()> {
    set_test_dir();
    // A port nothing listens on.
    let dead_port = {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        l.local_addr()?.port()
    };
    let host_key = SecretKey::generate();
    let client_key = SecretKey::generate();
    let (host_id, client_id) = (host_key.public(), client_key.public());
    let host = Node::start_with_secret_key(
        config(
            &[client_id],
            vec![Service::new("game", Protocol::Tcp, dead_port)],
        ),
        host_key,
    )
    .await?;
    let client = Node::start_with_secret_key(config(&[host_id], vec![]), client_key).await?;
    client.add_peer_addr(local_addr(&host));
    host.add_peer_addr(local_addr(&client));
    let mut events = client.subscribe();

    let tunnel = tokio::time::timeout(
        Duration::from_secs(20),
        client.open_tunnel(host_id, "game", (Ipv4Addr::LOCALHOST, 0).into()),
    )
    .await??;
    for _ in 0..3 {
        let mut sock = TcpStream::connect(tunnel.local_addr).await?;
        let mut buf = [0u8; 1];
        let res = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await?;
        assert!(!matches!(res, Ok(n) if n > 0), "nothing should come back");
    }

    // Pings keep events flowing, so collect until a fixed deadline.
    let mut errors = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(ev) = tokio::time::timeout_at(deadline, events.recv()).await {
        if let Ok(NodeEvent::Error(e)) = ev {
            errors.push(e);
        }
    }
    let expected = format!("Nothing is listening on 127.0.0.1:{dead_port} on ");
    let matching: Vec<_> = errors.iter().filter(|e| e.starts_with(&expected)).collect();
    assert_eq!(matching.len(), 1, "errors = {errors:?}");

    client.shutdown().await?;
    host.shutdown().await?;
    Ok(())
}
