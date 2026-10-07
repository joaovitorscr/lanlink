//! App state and all node interaction. A tokio runtime on a background thread owns the
//! `Node`; results and `NodeEvent`s flow back to the `Root` view over an unbounded channel.
//!
//! Every async node call goes through [`Root::run`], which runs it in its own tokio task so
//! a panic (e.g. a `todo!()` in core) becomes an error toast instead of a crash. The 1 s
//! snapshot polling is wrapped in `catch_unwind`; a feature that panics once is marked
//! unavailable and not polled again.

use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Entity, Subscription};
use lanlink_core::{
    ActiveTunnel, Config, LanWorld, NetworkStatus, Node, NodeEvent, NodeId, PeerInfo, PeerRequest,
    Protocol, Service, ServiceStatus, TunnelInfo,
};
use tokio::sync::{broadcast, mpsc};

use crate::format;
use crate::widgets::text_input::{InputEvent, TextInput};

/// Messages from the tokio side to the UI.
pub enum Msg {
    Started(Node),
    StartFailed(String),
    Event(NodeEvent),
    Resync,
    Error(String),
    Info(String),
    RelaySaved(bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Peers,
    Hosting,
    Tunnels,
    Settings,
}

/// A snapshot read every second. `available` turns false for good after the first panic.
pub struct Polled<T> {
    pub value: T,
    pub available: bool,
}

impl<T: Default> Default for Polled<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            available: true,
        }
    }
}

impl<T> Polled<T> {
    fn poll(&mut self, f: impl FnOnce() -> T) {
        if !self.available {
            return;
        }
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => self.value = v,
            Err(_) => self.available = false,
        }
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    generation: u64,
}

/// Previous byte counters of a tunnel, to show live up/down rates.
struct Rate {
    up: u64,
    down: u64,
    at: Instant,
    up_rate: f64,
    down_rate: f64,
}

pub struct Inputs {
    pub peer_id: Entity<TextInput>,
    pub peer_name: Entity<TextInput>,
    pub svc_name: Entity<TextInput>,
    pub svc_port: Entity<TextInput>,
    pub display_name: Entity<TextInput>,
    pub relay_url: Entity<TextInput>,
}

pub struct Root {
    rt: tokio::runtime::Handle,
    tx: mpsc::UnboundedSender<Msg>,
    pub node: Option<Node>,
    pub fatal: Option<String>,
    pub config: Config,
    pub peers: Polled<Vec<PeerInfo>>,
    pub services: Polled<Vec<ServiceStatus>>,
    pub tunnels: Polled<Vec<TunnelInfo>>,
    /// Fallback when `tunnels_info()` is unavailable, kept current from events.
    pub plain_tunnels: Vec<ActiveTunnel>,
    pub worlds: Polled<Vec<LanWorld>>,
    pub requests: Polled<Vec<PeerRequest>>,
    pub network: Polled<NetworkStatus>,
    rates: HashMap<(NodeId, String), Rate>,
    pub tab: Tab,
    pub toast: Option<Toast>,
    toast_generation: u64,
    pub inputs: Inputs,
    pub svc_udp: bool,
    pub svc_minecraft: bool,
    pub renaming: Option<(NodeId, Entity<TextInput>)>,
    pub confirm_remove: Option<NodeId>,
    /// Optional-name inputs for the Allow banners, one per pending request.
    pub request_names: HashMap<NodeId, Entity<TextInput>>,
    pub restart_required: bool,
    _subscriptions: Vec<Subscription>,
    dynamic_subscriptions: HashMap<NodeId, Subscription>,
}

impl Root {
    pub fn new(rt: tokio::runtime::Handle, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                if this.update(cx, |s, cx| s.handle(msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |s, cx| s.poll(cx)).is_err() {
                break;
            }
        })
        .detach();

        // Start the node. A panic inside core surfaces as a JoinError.
        let start_tx = tx.clone();
        rt.spawn(async move {
            let res = tokio::spawn(async {
                let config = Config::load()?;
                Node::start(config).await
            })
            .await;
            let msg = match res {
                Ok(Ok(node)) => Msg::Started(node),
                Ok(Err(e)) => Msg::StartFailed(format!("{e:#}")),
                Err(e) => Msg::StartFailed(panic_text(e)),
            };
            let _ = start_tx.send(msg);
        });

