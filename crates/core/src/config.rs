use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// A service this node exposes to allowed peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    /// Human name, unique per node. e.g. "minecraft".
    pub name: String,
    pub protocol: Protocol,
    /// Local port on the host machine, e.g. 25565.
    pub port: u16,
    /// Address the host connects to for this service. None = 127.0.0.1. Set it for services
    /// that only listen on a LAN address or on IPv6 (`::1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<IpAddr>,
    /// Disabled services are not advertised and refuse new streams. Default true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// This service is a Minecraft Java server or "Open to LAN" world.
    /// Host: if exactly one LAN world is detected, forward to its port instead of `port`.
    /// Client: tunnels to it are announced on the local LAN so the world shows in Multiplayer.
    #[serde(default)]
    pub minecraft_lan: bool,
}

impl Service {
    pub fn new(name: impl Into<String>, protocol: Protocol, port: u16) -> Self {
        Self {
            name: name.into(),
            protocol,
            port,
            host: None,
            enabled: true,
            minecraft_lan: false,
        }
    }

    /// The address the host dials: `host` (default 127.0.0.1) with `port`.
    pub fn local_addr(&self, port: u16) -> SocketAddr {
        SocketAddr::new(self.host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)), port)
    }
}

fn default_true() -> bool {
    true
}

fn default_tcp() -> Protocol {
    Protocol::Tcp
}

/// A client-side tunnel remembered across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTunnel {
    /// Peer NodeId string.
    pub peer: String,
    pub service: String,
    /// Protocol of the remote service, so the right socket is bound while the peer is offline.
    /// Config files written before this field existed only had TCP tunnels.
    #[serde(default = "default_tcp")]
    pub protocol: Protocol,
    /// Local port to listen on (127.0.0.1). 0 is never saved; the chosen port is saved instead.
    pub local_port: u16,
    /// Open this tunnel automatically when the app starts. Default true.
    #[serde(default = "default_true")]
    pub auto_open: bool,
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
    /// Our display name, sent to peers in Hello. None = hostname.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Client tunnels to reopen on startup.
    #[serde(default)]
    pub saved_tunnels: Vec<SavedTunnel>,
    /// Turn off listening for Minecraft "Open to LAN" broadcasts on this machine.
    #[serde(default)]
    pub disable_lan_detection: bool,
    /// Append every ping sample to daily `latency.<date>.csv` files in the logs dir (7 kept).
    #[serde(default)]
    pub latency_log: bool,
}

impl Config {
    /// Platform config dir, e.g. ~/.config/lanlink or %APPDATA%\lanlink.
    /// Set `LANLINK_CONFIG_DIR` to use a different folder, e.g. for tests or a second instance.
    pub fn dir() -> PathBuf {
        if let Some(d) = std::env::var_os("LANLINK_CONFIG_DIR") {
            return PathBuf::from(d);
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lanlink")
    }
    /// Folder for log files: `dir()/logs`.
    pub fn logs_dir() -> PathBuf {
        Self::dir().join("logs")
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
        self.write_file("config.json").map(|_| ())
    }
    /// Save to `config.json.bak` next to the config (overwriting an older backup), e.g. before
    /// an import replaces the config. Returns the backup path.
    pub fn save_backup(&self) -> anyhow::Result<PathBuf> {
        self.write_file("config.json.bak")
    }
    fn write_file(&self, name: &str) -> anyhow::Result<PathBuf> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(name);
        // Unique temp name so concurrent saves (e.g. two nodes in one test process) never collide.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!("{name}.{}.{seq}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_without_host_loads_as_loopback() {
        let svc: Service =
            serde_json::from_str(r#"{"name":"mc","protocol":"tcp","port":25565}"#).unwrap();
        assert_eq!(svc.host, None);
        assert_eq!(svc.local_addr(svc.port), "127.0.0.1:25565".parse().unwrap());
        // Old configs keep their exact shape when no host is set.
        assert!(!serde_json::to_string(&svc).unwrap().contains("host"));
    }

    #[test]
    fn service_host_round_trips() {
        let mut svc = Service::new("mc", Protocol::Tcp, 25565);
        svc.host = Some("::1".parse().unwrap());
        let back: Service = serde_json::from_str(&serde_json::to_string(&svc).unwrap()).unwrap();
        assert_eq!(back, svc);
        assert_eq!(back.local_addr(25565), "[::1]:25565".parse().unwrap());
    }
}
