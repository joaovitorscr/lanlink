use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Entity, Pixels, Point, Subscription};
use lanlink_core::{
    ActiveTunnel, Config, LanWorld, NetworkStatus, Node, NodeEvent, NodeId, PeerInfo, PeerRequest,
    Protocol, Service, ServiceStatus, TunnelInfo,
};
use tokio::sync::{broadcast, mpsc};

use crate::format;
use crate::theme::Prefs;
use crate::update::{self, Release};
use crate::widgets::text_input::{InputEvent, TextInput};

/// First update check this long after startup, then every [`UPDATE_INTERVAL`].
const UPDATE_FIRST_DELAY: Duration = Duration::from_secs(10);
const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// A modal sheet dropping from the title bar.
#[derive(Clone, PartialEq, Eq)]
pub enum Sheet {
    AddPeer,
    AddService,
    RenamePeer(NodeId),
    RemovePeer(NodeId),
    RemoveService(String),
}

/// Which popover menu is open.
#[derive(Clone, PartialEq, Eq)]
pub enum MenuKind {
    Peer(NodeId),
    Service(String),
    Protocol,
    Appearance,
}

pub struct OpenMenu {
    pub kind: MenuKind,
    /// Top-left corner, window coordinates.
    pub at: Point<Pixels>,
}

/// Messages from the tokio side to the UI.
pub enum Msg {
    Started(Node),
    StartFailed(String),
    Event(NodeEvent),
    Resync,
    Error(String),
    Info(String),
    RelaySaved(bool),
    UpdateChecked(Result<Option<Release>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Peers,
    Hosting,
    Tunnels,
    Settings,
}

/// Outcome of the update checks so far.
#[derive(Default)]
pub struct UpdateStatus {
    pub checking: bool,
    /// When the last check finished, successful or not.
    pub last_check: Option<Instant>,
    pub failed: bool,
    /// Newer release from the last successful check.
    pub available: Option<Release>,
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
    pub rename: Entity<TextInput>,
}

pub struct Root {
    rt: tokio::runtime::Handle,
    tx: mpsc::UnboundedSender<Msg>,
    pub node: Option<Node>,
    pub fatal: Option<String>,
    pub config: Config,
    pub peers: Vec<PeerInfo>,
    pub services: Vec<ServiceStatus>,
    pub tunnels: Vec<TunnelInfo>,
    pub worlds: Vec<LanWorld>,
    pub requests: Vec<PeerRequest>,
    pub network: NetworkStatus,
    rates: HashMap<(NodeId, String), Rate>,
    pub tab: Tab,
    pub toast: Option<Toast>,
    toast_generation: u64,
    pub inputs: Inputs,
    pub svc_protocol: Protocol,
    pub svc_minecraft: bool,
    pub sheet: Option<Sheet>,
    pub menu: Option<OpenMenu>,
    pub prefs: Prefs,
    background: Option<gpui::WindowBackgroundAppearance>,
    pub restart_required: bool,
    pub update: UpdateStatus,
    _subscriptions: Vec<Subscription>,
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