        let input = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| TextInput::new(placeholder, cx))
        };
        let inputs = Inputs {
            peer_id: input("Friend's ID", cx),
            peer_name: input("Name (optional)", cx),
            svc_name: input("Name, e.g. minecraft", cx),
            svc_port: input("Port", cx),
            display_name: input("Your name", cx),
            relay_url: input("https://relay.example.com (empty = default)", cx),
        };
        let subscriptions = vec![
            submit(&inputs.peer_id, cx, Self::add_peer),
            submit(&inputs.peer_name, cx, Self::add_peer),
            submit(&inputs.svc_name, cx, Self::add_service),
            submit(&inputs.svc_port, cx, Self::add_service),
            submit(&inputs.display_name, cx, Self::save_display_name),
            submit(&inputs.relay_url, cx, Self::save_relay_url),
        ];

        Self {
            rt,
            tx,
            node: None,
            fatal: None,
            config: Config::default(),
            peers: Polled::default(),
            services: Polled::default(),
            tunnels: Polled::default(),
            plain_tunnels: Vec::new(),
            worlds: Polled::default(),
            requests: Polled::default(),
            network: Polled::default(),
            rates: HashMap::new(),
            tab: Tab::Peers,
            toast: None,
            toast_generation: 0,
            inputs,
            svc_udp: false,
            svc_minecraft: false,
            renaming: None,
            confirm_remove: None,
            request_names: HashMap::new(),
            restart_required: false,
            _subscriptions: subscriptions,
            dynamic_subscriptions: HashMap::new(),
        }
    }

    fn handle(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Started(node) => self.on_started(node, cx),
            Msg::StartFailed(e) => self.fatal = Some(e),
            Msg::Event(ev) => match ev {
                NodeEvent::PeerStateChanged(p) => {
                    match self.peers.value.iter_mut().find(|x| x.id == p.id) {
                        Some(slot) => *slot = p,
                        None => self.peers.value.push(p),
                    }
                }
                NodeEvent::TunnelOpened(t) => {
                    if !self.plain_tunnels.iter().any(|x| same_tunnel(x, &t)) {
                        self.plain_tunnels.push(t);
                    }
                }
                NodeEvent::TunnelClosed(t) => self.plain_tunnels.retain(|x| !same_tunnel(x, &t)),
                NodeEvent::Error(e) => self.show_error(e, cx),
                NodeEvent::PeerRequest(r) => {
                    self.requests.value.retain(|x| x.id != r.id);
                    self.requests.value.insert(0, r);
                }
                NodeEvent::NetworkChanged(n) => self.network.value = n,
                NodeEvent::LanWorldsChanged(w) => self.worlds.value = w,
            },
            Msg::Resync => self.resync(),
            Msg::Error(e) => self.show_error(e, cx),
            Msg::Info(s) => self.show_toast(s, false, cx),
            Msg::RelaySaved(restart) => {
                self.restart_required |= restart;
                self.show_toast("Relay saved", false, cx);
                self.resync();
            }
        }
        self.sync_request_inputs(cx);
        cx.notify();
    }

    fn on_started(&mut self, node: Node, cx: &mut Context<Self>) {
        let mut events = node.subscribe();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            loop {
                let msg = match events.recv().await {
                    Ok(ev) => Msg::Event(ev),
                    Err(broadcast::error::RecvError::Lagged(_)) => Msg::Resync,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        self.node = Some(node);
        self.resync();
        self.poll(cx);
        let name = self.config.display_name.clone().unwrap_or_default();
        let relay = self.config.relay_url.clone().unwrap_or_default();
        self.inputs
            .display_name
            .update(cx, |i, cx| i.set_text(name, cx));
        self.inputs
            .relay_url
            .update(cx, |i, cx| i.set_text(relay, cx));
    }

    /// Re-read everything that is not polled.
    fn resync(&mut self) {
        if let Some(node) = &self.node {
            self.config = node.config();
            if let Ok(t) = catch_unwind(AssertUnwindSafe(|| node.tunnels())) {
                self.plain_tunnels = t;
            }
        }
    }

    /// Runs every second: fast-changing snapshots.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.node.clone() else {
            return;
        };
        self.peers.poll(|| node.peers());
        self.services.poll(|| node.services_status());
        self.tunnels.poll(|| node.tunnels_info());
        self.worlds.poll(|| node.lan_worlds());
        self.requests.poll(|| node.pending_requests());
        self.network.poll(|| node.network_status());
        self.update_rates();
        self.sync_request_inputs(cx);
        cx.notify();
    }

    fn update_rates(&mut self) {
        let now = Instant::now();
        let mut next = HashMap::new();
        for t in &self.tunnels.value {
            let key = (t.tunnel.peer, t.tunnel.service.clone());
            let rate = match self.rates.remove(&key) {
                Some(prev) => {
                    let dt = now.duration_since(prev.at).as_secs_f64().max(0.001);
                    Rate {
                        up: t.bytes_up,
                        down: t.bytes_down,
                        at: now,
                        up_rate: t.bytes_up.saturating_sub(prev.up) as f64 / dt,
                        down_rate: t.bytes_down.saturating_sub(prev.down) as f64 / dt,
                    }
                }
                None => Rate {
                    up: t.bytes_up,
                    down: t.bytes_down,
                    at: now,
                    up_rate: 0.0,
                    down_rate: 0.0,
                },
            };
            next.insert(key, rate);
        }
        self.rates = next;
    }

    /// Live (up, down) bytes per second of a tunnel.
    pub fn rate(&self, peer: NodeId, service: &str) -> (f64, f64) {
        self.rates
            .get(&(peer, service.to_string()))
            .map_or((0.0, 0.0), |r| (r.up_rate, r.down_rate))
    }

    /// Keep one name input per pending request, prefilled with the name they sent.
    fn sync_request_inputs(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<NodeId> = self.requests.value.iter().map(|r| r.id).collect();
        self.request_names.retain(|id, _| ids.contains(id));
        self.dynamic_subscriptions
            .retain(|id, _| ids.contains(id) || self.renaming.as_ref().is_some_and(|r| r.0 == *id));
        for r in self.requests.value.clone() {
            if self.request_names.contains_key(&r.id) {
                continue;
            }
            let name = r.name.clone().unwrap_or_default();
            let input = cx.new(|cx| {
                let mut i = TextInput::new("Name (optional)", cx);
                i.set_text(name, cx);
                i
            });
            let id = r.id;
            let sub = cx.subscribe(&input, move |this, _, _: &InputEvent, cx| {
                this.respond_request(id, true, cx)
            });
            self.dynamic_subscriptions.insert(id, sub);
            self.request_names.insert(id, input);
        }
    }

    // ---- toasts ----

    pub fn show_error(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.show_toast(text, true, cx);
    }

    pub fn show_toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.toast_generation += 1;
        let generation = self.toast_generation;
        self.toast = Some(Toast {
            text: text.into(),
            error,
            generation,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(6)).await;
            let _ = this.update(cx, |s, cx| {
                if s.toast.as_ref().is_some_and(|t| t.generation == generation) {
                    s.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ---- running node calls ----

    /// Run an async node operation on tokio. Ok sends `done(value)` (or a resync), Err and
    /// panics become an error toast.
    pub fn run_then<T, F, Fut>(&self, f: F, done: impl FnOnce(T) -> Msg + Send + 'static)
    where
        T: Send + 'static,
        F: FnOnce(Node) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let Some(node) = self.node.clone() else {
            let _ = self.tx.send(Msg::Error("lanlink is still starting".into()));
            return;
        };
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let msg = match tokio::spawn(f(node)).await {
                Ok(Ok(v)) => done(v),
                Ok(Err(e)) => Msg::Error(format!("{e:#}")),
                Err(e) => Msg::Error(panic_text(e)),
            };
            let _ = tx.send(msg);
            let _ = tx.send(Msg::Resync);
        });
    }

    pub fn run<F, Fut>(&self, f: F)
    where
        F: FnOnce(Node) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.run_then(f, |()| Msg::Resync);
    }

    // ---- peers ----

    pub fn add_peer(&mut self, cx: &mut Context<Self>) {
        let id = self.inputs.peer_id.read(cx).text().trim().to_string();
        let peer = match id.parse::<NodeId>() {
            Ok(p) => p,
            Err(_) => {
                self.show_error(
                    "That doesn't look like a lanlink ID. Ask your friend to copy it again.",
                    cx,
                );
                return;
            }
        };
        self.inputs.peer_id.update(cx, |i, cx| i.take(cx));
        let name = non_empty(self.inputs.peer_name.update(cx, |i, cx| i.take(cx)));
        self.run_then(
            move |n| async move { n.add_peer(peer, name).await },
            |()| Msg::Info("Friend added, connecting…".into()),
        );
    }

    pub fn respond_request(&mut self, id: NodeId, allow: bool, cx: &mut Context<Self>) {
        let name = self
            .request_names
            .get(&id)
            .map(|i| i.read(cx).text().trim().to_string())
            .and_then(non_empty);
        self.requests.value.retain(|r| r.id != id);
        self.run(move |n| async move { n.respond_request(id, allow, name).await });
        cx.notify();
    }

    pub fn start_rename(
        &mut self,
        id: NodeId,
        current: String,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Name", cx);
            i.set_text(current, cx);
            i
        });
        let sub = cx.subscribe(&input, move |this, _, _: &InputEvent, cx| {
            this.finish_rename(cx)
        });
        self.dynamic_subscriptions.insert(id, sub);
        self.renaming = Some((id, input.clone()));
        input
    }

    pub fn finish_rename(&mut self, cx: &mut Context<Self>) {
        let Some((id, input)) = self.renaming.take() else {
            return;
        };
        let name = non_empty(input.read(cx).text().trim().to_string());
        self.run(move |n| async move { n.rename_peer(id, name).await });
        cx.notify();
    }

    pub fn remove_peer(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if self.confirm_remove != Some(id) {
            self.confirm_remove = Some(id);
            cx.notify();
            return;
        }
        self.confirm_remove = None;
        self.peers.value.retain(|p| p.id != id);
        self.run(move |n| async move { n.remove_peer(id).await });
        cx.notify();
    }

    /// Open (and remember) a tunnel to a peer's service on the same port when it is free.
    pub fn connect_service(&mut self, peer: NodeId, svc: Service) {
        let local = if port_free(svc.port, svc.protocol) {
            svc.port
        } else {
            0
        };
        self.run_then(
            move |n| async move { n.save_tunnel(peer, &svc.name, local, true).await },
            |t| Msg::Info(format!("Tunnel open at {}", t.local_addr)),
        );
    }

    /// The open tunnel to `peer`/`service`, if any.
    pub fn open_tunnel(&self, peer: NodeId, service: &str) -> Option<ActiveTunnel> {
        self.tunnel_list()
            .into_iter()
            .map(|t| t.tunnel)
            .find(|t| t.peer == peer && t.service == service)
    }

    /// Tunnels with counters, or bare tunnels with zeros when `tunnels_info()` is unavailable.
    pub fn tunnel_list(&self) -> Vec<TunnelInfo> {
        if self.tunnels.available {
            return self.tunnels.value.clone();
        }
        self.plain_tunnels
            .iter()
            .map(|t| TunnelInfo {
                tunnel: t.clone(),
                protocol: self.service_protocol(t.peer, &t.service),
                connections: 0,
                bytes_up: 0,
                bytes_down: 0,
                saved: self
                    .config
                    .saved_tunnels
                    .iter()
                    .any(|s| s.peer == t.peer.to_string() && s.service == t.service),
            })
            .collect()
    }

    fn service_protocol(&self, peer: NodeId, service: &str) -> Protocol {
        self.peers
            .value
            .iter()
            .find(|p| p.id == peer)
            .and_then(|p| p.services.iter().find(|s| s.name == service))
            .map_or(Protocol::Tcp, |s| s.protocol)
    }

    pub fn peer_name(&self, id: NodeId) -> String {
        let key = id.to_string();
        self.peers
            .value
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.name.clone())
            .or_else(|| self.config.peer_names.get(&key).cloned())
            .unwrap_or_else(|| format::short_id(&key))
    }

    // ---- hosting ----

    pub fn add_service(&mut self, cx: &mut Context<Self>) {
        let Some(port) = format::parse_port(self.inputs.svc_port.read(cx).text()) else {
            self.show_error("Port must be a number from 1 to 65535", cx);
            return;
        };
        let name = self.inputs.svc_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.show_error("Give the service a name", cx);
            return;
        }
        self.inputs.svc_name.update(cx, |i, cx| i.take(cx));
        self.inputs.svc_port.update(cx, |i, cx| i.take(cx));
        let protocol = if self.svc_udp {
            Protocol::Udp
        } else {
            Protocol::Tcp
        };
        let mut svc = Service::new(name, protocol, port);
        svc.minecraft_lan = self.svc_minecraft;
        self.run(move |n| async move { n.add_service(svc).await });
    }

    /// Hosted services with status, or config entries with unknown status as a fallback.
    pub fn service_list(&self) -> Vec<ServiceStatus> {
        if self.services.available {
            return self.services.value.clone();
        }
        self.config
            .services
            .iter()
            .map(|s| ServiceStatus {
                service: s.clone(),
                reachable: None,
                effective_port: s.port,
                connections: 0,
                peers: Vec::new(),
            })
            .collect()
    }

    pub fn share_world(&mut self, world: LanWorld) {
        let name = world_service_name(&world.motd);
        let mut svc = Service::new(name, Protocol::Tcp, world.port);
        svc.minecraft_lan = true;
        self.run(move |n| async move { n.add_service(svc).await });
    }

    // ---- settings ----

    pub fn save_display_name(&mut self, cx: &mut Context<Self>) {
        let name = non_empty(self.inputs.display_name.read(cx).text().trim().to_string());
        self.run_then(
            move |n| async move { n.set_display_name(name).await },
            |()| Msg::Info("Name saved".into()),
        );
    }

    pub fn save_relay_url(&mut self, cx: &mut Context<Self>) {
        let url = non_empty(self.inputs.relay_url.read(cx).text().trim().to_string());
        self.run_then(
            move |n| async move { n.set_relay_url(url).await },
            Msg::RelaySaved,
        );
    }

    pub fn toggle_lan_detection(&mut self) {
        let mut config = self.config.clone();
        config.disable_lan_detection = !config.disable_lan_detection;
        self.config = config.clone();
        self.run(move |n| async move { n.update_config(config).await });
    }

    pub fn open_folder(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let res = std::fs::create_dir_all(&path).and_then(|_| open::that(&path));
        if let Err(e) = res {
            self.show_error(format!("Could not open {}: {e}", path.display()), cx);
        }
    }
}

