use anyhow::Context;
use clap::{Parser, Subcommand};
use lanlink_core::{Config, ConnState, Node, NodeEvent, NodeId, PeerInfo, Protocol, Service};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// How long to wait for peers to be told we are leaving before exiting anyway.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(
    name = "lanlink",
    version = lanlink_core::build_info::VERSION,
    about = "Peer-to-peer port forwarding over iroh"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print our node id.
    Id,
    /// Expose a local port to allowed peers and run until ctrl-c.
    Host {
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        /// Forward UDP instead of TCP.
        #[arg(long)]
        udp: bool,
    },
    /// Allow a peer to connect to us.
    Allow {
        node_id: NodeId,
        #[arg(long)]
        name: Option<String>,
    },
    /// Connect to a service on a peer and forward it to a local address.
    Connect {
        node_id: NodeId,
        service: String,
        #[arg(long, default_value = "127.0.0.1:0")]
        local: SocketAddr,
    },
    /// List allowed peers.
    Peers,
    /// Run briefly and list peers that asked to connect to us.
    Requests {
        /// How long to listen for requests, in seconds.
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
    /// Allow a peer that asked to connect (same as `allow`).
    Accept {
        node_id: NodeId,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a peer from the allowlist, names and saved tunnels.
    Remove { node_id: NodeId },
    /// Set the local display name of a peer.
    Rename { node_id: NodeId, name: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _log = lanlink_core::init_logging("cli");

    let cli = Cli::parse();
    let mut config = Config::load()?;
    match cli.cmd {
        Cmd::Id => {
            let key =
                lanlink_core::node::load_or_create_secret_key(&Config::dir().join("identity.key"))?;
            println!("{}", key.public());
        }
        Cmd::Allow { node_id, name } | Cmd::Accept { node_id, name } => {
            let id = node_id.to_string();
            if !config.allowed_peers.contains(&id) {
                config.allowed_peers.push(id.clone());
            }
            if let Some(name) = name {
                config.peer_names.insert(id.clone(), name);
            }
            config.save()?;
            println!("allowed {id}");
        }
        Cmd::Peers => {
            if config.allowed_peers.is_empty() {
                println!("no allowed peers");
            }
            for p in &config.allowed_peers {
                match config.peer_names.get(p) {
                    Some(n) => println!("{p}  {n}"),
                    None => println!("{p}"),
                }
            }
        }
        Cmd::Remove { node_id } => {
            let id = node_id.to_string();
            let before = config.allowed_peers.len();
            config.allowed_peers.retain(|p| p != &id);
            config.peer_names.remove(&id);
            config.saved_tunnels.retain(|t| t.peer != id);
            config.save()?;
            if config.allowed_peers.len() < before {
                println!("removed {id}");
            } else {
                println!("{id} was not in the allowlist");
            }
        }
        Cmd::Rename { node_id, name } => {
            let id = node_id.to_string();
            config.peer_names.insert(id.clone(), name.clone());
            config.save()?;
            println!("{id} is now {name}");
        }
        Cmd::Requests { secs } => {
            let node = Node::start(config).await?;
            println!("node id: {}", node.id());
            println!("listening for connection requests for {secs}s; ctrl-c to stop early");
            let mut events = node.subscribe();
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(secs),
                run_until_ctrl_c(&mut events, |ev| {
                    if let NodeEvent::PeerRequest(r) = ev {
                        println!(
                            "request from {} ({})",
                            r.id,
                            r.name.as_deref().unwrap_or("no name")
                        );
                    }
                }),
            )
            .await;
            let reqs = node.pending_requests();
            if reqs.is_empty() {
                println!("no pending requests");
            } else {
                println!("pending requests (accept with `lanlink accept <id> --name <name>`):");
                for r in reqs {
                    println!("{}  {}", r.id, r.name.as_deref().unwrap_or("-"));
                }
            }
            shutdown(node).await?;
        }
        Cmd::Host { name, port, udp } => {
            let protocol = if udp { Protocol::Udp } else { Protocol::Tcp };
            config.services.retain(|s| s.name != name);
            config
                .services
                .push(Service::new(name.clone(), protocol, port));
            // Start first: if the app is already running, leave its config alone.
            let node = Node::start(config.clone()).await?;
            config.save()?;
            println!("node id: {}", node.id());
            println!("hosting {name} ({protocol:?}) on local port {port}; ctrl-c to stop");
            let mut events = node.subscribe();
            run_until_ctrl_c(&mut events, |ev| print_event(&ev)).await;
            shutdown(node).await?;
        }
        Cmd::Connect {
            node_id,
            service,
            local,
        } => {
            let node = Node::start(config).await?;
            println!("connecting to {node_id}...");
            let mut events = node.subscribe();
            let info = node.connect(node_id).await.context("connect failed")?;
            print_peer(&info);
            let tunnel = node.open_tunnel(node_id, &service, local).await?;
            println!(
                "{} forwarded to {}; ctrl-c to stop",
                service, tunnel.local_addr
            );
            let mut last_state = info.state;
            run_until_ctrl_c(&mut events, |ev| match ev {
                NodeEvent::PeerStateChanged(p) if p.id == node_id => {
                    // Print on path changes, plus a stats summary every 10 pings.
                    if p.state != last_state || (p.stats.total > 0 && p.stats.total % 10 == 0) {
                        last_state = p.state;
                        print_peer(&p);
                    }
                }
                NodeEvent::PeerStateChanged(_) => {}
                other => print_event(&other),
            })
            .await;
            shutdown(node).await?;
        }
    }
    Ok(())
}

async fn run_until_ctrl_c(
    events: &mut tokio::sync::broadcast::Receiver<NodeEvent>,
    mut on_event: impl FnMut(NodeEvent),
) {
    let stop = stop_signal();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            ev = events.recv() => match ev {
                Ok(ev) => on_event(ev),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Resolves on ctrl-c, or SIGTERM on Unix.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Close connections so peers see us leave now, not after the idle timeout.
async fn shutdown(node: Node) -> anyhow::Result<()> {
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, node.shutdown()).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!("shutdown timed out after {SHUTDOWN_TIMEOUT:?}");
            Ok(())
        }
    }
}

