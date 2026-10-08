use anyhow::Context;
use clap::{Parser, Subcommand};
use lanlink_core::export::{self, ImportMode};
use lanlink_core::instance::InstanceLock;
use lanlink_core::{
    Approval, Config, ConnState, InviteExpiry, JoinStatus, Network, NetworkColor, NetworkPolicy,
    Node, NodeEvent, NodeId, PeerInfo, Protocol, Service,
};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
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
        /// Address the service listens on, e.g. 192.168.1.20 or ::1. Default 127.0.0.1.
        #[arg(long)]
        host: Option<IpAddr>,
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
    /// Run briefly and list join requests to our networks.
    Requests {
        /// How long to listen for requests, in seconds.
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
    /// Allow a peer by ID (same as `allow`).
    Accept {
        node_id: NodeId,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a peer from the allowlist, names and saved tunnels.
    Remove { node_id: NodeId },
    /// Set the local display name of a peer.
    Rename { node_id: NodeId, name: String },
    /// Export or import the configuration (peers, services, saved tunnels, settings).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Networks: groups of friends who can reach each other's shared services.
    #[command(subcommand)]
    Network(NetCmd),
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write the configuration to a file, or to stdout. The identity key is not included.
    Export { path: Option<PathBuf> },
    /// Import a file written by `config export` or the app. lanlink must not be running.
    /// The current config is backed up to config.json.bak first.
    Import {
        path: PathBuf,
        /// The file replaces the current configuration.
        #[arg(long, conflicts_with = "merge")]
        replace: bool,
        /// Add the file's peers, services and tunnels to the current ones (default).
        #[arg(long)]
        merge: bool,
    },
}

/// A network is named by its name or id (a unique id prefix is enough).
#[derive(Subcommand)]
enum NetCmd {
    /// Create a network you own and print its invite code.
    Create {
        name: String,
        /// Ask me before anyone joins (default: anyone with the code joins at once).
        #[arg(long)]
        ask: bool,
        /// When the code stops working: never, 1h, 24h, once.
        #[arg(long, default_value = "never")]
        expires: String,
    },
    /// List networks and their members.
    List,
    /// Print the invite code of a network you own (a new one with --new).
    Invite {
        network: String,
        /// Make a new code; the old one stops working.
        #[arg(long)]
        new: bool,
        #[arg(long)]
        ask: bool,
        #[arg(long, default_value = "never")]
        expires: String,
    },
    /// Join a network with an invite code.
    Join {
        code: String,
        /// Name shown to the owner and members. Default: your display name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Approve someone who asked to join a network you own.
    Approve { network: String, node_id: NodeId },
    /// Leave a network.
    Leave { network: String },
    /// Remove a member from a network you own.
    RemoveMember { network: String, node_id: NodeId },
    /// Delete a network you own.
    Delete { network: String },
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
        Cmd::Config {
            cmd: ConfigCmd::Export { path },
        } => {
            let text = export::export(&config);
            match path {
                Some(path) => {
                    std::fs::write(&path, text + "\n")
                        .with_context(|| format!("writing {}", path.display()))?;
                    println!("exported to {} (identity key not included)", path.display());
                }
                None => println!("{text}"),
            }
        }
        Cmd::Config {
            cmd: ConfigCmd::Import { path, replace, .. },
        } => {
            // Hold the instance lock so a running app cannot overwrite the result.
            let _lock = InstanceLock::acquire(&Config::dir())
                .context("can't import while lanlink is running; quit it, or use Settings > Import config in the app")?;
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let import = export::parse(&bytes)?;
            let mode = if replace {
                ImportMode::Replace
            } else {
                ImportMode::Merge
            };
            let current = Config::load()?;
            let backup = current.save_backup()?;
            import.resolve(&current, mode).save()?;
            println!(
                "imported {} ({}) by {}",
                path.display(),
                import.summary().describe(),
                if replace { "replacing" } else { "merging" }
            );
            println!("previous config saved to {}", backup.display());
        }
        Cmd::Network(cmd) => network_cmd(cmd, config).await?,
        Cmd::Requests { secs } => {
            let node = Node::start(config).await?;
            println!("node id: {}", node.id());
            println!("listening for join requests for {secs}s; ctrl-c to stop early");
            let mut events = node.subscribe();
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(secs),
                run_until_ctrl_c(&mut events, |ev| {
                    if let NodeEvent::PeerRequest(r) = ev {
                        println!(
                            "join request from {} ({})",
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
                println!("pending join requests (answer with `lanlink network approve`):");
                for r in reqs {
                    println!(
                        "{}  {}  wants to join network {}",
                        r.id,
                        r.name.as_deref().unwrap_or("-"),
                        r.network.as_deref().unwrap_or("-")
                    );
                }
            }
            shutdown(node).await?;
        }
        Cmd::Host {
            name,
            port,
            udp,
            host,
        } => {
            let protocol = if udp { Protocol::Udp } else { Protocol::Tcp };
            let mut svc = Service::new(name.clone(), protocol, port);
            svc.host = host;
            let addr = svc.local_addr(port);
            config.services.retain(|s| s.name != name);
            config.services.push(svc);
            // Start first: if the app is already running, leave its config alone.
            let node = Node::start(config.clone()).await?;
            config.save()?;
            println!("node id: {}", node.id());
            println!("hosting {name} ({protocol:?}) at {addr}; ctrl-c to stop");
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

fn parse_expiry(s: &str) -> anyhow::Result<InviteExpiry> {
    Ok(match s {
        "never" => InviteExpiry::Never,
        "once" => InviteExpiry::FirstUse,
        _ => match s.strip_suffix('h').and_then(|h| h.parse().ok()) {
            Some(hours) => InviteExpiry::AfterHours { hours },
            None => anyhow::bail!("--expires must be never, once, or hours like 24h"),
        },
    })
}

fn approval(ask: bool) -> Approval {
    if ask {
        Approval::AskMe
    } else {
        Approval::Auto
    }
}

/// Find a network by exact id, name (any case), or unique id prefix.
fn find_network(networks: &[Network], key: &str) -> anyhow::Result<Network> {
    let by = |f: &dyn Fn(&Network) -> bool| -> Vec<&Network> {
        networks.iter().filter(|n| f(n)).collect()
    };
    for found in [
        by(&|n| n.id == key),
        by(&|n| n.name.eq_ignore_ascii_case(key)),
        by(&|n| n.id.starts_with(key)),
    ] {
        match found.as_slice() {
            [n] => return Ok((*n).clone()),
            [] => continue,
            _ => anyhow::bail!("{key:?} matches several networks; use the id"),
        }
    }
    anyhow::bail!("no network {key:?}; see `lanlink network list`")
}

async fn network_cmd(cmd: NetCmd, config: Config) -> anyhow::Result<()> {
    if let NetCmd::List = cmd {
        if config.networks.is_empty() {
            println!("no networks");
        }
        for n in &config.networks {
            let state = if n.pending {
                " (waiting for approval)"
            } else if n.leaving {
                " (leaving)"
            } else {
                ""
            };
            println!("{}  {}{state}", n.id, n.name);
            for m in &n.members {
                let role = if m.id == n.owner { "  owner" } else { "" };
                println!("    {}  {}{role}", m.id, m.name);
            }
            if let Some(code) = n.invite_code() {
                println!("    invite: {code}");
            }
        }
        return Ok(());
    }
    let node = Node::start(config).await?;
    let res = async {
        match cmd {
            NetCmd::List => {}
            NetCmd::Create { name, ask, expires } => {
                let invite = (parse_expiry(&expires)?, approval(ask));
                let n = node
                    .create_network(
                        &name,
                        NetworkColor::default(),
                        NetworkPolicy::default(),
                        Some(invite),
                    )
                    .await?;
                println!("created {} ({})", n.name, n.id);
                println!("invite code: {}", n.invite_code().unwrap_or_default());
            }
            NetCmd::Invite {
                network,
                new,
                ask,
                expires,
            } => {
                let n = find_network(&node.networks(), &network)?;
                let code = match n.invite_code() {
                    Some(code) if !new => code,
                    _ => {
                        node.create_invite(&n.id, parse_expiry(&expires)?, approval(ask))
                            .await?
                    }
                };
                println!("{code}");
            }
            NetCmd::Join { code, name } => match node.join_network(&code, name).await? {
                JoinStatus::Pending => println!(
                    "asked to join; the owner has to approve you. You are let in the next time \
                     lanlink runs here while they are online."
                ),
                _ => println!("joined"),
            },
            NetCmd::Approve { network, node_id } => {
                let n = find_network(&node.networks(), &network)?;
                node.respond_join(&n.id, node_id, true).await?;
                println!("{node_id} is now in {}", n.name);
            }
            NetCmd::Leave { network } => {
                let n = find_network(&node.networks(), &network)?;
                node.leave_network(&n.id).await?;
                // Give the owner a moment to confirm; otherwise it is told later.
                let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                while tokio::time::Instant::now() < deadline
                    && node.networks().iter().any(|x| x.id == n.id)
                {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                if node.networks().iter().any(|x| x.id == n.id) {
                    println!("left {}; the owner is told when it is next online", n.name);
                } else {
                    println!("left {}", n.name);
                }
            }
            NetCmd::RemoveMember { network, node_id } => {
                let n = find_network(&node.networks(), &network)?;
                node.remove_member(&n.id, node_id).await?;
                println!("removed {node_id} from {}", n.name);
            }
            NetCmd::Delete { network } => {
                let n = find_network(&node.networks(), &network)?;
                node.delete_network(&n.id).await?;
                println!("deleted {}", n.name);
            }
        }
        anyhow::Ok(())
    }
    .await;
    shutdown(node).await?;
    res
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
            "join request from {} ({}) for network {}",
            r.name.as_deref().unwrap_or("no name"),
            r.id,
            r.network.as_deref().unwrap_or("-")
        ),
        NodeEvent::NetworkChanged(_)
        | NodeEvent::LanWorldsChanged(_)
        | NodeEvent::NetworksChanged => {}
    }
}