fn submit(
    input: &Entity<TextInput>,
    cx: &mut Context<Root>,
    f: fn(&mut Root, &mut Context<Root>),
) -> Subscription {
    cx.subscribe(input, move |this, _, _: &InputEvent, cx| f(this, cx))
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Service name for a shared LAN world: its MOTD, or "minecraft".
pub fn world_service_name(motd: &str) -> String {
    let name: String = motd
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if name.is_empty() {
        "minecraft".into()
    } else {
        name
    }
}

pub fn same_tunnel(a: &ActiveTunnel, b: &ActiveTunnel) -> bool {
    a.peer == b.peer && a.service == b.service && a.local_addr == b.local_addr
}

/// Whether `port` can be bound on localhost, so the tunnel can reuse the host's port number.
fn port_free(port: u16, proto: Protocol) -> bool {
    let addr = (Ipv4Addr::LOCALHOST, port);
    match proto {
        Protocol::Tcp => TcpListener::bind(addr).is_ok(),
        Protocol::Udp => UdpSocket::bind(addr).is_ok(),
    }
}

pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".into())
}

fn panic_text(e: tokio::task::JoinError) -> String {
    match e.try_into_panic() {
        Ok(p) => {
            let m = panic_message(&*p);
            if m.contains("not yet implemented") {
                "Not available yet in this lanlink build".into()
            } else {
                format!("Internal error: {m}")
            }
        }
        Err(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_names() {
        assert_eq!(world_service_name("  My World "), "My World");
        assert_eq!(world_service_name(""), "minecraft");
    }
}
