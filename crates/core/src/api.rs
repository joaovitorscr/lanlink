//! Management API: everything the GUI needs so nobody has to use a terminal
//! (peers and requests, hosted services, saved tunnels, settings, logging).
//!
//! Polling vs events: discrete changes arrive as `NodeEvent`s. Fast-changing counters
//! (bytes, connection counts, service reachability) are read by the UI about once a second
//! through `services_status()` and `tunnels_info()`; they must be cheap (no I/O, no awaits).

use crate::node::NodeEvent;
use crate::{ActiveTunnel, Config, Node, NodeId, Protocol, SavedTunnel, Service};
use anyhow::Context;
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
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
    /// TCP: something is listening on the service address (`Service::local_addr`), probed
    /// every few seconds.
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

/// Result of [`Node::apply_config`].
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    /// Where the previous config was saved.
    pub backup: PathBuf,
    /// The relay changed; it takes effect after a restart.
    pub restart_required: bool,
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
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{fmt, EnvFilter, Layer};

    const DEFAULT_FILTER: &str =
        "lanlink=info,lanlink_core=info,lanlink_app=info,lanlink_cli=info,warn";
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| DEFAULT_FILTER.into());
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());

    let dir = Config::logs_dir();
    let appender = std::fs::create_dir_all(&dir)
        .map_err(anyhow::Error::from)
        .and_then(|_| {
            tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix(prefix)
                .filename_suffix("log")
                .max_log_files(7)
                .build(&dir)
                .map_err(anyhow::Error::from)
        });
    match appender {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            let _ = tracing_subscriber::registry()
                .with(stderr)
                .with(file)
                .try_init();
            LogGuard {
                inner: Some(Box::new(guard)),
            }
        }
        Err(e) => {
            let _ = tracing_subscriber::registry().with(stderr).try_init();
            tracing::warn!("file logging disabled ({}): {e:#}", dir.display());
            LogGuard { inner: None }
        }
    }
}

