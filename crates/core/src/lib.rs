//! lanlink-core: peer-to-peer port forwarding over iroh (QUIC, TLS 1.3, hole punching + relay fallback).
//!
//! This file is the API CONTRACT shared by `cli` and `app`. Keep public signatures stable;
//! implementers fill in the bodies in submodules.
//!
//! Model:
//! - Every node has a persistent Ed25519 identity (`NodeId`), stored under the config dir.
//! - A node may HOST services (local TCP/UDP ports it exposes) and may CONNECT to services on allowed peers.
//! - Only peers in the allowlist may open connections; everyone else is rejected at the QUIC handshake.
//! - One QUIC connection per peer. Each inbound TCP socket maps to one bidirectional QUIC stream.
//!   UDP uses QUIC datagrams with a small header carrying the service id.
//! - Latency: TCP_NODELAY on local sockets, small buffers, no extra framing on TCP streams.

pub mod config;
pub mod node;
pub mod forward;
pub mod protocol;

pub use config::{Config, Protocol, Service};
pub use node::{ConnState, Node, NodeEvent, PeerInfo};
pub use iroh::{EndpointId as NodeId, SecretKey};

use std::net::SocketAddr;

/// ALPN used for lanlink connections. Bump on incompatible protocol changes.
pub const ALPN: &[u8] = b"lanlink/0";

/// A service a client is currently connected to: local listener -> remote peer service.
#[derive(Debug, Clone)]
pub struct ActiveTunnel {
    pub peer: NodeId,
    pub service: String,
    pub local_addr: SocketAddr,
}
