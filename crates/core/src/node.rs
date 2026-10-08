use crate::api::{LanWorld, NetworkStatus};
use crate::forward::{self, ClientFlow, Counters, FlowSender, UDP_BUF};
use crate::instance::InstanceLock;
use crate::lan;
use crate::protocol::{
    decode_datagram, read_frame, read_msg, write_msg, ControlMsg, DatagramHeader, Reassembler,
    StreamHeader,
};
use crate::stats::{LatencyLog, LatencyStats, LatencyWindow};
use crate::NodeId;
use crate::{ActiveTunnel, Config, Protocol, SavedTunnel, Service, ALPN};
use anyhow::{bail, Context};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{
    presets, Connection, ConnectionError, ReadError, RecvStream, SendStream, VarInt, WriteError,
};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, Watcher};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, Notify};
use tokio::task::{AbortHandle, JoinSet};

/// 1s pings give a useful latency history at negligible cost (a few bytes per second).
const PING_INTERVAL: Duration = Duration::from_secs(1);
const PATH_POLL_INTERVAL: Duration = Duration::from_secs(1);
const EVENT_CAPACITY: usize = 64;
const CLOSE_NOT_ALLOWED: u32 = 1;
const CLOSE_SHUTDOWN: u32 = 2;
/// We do not know the dialer yet; it was recorded as a connection request.
pub(crate) const CLOSE_PENDING: u32 = 3;
/// A join request was answered.
pub(crate) const CLOSE_JOINED: u32 = 4;
/// Data stream reset codes (host -> client).
const RESET_UNKNOWN_SERVICE: u32 = 1;
/// The host could not connect to the local service (nothing listening there).
const RESET_NOT_LISTENING: u32 = 2;
/// At most one "nothing is listening" error per tunnel in this window.
const NOT_LISTENING_REPORT_GAP: Duration = Duration::from_secs(60);

pub(crate) const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const NETWORK_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const LAN_WORLD_TTL: Duration = Duration::from_secs(5);
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);
/// A UDP flow (one local sender on the client, one local socket on the host) is closed after
/// this long without traffic in either direction.
const UDP_FLOW_IDLE: Duration = Duration::from_secs(30);
/// How often idle UDP flows are looked for.
const UDP_FLOW_SWEEP: Duration = Duration::from_secs(5);

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
    /// Rolling latency stats over the last minute of pings. Default until the first pong.
    pub stats: LatencyStats,
    /// Why the last connect attempt failed, in plain words for the UI
    /// (e.g. "Waiting for Arthur to accept your request", "Peer is offline"). None when connected.
    pub last_error: Option<String>,
    /// When the current connection was established.
    pub connected_since: Option<std::time::SystemTime>,
    /// The peer currently has a connection open to us (they can use our services).
    pub inbound: bool,
    /// Total tunnel payload bytes to / from this peer since the app started.
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Services the peer advertises to us (learned on connect).
    pub services: Vec<Service>,
}

#[derive(Debug, Clone)]
pub enum NodeEvent {
    PeerStateChanged(PeerInfo),
    TunnelOpened(ActiveTunnel),
    TunnelClosed(ActiveTunnel),
    Error(String),
    /// An unknown peer tried to connect. The UI should offer Allow / Deny.
    PeerRequest(crate::PeerRequest),
    /// Our own connectivity changed (relay, addresses, online).
    NetworkChanged(crate::NetworkStatus),
    /// The set of Minecraft "Open to LAN" worlds seen on this machine changed.
    LanWorldsChanged(Vec<crate::LanWorld>),
    /// `Config::networks` changed (a join, a push from an owner, a local edit). Re-read
    /// `Node::config()`.
    NetworksChanged,
}

/// The running lanlink node. Cheap to clone (Arc inside).
#[derive(Clone)]
pub struct Node {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) type ControlSend = Arc<tokio::sync::Mutex<SendStream>>;

pub(crate) struct PeerEntry {
    pub(crate) info: PeerInfo,
    /// Connection we dialed (used for tunnels).
    dialed: Option<Connection>,
    /// Connections the peer dialed to us.
    inbound: Vec<Connection>,
    /// Control stream senders of every live connection, keyed by connection stable id.
    pub(crate) controls: Vec<(usize, ControlSend)>,
    /// Client UDP flows on the dialed connection, keyed by flow id.
    udp: HashMap<u32, (Arc<ClientFlow>, Arc<TunnelLive>)>,
    /// Recent ping samples backing `info.stats`.
    latency: LatencyWindow,
    /// Name the peer sent in Hello. Used when we have no local name for it.
    pub(crate) remote_name: Option<String>,
}

/// Live counters of one client tunnel.
#[derive(Default)]
pub(crate) struct TunnelLive {
    pub(crate) up: Arc<AtomicU64>,
    pub(crate) down: Arc<AtomicU64>,
    /// Open TCP connections, or live UDP flows (local senders).
    pub(crate) conns: AtomicU32,
    /// Milliseconds since node epoch of the last "nothing is listening" error (0 = never).
    last_error_ms: AtomicU64,
}

/// Total tunnel bytes per peer.
#[derive(Default)]
pub(crate) struct PeerBytes {
    pub(crate) sent: Arc<AtomicU64>,
    pub(crate) received: Arc<AtomicU64>,
}

pub(crate) struct TunnelEntry {
    pub(crate) tunnel: ActiveTunnel,
    pub(crate) protocol: Protocol,
    pub(crate) live: Arc<TunnelLive>,
    task: AbortHandle,
}

/// Live state of a hosted service.
#[derive(Default)]
pub(crate) struct SvcLive {
    pub(crate) reachable: Option<bool>,
    pub(crate) conns: u32,
    pub(crate) peers: HashMap<NodeId, u32>,
    /// Open streams / UDP flows, so they can be closed when the service is removed.
    streams: HashMap<u64, AbortHandle>,
}

pub(crate) struct Inner {
    pub(crate) endpoint: Endpoint,
    lookup: MemoryLookup,
    /// Current config. Only read locks are held for long; writers go through `edit_config`.
    pub(crate) config: RwLock<Config>,
    /// Serialises `edit_config` so concurrent edits never interleave or lose each other.
    config_edit: Mutex<()>,
    events: broadcast::Sender<NodeEvent>,
    pub(crate) peers: Mutex<HashMap<NodeId, PeerEntry>>,
    pub(crate) tunnels: Mutex<Vec<TunnelEntry>>,
    tasks: Mutex<Vec<AbortHandle>>,
    dial_locks: Mutex<HashMap<NodeId, Arc<tokio::sync::Mutex<()>>>>,
    /// Auto-reconnect loops for allowed peers, with a handle to wake them early.
    dialers: Mutex<HashMap<NodeId, (AbortHandle, Arc<Notify>)>>,
    epoch: Instant,
    latency_log: Mutex<LatencyLog>,
    /// Minecraft LAN listener, running while LAN detection is enabled.
    lan_task: Mutex<Option<AbortHandle>>,
    pub(crate) network: Mutex<NetworkStatus>,
    pub(crate) lan: Mutex<Vec<(LanWorld, Instant)>>,
    pub(crate) svc_live: Mutex<HashMap<String, SvcLive>>,
    /// Bumped whenever the advertised service list changes (UDP indexes may shift).
    svc_gen: AtomicU64,
    next_id: AtomicU64,
    pub(crate) peer_bytes: Mutex<HashMap<NodeId, Arc<PeerBytes>>>,
    hostname: String,
    /// Single-instance lock, released on shutdown. None for explicit-key nodes (tests).
    instance: Mutex<Option<InstanceLock>>,
}

/// The remote closed our connection because it does not (yet) allow us.
#[derive(Debug)]
struct NotAccepted;

impl std::fmt::Display for NotAccepted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("peer has not accepted us yet")
    }
}

impl std::error::Error for NotAccepted {}

/// Load the persistent identity from `path`, creating it (mode 0600) if missing.
pub fn load_or_create_secret_key(path: &Path) -> anyhow::Result<SecretKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("{} is not a 32 byte key", path.display()))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let key = SecretKey::generate();
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write;
            let mut f = opts
                .open(path)
                .with_context(|| format!("creating {}", path.display()))?;
            f.write_all(&key.to_bytes())?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn path_state(conn: &Connection) -> ConnState {
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    match selected {
        Some(p) if p.is_ip() => ConnState::Direct,
        Some(_) => ConnState::Relayed,
        None if paths.is_empty() => ConnState::Connecting,
        None => ConnState::Relayed,
    }
}

