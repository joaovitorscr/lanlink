use anyhow::Context;
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
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lanlink")
    }
    pub fn load() -> anyhow::Result<Config> {
        let path = Self::dir().join("config.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("config.json");
        let tmp = dir.join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
