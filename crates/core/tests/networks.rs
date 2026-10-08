//! Networks with real nodes on loopback: invites, approval, per-network services, removal.

use lanlink_core::{
    Approval, Config, ConnState, InviteExpiry, JoinStatus, NetworkColor, NetworkPolicy, Node,
    NodeEvent, NodeId, Protocol, SecretKey, Service,
};
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

/// Tell every node how to reach every other one on loopback.
fn introduce(nodes: &[&Node]) {
    for a in nodes {
        for b in nodes {
            if a.id() != b.id() {
                a.add_peer_addr(local_addr(b));
            }
        }
    }
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

async fn start(name: &str, services: Vec<Service>) -> anyhow::Result<Node> {
    let config = Config {
        display_name: Some(name.into()),
        services,
        disable_lan_detection: true,
        ..Default::default()
    };
    Node::start_with_secret_key(config, SecretKey::generate()).await
}

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

fn sees(node: &Node, peer: NodeId, service: &str) -> bool {
    node.peers()
        .iter()
        .any(|p| p.id == peer && p.services.iter().any(|s| s.name == service))
}

fn member_of(node: &Node, network: &str) -> bool {
    node.networks()
        .iter()
        .any(|n| n.id == network && n.active() && n.has_member(&node.id()))
}

#[tokio::test(flavor = "multi_thread")]
async fn invite_join_approve_share_remove() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let owner = start("Olivia", vec![]).await?;
    let a = start("Ana", vec![]).await?;
    let b = start("Bruno", vec![]).await?;
    introduce(&[&owner, &a, &b]);
    let (a_id, b_id) = (a.id(), b.id());

    let crew = owner
        .create_network(
            "Crew",
            NetworkColor::Green,
            NetworkPolicy::default(),
            Some((InviteExpiry::Never, Approval::Auto)),
        )
        .await?;
    let other = owner
        .create_network("Other", NetworkColor::Blue, NetworkPolicy::default(), None)
        .await?;
    let auto_code = crew.invite_code().expect("invite code");

    // A joins with an auto-approve code.
    let status = a.join_network(&auto_code, None).await?;
    assert_eq!(status, JoinStatus::Approved);
    assert!(member_of(&a, &crew.id));
    assert!(wait_for(20, || connected(&a, owner.id())).await);

    // B asks with an "ask me" code: pending until the owner approves.
    let mut owner_events = owner.subscribe();
    let ask_code = owner
        .create_invite(&crew.id, InviteExpiry::Never, Approval::AskMe)
        .await?;
    assert!(
        a.join_network(&auto_code, None).await.is_ok(),
        "already a member, the old code does not matter"
    );
    let status = b.join_network(&ask_code, Some("Bruno B".into())).await?;
    assert_eq!(status, JoinStatus::Pending);
    assert!(b.networks().iter().any(|n| n.id == crew.id && n.pending));
    let req = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(NodeEvent::PeerRequest(r)) = owner_events.recv().await {
                return r;
            }
        }
    })
    .await?;
    assert_eq!(req.id, b_id);
    assert_eq!(req.network.as_deref(), Some(crew.id.as_str()));
    assert_eq!(req.name.as_deref(), Some("Bruno B"));
    assert_eq!(owner.pending_requests().len(), 1);
    owner.respond_join(&crew.id, b_id, true).await?;
    assert!(owner.pending_requests().is_empty());

    // The owner tells B; B and A now allow each other and connect.
    assert!(
        wait_for(30, || member_of(&b, &crew.id)).await,
        "B never learned it was approved: {:?}",
        b.networks()
    );
    assert!(
        wait_for(20, || a
            .networks()
            .iter()
            .any(|n| n.id == crew.id && n.has_member(&b_id)))
        .await
    );
    assert!(
        wait_for(40, || connected(&b, a_id) && connected(&a, b_id)).await,
        "A and B did not connect: {:?} / {:?}",
        a.peers(),
        b.peers()
    );

    // A shares one service with Crew and one with Other only.
    let mut crew_svc = Service::new("echo", Protocol::Tcp, echo_port);
    crew_svc.networks = vec![crew.id.clone()];
    a.add_service(crew_svc).await?;
    let mut other_svc = Service::new("secret", Protocol::Tcp, echo_port);
    other_svc.networks = vec![other.id.clone()];
    a.add_service(other_svc).await?;
    assert!(wait_for(10, || sees(&b, a_id, "echo")).await);
    assert!(
        !sees(&b, a_id, "secret"),
        "service for another network leaked"
    );
    let tunnel = b
        .open_tunnel(a_id, "echo", (Ipv4Addr::LOCALHOST, 0).into())
        .await?;
    round_trip(tunnel.local_addr, b"crew only").await?;
    // The owner is in Crew too.
    assert!(wait_for(10, || sees(&owner, a_id, "echo")).await);
    assert!(!sees(&owner, a_id, "secret"));
    // A stream for the hidden service is refused even when asked for by name.
    let hidden = b
        .open_tunnel(a_id, "secret", (Ipv4Addr::LOCALHOST, 0).into())
        .await;
    assert!(hidden.is_err());

    // Removing B: B drops the network, A drops B, the tunnel stops working.
    owner.remove_member(&crew.id, b_id).await?;
    assert!(
        wait_for(15, || !b.networks().iter().any(|n| n.id == crew.id)).await,
        "B still has the network: {:?}",
        b.networks()
    );
    assert!(
        wait_for(15, || !a
            .networks()
            .iter()
            .any(|n| n.id == crew.id && n.has_member(&b_id)))
        .await
    );
    assert!(wait_for(15, || !connected(&a, b_id) && !connected(&b, a_id)).await);
    assert!(
        b.tunnels().is_empty(),
        "B's tunnel for the network stayed open"
    );
    let res = tokio::time::timeout(Duration::from_secs(20), b.connect(a_id)).await?;
    assert!(res.is_err(), "removed member can still connect");

    // A leaves; the owner sees it.
    a.leave_network(&crew.id).await?;
    assert!(
        wait_for(15, || !owner
            .networks()
            .iter()
            .any(|n| n.id == crew.id && n.has_member(&a_id)))
        .await
    );
    assert!(wait_for(15, || !a.networks().iter().any(|n| n.id == crew.id)).await);

    b.shutdown().await?;
    a.shutdown().await?;
    owner.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_revoked_and_used_invites_are_rejected() -> anyhow::Result<()> {
    set_test_dir();
    let owner = start("Olivia", vec![]).await?;
    let a = start("Ana", vec![]).await?;
    let b = start("Bruno", vec![]).await?;
    introduce(&[&owner, &a, &b]);

    let net = owner
        .create_network("Crew", NetworkColor::Pink, NetworkPolicy::default(), None)
        .await?;

    // Revoked.
    let code = owner
        .create_invite(&net.id, InviteExpiry::Never, Approval::Auto)
        .await?;
    owner.revoke_invites(&net.id).await?;
    let err = a.join_network(&code, None).await.unwrap_err();
    assert!(err.to_string().contains("no longer valid"), "{err:#}");

    // Replaced by a newer code.
    let old = owner
        .create_invite(&net.id, InviteExpiry::Never, Approval::Auto)
        .await?;
    let new = owner
        .create_invite(&net.id, InviteExpiry::Never, Approval::Auto)
        .await?;
    assert_ne!(old, new);
    assert!(a.join_network(&old, None).await.is_err());

    // Expired: "after 0 hours" is already over.
    let code = owner
        .create_invite(
            &net.id,
            InviteExpiry::AfterHours { hours: 0 },
            Approval::Auto,
        )
        .await?;
    assert!(a.join_network(&code, None).await.is_err());

    // First use only.
    let code = owner
        .create_invite(&net.id, InviteExpiry::FirstUse, Approval::Auto)
        .await?;
    assert_eq!(a.join_network(&code, None).await?, JoinStatus::Approved);
    let err = b.join_network(&code, None).await.unwrap_err();
    assert!(err.to_string().contains("no longer valid"), "{err:#}");

    // Rejected joiners got nothing and are not allowed.
    assert!(b.networks().is_empty());
    assert!(!owner.networks().iter().any(|n| n.has_member(&b.id())));
    assert!(!owner.peers().iter().any(|p| p.id == b.id()));

    // Garbage.
    assert!(a.join_network("lanlink-AAAA-BBBB", None).await.is_err());

    b.shutdown().await?;
    a.shutdown().await?;
    owner.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_network_and_direct_peers() -> anyhow::Result<()> {
    set_test_dir();
    let echo_port = echo_server().await?;
    let owner = start(
        "Olivia",
        vec![Service::new("open", Protocol::Tcp, echo_port)],
    )
    .await?;
    let a = start("Ana", vec![]).await?;
    introduce(&[&owner, &a]);

    let net = owner
        .create_network(
            "Crew",
            NetworkColor::Teal,
            NetworkPolicy::default(),
            Some((InviteExpiry::Never, Approval::Auto)),
        )
        .await?;
    a.join_network(&net.invite_code().unwrap(), None).await?;
    // A service with no networks is visible to network members.
    assert!(wait_for(20, || sees(&a, owner.id(), "open")).await);

    owner.delete_network(&net.id).await?;
    assert!(wait_for(15, || a.networks().is_empty()).await);
    assert!(wait_for(15, || !connected(&a, owner.id())).await);

    // Direct peers still work as before, outside any network.
    owner.add_peer(a.id(), Some("Ana".into())).await?;
    a.add_peer(owner.id(), None).await?;
    assert!(wait_for(40, || sees(&a, owner.id(), "open")).await);

    a.shutdown().await?;
    owner.shutdown().await?;
    Ok(())
}
