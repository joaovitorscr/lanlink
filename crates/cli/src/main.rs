use anyhow::Context;
use clap::{Parser, Subcommand};
use lanlink_core::{Config, ConnState, Node, NodeEvent, NodeId, PeerInfo, Protocol, Service};
use std::net::SocketAddr;
use tokio::sync::broadcast::error::RecvError;

#[derive(Parser)]
#[command(
    name = "lanlink",
    version,
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,lanlink_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let mut config = Config::load()?;
    match cli.cmd {
        Cmd::Id => {
            let key =
                lanlink_core::node::load_or_create_secret_key(&Config::dir().join("identity.key"))?;
            println!("{}", key.public());
        }
        Cmd::Allow { node_id, name } => {
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
        Cmd::Host { name, port, udp } => {
            let protocol = if udp { Protocol::Udp } else { Protocol::Tcp };
            config.services.retain(|s| s.name != name);
            config.services.push(Service {
                name: name.clone(),
                protocol,
                port,
            });
            config.save()?;
            let node = Node::start(config).await?;
            println!("node id: {}", node.id());
            println!("hosting {name} ({protocol:?}) on local port {port}; ctrl-c to stop");
            let mut events = node.subscribe();
            run_until_ctrl_c(&mut events, |ev| print_event(&ev)).await;
            node.shutdown().await?;
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
            let mut last = (info.state, info.latency_ms);
            run_until_ctrl_c(&mut events, |ev| match ev {
                NodeEvent::PeerStateChanged(p) if p.id == node_id => {
                    if (p.state, p.latency_ms) != last {
                        last = (p.state, p.latency_ms);
                        print_peer(&p);
                    }
                }
                NodeEvent::PeerStateChanged(_) => {}
                other => print_event(&other),
            })
            .await;
            node.shutdown().await?;
        }
    }
    Ok(())
}

async fn run_until_ctrl_c(
    events: &mut tokio::sync::broadcast::Receiver<NodeEvent>,
    mut on_event: impl FnMut(NodeEvent),
) {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = events.recv() => match ev {
                Ok(ev) => on_event(ev),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
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
    match p.latency_ms {
        Some(ms) => println!("{name}: {} ({ms} ms)", state_str(p.state)),
        None => println!("{name}: {}", state_str(p.state)),
    }
}

fn print_event(ev: &NodeEvent) {
    match ev {
        NodeEvent::PeerStateChanged(p) => print_peer(p),
        NodeEvent::TunnelOpened(t) => println!("tunnel opened: {} -> {}", t.local_addr, t.service),
        NodeEvent::TunnelClosed(t) => println!("tunnel closed: {} -> {}", t.local_addr, t.service),
        NodeEvent::Error(e) => eprintln!("error: {e}"),
    }
}