/// Closed by the remote with "not allowed" (old peers) or "pending" (new peers).
fn is_rejection(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(c))
            if c.error_code == CLOSE_NOT_ALLOWED.into() || c.error_code == CLOSE_PENDING.into()
    )
}

/// Untrusted names from the network: printable, trimmed, bounded.
pub(crate) fn sanitize_name(name: Option<String>) -> Option<String> {
    let name: String = name?.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn new_peer_entry(id: NodeId) -> PeerEntry {
    PeerEntry {
        info: PeerInfo {
            id,
            name: None,
            state: ConnState::Disconnected,
            latency_ms: None,
            stats: LatencyStats::default(),
            services: Vec::new(),
            last_error: None,
            connected_since: None,
            inbound: false,
            bytes_sent: 0,
            bytes_received: 0,
        },
        dialed: None,
        inbound: Vec::new(),
        controls: Vec::new(),
        udp: HashMap::new(),
        latency: LatencyWindow::default(),
        remote_name: None,
    }
}

/// Decrements a hosted service's live counts when a stream or UDP flow ends.
struct SvcGuard {
    node: Node,
    service: String,
    peer: NodeId,
    id: u64,
}

impl Drop for SvcGuard {
    fn drop(&mut self) {
        let mut live = self.node.inner.svc_live.lock().unwrap();
        if let Some(s) = live.get_mut(&self.service) {
            s.conns = s.conns.saturating_sub(1);
            if let Some(n) = s.peers.get_mut(&self.peer) {
                *n -= 1;
                if *n == 0 {
                    s.peers.remove(&self.peer);
                }
            }
            s.streams.remove(&self.id);
        }
    }
}

/// Host side state of one client UDP flow. Dropping it closes the local socket.
struct HostFlow {
    sock: Arc<UdpSocket>,
    /// Reads replies from the local service.
    task: AbortHandle,
    /// Milliseconds since node start of the last packet in either direction.
    last_ms: Arc<AtomicU64>,
    /// Counts the flow as a live connection of the service.
    _guard: Option<SvcGuard>,
}

impl HostFlow {
    fn idle(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_ms.load(Ordering::Relaxed))
            >= UDP_FLOW_IDLE.as_millis() as u64
    }
}

impl Drop for HostFlow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Decrements a tunnel's live connection count when a local TCP connection ends.
struct TunnelConnGuard(Arc<TunnelLive>);

impl Drop for TunnelConnGuard {
    fn drop(&mut self) {
        self.0.conns.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Node {
    /// Load or create identity, bind iroh endpoint, start accept loop. Non-blocking.
    /// Fails if another lanlink process already runs a node on this config dir.
    pub async fn start(config: Config) -> anyhow::Result<Node> {
        let instance = InstanceLock::acquire(&Config::dir())?;
        let key = load_or_create_secret_key(&Config::dir().join("identity.key"))?;
        Self::start_inner(config, key, Some(instance)).await
    }

    /// Like [`Node::start`] but with an explicit identity (does not touch the config dir).
    pub async fn start_with_secret_key(
        config: Config,
        secret_key: SecretKey,
    ) -> anyhow::Result<Node> {
        Self::start_inner(config, secret_key, None).await
    }

    async fn start_inner(
        config: Config,
        secret_key: SecretKey,
        instance: Option<InstanceLock>,
    ) -> anyhow::Result<Node> {
        let lookup = MemoryLookup::new();
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()])
            .address_lookup(lookup.clone());
        if let Some(url) = &config.relay_url {
            let url: RelayUrl = url
                .parse()
                .with_context(|| format!("invalid relay url {url}"))?;
            builder = builder.relay_mode(RelayMode::custom([url]));
        }
        let endpoint = builder.bind().await?;
        tracing::info!(id = %endpoint.id(), "lanlink node started");

        let hostname = gethostname::gethostname().to_string_lossy().into_owned();
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let latency_log = LatencyLog::new(Config::logs_dir(), config.latency_log);
        let inner = Arc::new(Inner {
            endpoint,
            lookup,
            config: RwLock::new(config),
            config_edit: Mutex::new(()),
            events,
            peers: Mutex::new(HashMap::new()),
            tunnels: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            dial_locks: Mutex::new(HashMap::new()),
            dialers: Mutex::new(HashMap::new()),
            epoch: Instant::now(),
            latency_log: Mutex::new(latency_log),
            lan_task: Mutex::new(None),
            network: Mutex::new(NetworkStatus::default()),
            lan: Mutex::new(Vec::new()),
            svc_live: Mutex::new(HashMap::new()),
            svc_gen: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            peer_bytes: Mutex::new(HashMap::new()),
            hostname,
            instance: Mutex::new(instance),
        });
        let node = Node { inner };
        let n = node.clone();
        node.spawn(async move { n.accept_loop().await });
        let n = node.clone();
        node.spawn(async move { n.network_loop().await });
        let n = node.clone();
        node.spawn(async move { n.probe_loop().await });
        let n = node.clone();
        node.spawn(async move { n.announce_loop().await });
        node.sync_lan_listener();
        node.sync_dialers();
        node.open_saved_tunnels(node.config().saved_tunnels).await;
        Ok(node)
    }

    pub fn id(&self) -> NodeId {
        self.inner.endpoint.id()
    }

    /// Our current addressing info (relay url and direct addresses).
    pub fn addr(&self) -> EndpointAddr {
        self.inner.endpoint.addr()
    }

    /// Local UDP sockets the endpoint is bound to.
    pub fn bound_sockets(&self) -> Vec<SocketAddr> {
        self.inner.endpoint.bound_sockets()
    }

    /// Register known addresses for a peer, bypassing discovery. Wakes its reconnect loop.
    pub fn add_peer_addr(&self, addr: EndpointAddr) {
        let id = addr.id;
        self.inner.lookup.add_endpoint_info(addr);
        self.wake_dialer(id);
    }

    pub fn config(&self) -> Config {
        self.inner.config.read().unwrap().clone()
    }

