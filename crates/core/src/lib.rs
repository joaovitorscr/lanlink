//! lanlink-core: peer-to-peer port forwarding over iroh (QUIC, TLS 1.3, hole punching + relay fallback).
//!
//! Used by both `cli` and `app`. [`Node`] (node.rs) runs the endpoint, connections and tunnels;
//! api.rs adds the management calls the GUI uses; protocol.rs is the wire format.
//!
//! Model:
//! - Every node has a persistent Ed25519 identity (`NodeId`), stored under the config dir.
//! - A node may HOST services (local TCP/UDP ports it exposes) and may CONNECT to services on allowed peers.
//! - Only peers in the allowlist may open connections; everyone else is rejected at the QUIC handshake.
//! - One QUIC connection per peer. Each inbound TCP socket maps to one bidirectional QUIC stream.
//!   UDP uses QUIC datagrams with a small header carrying the service id.
//! - Latency: TCP_NODELAY on local sockets, small buffers, no extra framing on TCP streams.

pub mod api;
pub mod build_info;
pub mod config;
pub mod forward;
pub mod lan;
pub mod node;
pub mod protocol;
pub mod stats;

pub use api::{
    init_logging, LanWorld, LogGuard, NetworkStatus, PeerRequest, ServiceStatus, TunnelInfo,
};
pub use config::{Config, Protocol, SavedTunnel, Service};
pub use iroh::{EndpointId as NodeId, SecretKey};
pub use node::{ConnState, Node, NodeEvent, PeerInfo};
pub use stats::LatencyStats;

use std::net::SocketAddr;

/// ALPN used for lanlink connections. Bump on incompatible protocol changes.
/// 1: control messages are tagged JSON objects (`{"type":"Ping","t":1}`).
pub const ALPN: &[u8] = b"lanlink/1";

/// A service a client is currently connected to: local listener -> remote peer service.
#[derive(Debug, Clone)]
pub struct ActiveTunnel {
    pub peer: NodeId,
    pub service: String,
    pub local_addr: SocketAddr,
}
