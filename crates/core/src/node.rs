use crate::forward::{self, UdpClient};
use crate::protocol::{decode_datagram, encode_datagram, read_msg, write_msg, ControlMsg};
use crate::stats::{LatencyLog, LatencyStats, LatencyWindow};
use crate::NodeId;
use crate::{ActiveTunnel, Config, Protocol, Service, ALPN};
use anyhow::{bail, Context};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::broadcast;
use tokio::task::{AbortHandle, JoinSet};

/// 1s pings give a useful latency history at negligible cost (a few bytes per second).
const PING_INTERVAL: Duration = Duration::from_secs(1);
const PATH_POLL_INTERVAL: Duration = Duration::from_secs(1);
const EVENT_CAPACITY: usize = 64;
const CLOSE_NOT_ALLOWED: u32 = 1;
const CLOSE_SHUTDOWN: u32 = 2;

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
}

/// The running lanlink node. Cheap to clone (Arc inside).
#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
}

struct PeerEntry {
    info: PeerInfo,
    /// Connection we dialed (used for tunnels).
    dialed: Option<Connection>,
    /// Client UDP tunnels on the dialed connection, keyed by remote service index.
    udp: HashMap<u16, UdpClient>,
    /// Recent ping samples backing `info.stats`.
    latency: LatencyWindow,
}

struct TunnelEntry {
    tunnel: ActiveTunnel,
    task: AbortHandle,
}

struct Inner {
    endpoint: Endpoint,
    lookup: MemoryLookup,
    config: RwLock<Config>,
    events: broadcast::Sender<NodeEvent>,
    peers: Mutex<HashMap<NodeId, PeerEntry>>,
    tunnels: Mutex<Vec<TunnelEntry>>,
    tasks: Mutex<Vec<AbortHandle>>,
    dial_lock: tokio::sync::Mutex<()>,
    epoch: Instant,
    latency_log: Mutex<LatencyLog>,
}

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

impl Node {
    /// Load or create identity, bind iroh endpoint, start accept loop. Non-blocking.
    pub async fn start(config: Config) -> anyhow::Result<Node> {
        let key = load_or_create_secret_key(&Config::dir().join("identity.key"))?;
        Self::start_with_secret_key(config, key).await
    }

