//! v2 management API: everything the GUI needs so nobody has to use a terminal.
//!
//! CONTRACT shared by `cli` and `app`. The signatures below are fixed; implementers replace
//! the `todo!()` bodies (and may move them into other modules as long as the paths in lib.rs
//! still resolve). Behaviour each method must have is documented on it.
//!
//! Polling vs events: discrete changes arrive as `NodeEvent`s. Fast-changing counters
//! (bytes, connection counts, service reachability) are read by the UI about once a second
//! through `services_status()` and `tunnels_info()`; they must be cheap (no I/O, no awaits).

use crate::{ActiveTunnel, Node, NodeId, Protocol, Service};
use std::net::SocketAddr;
use std::time::SystemTime;

/// An unknown peer tried to connect to us.
#[derive(Debug, Clone)]
pub struct PeerRequest {
    pub id: NodeId,
    /// Name the peer sent in Hello, if any. Untrusted, display only.
    pub name: Option<String>,
    /// Most recent attempt.
    pub at: SystemTime,
}

/// Our own connectivity.
#[derive(Debug, Clone, Default)]
pub struct NetworkStatus {
    /// Connected to a home relay, so peers can reach us.
    pub online: bool,
    /// Home relay URL, e.g. "https://use1-1.relay.n0.iroh.link./".
    pub home_relay: Option<String>,
    /// Our directly reachable addresses (LAN and public) as discovered by iroh.
    pub direct_addrs: Vec<SocketAddr>,
}

/// Live status of a service we host.
#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub service: Service,
    /// TCP: something is listening on 127.0.0.1:port (probed every few seconds).
    /// UDP: None, it cannot be probed.
    pub reachable: Option<bool>,
    /// Port actually forwarded to. Differs from `service.port` when a Minecraft LAN world
    /// was detected on another port (see `Service::minecraft_lan`).
    pub effective_port: u16,
    /// Open TCP streams / active UDP flows from peers right now.
    pub connections: u32,
    /// Peers currently using this service.
    pub peers: Vec<NodeId>,
}

/// Live status of a client tunnel.
#[derive(Debug, Clone)]
pub struct TunnelInfo {
    pub tunnel: ActiveTunnel,
    pub protocol: Protocol,
    /// Open local TCP connections (or recent UDP flows) through this tunnel.
    pub connections: u32,
    /// Payload bytes local -> peer and peer -> local.
    pub bytes_up: u64,
    pub bytes_down: u64,
    /// This tunnel is in `Config::saved_tunnels`.
    pub saved: bool,
}

/// A Minecraft "Open to LAN" world seen on this machine (multicast 224.0.2.60:4445).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanWorld {
    pub motd: String,
    pub port: u16,
    pub last_seen: SystemTime,
}

/// Keeps file logging alive; drop it at process exit.
pub struct LogGuard {
    #[allow(dead_code)]
    pub(crate) inner: Option<Box<dyn std::any::Any + Send>>,
}

/// Initialise tracing: stderr plus a daily-rotated file `Config::logs_dir()/<prefix>.log`.
/// `RUST_LOG` overrides the default filter (info for lanlink, warn for everything else).
/// Keeps at most 7 log files.
pub fn init_logging(prefix: &str) -> LogGuard {
    let _ = prefix;
    todo!("implement")
}

impl Node {
    // ---- peers ----

    /// Allow a peer, save its name, and start connecting (auto-reconnect keeps trying).
    /// Also removes any pending request from it. Saves config.
    pub async fn add_peer(&self, id: NodeId, name: Option<String>) -> anyhow::Result<()> {
        let _ = (id, name);
        todo!("implement")
    }

    /// Remove from allowlist, names and saved tunnels; close its tunnels and connections. Saves config.
    pub async fn remove_peer(&self, id: NodeId) -> anyhow::Result<()> {
        let _ = id;
        todo!("implement")
    }

    /// Set or clear the local display name for a peer. Saves config. Emits PeerStateChanged.
    pub async fn rename_peer(&self, id: NodeId, name: Option<String>) -> anyhow::Result<()> {
        let _ = (id, name);
        todo!("implement")
    }

    /// Dial now, resetting the auto-reconnect backoff.
    pub async fn reconnect(&self, id: NodeId) -> anyhow::Result<()> {
        let _ = id;
        todo!("implement")
    }

    /// Unknown peers that tried to connect recently (deduped by id, newest first, at most 20,
    /// entries expire after 10 minutes).
    pub fn pending_requests(&self) -> Vec<PeerRequest> {
        todo!("implement")
    }

    /// Allow = `add_peer(id, name)`. Deny = drop the request; it may come back if they retry.
    pub async fn respond_request(
        &self,
        id: NodeId,
        allow: bool,
        name: Option<String>,
    ) -> anyhow::Result<()> {
        let _ = (id, allow, name);
        todo!("implement")
    }

    // ---- us ----

    pub fn network_status(&self) -> NetworkStatus {
        todo!("implement")
    }

    /// Set our display name sent to peers. Saves config; takes effect on new connections.
    pub async fn set_display_name(&self, name: Option<String>) -> anyhow::Result<()> {
        let _ = name;
        todo!("implement")
    }

    /// Set or clear the custom relay. Saves config. Returns true when a restart is needed to apply it.
    pub async fn set_relay_url(&self, url: Option<String>) -> anyhow::Result<bool> {
        let _ = url;
        todo!("implement")
    }

    // ---- hosted services ----

    /// Add or replace (by name) a hosted service. Saves config and pushes the new service list
    /// to connected peers.
    pub async fn add_service(&self, service: Service) -> anyhow::Result<()> {
        let _ = service;
        todo!("implement")
    }

    /// Remove a hosted service, close its open streams, push the new list to peers. Saves config.
    pub async fn remove_service(&self, name: &str) -> anyhow::Result<()> {
        let _ = name;
        todo!("implement")
    }

    /// Enable or disable a hosted service without deleting it. Saves config, pushes list to peers.
    pub async fn set_service_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let _ = (name, enabled);
        todo!("implement")
    }

    /// Cheap snapshot, one entry per hosted service in config order.
    pub fn services_status(&self) -> Vec<ServiceStatus> {
        todo!("implement")
    }

    /// Minecraft LAN worlds currently seen on this machine (seen in the last ~5 s).
    pub fn lan_worlds(&self) -> Vec<LanWorld> {
        todo!("implement")
    }

    // ---- client tunnels ----

    /// Cheap snapshot of open tunnels with live counters.
    pub fn tunnels_info(&self) -> Vec<TunnelInfo> {
        todo!("implement")
    }

    /// Remember a tunnel so it reopens on startup (and open it now if not open). Saves config.
    /// `local_port` 0 means pick a free port; the chosen port is what gets saved.
    pub async fn save_tunnel(
        &self,
        peer: NodeId,
        service: &str,
        local_port: u16,
        auto_open: bool,
    ) -> anyhow::Result<ActiveTunnel> {
        let _ = (peer, service, local_port, auto_open);
        todo!("implement")
    }

    /// Forget a saved tunnel. Does not close it if open. Saves config.
    pub async fn forget_tunnel(&self, peer: NodeId, service: &str) -> anyhow::Result<()> {
        let _ = (peer, service);
        todo!("implement")
    }
}
