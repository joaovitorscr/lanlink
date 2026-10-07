use crate::{ActiveTunnel, Config, Service};
use crate::NodeId;
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    /// Connected via relay (higher latency).
    Relayed,
    /// Direct UDP path established.
    Direct,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: NodeId,
    pub name: Option<String>,
    pub state: ConnState,
    pub latency_ms: Option<u32>,
    /// Services the peer advertises to us (learned on connect).
    pub services: Vec<Service>,
}

#[derive(Debug, Clone)]
pub enum NodeEvent {
    PeerStateChanged(PeerInfo),
    TunnelOpened(ActiveTunnel),
    TunnelClosed(ActiveTunnel),
    Error(String),
}

/// The running lanlink node. Cheap to clone (Arc inside).
#[derive(Clone)]
pub struct Node {
    // implementers: wrap an Arc<Inner> here
}

impl Node {
    /// Load or create identity, bind iroh endpoint, start accept loop. Non-blocking.
    pub async fn start(config: Config) -> anyhow::Result<Node> {
        todo!("implement")
    }
    pub fn id(&self) -> NodeId {
        todo!("implement")
    }
    pub fn config(&self) -> Config {
        todo!("implement")
    }
    /// Persist and apply a new config (allowlist, services) at runtime.
    pub async fn update_config(&self, config: Config) -> anyhow::Result<()> {
        todo!("implement")
    }
    /// Subscribe to state changes for the UI.
    pub fn subscribe(&self) -> broadcast::Receiver<NodeEvent> {
        todo!("implement")
    }
    pub fn peers(&self) -> Vec<PeerInfo> {
        todo!("implement")
    }
    /// Connect to a peer and fetch its service list. Idempotent.
    pub async fn connect(&self, peer: NodeId) -> anyhow::Result<PeerInfo> {
        todo!("implement")
    }
    /// Open a local listener on `local` (port 0 = any) that forwards to `service` on `peer`.
    pub async fn open_tunnel(&self, peer: NodeId, service: &str, local: SocketAddr) -> anyhow::Result<ActiveTunnel> {
        todo!("implement")
    }
    pub async fn close_tunnel(&self, tunnel: &ActiveTunnel) -> anyhow::Result<()> {
        todo!("implement")
    }
    pub fn tunnels(&self) -> Vec<ActiveTunnel> {
        todo!("implement")
    }
    pub async fn shutdown(self) -> anyhow::Result<()> {
        todo!("implement")
    }
}
