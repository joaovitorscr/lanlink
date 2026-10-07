use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// A service this node exposes to allowed peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    /// Human name, unique per node. e.g. "minecraft".
    pub name: String,
    pub protocol: Protocol,
    /// Local port on the host machine, e.g. 25565.
    pub port: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Peers (public keys, iroh EndpointId strings) allowed to connect to us.
    pub allowed_peers: Vec<String>,
    /// Optional display names for peers, keyed by NodeId string.
    #[serde(default)]
    pub peer_names: std::collections::HashMap<String, String>,
    /// Services we host.
    #[serde(default)]
    pub services: Vec<Service>,
    /// Optional self-hosted relay URL, e.g. "https://relay.example.com". None = iroh defaults.
    #[serde(default)]
    pub relay_url: Option<String>,
}

impl Config {
    /// Platform config dir, e.g. ~/.config/lanlink or %APPDATA%\lanlink.
    pub fn dir() -> PathBuf {
        todo!("implement")
    }
    pub fn load() -> anyhow::Result<Config> {
        todo!("implement: load config.json from dir(), default if missing")
    }
    pub fn save(&self) -> anyhow::Result<()> {
        todo!("implement")
    }
}