    /// Like [`Node::start`] but with an explicit identity (does not touch the config dir).
    pub async fn start_with_secret_key(
        config: Config,
        secret_key: SecretKey,
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

        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            endpoint,
            lookup,
            config: RwLock::new(config),
            events,
            peers: Mutex::new(HashMap::new()),
            tunnels: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            dial_lock: tokio::sync::Mutex::new(()),
            epoch: Instant::now(),
            latency_log: Mutex::new(LatencyLog::open(&Config::dir().join("latency.csv"))),
        });
        let node = Node { inner };
        let n = node.clone();
        node.spawn(async move { n.accept_loop().await });
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

    /// Register known addresses for a peer, bypassing discovery.
    pub fn add_peer_addr(&self, addr: EndpointAddr) {
        self.inner.lookup.add_endpoint_info(addr);
    }

    pub fn config(&self) -> Config {
        self.inner.config.read().unwrap().clone()
    }

    /// Persist and apply a new config (allowlist, services) at runtime.
    pub async fn update_config(&self, config: Config) -> anyhow::Result<()> {
        config.save()?;
        let allowed = config.allowed_peers.clone();
        *self.inner.config.write().unwrap() = config;
        // Drop connections from peers that are no longer allowed.
        let peers: Vec<NodeId> = self.inner.peers.lock().unwrap().keys().copied().collect();
        for id in peers {
            if !allowed.iter().any(|a| a == &id.to_string()) {
                self.disconnect_peer(id);
            }
        }
        Ok(())
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
        let config = self.config();
        for s in &config.allowed_peers {
            if let Ok(id) = s.parse::<NodeId>() {
                map.entry(id).or_insert_with(|| PeerInfo {
                    id,
                    name: config.peer_names.get(s).cloned(),
                    state: ConnState::Disconnected,
                    latency_ms: None,
                    stats: LatencyStats::default(),
                    services: Vec::new(),
                    last_error: None,
                    connected_since: None,
                    inbound: false,
                    bytes_sent: 0,
                    bytes_received: 0,
                });
            }
        }
        let mut v: Vec<PeerInfo> = map.into_values().collect();
        v.sort_by_key(|p| p.id.to_string());
        v
    }

    /// Connect to a peer and fetch its service list. Idempotent.
    pub async fn connect(&self, peer: NodeId) -> anyhow::Result<PeerInfo> {
        Ok(self.ensure_conn(peer).await?.1)
    }

    /// Open a local listener on `local` (port 0 = any) that forwards to `service` on `peer`.
    pub async fn open_tunnel(
        &self,
        peer: NodeId,
        service: &str,
        local: SocketAddr,
    ) -> anyhow::Result<ActiveTunnel> {
        let (_, info) = self.ensure_conn(peer).await?;
        let (index, svc) = info
            .services
            .iter()
            .enumerate()
            .find(|(_, s)| s.name == service)
            .with_context(|| format!("peer does not offer service {service:?}"))?;
        let index = u16::try_from(index)?;
        let svc = svc.clone();

        let (local_addr, task) = match svc.protocol {
            Protocol::Tcp => {
                let listener = TcpListener::bind(local).await?;
                let local_addr = listener.local_addr()?;
                let n = self.clone();
                let task =
                    tokio::spawn(async move { n.tcp_tunnel_loop(listener, peer, svc.name).await });
                (local_addr, task.abort_handle())
            }
            Protocol::Udp => {
                let socket = Arc::new(UdpSocket::bind(local).await?);
                let local_addr = socket.local_addr()?;
                let client = UdpClient {
                    socket,
                    last_sender: Arc::new(Mutex::new(None)),
                };
                if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&peer) {
                    e.udp.insert(index, client.clone());
                }
                let n = self.clone();
                let task =
                    tokio::spawn(async move { n.udp_tunnel_loop(client, peer, index).await });
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
            task,
        });
        tracing::info!(%peer, service, %local_addr, "tunnel opened");
        self.emit(NodeEvent::TunnelOpened(tunnel.clone()));
        Ok(tunnel)
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
        entry.task.abort();
        // Unregister UDP tunnel endpoint if any.
        if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&tunnel.peer) {
            e.udp
                .retain(|_, c| c.socket.local_addr().ok() != Some(tunnel.local_addr));
        }
        self.emit(NodeEvent::TunnelClosed(entry.tunnel));
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
        for t in self.inner.tunnels.lock().unwrap().drain(..) {
            t.task.abort();
        }
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        for (_, e) in self.inner.peers.lock().unwrap().drain() {
            if let Some(c) = e.dialed {
                c.close(CLOSE_SHUTDOWN.into(), b"shutdown");
            }
        }
        self.inner.endpoint.close().await;
        Ok(())
    }

    // ---- internals ----

    fn emit(&self, ev: NodeEvent) {
        let _ = self.inner.events.send(ev);
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.inner.tasks.lock().unwrap();
        tasks.retain(|t| !t.is_finished());
        tasks.push(tokio::spawn(fut).abort_handle());
    }

    fn is_allowed(&self, id: &NodeId) -> bool {
        let s = id.to_string();
        self.inner
            .config
            .read()
            .unwrap()
            .allowed_peers
            .iter()
            .any(|a| a == &s)
    }

    fn peer_name(&self, id: &NodeId) -> Option<String> {
        self.inner
            .config
            .read()
            .unwrap()
            .peer_names
            .get(&id.to_string())
            .cloned()
    }

    /// Mutate a peer entry (creating it if needed) and emit PeerStateChanged if info changed.
    fn update_peer(&self, id: NodeId, f: impl FnOnce(&mut PeerEntry)) {
        let changed = {
            let mut peers = self.inner.peers.lock().unwrap();
            let name = self.peer_name(&id);
            let entry = peers.entry(id).or_insert_with(|| PeerEntry {
                info: PeerInfo {
                    id,
                    name: name.clone(),
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
                udp: HashMap::new(),
                latency: LatencyWindow::default(),
            });
            let before = (
                entry.info.state,
                entry.info.latency_ms,
                entry.info.stats.total,
                entry.info.services.len(),
                entry.info.name.clone(),
            );
            if name.is_some() {
                entry.info.name = name;
            }
            f(entry);
            let after = (
                entry.info.state,
                entry.info.latency_ms,
                entry.info.stats.total,
                entry.info.services.len(),
                entry.info.name.clone(),
            );
            (before != after).then(|| entry.info.clone())
        };
        if let Some(info) = changed {
            self.emit(NodeEvent::PeerStateChanged(info));
        }
    }

    /// Record one ping round trip: update rolling stats, emit a peer event, append to latency.csv.
    fn record_latency(&self, id: NodeId, rtt_ms: f32) {
        let mut snapshot = None;
        self.update_peer(id, |e| {
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

    fn disconnect_peer(&self, id: NodeId) {
        if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&id) {
            if let Some(c) = e.dialed.take() {
                c.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
            }
        }
    }

    fn our_services(&self) -> Vec<Service> {
        self.inner.config.read().unwrap().services.clone()
    }

    /// Return the dialed connection to `peer`, dialing it if needed.
    async fn ensure_conn(&self, peer: NodeId) -> anyhow::Result<(Connection, PeerInfo)> {
        let _guard = self.inner.dial_lock.lock().await;
        if let Some(e) = self.inner.peers.lock().unwrap().get(&peer) {
            if let Some(c) = &e.dialed {
                if c.close_reason().is_none() {
                    return Ok((c.clone(), e.info.clone()));
                }
            }
        }
        self.update_peer(peer, |e| e.info.state = ConnState::Connecting);
        match self.dial(peer).await {
            Ok(r) => Ok(r),
            Err(e) => {
                self.update_peer(peer, |e| {
                    e.info.state = ConnState::Disconnected;
                    e.dialed = None;
                });
                self.emit(NodeEvent::Error(format!(
                    "connect to {}: {e:#}",
                    peer.fmt_short()
                )));
                Err(e)
            }
        }
    }

    async fn dial(&self, peer: NodeId) -> anyhow::Result<(Connection, PeerInfo)> {
        tracing::info!(%peer, "dialing");
        let conn = self.inner.endpoint.connect(peer, ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_msg(&mut send, &ControlMsg::Hello { name: None }).await?;
        write_msg(&mut send, &ControlMsg::Services(self.our_services())).await?;
        let services = handshake_read(&mut recv).await?;

        let state = path_state(&conn);
        let c = conn.clone();
        self.update_peer(peer, move |e| {
            e.dialed = Some(c);
            e.udp.clear();
            e.info.services = services;
            e.info.state = state;
        });

        let send = Arc::new(tokio::sync::Mutex::new(send));
        let n = self.clone();
        let c = conn.clone();
        self.spawn(async move { n.run_connection(c, send, recv, true).await });

        let info = self.inner.peers.lock().unwrap()[&peer].info.clone();
        Ok((conn, info))
    }

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
        if !self.is_allowed(&peer) {
            tracing::warn!(%peer, "rejecting connection from peer not in allowlist");
            conn.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
            return Ok(());
        }
        tracing::info!(%peer, "accepted connection");
        let (mut send, mut recv) = conn.accept_bi().await?;
        let services = handshake_read(&mut recv).await?;
        write_msg(&mut send, &ControlMsg::Hello { name: None }).await?;
        write_msg(&mut send, &ControlMsg::Services(self.our_services())).await?;

        let state = path_state(&conn);
        self.update_peer(peer, |e| {
            e.info.services = services;
            e.info.state = state;
        });
        let send = Arc::new(tokio::sync::Mutex::new(send));
        let n = self.clone();
        self.spawn(async move { n.run_connection(conn, send, recv, false).await });
        Ok(())
    }

    /// Drive one established connection until it closes.
    async fn run_connection(
        self,
        conn: Connection,
        send: Arc<tokio::sync::Mutex<SendStream>>,
        mut recv: RecvStream,
        dialer: bool,
    ) {
        let peer = conn.remote_id();
        let mut set: JoinSet<()> = JoinSet::new();

        // Control stream reader: answer pings, record pong latency.
        {
            let n = self.clone();
            let send = send.clone();
            set.spawn(async move {
                loop {
                    let msg: ControlMsg = match read_msg(&mut recv).await {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::debug!(%peer, "control stream ended: {e:#}");
                            break;
                        }
                    };
                    match msg {
                        ControlMsg::Ping(t) => {
                            if write_msg(&mut *send.lock().await, &ControlMsg::Pong(t))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        ControlMsg::Pong(t) => {
                            let now = n.inner.epoch.elapsed().as_micros() as u64;
                            let rtt_ms = now.saturating_sub(t) as f32 / 1000.0;
                            n.record_latency(peer, rtt_ms);
                        }
                        ControlMsg::Services(s) => n.update_peer(peer, |e| e.info.services = s),
                        ControlMsg::Hello { .. } => {}
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
                    if write_msg(&mut *send.lock().await, &ControlMsg::Ping(t))
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
                    n.update_peer(peer, |e| {
                        if e.info.state != ConnState::Disconnected {
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
                while let Ok(d) = conn.read_datagram().await {
                    let Some((idx, payload)) = decode_datagram(&d) else {
                        continue;
                    };
                    let client = n
                        .inner
                        .peers
                        .lock()
                        .unwrap()
                        .get(&peer)
                        .and_then(|e| e.udp.get(&idx).cloned());
                    if let Some(c) = client {
                        c.deliver(payload).await;
                    }
                }
            });
        } else {
            // Host: incoming data streams.
            let n = self.clone();
            let c = conn.clone();
            set.spawn(async move {
                while let Ok((send, recv)) = c.accept_bi().await {
                    let services = n.our_services();
                    tokio::spawn(async move {
                        if let Err(e) = forward::host_tcp(services, send, recv).await {
                            tracing::debug!(%peer, "tcp stream ended: {e:#}");
                        }
                    });
                }
            });
            // Host: datagrams to local UDP services.
            let n = self.clone();
            let c = conn.clone();
            set.spawn(async move {
                let mut sockets: HashMap<u16, Arc<UdpSocket>> = HashMap::new();
                while let Ok(d) = c.read_datagram().await {
                    let Some((idx, payload)) = decode_datagram(&d) else {
                        continue;
                    };
                    let sock = match sockets.get(&idx) {
                        Some(s) => s.clone(),
                        None => {
                            let svc = n.our_services().get(idx as usize).cloned();
                            let Some(svc) = svc.filter(|s| s.protocol == Protocol::Udp) else {
                                tracing::debug!(%peer, idx, "datagram for unknown udp service");
                                continue;
                            };
                            match forward::host_udp_socket(c.clone(), idx, svc.port).await {
                                Ok(s) => {
                                    sockets.insert(idx, s.clone());
                                    s
                                }
                                Err(e) => {
                                    tracing::warn!("udp socket for {}: {e:#}", svc.name);
                                    continue;
                                }
                            }
                        }
                    };
                    if let Err(e) = sock.send(payload).await {
                        tracing::debug!("udp send to local service: {e}");
                    }
                }
            });
        }

        let reason = conn.closed().await;
        set.abort_all();
        tracing::info!(%peer, "connection closed: {reason}");
        self.update_peer(peer, |e| {
            if dialer {
                if e.dialed.as_ref().map(|d| d.stable_id()) == Some(conn.stable_id()) {
                    e.dialed = None;
                    e.udp.clear();
                    e.info.state = ConnState::Disconnected;
                    e.info.latency_ms = None;
                    e.info.stats = LatencyStats::default();
                    e.latency.clear();
                }
            } else if e.dialed.is_none() {
                e.info.state = ConnState::Disconnected;
                e.info.latency_ms = None;
                e.info.stats = LatencyStats::default();
                e.latency.clear();
            }
        });
    }

    async fn tcp_tunnel_loop(self, listener: TcpListener, peer: NodeId, service: String) {
        let mut conns = JoinSet::new();
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
            conns.spawn(async move {
                let res = async {
                    let (conn, _) = n.ensure_conn(peer).await?;
                    forward::client_tcp(conn, service, tcp).await
                }
                .await;
                if let Err(e) = res {
                    tracing::debug!(%from, "tunnel connection ended: {e:#}");
                }
            });
            while conns.try_join_next().is_some() {}
        }
    }

    async fn udp_tunnel_loop(self, client: UdpClient, peer: NodeId, index: u16) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (len, from) = match client.socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::debug!("udp tunnel recv: {e}");
                    continue;
                }
            };
            *client.last_sender.lock().unwrap() = Some(from);
            let conn = match self.ensure_conn(peer).await {
                Ok((c, _)) => c,
                Err(_) => continue,
            };
            // Re-register after a reconnect cleared the map.
            if let Some(e) = self.inner.peers.lock().unwrap().get_mut(&peer) {
                e.udp.entry(index).or_insert_with(|| client.clone());
            }
            if let Err(e) = conn.send_datagram(encode_datagram(index, &buf[..len]).into()) {
                tracing::debug!("udp send_datagram: {e}");
            }
        }
    }
}

/// Read the peer's Hello and Services messages from the control stream.
async fn handshake_read(recv: &mut RecvStream) -> anyhow::Result<Vec<Service>> {
    match read_msg(recv).await? {
        ControlMsg::Hello { .. } => {}
        other => bail!("expected Hello, got {other:?}"),
    }
    match read_msg(recv).await? {
        ControlMsg::Services(s) => Ok(s),
        other => bail!("expected Services, got {other:?}"),
    }
}