        if update::supported() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(UPDATE_FIRST_DELAY).await;
                loop {
                    let res = this.update(cx, |s, cx| {
                        if s.prefs.check_updates {
                            s.check_for_updates(cx);
                        }
                    });
                    if res.is_err() {
                        break;
                    }
                    cx.background_executor().timer(UPDATE_INTERVAL).await;
                }
            })
            .detach();
        }

        // Start the node. A panic inside core surfaces as a JoinError.
        let start_tx = tx.clone();
        rt.spawn(async move {
            let res = async {
                let config = Config::load()?;
                Node::start(config).await
            }
            .await;
            let msg = match res {
                Ok(node) => Msg::Started(node),
                Err(e) => Msg::StartFailed(format!("{e:#}")),
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
            rename: input("Name", cx),
        };
        let subscriptions = vec![
            submit(&inputs.peer_id, cx, Self::add_peer),
            submit(&inputs.peer_name, cx, Self::add_peer),
            submit(&inputs.svc_name, cx, Self::add_service),
            submit(&inputs.svc_port, cx, Self::add_service),
            submit(&inputs.display_name, cx, Self::save_display_name),
            submit(&inputs.relay_url, cx, Self::save_relay_url),
            submit(&inputs.rename, cx, Self::finish_rename),
        ];

        Self {
            rt,
            tx,
            node: None,
            fatal: None,
            config: Config::default(),
            peers: Vec::new(),
            services: Vec::new(),
            tunnels: Vec::new(),
            worlds: Vec::new(),
            requests: Vec::new(),
            network: NetworkStatus::default(),
            rates: HashMap::new(),
            tab: Tab::Peers,
            toast: None,
            toast_generation: 0,
            inputs,
            svc_protocol: Protocol::Tcp,
            svc_minecraft: false,
            sheet: None,
            menu: None,
            prefs: Prefs::load(),
            background: None,
            restart_required: false,
            update: UpdateStatus::default(),
            _subscriptions: subscriptions,
        }
    }

    fn handle(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Started(node) => self.on_started(node, cx),
            Msg::StartFailed(e) => self.fatal = Some(e),
            Msg::Event(ev) => match ev {
                NodeEvent::PeerStateChanged(p) => {
                    match self.peers.iter_mut().find(|x| x.id == p.id) {
                        Some(slot) => *slot = p,
                        None => self.peers.push(p),
                    }
                }
                // Tunnels are read by the 1 s poll.
                NodeEvent::TunnelOpened(_) | NodeEvent::TunnelClosed(_) => {}
                NodeEvent::Error(e) => self.show_error(e, cx),
                NodeEvent::PeerRequest(r) => {
                    self.requests.retain(|x| x.id != r.id);
                    self.requests.insert(0, r);
                }
                NodeEvent::NetworkChanged(n) => self.network = n,
                NodeEvent::LanWorldsChanged(w) => self.worlds = w,
            },
            Msg::Resync => self.resync(),
            Msg::Error(e) => self.show_error(e, cx),
            Msg::Info(s) => self.show_toast(s, false, cx),
            Msg::RelaySaved(restart) => {
                self.restart_required |= restart;
                self.show_toast("Relay saved", false, cx);
                self.resync();
            }
            Msg::UpdateChecked(res) => {
                self.update.checking = false;
                self.update.last_check = Some(Instant::now());
                match res {
                    Ok(release) => {
                        self.update.failed = false;
                        self.update.available = release;
                    }
                    Err(e) => {
                        tracing::debug!("update check failed: {e}");
                        self.update.failed = true;
                    }
                }
            }
        }
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
        }
    }

    /// Runs every second: fast-changing snapshots.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(node) = &self.node else {
            return;
        };
        self.peers = node.peers();
        self.services = node.services_status();
        self.tunnels = node.tunnels_info();
        self.worlds = node.lan_worlds();
        self.requests = node.pending_requests();
        self.network = node.network_status();
        self.update_rates();
        cx.notify();
    }

    fn update_rates(&mut self) {
        let now = Instant::now();
        let mut next = HashMap::new();
        for t in &self.tunnels {
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

    /// Run an async node operation on tokio. Ok sends `done(value)` (or a resync), Err
    /// becomes an error toast.
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
            let msg = match f(node).await {
                Ok(v) => done(v),
                Err(e) => Msg::Error(format!("{e:#}")),
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
        self.sheet = None;
        self.run_then(
            move |n| async move { n.add_peer(peer, name).await },
            |()| Msg::Info("Friend added, connecting…".into()),
        );
        cx.notify();
    }

    pub fn respond_request(&mut self, id: NodeId, allow: bool, cx: &mut Context<Self>) {
        self.requests.retain(|r| r.id != id);
        self.run(move |n| async move { n.respond_request(id, allow, None).await });
        cx.notify();
    }

    /// Open the rename sheet prefilled with the current local name.
    pub fn start_rename(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let current = self
            .config
            .peer_names
            .get(&id.to_string())
            .cloned()
            .unwrap_or_default();
        self.inputs
            .rename
            .update(cx, |i, cx| i.set_text(current, cx));
        self.open_sheet(Sheet::RenamePeer(id), cx);
    }

    pub fn finish_rename(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::RenamePeer(id)) = self.sheet.take() else {
            return;
        };
        let name = non_empty(self.inputs.rename.update(cx, |i, cx| i.take(cx)));
        self.run(move |n| async move { n.rename_peer(id, name).await });
        cx.notify();
    }

    /// Remove for real (the sheet asked for confirmation).
    pub fn remove_peer(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.sheet = None;
        self.peers.retain(|p| p.id != id);
        self.run(move |n| async move { n.remove_peer(id).await });
        cx.notify();
    }

    pub fn remove_service(&mut self, name: String, cx: &mut Context<Self>) {
        self.sheet = None;
        self.run(move |n| async move { n.remove_service(&name).await });
        cx.notify();
    }

    // ---- overlays ----

    pub fn open_sheet(&mut self, sheet: Sheet, cx: &mut Context<Self>) {
        self.menu = None;
        self.sheet = Some(sheet);
        cx.notify();
    }

    pub fn close_overlays(&mut self, cx: &mut Context<Self>) {
        self.sheet = None;
        self.menu = None;
        cx.notify();
    }

    pub fn toggle_menu(&mut self, kind: MenuKind, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = match &self.menu {
            Some(m) if m.kind == kind => None,
            _ => Some(OpenMenu { kind, at }),
        };
        cx.notify();
    }

    /// Keep the window backdrop in sync with the transparency preference.
    pub fn apply_background(&mut self, window: &gpui::Window) {
        let want = crate::glass::apply(window, self.prefs.transparency);
        if self.background != Some(want) {
            window.set_background_appearance(want);
            self.background = Some(want);
        }
    }

    pub fn save_prefs(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.prefs.save() {
            self.show_error(format!("Could not save preferences: {e}"), cx);
        }
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

    pub fn peer_name(&self, id: NodeId) -> String {
        let key = id.to_string();
        self.peers
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
        let mut svc = Service::new(name, self.svc_protocol, port);
        svc.minecraft_lan = self.svc_minecraft;
        self.sheet = None;
        self.run(move |n| async move { n.add_service(svc).await });
        cx.notify();
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
        let enabled = self.config.disable_lan_detection;
        self.config.disable_lan_detection = !enabled;
        self.run(move |n| async move { n.set_lan_detection(enabled).await });
    }

    pub fn toggle_latency_log(&mut self) {
        let enabled = !self.config.latency_log;
        self.config.latency_log = enabled;
        self.run(move |n| async move { n.set_latency_log(enabled).await });
    }

    // ---- updates ----

    /// Ask GitHub for a newer release in the background. Failures are only logged.
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !update::supported() || self.update.checking {
            return;
        }
        self.update.checking = true;
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let res = update::check().await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::UpdateChecked(res));
        });
        cx.notify();
    }

    /// The release to announce in the banner: newer, not dismissed, checks enabled.
    pub fn update_banner(&self) -> Option<&Release> {
        let r = self.update.available.as_ref()?;
        let dismissed = self.prefs.dismissed_update.as_deref() == Some(r.version.as_str());
        (self.prefs.check_updates && !dismissed).then_some(r)
    }

    pub fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.update.available {
            self.prefs.dismissed_update = Some(r.version.clone());
            self.save_prefs(cx);
        }
    }

    pub fn toggle_update_checks(&mut self, cx: &mut Context<Self>) {
        self.prefs.check_updates = !self.prefs.check_updates;
        self.save_prefs(cx);
        if self.prefs.check_updates {
            self.check_for_updates(cx);
        }
    }

    pub fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        if let Err(e) = open::that(&url) {
            self.show_error(format!("Could not open {url}: {e}"), cx);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_names() {
        assert_eq!(world_service_name("  My World "), "My World");
        assert_eq!(world_service_name(""), "minecraft");
    }
}