const REQUEST_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl Node {
    // ---- peers ----

    /// Allow a peer, save its name, and start connecting (auto-reconnect keeps trying).
    /// Also removes any pending request from it. Saves config.
    pub async fn add_peer(&self, id: NodeId, name: Option<String>) -> anyhow::Result<()> {
        let key = id.to_string();
        let name = name.filter(|n| !n.trim().is_empty());
        self.edit_config(|c| {
            if !c.allowed_peers.contains(&key) {
                c.allowed_peers.push(key.clone());
            }
            if let Some(n) = name {
                c.peer_names.insert(key.clone(), n);
            }
        })?;
        self.activate_peer(id);
        Ok(())
    }

    /// Runtime side of allowing a peer: drop its request and start connecting.
    fn activate_peer(&self, id: NodeId) {
        self.inner
            .requests
            .lock()
            .unwrap()
            .retain(|(r, _)| r.id != id);
        self.update_peer(id, |_| {});
        self.start_dialer(id);
        self.wake_dialer(id);
    }

    /// Remove from allowlist, names and saved tunnels; close its tunnels and connections. Saves config.
    pub async fn remove_peer(&self, id: NodeId) -> anyhow::Result<()> {
        let key = id.to_string();
        self.stop_dialer(id);
        self.edit_config(|c| {
            c.allowed_peers.retain(|p| p != &key);
            c.peer_names.remove(&key);
            c.saved_tunnels.retain(|t| t.peer != key);
        })?;
        self.deactivate_peer(id);
        Ok(())
    }

    /// Runtime side of removing a peer (after its dialer stopped and config dropped it):
    /// close its tunnels and connections and forget its live state.
    fn deactivate_peer(&self, id: NodeId) {
        self.close_peer_tunnels(id);
        self.disconnect_peer(id);
        self.inner.peers.lock().unwrap().remove(&id);
        self.inner
            .requests
            .lock()
            .unwrap()
            .retain(|(r, _)| r.id != id);
    }

    /// Set or clear the local display name for a peer. Saves config. Emits PeerStateChanged.
    pub async fn rename_peer(&self, id: NodeId, name: Option<String>) -> anyhow::Result<()> {
        let key = id.to_string();
        let name = name.filter(|n| !n.trim().is_empty());
        self.edit_config(|c| match name.clone() {
            Some(n) => {
                c.peer_names.insert(key.clone(), n);
            }
            None => {
                c.peer_names.remove(&key);
            }
        })?;
        if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&id) {
            e.info.name = name.clone().or_else(|| e.remote_name.clone());
        }
        // Always emit so the UI refreshes, even when the visible name did not change.
        if let Some(p) = self.peers().into_iter().find(|p| p.id == id) {
            self.emit(NodeEvent::PeerStateChanged(p));
        }
        Ok(())
    }

    /// Dial now, resetting the auto-reconnect backoff.
    pub async fn reconnect(&self, id: NodeId) -> anyhow::Result<()> {
        if self.is_allowed(&id) {
            self.start_dialer(id);
            self.wake_dialer(id);
            Ok(())
        } else {
            self.ensure_conn(id).await.map(|_| ())
        }
    }

    /// Unknown peers that tried to connect recently (deduped by id, newest first, at most 20,
    /// entries expire after 10 minutes).
    pub fn pending_requests(&self) -> Vec<PeerRequest> {
        let mut reqs = self.inner.requests.lock().unwrap();
        reqs.retain(|(_, at)| at.elapsed() < REQUEST_TTL);
        reqs.iter().map(|(r, _)| r.clone()).collect()
    }

    /// Allow = `add_peer(id, name)`. Deny = drop the request; it may come back if they retry.
    pub async fn respond_request(
        &self,
        id: NodeId,
        allow: bool,
        name: Option<String>,
    ) -> anyhow::Result<()> {
        if allow {
            let name = name.or_else(|| {
                self.inner
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(r, _)| r.id == id)
                    .and_then(|(r, _)| r.name.clone())
            });
            self.add_peer(id, name).await
        } else {
            self.inner
                .requests
                .lock()
                .unwrap()
                .retain(|(r, _)| r.id != id);
            Ok(())
        }
    }

    // ---- us ----

    pub fn network_status(&self) -> NetworkStatus {
        self.inner.network.lock().unwrap().clone()
    }

    /// Set our display name sent to peers. Saves config; takes effect on new connections.
    pub async fn set_display_name(&self, name: Option<String>) -> anyhow::Result<()> {
        let name = name.filter(|n| !n.trim().is_empty());
        self.edit_config(|c| c.display_name = name)
    }

    /// Set or clear the custom relay. Saves config. Returns true when a restart is needed to apply it.
    pub async fn set_relay_url(&self, url: Option<String>) -> anyhow::Result<bool> {
        let url = normalize_relay_url(url)?;
        let mut changed = false;
        self.edit_config(|c| {
            changed = c.relay_url != url;
            c.relay_url = url;
        })?;
        Ok(changed)
    }

    /// Turn Minecraft LAN world detection on or off. Saves config and starts or stops the
    /// listener right away; turning it off clears `lan_worlds()`.
    pub async fn set_lan_detection(&self, enabled: bool) -> anyhow::Result<()> {
        self.edit_config(|c| c.disable_lan_detection = !enabled)?;
        self.sync_lan_listener();
        Ok(())
    }

    /// Turn the latency CSV log (daily files in `Config::logs_dir()`, 7 kept) on or off.
    /// Saves config; takes effect with the next ping.
    pub async fn set_latency_log(&self, enabled: bool) -> anyhow::Result<()> {
        self.edit_config(|c| c.latency_log = enabled)?;
        self.set_latency_logging(enabled);
        Ok(())
    }

    // ---- import ----

    /// Replace the whole config (an import, see [`crate::export`]) and bring the running node
    /// in line with it: peers no longer allowed are disconnected and their tunnels closed, new
    /// peers are dialed, removed services close their streams and the service list is pushed,
    /// tunnels no longer saved are closed and new auto-open ones are opened, and LAN detection
    /// and the latency log follow the new settings. The previous config is first saved to
    /// `config.json.bak`. A new relay URL needs a restart (`restart_required`).
    pub async fn apply_config(&self, mut new: Config) -> anyhow::Result<ImportOutcome> {
        crate::export::validate(&new)?;
        new.relay_url = normalize_relay_url(new.relay_url)?;
        new.display_name = new.display_name.filter(|n| !n.trim().is_empty());
        let old = self.config();
        let backup = old.save_backup()?;

        let ids = |c: &Config| -> HashSet<NodeId> {
            c.allowed_peers
                .iter()
                .filter_map(|p| p.parse().ok())
                .collect()
        };
        let (old_ids, new_ids) = (ids(&old), ids(&new));
        let removed: Vec<NodeId> = old_ids.difference(&new_ids).copied().collect();
        for &id in &removed {
            self.stop_dialer(id);
        }
        self.edit_config(|c| *c = new.clone())?;

        // Peers.
        for id in removed {
            self.deactivate_peer(id);
        }
        for &id in &new_ids {
            if old_ids.contains(&id) {
                // Picks up a changed local name.
                self.update_peer(id, |_| {});
            } else {
                self.activate_peer(id);
            }
        }

        // Hosted services.
        for s in &old.services {
            if !new.services.iter().any(|n| n.name == s.name) {
                self.close_service_streams(&s.name);
            }
        }
        if old.services != new.services {
            self.push_services();
        }

        // Saved tunnels: close the ones that were saved and are gone or moved, open new ones.
        let is_saved = |c: &Config, t: &ActiveTunnel, port: Option<u16>| {
            c.saved_tunnels.iter().any(|s| {
                s.peer == t.peer.to_string()
                    && s.service == t.service
                    && port.is_none_or(|p| p == s.local_port)
            })
        };
        for t in self.tunnels() {
            if is_saved(&old, &t, None) && !is_saved(&new, &t, Some(t.local_addr.port())) {
                let _ = self.close_tunnel(&t).await;
            }
        }
        let open = self.tunnels();
        let to_open: Vec<SavedTunnel> = new
            .saved_tunnels
            .iter()
            .filter(|s| {
                s.auto_open
                    && !open.iter().any(|t| {
                        t.peer.to_string() == s.peer
                            && t.service == s.service
                            && t.local_addr.port() == s.local_port
                    })
            })
            .cloned()
            .collect();
        self.open_saved_tunnels(to_open).await;

        // Settings. The display name is read on each new connection.
        self.sync_lan_listener();
        self.set_latency_logging(new.latency_log);

        Ok(ImportOutcome {
            backup,
            restart_required: old.relay_url != new.relay_url,
        })
    }

    // ---- hosted services ----

    /// Add or replace (by name) a hosted service. Saves config and pushes the new service list
    /// to connected peers.
    pub async fn add_service(&self, service: Service) -> anyhow::Result<()> {
        anyhow::ensure!(!service.name.trim().is_empty(), "service name is empty");
        anyhow::ensure!(service.port != 0, "service port must not be 0");
        self.edit_config(
            |c| match c.services.iter_mut().find(|s| s.name == service.name) {
                Some(s) => *s = service,
                None => c.services.push(service),
            },
        )?;
        self.push_services();
        Ok(())
    }

    /// Remove a hosted service, close its open streams, push the new list to peers. Saves config.
    pub async fn remove_service(&self, name: &str) -> anyhow::Result<()> {
        self.edit_config(|c| c.services.retain(|s| s.name != name))?;
        self.close_service_streams(name);
        self.push_services();
        Ok(())
    }

    /// Enable or disable a hosted service without deleting it. Saves config, pushes list to peers.
    pub async fn set_service_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let mut found = false;
        self.edit_config(|c| {
            if let Some(s) = c.services.iter_mut().find(|s| s.name == name) {
                s.enabled = enabled;
                found = true;
            }
        })?;
        anyhow::ensure!(found, "no hosted service named {name:?}");
        self.push_services();
        Ok(())
    }

    /// Cheap snapshot, one entry per hosted service in config order.
    pub fn services_status(&self) -> Vec<ServiceStatus> {
        let services = self.config().services;
        let live = self.inner.svc_live.lock().unwrap();
        services
            .into_iter()
            .map(|service| {
                let l = live.get(&service.name);
                let mut peers: Vec<NodeId> = l
                    .map(|l| l.peers.keys().copied().collect())
                    .unwrap_or_default();
                peers.sort_by_key(|p| p.to_string());
                ServiceStatus {
                    reachable: match service.protocol {
                        Protocol::Tcp => l.and_then(|l| l.reachable),
                        Protocol::Udp => None,
                    },
                    effective_port: self.effective_port(&service),
                    connections: l.map_or(0, |l| l.conns),
                    peers,
                    service,
                }
            })
            .collect()
    }

    /// Minecraft LAN worlds currently seen on this machine (seen in the last ~5 s).
    pub fn lan_worlds(&self) -> Vec<LanWorld> {
        self.lan_worlds_now()
    }

    // ---- client tunnels ----

    /// Cheap snapshot of open tunnels with live counters.
    pub fn tunnels_info(&self) -> Vec<TunnelInfo> {
        let saved = self.inner.config.read().unwrap().saved_tunnels.clone();
        let tunnels = self.inner.tunnels.lock().unwrap();
        tunnels
            .iter()
            .map(|t| TunnelInfo {
                tunnel: t.tunnel.clone(),
                protocol: t.protocol,
                connections: self.tunnel_conns(t),
                bytes_up: t.live.up.load(Ordering::Relaxed),
                bytes_down: t.live.down.load(Ordering::Relaxed),
                saved: saved
                    .iter()
                    .any(|s| s.peer == t.tunnel.peer.to_string() && s.service == t.tunnel.service),
            })
            .collect()
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
        let existing = self.tunnels().into_iter().find(|t| {
            t.peer == peer
                && t.service == service
                && (local_port == 0 || t.local_addr.port() == local_port)
        });
        let tunnel = match existing {
            Some(t) => t,
            None => {
                let local = SocketAddr::from((Ipv4Addr::LOCALHOST, local_port));
                match self.ensure_conn(peer).await {
                    Ok((_, info)) => {
                        self.open_tunnel_with(peer, service, local, Some(&info), Protocol::Tcp)
                            .await?
                    }
                    // Peer offline: bind now, dial lazily when something connects.
                    Err(_) => {
                        let protocol = self.known_protocol(peer, service);
                        self.open_tunnel_with(peer, service, local, None, protocol)
                            .await?
                    }
                }
            }
        };
        let protocol = self
            .tunnel_protocol(&tunnel)
            .unwrap_or_else(|| self.known_protocol(peer, service));
        let saved = SavedTunnel {
            peer: peer.to_string(),
            service: service.to_string(),
            protocol,
            local_port: tunnel.local_addr.port(),
            auto_open,
        };
        self.edit_config(|c| {
            match c
                .saved_tunnels
                .iter_mut()
                .find(|s| s.peer == saved.peer && s.service == saved.service)
            {
                Some(s) => *s = saved,
                None => c.saved_tunnels.push(saved),
            }
        })?;
        Ok(tunnel)
    }

    /// Best guess of a service's protocol while its peer is offline: the saved tunnel's, else
    /// the last service list the peer sent, else TCP.
    fn known_protocol(&self, peer: NodeId, service: &str) -> Protocol {
        let key = peer.to_string();
        let saved = self
            .inner
            .config
            .read()
            .unwrap()
            .saved_tunnels
            .iter()
            .find(|s| s.peer == key && s.service == service)
            .map(|s| s.protocol);
        saved
            .or_else(|| {
                self.inner.peers.lock().unwrap().get(&peer).and_then(|e| {
                    e.info
                        .services
                        .iter()
                        .find(|s| s.name == service)
                        .map(|s| s.protocol)
                })
            })
            .unwrap_or(Protocol::Tcp)
    }

    /// Forget a saved tunnel. Does not close it if open. Saves config.
    pub async fn forget_tunnel(&self, peer: NodeId, service: &str) -> anyhow::Result<()> {
        let key = peer.to_string();
        self.edit_config(|c| {
            c.saved_tunnels
                .retain(|s| !(s.peer == key && s.service == service))
        })
    }
}

/// Trim a relay URL, treat empty as unset, and check it parses.
fn normalize_relay_url(url: Option<String>) -> anyhow::Result<Option<String>> {
    let url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    if let Some(u) = &url {
        u.parse::<iroh::RelayUrl>()
            .with_context(|| format!("invalid relay url {u}"))?;
    }
    Ok(url)
}