fn state_str(s: ConnState) -> &'static str {
    match s {
        ConnState::Disconnected => "Disconnected",
        ConnState::Connecting => "Connecting",
        ConnState::Relayed => "Relayed",
        ConnState::Direct => "Direct",
    }
}

fn print_peer(p: &PeerInfo) {
    let name = p
        .name
        .clone()
        .unwrap_or_else(|| p.id.fmt_short().to_string());
    let st = p.stats;
    if st.samples > 0 {
        println!(
            "{name}: {}  last {:.1}  min {:.1}  avg {:.1}  max {:.1}  jitter {:.1} ms  ({} samples)",
            state_str(p.state),
            st.last_ms,
            st.min_ms,
            st.avg_ms,
            st.max_ms,
            st.jitter_ms,
            st.samples
        );
    } else {
        match p.latency_ms {
            Some(ms) => println!("{name}: {} ({ms} ms)", state_str(p.state)),
            None => match &p.last_error {
                Some(e) => println!("{name}: {} ({e})", state_str(p.state)),
                None => println!("{name}: {}", state_str(p.state)),
            },
        }
    }
}

fn print_event(ev: &NodeEvent) {
    match ev {
        NodeEvent::PeerStateChanged(p) => print_peer(p),
        NodeEvent::TunnelOpened(t) => println!("tunnel opened: {} -> {}", t.local_addr, t.service),
        NodeEvent::TunnelClosed(t) => println!("tunnel closed: {} -> {}", t.local_addr, t.service),
        NodeEvent::Error(e) => eprintln!("error: {e}"),
        NodeEvent::PeerRequest(r) => println!(
            "connection request from {} ({}); allow with `lanlink accept {}`",
            r.name.as_deref().unwrap_or("no name"),
            r.id,
            r.id
        ),
        NodeEvent::NetworkChanged(_) | NodeEvent::LanWorldsChanged(_) => {}
    }
}