    /// Subscribe to state changes for the UI.
    pub fn subscribe(&self) -> broadcast::Receiver<NodeEvent> {
        self.inner.events.subscribe()
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut map: HashMap<NodeId, PeerInfo> = self
            .inner
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.info.clone()))
            .collect();
        for id in self.dial_set() {
            map.entry(id).or_insert_with(|| {
                let mut info = new_peer_entry(id).info;
                info.name = self.peer_name(&id);
                info
            });
        }
        let mut v: Vec<PeerInfo> = map.into_values().collect();
        for p in &mut v {
            self.fill_bytes(p);
        }
        v.sort_by_key(|p| p.id.to_string());
        v
    }

    /// Connect to a peer and fetch its service list. Idempotent.
    pub async fn connect(&self, peer: NodeId) -> anyhow::Result<PeerInfo> {
        Ok(self.ensure_conn_reporting(peer).await?.1)
    }

    /// Open a local listener on `local` (port 0 = any) that forwards to `service` on `peer`.
    pub async fn open_tunnel(
        &self,
        peer: NodeId,
        service: &str,
        local: SocketAddr,
    ) -> anyhow::Result<ActiveTunnel> {
        let (_, info) = self.ensure_conn_reporting(peer).await?;
        self.open_tunnel_with(peer, service, local, Some(&info), Protocol::Tcp)
            .await
    }

    pub async fn close_tunnel(&self, tunnel: &ActiveTunnel) -> anyhow::Result<()> {
        let removed = {
            let mut tunnels = self.inner.tunnels.lock().unwrap();
            let pos = tunnels.iter().position(|t| {
                t.tunnel.peer == tunnel.peer
                    && t.tunnel.service == tunnel.service
                    && t.tunnel.local_addr == tunnel.local_addr
            });
            pos.map(|p| tunnels.remove(p))
        };
        let Some(entry) = removed else {
            bail!("no such tunnel");
        };
        self.drop_tunnel(entry);
        Ok(())
    }

    pub fn tunnels(&self) -> Vec<ActiveTunnel> {
        self.inner
            .tunnels
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.tunnel.clone())
            .collect()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        for (_, (task, _)) in self.inner.dialers.lock().unwrap().drain() {
            task.abort();
        }
        for t in self.inner.tunnels.lock().unwrap().drain(..) {
            t.task.abort();
        }
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        if let Some(t) = self.inner.lan_task.lock().unwrap().take() {
            t.abort();
        }
        for s in self.inner.svc_live.lock().unwrap().values_mut() {
            for (_, t) in s.streams.drain() {
                t.abort();
            }
        }
        for (_, e) in self.inner.peers.lock().unwrap().drain() {
            for c in e.dialed.iter().chain(e.inbound.iter()) {
                c.close(CLOSE_SHUTDOWN.into(), b"shutdown");
            }
        }
        self.inner.endpoint.close().await;
        self.inner.instance.lock().unwrap().take();
        Ok(())
    }

    // ---- internals ----

    pub(crate) fn emit(&self, ev: NodeEvent) {
        let _ = self.inner.events.send(ev);
    }

    pub(crate) fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.inner.tasks.lock().unwrap();
        tasks.retain(|t| !t.is_finished());
        tasks.push(tokio::spawn(fut).abort_handle());
    }

    /// May connect to us: in `allowed_peers`, a member of one of our networks, or the owner
    /// of a network we wait to join or are leaving.
    pub(crate) fn is_allowed(&self, id: &NodeId) -> bool {
        crate::membership::allowed(&self.inner.config.read().unwrap(), &self.id(), id, false)
    }

    /// Local name for a peer, else the name it has in one of our networks.
    fn peer_name(&self, id: &NodeId) -> Option<String> {
        let c = self.inner.config.read().unwrap();
        let key = id.to_string();
        c.peer_names.get(&key).cloned().or_else(|| {
            c.networks
                .iter()
                .flat_map(|n| &n.members)
                .find(|m| m.id == key && !m.name.trim().is_empty())
                .map(|m| m.name.clone())
        })
    }

    /// Best human label for a peer: local name, else the name it sent, else short id.
    pub(crate) fn peer_label(&self, id: &NodeId) -> String {
        if let Some(n) = self.peer_name(id) {
            return n;
        }
        let remote = self
            .inner
            .peers
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.remote_name.clone());
        remote.unwrap_or_else(|| id.fmt_short().to_string())
    }

    /// Our display name sent in Hello.
    pub(crate) fn our_name(&self) -> String {
        self.inner
            .config
            .read()
            .unwrap()
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.inner.hostname.clone())
    }

    /// Apply `f` to the config, save it, and store it. Nothing changes if saving fails.
    /// The disk write happens outside the config lock so readers (the accept path, the UI)
    /// never wait on it; `config_edit` keeps concurrent edits from interleaving.
    pub(crate) fn edit_config(&self, f: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        self.edit_config_if(|c| {
            f(c);
            true
        })
        .map(|_| ())
    }

    /// Like `edit_config`, but nothing is saved when `f` returns false. Returns whether it saved.
    pub(crate) fn edit_config_if(
        &self,
        f: impl FnOnce(&mut Config) -> bool,
    ) -> anyhow::Result<bool> {
        let _edit = self.inner.config_edit.lock().unwrap();
        let mut new = self.inner.config.read().unwrap().clone();
        if !f(&mut new) {
            return Ok(false);
        }
        new.save()?;
        *self.inner.config.write().unwrap() = new;
        Ok(true)
    }

    /// Start or stop the Minecraft LAN listener to match `disable_lan_detection`.
    pub(crate) fn sync_lan_listener(&self) {
        // Read the flag under the task lock so racing toggles settle on the latest value.
        let mut task = self.inner.lan_task.lock().unwrap();
        let enabled = !self.inner.config.read().unwrap().disable_lan_detection;
        if enabled {
            // A listener that failed to bind has finished; try again.
            if task.as_ref().is_none_or(|t| t.is_finished()) {
                let n = self.clone();
                *task = Some(tokio::spawn(async move { n.lan_listen_loop().await }).abort_handle());
            }
        } else if let Some(t) = task.take() {
            t.abort();
            drop(task);
            let had_worlds = {
                let mut lan = self.inner.lan.lock().unwrap();
                let had = !lan.is_empty();
                lan.clear();
                had
            };
            if had_worlds {
                self.emit(NodeEvent::LanWorldsChanged(Vec::new()));
            }
        }
    }

    pub(crate) fn set_latency_logging(&self, enabled: bool) {
        self.inner.latency_log.lock().unwrap().set_enabled(enabled);
    }

    pub(crate) fn peer_bytes(&self, id: NodeId) -> Arc<PeerBytes> {
        self.inner
            .peer_bytes
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .clone()
    }

    fn fill_bytes(&self, info: &mut PeerInfo) {
        if let Some(b) = self.inner.peer_bytes.lock().unwrap().get(&info.id) {
            info.bytes_sent = b.sent.load(Ordering::Relaxed);
            info.bytes_received = b.received.load(Ordering::Relaxed);
        }
    }

    /// Mutate a peer entry (creating it if needed) and emit PeerStateChanged if info changed.
    pub(crate) fn update_peer(&self, id: NodeId, f: impl FnOnce(&mut PeerEntry)) {
        self.update_peer_inner(id, true, f)
    }

    /// Like `update_peer` but does nothing if the peer has no entry (e.g. it was removed).
    pub(crate) fn update_existing_peer(&self, id: NodeId, f: impl FnOnce(&mut PeerEntry)) {
        self.update_peer_inner(id, false, f)
    }

    fn update_peer_inner(&self, id: NodeId, create: bool, f: impl FnOnce(&mut PeerEntry)) {
        let name = self.peer_name(&id);
        let changed = {
            let mut peers = self.inner.peers.lock().unwrap();
            let entry = if create {
                peers.entry(id).or_insert_with(|| new_peer_entry(id))
            } else {
                match peers.get_mut(&id) {
                    Some(e) => e,
                    None => return,
                }
            };
            let snapshot = |i: &PeerInfo| {
                (
                    i.state,
                    i.latency_ms,
                    i.stats.total,
                    i.services.len(),
                    i.name.clone(),
                    i.last_error.clone(),
                    i.connected_since,
                    i.inbound,
                )
            };
            let before = snapshot(&entry.info);
            f(entry);
            entry.info.name = name.or_else(|| entry.remote_name.clone());
            let after = snapshot(&entry.info);
            (before != after).then(|| entry.info.clone())
        };
        if let Some(mut info) = changed {
            self.fill_bytes(&mut info);
            self.emit(NodeEvent::PeerStateChanged(info));
        }
    }

    /// Record one ping round trip: update rolling stats, emit a peer event, append to the
    /// latency log if enabled.
    fn record_latency(&self, id: NodeId, rtt_ms: f32) {
        let mut snapshot = None;
        self.update_existing_peer(id, |e| {
            e.info.stats = e.latency.push(rtt_ms);
            e.info.latency_ms = Some(rtt_ms.round() as u32);
            snapshot = Some((e.info.name.clone(), e.info.state));
        });
        if let Some((name, state)) = snapshot {
            self.inner
                .latency_log
                .lock()
                .unwrap()
                .record(&id, name.as_deref(), state, rtt_ms);
        }
    }

    /// Close every connection with `id` (both directions).
    pub(crate) fn disconnect_peer(&self, id: NodeId) {
        if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&id) {
            if let Some(c) = e.dialed.take() {
                c.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
            }
            for c in e.inbound.drain(..) {
                c.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
            }
        }
    }

    /// Services `peer` may use: enabled ones it can see (see `Service::networks`), in config
    /// order. UDP indexes from that peer refer to this list.
    pub(crate) fn services_for(&self, peer: &NodeId) -> Vec<Service> {
        crate::membership::visible_services(&self.inner.config.read().unwrap(), &self.id(), peer)
    }

    /// Port to forward to: a detected Minecraft LAN world's port when exactly one is seen.
    pub(crate) fn effective_port(&self, svc: &Service) -> u16 {
        if svc.minecraft_lan {
            let worlds = self.lan_worlds_now();
            if worlds.len() == 1 {
                return worlds[0].port;
            }
        }
        svc.port
    }

    pub(crate) fn lan_worlds_now(&self) -> Vec<LanWorld> {
        self.inner
            .lan
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, at)| at.elapsed() < LAN_WORLD_TTL)
            .map(|(w, _)| w.clone())
            .collect()
    }

    /// Send the current service list to every connected peer.
    pub(crate) fn push_services(&self) {
        self.inner.svc_gen.fetch_add(1, Ordering::SeqCst);
        let controls: Vec<(NodeId, Vec<ControlSend>)> = self
            .inner
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, e)| (*id, e.controls.iter().map(|(_, s)| s.clone()).collect()))
            .collect();
        for (send, services) in controls.into_iter().flat_map(|(id, sends)| {
            let services = self.services_for(&id);
            sends.into_iter().map(move |s| (s, services.clone()))
        }) {
            let msg = ControlMsg::Services { services };
            tokio::spawn(async move {
                if let Err(e) = write_msg(&mut *send.lock().await, &msg).await {
                    tracing::debug!("pushing services: {e:#}");
                }
            });
        }
    }

    /// A push that ran while a handshake was in flight missed that connection; send it now.
    fn resend_services_if_changed(&self, peer: &NodeId, gen: u64, send: &ControlSend) {
        if self.inner.svc_gen.load(Ordering::SeqCst) != gen {
            let msg = ControlMsg::Services {
                services: self.services_for(peer),
            };
            let send = send.clone();
            tokio::spawn(async move {
                let _ = write_msg(&mut *send.lock().await, &msg).await;
            });
        }
    }

    /// Close all open streams / UDP flows of a hosted service.
    pub(crate) fn close_service_streams(&self, name: &str) {
        if let Some(mut s) = self.inner.svc_live.lock().unwrap().remove(name) {
            for (_, t) in s.streams.drain() {
                t.abort();
            }
        }
    }

    fn drop_tunnel(&self, entry: TunnelEntry) {
        entry.task.abort();
        // Unregister UDP tunnel endpoint if any.
        if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&entry.tunnel.peer) {
            e.udp
                .retain(|_, (f, _)| f.socket.local_addr().ok() != Some(entry.tunnel.local_addr));
        }
        tracing::info!(peer = %entry.tunnel.peer, service = %entry.tunnel.service, "tunnel closed");
        self.emit(NodeEvent::TunnelClosed(entry.tunnel));
    }

    /// Close every tunnel to `peer`.
    pub(crate) fn close_peer_tunnels(&self, peer: NodeId) {
        let removed: Vec<TunnelEntry> = {
            let mut tunnels = self.inner.tunnels.lock().unwrap();
            let (gone, keep) = tunnels.drain(..).partition(|t| t.tunnel.peer == peer);
            *tunnels = keep;
            gone
        };
        for t in removed {
            self.drop_tunnel(t);
        }
    }

    /// Bind the local side of a tunnel. With `info` None (peer unreachable right now) the
    /// service is assumed to use `offline_protocol` and the peer is dialed lazily on the first
    /// local connection (TCP) or packet (UDP).
    pub(crate) async fn open_tunnel_with(
        &self,
        peer: NodeId,
        service: &str,
        local: SocketAddr,
        info: Option<&PeerInfo>,
        offline_protocol: Protocol,
    ) -> anyhow::Result<ActiveTunnel> {
        let svc = match info {
            Some(info) => info
                .services
                .iter()
                .find(|s| s.name == service)
                .cloned()
                .with_context(|| format!("peer does not offer service {service:?}"))?,
            None => Service::new(service, offline_protocol, 0),
        };
        let live = Arc::new(TunnelLive::default());

        let (local_addr, task) =
            match svc.protocol {
                Protocol::Tcp => {
                    let listener = TcpListener::bind(local).await?;
                    let local_addr = listener.local_addr()?;
                    let n = self.clone();
                    let live = live.clone();
                    let task = tokio::spawn(async move {
                        n.tcp_tunnel_loop(listener, peer, svc.name, live).await
                    });
                    (local_addr, task.abort_handle())
                }
                Protocol::Udp => {
                    let socket = Arc::new(UdpSocket::bind(local).await?);
                    let local_addr = socket.local_addr()?;
                    let n = self.clone();
                    let live = live.clone();
                    let task = tokio::spawn(async move {
                        n.udp_tunnel_loop(socket, peer, svc.name, live).await
                    });
                    (local_addr, task.abort_handle())
                }
            };
        let tunnel = ActiveTunnel {
            peer,
            service: service.to_string(),
            local_addr,
        };
        self.inner.tunnels.lock().unwrap().push(TunnelEntry {
            tunnel: tunnel.clone(),
            protocol: svc.protocol,
            live,
            task,
        });
        tracing::info!(%peer, service, %local_addr, "tunnel opened");
        self.emit(NodeEvent::TunnelOpened(tunnel.clone()));
        Ok(tunnel)
    }

    /// Reopen saved tunnels with `auto_open`. Listeners are bound right away; peers that are
    /// online get a short chance to tell us the service protocol first.
    pub(crate) async fn open_saved_tunnels(&self, saved: Vec<SavedTunnel>) {
        let mut set = JoinSet::new();
        for st in saved.into_iter().filter(|s| s.auto_open) {
            let Ok(peer) = st.peer.parse::<NodeId>() else {
                self.emit(NodeEvent::Error(format!(
                    "saved tunnel has an invalid peer id: {}",
                    st.peer
                )));
                continue;
            };
            let n = self.clone();
            set.spawn(async move {
                let info = tokio::time::timeout(Duration::from_secs(3), n.ensure_conn(peer))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|(_, i)| i)
                    .filter(|i| i.services.iter().any(|s| s.name == st.service));
                let local = SocketAddr::from((Ipv4Addr::LOCALHOST, st.local_port));
                match n
                    .open_tunnel_with(peer, &st.service, local, info.as_ref(), st.protocol)
                    .await
                {
                    Ok(t) => n.correct_saved_protocol(&t),
                    Err(e) => n.emit(NodeEvent::Error(format!(
                        "could not reopen {} on port {}: {e:#}",
                        st.service, st.local_port
                    ))),
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    /// Protocol of an open tunnel.
    pub(crate) fn tunnel_protocol(&self, tunnel: &ActiveTunnel) -> Option<Protocol> {
        self.inner
            .tunnels
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.tunnel.local_addr == tunnel.local_addr && t.tunnel.peer == tunnel.peer)
            .map(|t| t.protocol)
    }

    /// The peer told us the real protocol of a saved tunnel (e.g. a config from before
    /// `SavedTunnel::protocol` existed); remember it for the next offline start.
    fn correct_saved_protocol(&self, tunnel: &ActiveTunnel) {
        let Some(protocol) = self.tunnel_protocol(tunnel) else {
            return;
        };
        let key = tunnel.peer.to_string();
        let stale = |s: &SavedTunnel| {
            s.peer == key && s.service == tunnel.service && s.protocol != protocol
        };
        if !self.config().saved_tunnels.iter().any(stale) {
            return;
        }
        let res = self.edit_config(|c| {
            for s in c.saved_tunnels.iter_mut().filter(|s| stale(s)) {
                s.protocol = protocol;
            }
        });
        if let Err(e) = res {
            tracing::warn!("saving tunnel protocol: {e:#}");
        }
    }

    fn dial_lock(&self, peer: NodeId) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .dial_locks
            .lock()
            .unwrap()
            .entry(peer)
            .or_default()
            .clone()
    }

    fn live_dialed(&self, peer: &NodeId) -> Option<(Connection, PeerInfo)> {
        let peers = self.inner.peers.lock().unwrap();
        let e = peers.get(peer)?;
        let c = e.dialed.as_ref()?;
        c.close_reason().is_none().then(|| {
            let mut info = e.info.clone();
            self.fill_bytes(&mut info);
            (c.clone(), info)
        })
    }

    /// `ensure_conn` that also reports failures as `NodeEvent::Error` (user-initiated actions).
    async fn ensure_conn_reporting(&self, peer: NodeId) -> anyhow::Result<(Connection, PeerInfo)> {
        let res = self.ensure_conn(peer).await;
        if let Err(e) = &res {
            self.emit(NodeEvent::Error(format!("{e:#}")));
        }
        res
    }

    /// Return the dialed connection to `peer`, dialing it if needed.
    /// On failure `PeerInfo::last_error` explains why in plain words.
    pub(crate) async fn ensure_conn(&self, peer: NodeId) -> anyhow::Result<(Connection, PeerInfo)> {
        if let Some(r) = self.live_dialed(&peer) {
            return Ok(r);
        }
        let lock = self.dial_lock(peer);
        let _guard = lock.lock().await;
        if let Some(r) = self.live_dialed(&peer) {
            return Ok(r);
        }
        self.update_peer(peer, |e| e.info.state = ConnState::Connecting);
        let res = tokio::time::timeout(DIAL_TIMEOUT, self.dial(peer)).await;
        let err = match res {
            Ok(Ok(r)) => return Ok(r),
            Ok(Err(e)) => e,
            Err(_) => anyhow::anyhow!("timed out"),
        };
        let who = self.peer_label(&peer);
        if err.is::<NotAccepted>() {
            self.owner_rejected(peer);
        }
        let msg = if err.is::<NotAccepted>() {
            format!("Waiting for {who} to accept your request")
        } else if err.to_string() == "timed out" {
            format!("{who} did not answer (timed out). They may be offline.")
        } else {
            format!("{who} is offline or unreachable")
        };
        tracing::info!(%peer, "connect failed: {err:#}");
        let m = msg.clone();
        self.update_peer(peer, |e| {
            if e.inbound.is_empty() {
                e.info.state = ConnState::Disconnected;
            }
            e.dialed = None;
            e.info.last_error = Some(m);
        });
        Err(anyhow::anyhow!(msg))
    }

    async fn dial(&self, peer: NodeId) -> anyhow::Result<(Connection, PeerInfo)> {
        tracing::info!(%peer, "dialing");
        let conn = self.inner.endpoint.connect(peer, ALPN).await?;
        let gen = self.inner.svc_gen.load(Ordering::SeqCst);
        let handshake = async {
            let (mut send, mut recv) = conn.open_bi().await?;
            write_msg(
                &mut send,
                &ControlMsg::Hello {
                    name: Some(self.our_name()),
                },
            )
            .await?;
            write_msg(
                &mut send,
                &ControlMsg::Services {
                    services: self.services_for(&peer),
                },
            )
            .await?;
            let (name, services) = handshake_read(&mut recv).await?;
            anyhow::Ok((send, recv, name, services))
        }
        .await;
        let (send, recv, name, services) = match handshake {
            Ok(x) => x,
            Err(e) => {
                if is_rejection(&conn) {
                    return Err(NotAccepted.into());
                }
                return Err(e);
            }
        };

        let state = path_state(&conn);
        let send = Arc::new(tokio::sync::Mutex::new(send));
        let c = conn.clone();
        let ctl = send.clone();
        self.update_peer(peer, move |e| {
            e.controls.push((c.stable_id(), ctl));
            e.dialed = Some(c);
            e.udp.clear();
            e.info.services = services;
            e.info.state = state;
            e.info.last_error = None;
            e.info.connected_since = Some(SystemTime::now());
            if name.is_some() {
                e.remote_name = name;
            }
        });

        self.resend_services_if_changed(&peer, gen, &send);
        self.send_network_sync(peer, &send);
        let n = self.clone();
        let c = conn.clone();
        self.spawn(async move { n.run_connection(c, send, recv, true).await });

        let mut info = self.inner.peers.lock().unwrap()[&peer].info.clone();
        self.fill_bytes(&mut info);
        tracing::info!(%peer, "connected");
        Ok((conn, info))
    }

    // ---- auto-reconnect ----

    /// Start reconnect loops for allowed peers and stop loops of peers no longer allowed.
    pub(crate) fn sync_dialers(&self) {
        let allowed = self.dial_set();
        let stale: Vec<NodeId> = self
            .inner
            .dialers
            .lock()
            .unwrap()
            .keys()
            .filter(|id| !allowed.contains(*id))
            .copied()
            .collect();
        for id in stale {
            self.stop_dialer(id);
        }
        for id in allowed {
            self.start_dialer(id);
        }
    }

    pub(crate) fn start_dialer(&self, peer: NodeId) {
        let mut dialers = self.inner.dialers.lock().unwrap();
        if dialers.contains_key(&peer) {
            return;
        }
        let wake = Arc::new(Notify::new());
        let n = self.clone();
        let w = wake.clone();
        let task = tokio::spawn(async move { n.dial_loop(peer, w).await });
        dialers.insert(peer, (task.abort_handle(), wake));
    }

    pub(crate) fn stop_dialer(&self, peer: NodeId) {
        if let Some((task, _)) = self.inner.dialers.lock().unwrap().remove(&peer) {
            task.abort();
        }
    }

    /// Make a reconnect loop retry now with its backoff reset.
    pub(crate) fn wake_dialer(&self, peer: NodeId) {
        if let Some((_, wake)) = self.inner.dialers.lock().unwrap().get(&peer) {
            wake.notify_one();
        }
    }

    fn wake_all_dialers(&self) {
        for (_, wake) in self.inner.dialers.lock().unwrap().values() {
            wake.notify_one();
        }
    }

    async fn dial_loop(self, peer: NodeId, wake: Arc<Notify>) {
        let mut backoff = BACKOFF_MIN;
        loop {
            if let Some((conn, _)) = self.live_dialed(&peer) {
                tokio::select! {
                    _ = conn.closed() => {
                        // Avoid a hot loop if connections keep dropping right away.
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    _ = wake.notified() => {}
                }
                backoff = BACKOFF_MIN;
                continue;
            }
            match self.ensure_conn(peer).await {
                Ok(_) => backoff = BACKOFF_MIN,
                Err(_) => {
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                        }
                        _ = wake.notified() => backoff = BACKOFF_MIN,
                    }
                }
            }
        }
    }

    // ---- incoming ----

    async fn accept_loop(self) {
        while let Some(incoming) = self.inner.endpoint.accept().await {
            let n = self.clone();
            tokio::spawn(async move {
                if let Err(e) = n.handle_incoming(incoming).await {
                    tracing::warn!("incoming connection failed: {e:#}");
                }
            });
        }
    }

    async fn handle_incoming(self, incoming: iroh::endpoint::Incoming) -> anyhow::Result<()> {
        let conn = incoming.accept()?.await?;
        let peer = conn.remote_id();
        let allowed = self.is_allowed(&peer);
        // Peers that are not allowed only get to send one message: a JoinRequest is handled,
        // anything else is turned away. Strangers only get in through a network.
        let limit = if allowed {
            HANDSHAKE_TIMEOUT
        } else {
            HELLO_TIMEOUT
        };
        let first = tokio::time::timeout(limit, async {
            let (send, mut recv) = conn.accept_bi().await?;
            let first: ControlMsg = read_msg(&mut recv).await?;
            anyhow::Ok((send, recv, first))
        })
        .await;
        let (mut send, mut recv, first) = match first {
            Ok(Ok(x)) => x,
            _ if !allowed => {
                conn.close(CLOSE_PENDING.into(), b"pending");
                return Ok(());
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => bail!("handshake timed out"),
        };
        let name = match first {
            ControlMsg::JoinRequest {
                network,
                token,
                name,
            } => {
                self.handle_join(conn, send, network, token, name).await;
                return Ok(());
            }
            ControlMsg::Hello { name } if allowed => sanitize_name(name),
            other if allowed => bail!("expected Hello, got {other:?}"),
            _ => {
                tracing::debug!(%peer, "turned away unknown peer");
                conn.close(CLOSE_PENDING.into(), b"pending");
                return Ok(());
            }
        };
        tracing::info!(%peer, "accepted connection");
        let gen = self.inner.svc_gen.load(Ordering::SeqCst);
        let services = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let services = read_services(&mut recv).await?;
            write_msg(
                &mut send,
                &ControlMsg::Hello {
                    name: Some(self.our_name()),
                },
            )
            .await?;
            write_msg(
                &mut send,
                &ControlMsg::Services {
                    services: self.services_for(&peer),
                },
            )
            .await?;
            anyhow::Ok(services)
        })
        .await
        .context("handshake timed out")??;

        let state = path_state(&conn);
        let send = Arc::new(tokio::sync::Mutex::new(send));
        let c = conn.clone();
        let ctl = send.clone();
        self.update_peer(peer, |e| {
            e.controls.push((c.stable_id(), ctl));
            e.inbound.push(c);
            e.info.inbound = true;
            e.info.services = services;
            if e.dialed.is_none() {
                e.info.state = state;
            }
            if name.is_some() {
                e.remote_name = name;
            }
        });
        self.resend_services_if_changed(&peer, gen, &send);
        self.send_network_sync(peer, &send);
        // They can reach us, so our own dial to them will likely work now too.
        if self.live_dialed(&peer).is_none() {
            self.wake_dialer(peer);
        }
        let n = self.clone();
        self.spawn(async move { n.run_connection(conn, send, recv, false).await });
        Ok(())
    }

    /// Drive one established connection until it closes.
    async fn run_connection(
        self,
        conn: Connection,
        send: ControlSend,
        mut recv: RecvStream,
        dialer: bool,
    ) {
        let peer = conn.remote_id();
        let mut set: JoinSet<()> = JoinSet::new();

        // Control stream reader: answer pings, record pong latency.
        // Messages it cannot parse are skipped, so a newer peer cannot stall the stream.
        {
            let n = self.clone();
            let send = send.clone();
            set.spawn(async move {
                loop {
                    let frame = match read_frame(&mut recv).await {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::debug!(%peer, "control stream ended: {e:#}");
                            break;
                        }
                    };
                    let msg: ControlMsg = match serde_json::from_slice(&frame) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::debug!(%peer, "skipping unreadable control message: {e}");
                            continue;
                        }
                    };
                    match msg {
                        ControlMsg::Ping { t } => {
                            if write_msg(&mut *send.lock().await, &ControlMsg::Pong { t })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        ControlMsg::Pong { t } => {
                            let now = n.inner.epoch.elapsed().as_micros() as u64;
                            let rtt_ms = now.saturating_sub(t) as f32 / 1000.0;
                            n.record_latency(peer, rtt_ms);
                        }
                        ControlMsg::Services { services } => {
                            n.update_existing_peer(peer, |e| e.info.services = services)
                        }
                        ControlMsg::NetworkState { network } => {
                            n.apply_network_state(peer, network)
                        }
                        ControlMsg::Networks { networks } => n.apply_network_sync(peer, networks),
                        ControlMsg::NetworkLeave { network } => {
                            let n = n.clone();
                            tokio::spawn(async move { n.handle_leave(peer, &network).await });
                        }
                        // Only valid as the first message of a connection.
                        ControlMsg::Hello { .. }
                        | ControlMsg::JoinRequest { .. }
                        | ControlMsg::JoinResponse { .. } => {}
                        ControlMsg::Unknown => {
                            tracing::debug!(%peer, "ignoring unknown control message");
                        }
                    }
                }
            });
        }

        // Pinger (dialer only).
        if dialer {
            let n = self.clone();
            let send = send.clone();
            set.spawn(async move {
                let mut tick = tokio::time::interval(PING_INTERVAL);
                loop {
                    tick.tick().await;
                    let t = n.inner.epoch.elapsed().as_micros() as u64;
                    if write_msg(&mut *send.lock().await, &ControlMsg::Ping { t })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }

        // Path watcher: Direct vs Relayed.
        {
            let n = self.clone();
            let conn = conn.clone();
            set.spawn(async move {
                let mut tick = tokio::time::interval(PATH_POLL_INTERVAL);
                loop {
                    tick.tick().await;
                    let state = path_state(&conn);
                    n.update_existing_peer(peer, |e| {
                        let primary = match &e.dialed {
                            Some(d) => d.stable_id() == conn.stable_id(),
                            None => true,
                        };
                        if primary && e.info.state != ConnState::Disconnected {
                            e.info.state = state;
                        }
                    });
                }
            });
        }

        if dialer {
            // Client: datagrams are replies for our UDP tunnels.
            let n = self.clone();
            let conn = conn.clone();
            set.spawn(async move {
                let bytes = n.peer_bytes(peer);
                let mut reassembler = Reassembler::default();
                while let Ok(d) = conn.read_datagram().await {
                    let Some((h, payload)) = decode_datagram(&d) else {
                        continue;
                    };
                    let flow = n
                        .inner
                        .peers
                        .lock()
                        .unwrap()
                        .get(&peer)
                        .and_then(|e| e.udp.get(&h.flow).cloned());
                    let Some((flow, live)) = flow else {
                        continue;
                    };
                    let Some(payload) = reassembler.accept(&h, payload) else {
                        continue;
                    };
                    let len = payload.len() as u64;
                    live.down.fetch_add(len, Ordering::Relaxed);
                    flow.last_ms.store(n.now_ms(), Ordering::Relaxed);
                    bytes.received.fetch_add(len, Ordering::Relaxed);
                    flow.deliver(&payload).await;
                }
            });
        } else {
            // Host: incoming data streams.
            let n = self.clone();
            let c = conn.clone();
            set.spawn(async move {
                while let Ok((send, recv)) = c.accept_bi().await {
                    let n = n.clone();
                    tokio::spawn(async move {
                        if let Err(e) = n.host_stream(peer, send, recv).await {
                            tracing::debug!(%peer, "tcp stream ended: {e:#}");
                        }
                    });
                }
            });
            // Host: datagrams to local UDP services.
            let n = self.clone();
            let c = conn.clone();
            set.spawn(async move { n.host_datagrams(c).await });
        }

        let reason = conn.closed().await;
        set.abort_all();
        tracing::info!(%peer, "connection closed: {reason}");
        let id = conn.stable_id();
        self.update_existing_peer(peer, |e| {
            e.controls.retain(|(s, _)| *s != id);
            if dialer {
                if e.dialed.as_ref().map(|d| d.stable_id()) == Some(id) {
                    e.dialed = None;
                    e.udp.clear();
                    e.info.connected_since = None;
                }
            } else {
                e.inbound.retain(|c| c.stable_id() != id);
                e.info.inbound = !e.inbound.is_empty();
            }
            if e.dialed.is_none() {
                e.info.state = if e.inbound.is_empty() {
                    ConnState::Disconnected
                } else {
                    path_state(&e.inbound[0])
                };
                e.info.latency_ms = None;
                e.info.stats = LatencyStats::default();
                e.latency.clear();
            }
        });
    }

    fn now_ms(&self) -> u64 {
        self.inner.epoch.elapsed().as_millis() as u64
    }

    /// Register a live stream or UDP flow of a hosted service. Returns its id.
    fn svc_open(&self, service: &str, peer: NodeId, task: Option<AbortHandle>) -> SvcGuard {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut live = self.inner.svc_live.lock().unwrap();
        let s = live.entry(service.to_string()).or_default();
        s.conns += 1;
        *s.peers.entry(peer).or_default() += 1;
        if let Some(t) = task {
            s.streams.insert(id, t);
        }
        SvcGuard {
            node: self.clone(),
            service: service.to_string(),
            peer,
            id,
        }
    }

    /// Host side: one incoming data stream. Reads the header and connects to the local service.
    async fn host_stream(
        &self,
        peer: NodeId,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> anyhow::Result<()> {
        let header: StreamHeader = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_msg(&mut recv))
            .await
            .context("stream header timed out")??;
        let svc = self
            .services_for(&peer)
            .into_iter()
            .find(|s| s.name == header.service && s.protocol == Protocol::Tcp);
        let Some(svc) = svc else {
            tracing::warn!(%peer, service = %header.service, "refusing stream for unknown or disabled service");
            let _ = send.reset(RESET_UNKNOWN_SERVICE.into());
            return Ok(());
        };
        let addr = svc.local_addr(self.effective_port(&svc));
        let tcp = match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await
        {
            Ok(Ok(t)) => t,
            res => {
                let why = match res {
                    Ok(Err(e)) => e.to_string(),
                    _ => "timed out".to_string(),
                };
                tracing::warn!(%peer, service = %svc.name, "cannot connect to local service at {addr}: {why}");
                // Both directions carry the code, whichever the client notices first.
                let _ = send.reset(RESET_NOT_LISTENING.into());
                let _ = recv.stop(RESET_NOT_LISTENING.into());
                return Ok(());
            }
        };
        let bytes = self.peer_bytes(peer);
        let out: Counters = vec![bytes.sent.clone()];
        let inn: Counters = vec![bytes.received.clone()];
        // Start only once the abort handle is registered, so removing the service can close it.
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<SvcGuard>();
        let task = tokio::spawn(async move {
            let Ok(_guard) = go_rx.await else { return };
            if let Err(e) = forward::copy_counted(tcp, send, recv, &out, &inn).await {
                tracing::debug!("host stream: {e:#}");
            }
        });
        let guard = self.svc_open(&svc.name, peer, Some(task.abort_handle()));
        let _ = go_tx.send(guard);
        let _ = task.await;
        Ok(())
    }

    /// Host side: relay datagrams to local UDP services, one local socket per
    /// (service, flow), so the service sees each of the client's local senders separately.
    async fn host_datagrams(&self, conn: Connection) {
        let peer = conn.remote_id();
        let bytes = self.peer_bytes(peer);
        let mut gen = self.inner.svc_gen.load(Ordering::Relaxed);
        let mut flows: HashMap<(u16, u32), HostFlow> = HashMap::new();
        let mut reassembler = Reassembler::default();
        let mut sweep = tokio::time::interval(UDP_FLOW_SWEEP);
        loop {
            let d = tokio::select! {
                _ = sweep.tick() => {
                    let now = self.now_ms();
                    flows.retain(|_, f| !f.task.is_finished() && !f.idle(now));
                    continue;
                }
                d = conn.read_datagram() => match d {
                    Ok(d) => d,
                    Err(_) => break,
                },
            };
            let Some((h, payload)) = decode_datagram(&d) else {
                continue;
            };
            let g = self.inner.svc_gen.load(Ordering::Relaxed);
            if g != gen {
                // Service list changed: indexes may now point elsewhere.
                gen = g;
                flows.clear();
            }
            let Some(payload) = reassembler.accept(&h, payload) else {
                continue;
            };
            let key = (h.service, h.flow);
            if flows.get(&key).is_some_and(|f| f.task.is_finished()) {
                flows.remove(&key);
            }
            let flow = match flows.get(&key) {
                Some(f) => f,
                None => {
                    let svc = self.services_for(&peer).get(h.service as usize).cloned();
                    let Some(svc) = svc.filter(|s| s.protocol == Protocol::Udp) else {
                        tracing::debug!(%peer, idx = h.service, "datagram for unknown udp service");
                        continue;
                    };
                    let addr = svc.local_addr(svc.port);
                    match self.host_udp_flow(conn.clone(), h, addr).await {
                        Ok(mut f) => {
                            f._guard = Some(self.svc_open(&svc.name, peer, Some(f.task.clone())));
                            flows.entry(key).or_insert(f)
                        }
                        Err(e) => {
                            tracing::warn!("udp socket for {} at {addr}: {e:#}", svc.name);
                            continue;
                        }
                    }
                }
            };
            flow.last_ms.store(self.now_ms(), Ordering::Relaxed);
            bytes
                .received
                .fetch_add(payload.len() as u64, Ordering::Relaxed);
            if let Err(e) = flow.sock.send(&payload).await {
                tracing::debug!("udp send to local service: {e}");
            }
        }
    }

    /// A UDP socket "connected" to the local service at `addr` for one client flow, relaying
    /// replies back as datagrams tagged with the flow.
    async fn host_udp_flow(
        &self,
        conn: Connection,
        h: DatagramHeader,
        addr: SocketAddr,
    ) -> anyhow::Result<HostFlow> {
        let bind: SocketAddr = match addr.ip() {
            ip if ip.is_loopback() => (ip, 0).into(),
            IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let sock = UdpSocket::bind(bind).await?;
        sock.connect(addr).await?;
        let sock = Arc::new(sock);
        let reader = sock.clone();
        let sent = self.peer_bytes(conn.remote_id()).sent.clone();
        let last_ms = Arc::new(AtomicU64::new(self.now_ms()));
        let last = last_ms.clone();
        let n = self.clone();
        let task = tokio::spawn(async move {
            let sender = FlowSender::default();
            let mut buf = vec![0u8; UDP_BUF];
            loop {
                match reader.recv(&mut buf).await {
                    Ok(len) => {
                        sent.fetch_add(len as u64, Ordering::Relaxed);
                        last.store(n.now_ms(), Ordering::Relaxed);
                        sender.send(&conn, h.service, h.flow, &buf[..len]);
                        if conn.close_reason().is_some() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("host udp recv: {e}");
                        break;
                    }
                }
            }
        });
        Ok(HostFlow {
            sock,
            task: task.abort_handle(),
            last_ms,
            _guard: None,
        })
    }

    async fn tcp_tunnel_loop(
        self,
        listener: TcpListener,
        peer: NodeId,
        service: String,
        live: Arc<TunnelLive>,
    ) {
        let mut conns = JoinSet::new();
        let bytes = self.peer_bytes(peer);
        loop {
            let (tcp, from) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("tunnel accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let n = self.clone();
            let service = service.clone();
            let out: Counters = vec![live.up.clone(), bytes.sent.clone()];
            let inn: Counters = vec![live.down.clone(), bytes.received.clone()];
            live.conns.fetch_add(1, Ordering::Relaxed);
            let guard = TunnelConnGuard(live.clone());
            let live = live.clone();
            conns.spawn(async move {
                let _guard = guard;
                // Where the host dials, as advertised by it (for the error message).
                let mut target = None;
                let res = async {
                    let (conn, info) = n.ensure_conn(peer).await?;
                    target = info
                        .services
                        .iter()
                        .find(|s| s.name == service)
                        .map(|s| s.local_addr(s.port));
                    let (mut send, recv) = conn.open_bi().await?;
                    let header = StreamHeader {
                        service: service.clone(),
                    };
                    write_msg(&mut send, &header).await?;
                    forward::copy_counted(tcp, send, recv, &out, &inn).await
                }
                .await;
                if let Err(e) = res {
                    tracing::debug!(%from, "tunnel connection ended: {e:#}");
                    if stream_error_code(&e) == Some(RESET_NOT_LISTENING.into()) {
                        n.report_not_listening(peer, &service, target, &live);
                    }
                }
            });
            while conns.try_join_next().is_some() {}
        }
    }

    async fn udp_tunnel_loop(
        self,
        socket: Arc<UdpSocket>,
        peer: NodeId,
        service: String,
        live: Arc<TunnelLive>,
    ) {
        let mut buf = vec![0u8; UDP_BUF];
        let bytes = self.peer_bytes(peer);
        // One flow per local sender, so replies reach the program that asked.
        let mut flows: HashMap<SocketAddr, Arc<ClientFlow>> = HashMap::new();
        let mut sweep = tokio::time::interval(UDP_FLOW_SWEEP);
        loop {
            let (len, from) = tokio::select! {
                _ = sweep.tick() => {
                    self.expire_udp_flows(peer, &mut flows, &live);
                    continue;
                }
                res = socket.recv_from(&mut buf) => match res {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!("udp tunnel recv: {e}");
                        continue;
                    }
                },
            };
            let flow = flows
                .entry(from)
                .or_insert_with(|| {
                    Arc::new(ClientFlow {
                        id: self.inner.next_id.fetch_add(1, Ordering::Relaxed) as u32,
                        local: from,
                        socket: socket.clone(),
                        sender: FlowSender::default(),
                        last_ms: AtomicU64::new(0),
                    })
                })
                .clone();
            flow.last_ms.store(self.now_ms(), Ordering::Relaxed);
            live.conns.store(flows.len() as u32, Ordering::Relaxed);
            let (conn, info) = match self.ensure_conn(peer).await {
                Ok(x) => x,
                Err(_) => continue,
            };
            // The index may change when the host's service list changes; resolve by name.
            let Some(index) = info
                .services
                .iter()
                .position(|s| s.name == service && s.protocol == Protocol::Udp)
                .and_then(|i| u16::try_from(i).ok())
            else {
                continue;
            };
            // Registered per connection: a new dialed connection starts with none.
            if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&peer) {
                e.udp
                    .entry(flow.id)
                    .or_insert_with(|| (flow.clone(), live.clone()));
            }
            live.up.fetch_add(len as u64, Ordering::Relaxed);
            bytes.sent.fetch_add(len as u64, Ordering::Relaxed);
            flow.sender.send(&conn, index, flow.id, &buf[..len]);
        }
    }

    /// The host has nothing listening behind a tunnel: tell the user, at most once a minute
    /// per tunnel.
    fn report_not_listening(
        &self,
        peer: NodeId,
        service: &str,
        target: Option<SocketAddr>,
        live: &TunnelLive,
    ) {
        let now = self.now_ms().max(1);
        let last = live.last_error_ms.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < NOT_LISTENING_REPORT_GAP.as_millis() as u64 {
            return;
        }
        live.last_error_ms.store(now, Ordering::Relaxed);
        let who = self.peer_label(&peer);
        let msg = match target {
            Some(addr) => format!("Nothing is listening on {addr} on {who}'s computer"),
            None => format!("Nothing is listening behind {service} on {who}'s computer"),
        };
        tracing::warn!(%peer, service, "{msg}");
        self.emit(NodeEvent::Error(msg));
    }

    /// Forget client UDP flows that saw no traffic for [`UDP_FLOW_IDLE`].
    fn expire_udp_flows(
        &self,
        peer: NodeId,
        flows: &mut HashMap<SocketAddr, Arc<ClientFlow>>,
        live: &TunnelLive,
    ) {
        let now = self.now_ms();
        let idle = UDP_FLOW_IDLE.as_millis() as u64;
        let before = flows.len();
        let mut gone = Vec::new();
        flows.retain(|_, f| {
            let keep = now.saturating_sub(f.last_ms.load(Ordering::Relaxed)) < idle;
            if !keep {
                gone.push(f.id);
            }
            keep
        });
        if flows.len() != before {
            if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&peer) {
                for id in gone {
                    e.udp.remove(&id);
                }
            }
            live.conns.store(flows.len() as u32, Ordering::Relaxed);
        }
    }

    /// Live connection count of a tunnel (UDP: local senders seen recently).
    pub(crate) fn tunnel_conns(&self, t: &TunnelEntry) -> u32 {
        t.live.conns.load(Ordering::Relaxed)
    }

    // ---- background loops ----

    /// Track our relay connection and direct addresses.
    async fn network_loop(self) {
        let mut relays = self.inner.endpoint.home_relay_status();
        let mut addrs = self.inner.endpoint.watch_addr();
        let mut first = true;
        let mut tick = tokio::time::interval(NETWORK_POLL_INTERVAL);
        loop {
            tick.tick().await;
            let relays = relays.get();
            let addr = addrs.get();
            let home = relays
                .iter()
                .find(|r| r.is_connected())
                .or(relays.first())
                .map(|r| r.url().to_string());
            let mut direct: Vec<SocketAddr> = addr.ip_addrs().copied().collect();
            direct.sort();
            let status = NetworkStatus {
                online: relays.iter().any(|r| r.is_connected()),
                home_relay: home,
                direct_addrs: direct,
            };
            let changed = {
                let mut cur = self.inner.network.lock().unwrap();
                let changed = cur.online != status.online
                    || cur.home_relay != status.home_relay
                    || cur.direct_addrs != status.direct_addrs;
                if changed {
                    *cur = status.clone();
                }
                changed
            };
            if changed {
                tracing::info!(online = status.online, relay = ?status.home_relay, "network changed");
                self.emit(NodeEvent::NetworkChanged(status));
                if !first {
                    self.wake_all_dialers();
                }
            }
            first = false;
        }
    }

    /// Probe whether TCP services are listening locally.
    async fn probe_loop(self) {
        let mut tick = tokio::time::interval(PROBE_INTERVAL);
        loop {
            tick.tick().await;
            let services: Vec<Service> = self.config().services;
            for svc in services {
                let reachable = match svc.protocol {
                    Protocol::Udp => None,
                    Protocol::Tcp => {
                        let addr = svc.local_addr(self.effective_port(&svc));
                        let res =
                            tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await;
                        Some(matches!(res, Ok(Ok(_))))
                    }
                };
                self.inner
                    .svc_live
                    .lock()
                    .unwrap()
                    .entry(svc.name.clone())
                    .or_default()
                    .reachable = reachable;
            }
        }
    }

    /// Listen for Minecraft "Open to LAN" announcements on this machine.
    async fn lan_listen_loop(self) {
        let sock = match lan::bind_listener() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Minecraft LAN detection disabled: cannot listen on port {}: {e}",
                    lan::LAN_PORT
                );
                return;
            }
        };
        let mut buf = vec![0u8; 2048];
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            let before = self.lan_key();
            tokio::select! {
                res = sock.recv_from(&mut buf) => {
                    let Ok((n, _from)) = res else { continue };
                    let msg = String::from_utf8_lossy(&buf[..n]);
                    let Some((motd, port)) = lan::parse_announcement(&msg) else { continue };
                    if motd.contains(lan::OUR_MARKER) {
                        continue;
                    }
                    let mut lan = self.inner.lan.lock().unwrap();
                    let world = LanWorld { motd: motd.clone(), port, last_seen: SystemTime::now() };
                    match lan.iter_mut().find(|(w, _)| w.motd == motd && w.port == port) {
                        Some(e) => *e = (world, Instant::now()),
                        None => lan.push((world, Instant::now())),
                    }
                }
                _ = tick.tick() => {
                    self.inner.lan.lock().unwrap().retain(|(_, at)| at.elapsed() < LAN_WORLD_TTL);
                }
            }
            if self.lan_key() != before {
                let worlds = self.lan_worlds_now();
                tracing::info!(count = worlds.len(), "Minecraft LAN worlds changed");
                self.emit(NodeEvent::LanWorldsChanged(worlds));
            }
        }
    }

    fn lan_key(&self) -> Vec<(String, u16)> {
        let mut v: Vec<(String, u16)> = self
            .inner
            .lan
            .lock()
            .unwrap()
            .iter()
            .map(|(w, _)| (w.motd.clone(), w.port))
            .collect();
        v.sort();
        v
    }

    /// Announce open tunnels to Minecraft servers as LAN worlds on this machine.
    async fn announce_loop(self) {
        let mut sock: Option<UdpSocket> = None;
        let mut tick = tokio::time::interval(ANNOUNCE_INTERVAL);
        loop {
            tick.tick().await;
            let targets: Vec<(NodeId, String, u16)> = {
                let tunnels = self.inner.tunnels.lock().unwrap();
                let peers = self.inner.peers.lock().unwrap();
                tunnels
                    .iter()
                    .filter(|t| {
                        peers.get(&t.tunnel.peer).is_some_and(|e| {
                            e.info
                                .services
                                .iter()
                                .any(|s| s.name == t.tunnel.service && s.minecraft_lan)
                        })
                    })
                    .map(|t| {
                        (
                            t.tunnel.peer,
                            t.tunnel.service.clone(),
                            t.tunnel.local_addr.port(),
                        )
                    })
                    .collect()
            };
            if targets.is_empty() {
                continue;
            }
            if sock.is_none() {
                match lan::bind_announcer() {
                    Ok(s) => sock = Some(s),
                    Err(e) => {
                        tracing::warn!("cannot announce Minecraft LAN worlds: {e}");
                        continue;
                    }
                }
            }
            let Some(s) = &sock else { continue };
            for (peer, service, port) in targets {
                let motd = format!(
                    "{} - {} {}",
                    self.peer_label(&peer),
                    service,
                    lan::OUR_MARKER
                );
                let msg = lan::format_announcement(&motd, port);
                if let Err(e) = s.send_to(msg.as_bytes(), lan::group_addr()).await {
                    tracing::debug!("LAN announce: {e}");
                }
            }
        }
    }
}

/// Read the peer's Hello and Services messages from the control stream.
async fn handshake_read(recv: &mut RecvStream) -> anyhow::Result<(Option<String>, Vec<Service>)> {
    let name = match read_msg(recv).await? {
        ControlMsg::Hello { name } => sanitize_name(name),
        other => bail!("expected Hello, got {other:?}"),
    };
    Ok((name, read_services(recv).await?))
}

async fn read_services(recv: &mut RecvStream) -> anyhow::Result<Vec<Service>> {
    match read_msg(recv).await? {
        ControlMsg::Services { services } => Ok(services),
        other => bail!("expected Services, got {other:?}"),
    }
}

/// The application error code a data stream failed with: the peer reset our receive side or
/// stopped our send side.
fn stream_error_code(e: &anyhow::Error) -> Option<VarInt> {
    let inner = e.downcast_ref::<std::io::Error>()?.get_ref()?;
    if let Some(ReadError::Reset(code)) = inner.downcast_ref::<ReadError>() {
        return Some(*code);
    }
    if let Some(WriteError::Stopped(code)) = inner.downcast_ref::<WriteError>() {
        return Some(*code);
    }
    None
}
