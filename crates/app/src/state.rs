use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Entity, PathPromptOptions, Pixels, Point, Subscription};
use lanlink_core::export::{self, Import, ImportMode};
use lanlink_core::{
    ActiveTunnel, Approval, Config, ImportOutcome, InviteExpiry, JoinStatus, LanWorld, Network,
    NetworkColor, NetworkPolicy, NetworkStatus, Node, NodeEvent, NodeId, PeerInfo, PeerRequest,
    Protocol, Service, ServiceStatus, TunnelInfo,
};
use tokio::sync::{broadcast, mpsc};

use crate::format;
use crate::theme::Prefs;
use crate::update::{self, Release, Staged};
use crate::widgets::text_input::{InputEvent, TextInput};

/// First update check this long after startup, then every [`UPDATE_INTERVAL`].
const UPDATE_FIRST_DELAY: Duration = Duration::from_secs(10);
const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// A modal sheet dropping from the title bar.
#[derive(Clone, PartialEq, Eq)]
pub enum Sheet {
    AddService,
    RenamePeer(NodeId),
    RemovePeer(NodeId),
    RemoveService(String),
    /// Confirm the file in `Root::pending_import`.
    Import,
    NewNetwork,
    JoinNetwork,
    /// Invite code of a network (by id).
    Invite(String),
    RenameNetwork(String),
    RemoveMember(String, NodeId),
    LeaveNetwork(String),
    DeleteNetwork(String),
    /// First launch: pick a display name, or take one made from the device name.
    Welcome,
}

/// Which popover menu is open.
#[derive(Clone, PartialEq, Eq)]
pub enum MenuKind {
    /// A direct peer (outside any network).
    Peer(NodeId),
    /// A member row of a network: (network id, member).
    Member(String, NodeId),
    Network(String),
    Service(String),
    Protocol,
    Appearance,
    NewNetWho,
    NewNetExpiry,
    InviteExpiry(String),
    InviteApproval(String),
}

/// Who can join a new network ("Who can join" in the New Network sheet).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WhoCanJoin {
    Anyone,
    Approved,
    NoCode,
}

impl WhoCanJoin {
    pub const ALL: [WhoCanJoin; 3] = [WhoCanJoin::Anyone, WhoCanJoin::Approved, WhoCanJoin::NoCode];

    pub fn label(self) -> &'static str {
        match self {
            WhoCanJoin::Anyone => "Anyone with the code",
            WhoCanJoin::Approved => "Only people I approve",
            WhoCanJoin::NoCode => "Invite only, no code",
        }
    }
}

/// Choices of the New Network sheet besides the name.
pub struct NewNetwork {
    pub color: NetworkColor,
    pub who: WhoCanJoin,
    pub expires: InviteExpiry,
    pub policy: NetworkPolicy,
}

impl Default for NewNetwork {
    fn default() -> Self {
        Self {
            color: NetworkColor::Green,
            who: WhoCanJoin::Anyone,
            expires: InviteExpiry::Never,
            policy: NetworkPolicy::default(),
        }
    }
}

/// Expiry choices offered in the UI.
pub const EXPIRY_CHOICES: [InviteExpiry; 4] = [
    InviteExpiry::Never,
    InviteExpiry::AfterHours { hours: 1 },
    InviteExpiry::AfterHours { hours: 24 },
    InviteExpiry::FirstUse,
];

pub fn expiry_label(e: InviteExpiry) -> String {
    match e {
        InviteExpiry::Never => "Never".into(),
        InviteExpiry::AfterHours { hours: 1 } => "After 1 hour".into(),
        InviteExpiry::AfterHours { hours } => format!("After {hours} hours"),
        InviteExpiry::FirstUse => "After first use".into(),
    }
}

pub fn approval_label(a: Approval) -> &'static str {
    match a {
        Approval::AskMe => "Ask me first",
        Approval::Auto => "Auto-approve",
    }
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
    OpenSheet(Sheet),
    Error(String),
    Info(String),
    RelaySaved(bool),
    Imported(ImportOutcome),
    UpdateChecked(Result<Option<Release>, String>),
    UpdateProgress {
        version: String,
        done: u64,
        total: Option<u64>,
    },
    UpdatePrepared {
        version: String,
        result: Result<Staged, String>,
    },
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
    /// Download / install of a release, once started.
    pub download: Option<UpdateDownload>,
}

pub struct UpdateDownload {
    pub version: String,
    pub phase: UpdatePhase,
    /// Install as soon as the download is verified (the user clicked Install). Background
    /// downloads wait for "Restart to update".
    install_when_ready: bool,
}

#[derive(Clone)]
pub enum UpdatePhase {
    Downloading { done: u64, total: Option<u64> },
    Ready(Staged),
    Installing,
    Failed(String),
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    generation: u64,
}

/// A config file picked for import, waiting for confirmation.
pub struct PendingImport {
    pub file_name: String,
    pub import: Import,
    pub mode: ImportMode,
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
    pub svc_name: Entity<TextInput>,
    pub svc_port: Entity<TextInput>,
    pub svc_host: Entity<TextInput>,
    pub display_name: Entity<TextInput>,
    pub relay_url: Entity<TextInput>,
    pub rename: Entity<TextInput>,
    pub net_name: Entity<TextInput>,
    pub join_code: Entity<TextInput>,
    pub join_name: Entity<TextInput>,
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
    pub new_network: NewNetwork,
    /// Networks whose member list is folded away (this session only).
    pub collapsed: HashSet<String>,
    /// IDs the user chose to show while Hide IDs is on. Not persisted.
    pub revealed_ids: HashSet<String>,
    pub sheet: Option<Sheet>,
    pub pending_import: Option<PendingImport>,
    pub menu: Option<OpenMenu>,
    pub prefs: Prefs,
    background: Option<gpui::WindowBackgroundAppearance>,
    pub restart_required: bool,
    pub update: UpdateStatus,
    /// Update to run once the app has shut down (see `update::relaunch`).
    pub pending_install: Option<Staged>,
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
            rt.spawn_blocking(update::cleanup);
        }
        if std::env::args().any(|a| a == update::FAILED_ARG) {
            tracing::warn!("the update installer did not finish");
            let _ = tx.send(Msg::Error(format!(
                "The update didn't install. You're still on lanlink {}.",
                lanlink_core::build_info::VERSION
            )));
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
            svc_name: input("Name, e.g. minecraft", cx),
            svc_port: input("Port", cx),
            svc_host: input("127.0.0.1 (optional)", cx),
            display_name: input("Your name", cx),
            relay_url: input("https://relay.example.com (empty = default)", cx),
            rename: input("Name", cx),
            net_name: input("e.g. Friday squad", cx),
            join_code: input("lanlink-…", cx),
            join_name: input("Your name", cx),
        };
        let subscriptions = vec![
            submit(&inputs.svc_name, cx, Self::add_service),
            submit(&inputs.svc_port, cx, Self::add_service),
            submit(&inputs.svc_host, cx, Self::add_service),
            submit(&inputs.display_name, cx, Self::save_display_name),
            submit(&inputs.relay_url, cx, Self::save_relay_url),
            submit(&inputs.rename, cx, Self::finish_rename),
            submit(&inputs.net_name, cx, Self::create_network),
            submit(&inputs.join_code, cx, Self::join_network),
            submit(&inputs.join_name, cx, Self::join_network),
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
            new_network: NewNetwork::default(),
            collapsed: HashSet::new(),
            revealed_ids: HashSet::new(),
            sheet: None,
            pending_import: None,
            menu: None,
            prefs: Prefs::load(),
            background: None,
            restart_required: false,
            update: UpdateStatus::default(),
            pending_install: None,
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
                    self.requests
                        .retain(|x| !(x.id == r.id && x.network == r.network));
                    self.requests.insert(0, r);
                    // Join requests are stored in the config.
                    self.resync();
                }
                NodeEvent::NetworkChanged(n) => self.network = n,
                NodeEvent::LanWorldsChanged(w) => self.worlds = w,
                NodeEvent::NetworksChanged => self.resync(),
            },
            Msg::Resync => self.resync(),
            Msg::OpenSheet(sheet) => {
                self.resync();
                self.menu = None;
                self.sheet = Some(sheet);
            }
            Msg::Error(e) => self.show_error(e, cx),
            Msg::Info(s) => self.show_toast(s, false, cx),
            Msg::RelaySaved(restart) => {
                self.restart_required |= restart;
                self.show_toast("Relay saved", false, cx);
                self.resync();
            }
            Msg::Imported(outcome) => {
                self.restart_required |= outcome.restart_required;
                self.resync();
                self.fill_setting_inputs(cx);
                let backup = outcome
                    .backup
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let mut text = format!(
                    "Config imported. The previous one is saved as {backup} in the config folder."
                );
                if outcome.restart_required {
                    text.push_str(" Restart lanlink to use the new relay.");
                }
                self.show_toast(text, false, cx);
            }
            Msg::UpdateChecked(res) => {
                self.update.checking = false;
                self.update.last_check = Some(Instant::now());
                match res {
                    Ok(release) => {
                        if let Some(r) = &release {
                            tracing::info!("update available: lanlink {}", r.version);
                        }
                        self.update.failed = false;
                        self.update.available = release;
                        self.auto_download_update(cx);
                    }
                    Err(e) => {
                        tracing::debug!("update check failed: {e}");
                        self.update.failed = true;
                    }
                }
            }
            Msg::UpdateProgress {
                version,
                done,
                total,
            } => {
                if let Some(d) = self.update.download.as_mut() {
                    if d.version == version && matches!(d.phase, UpdatePhase::Downloading { .. }) {
                        d.phase = UpdatePhase::Downloading { done, total };
                    }
                }
            }
            Msg::UpdatePrepared { version, result } => self.on_update_prepared(version, result, cx),
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
        self.fill_setting_inputs(cx);
        if self.config.display_name.is_none() && self.sheet.is_none() {
            self.open_sheet(Sheet::Welcome, cx);
        }
    }

    /// Put the saved display name and relay into their Settings fields.
    fn fill_setting_inputs(&mut self, cx: &mut Context<Self>) {
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
            cx.background_executor()
                .timer(Duration::from_secs(if error { 6 } else { 3 }))
                .await;
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

    pub fn respond_request(&mut self, req: PeerRequest, allow: bool, cx: &mut Context<Self>) {
        self.requests
            .retain(|r| !(r.id == req.id && r.network == req.network));
        let id = req.id;
        if let Some(net) = req.network {
            self.run(move |n| async move { n.respond_join(&net, id, allow).await });
        }
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
        match self.sheet.take() {
            Some(Sheet::RenamePeer(id)) => {
                let name = non_empty(self.inputs.rename.update(cx, |i, cx| i.take(cx)));
                self.run(move |n| async move { n.rename_peer(id, name).await });
            }
            Some(Sheet::RenameNetwork(net)) => {
                let name = self.inputs.rename.update(cx, |i, cx| i.take(cx));
                self.run(move |n| async move { n.rename_network(&net, name.trim()).await });
            }
            other => self.sheet = other,
        }
        cx.notify();
    }

    // ---- networks ----

    pub fn network(&self, id: &str) -> Option<&Network> {
        self.config.networks.iter().find(|n| n.id == id)
    }

    pub fn my_id(&self) -> Option<NodeId> {
        self.node.as_ref().map(|n| n.id())
    }

    /// Networks we own, other than `except`.
    pub fn owned_networks(&self, except: Option<&str>) -> Vec<Network> {
        let Some(me) = self.my_id() else {
            return Vec::new();
        };
        self.config
            .networks
            .iter()
            .filter(|n| n.active() && n.is_owner(&me) && Some(n.id.as_str()) != except)
            .cloned()
            .collect()
    }

    pub fn start_new_network(&mut self, cx: &mut Context<Self>) {
        self.new_network = NewNetwork::default();
        self.inputs.net_name.update(cx, |i, cx| i.take(cx));
        self.open_sheet(Sheet::NewNetwork, cx);
    }

    pub fn create_network(&mut self, cx: &mut Context<Self>) {
        if self.sheet != Some(Sheet::NewNetwork) {
            return;
        }
        let name = self.inputs.net_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.show_error("Give the network a name", cx);
            return;
        }
        self.inputs.net_name.update(cx, |i, cx| i.take(cx));
        let f = &self.new_network;
        let (color, policy) = (f.color, f.policy);
        let invite = match f.who {
            WhoCanJoin::Anyone => Some((f.expires, Approval::Auto)),
            WhoCanJoin::Approved => Some((f.expires, Approval::AskMe)),
            WhoCanJoin::NoCode => None,
        };
        let tx = self.tx.clone();
        self.sheet = None;
        self.run_then(
            move |n| async move { n.create_network(&name, color, policy, invite).await },
            move |net| {
                // Show the code right away so it can be sent to friends.
                if net.invite_code().is_some() {
                    let _ = tx.send(Msg::OpenSheet(Sheet::Invite(net.id.clone())));
                }
                Msg::Info(format!("{} created", net.name))
            },
        );
        cx.notify();
    }

    pub fn start_join(&mut self, cx: &mut Context<Self>) {
        let name = self.config.display_name.clone().unwrap_or_default();
        self.inputs.join_code.update(cx, |i, cx| i.take(cx));
        self.inputs
            .join_name
            .update(cx, |i, cx| i.set_text(name, cx));
        self.open_sheet(Sheet::JoinNetwork, cx);
    }

    pub fn join_network(&mut self, cx: &mut Context<Self>) {
        if self.sheet != Some(Sheet::JoinNetwork) {
            return;
        }
        let code = self.inputs.join_code.read(cx).text().trim().to_string();
        if let Err(e) = lanlink_core::InviteCode::parse(&code) {
            self.show_error(format!("{e}"), cx);
            return;
        }
        let name = non_empty(self.inputs.join_name.read(cx).text().trim().to_string());
        self.inputs.join_code.update(cx, |i, cx| i.take(cx));
        self.sheet = None;
        self.show_toast("Asking the owner…", false, cx);
        self.run_then(
            move |n| async move { n.join_network(&code, name).await },
            |status| {
                Msg::Info(match status {
                    JoinStatus::Pending => {
                        "Request sent. You're in once the owner approves you.".into()
                    }
                    _ => "You joined the network".into(),
                })
            },
        );
        cx.notify();
    }

    /// Copy a network's invite code.
    pub fn copy_invite(&mut self, code: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(code));
        self.show_toast("Invite code copied", false, cx);
    }

    /// Make a new code for a network we own (the old one stops working).
    pub fn new_invite(&mut self, net: String, expires: InviteExpiry, approval: Approval) {
        self.run_then(
            move |n| async move { n.create_invite(&net, expires, approval).await },
            |_| Msg::Info("New invite code made. The old one no longer works.".into()),
        );
    }

    pub fn update_invite(&mut self, net: String, expires: InviteExpiry, approval: Approval) {
        self.run(
            move |n| async move { n.update_invite(&net, expires, approval).await.map(|_| ()) },
        );
    }

    pub fn start_rename_network(&mut self, id: String, cx: &mut Context<Self>) {
        let current = self
            .network(&id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        self.inputs
            .rename
            .update(cx, |i, cx| i.set_text(current, cx));
        self.open_sheet(Sheet::RenameNetwork(id), cx);
    }

    pub fn toggle_revealed(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.revealed_ids.remove(&key) {
            self.revealed_ids.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_collapsed(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
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

    /// Close the welcome sheet, saving `name` or the device name when there is none.
    pub fn finish_welcome(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        let name = name.unwrap_or_else(format::device_name);
        self.config.display_name = Some(name.clone());
        self.inputs
            .display_name
            .update(cx, |i, cx| i.set_text(name.clone(), cx));
        self.sheet = None;
        self.run_then(
            move |n| async move { n.set_display_name(Some(name)).await },
            |()| Msg::Info("Name saved".into()),
        );
        cx.notify();
    }

    pub fn close_overlays(&mut self, cx: &mut Context<Self>) {
        // Dismissing the welcome sheet still settles on a name.
        if self.sheet == Some(Sheet::Welcome) {
            return self.finish_welcome(None, cx);
        }
        self.sheet = None;
        self.pending_import = None;
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
            .unwrap_or_else(|| self.shown_id(&key))
    }

    /// Short form of an ID for display, or a mask when IDs are hidden.
    pub fn shown_id(&self, key: &str) -> String {
        if self.prefs.hide_ids && !self.revealed_ids.contains(key) {
            "••••••…••••".into()
        } else {
            format::short_id(key)
        }
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
        let host = self.inputs.svc_host.read(cx).text().trim().to_string();
        let host = if host.is_empty() {
            None
        } else {
            match host.trim_start_matches('[').trim_end_matches(']').parse() {
                Ok(ip) => Some(ip),
                Err(_) => {
                    self.show_error("Address must be an IP, e.g. 192.168.1.20 or ::1", cx);
                    return;
                }
            }
        };
        self.inputs.svc_name.update(cx, |i, cx| i.take(cx));
        self.inputs.svc_port.update(cx, |i, cx| i.take(cx));
        self.inputs.svc_host.update(cx, |i, cx| i.take(cx));
        let mut svc = Service::new(name, self.svc_protocol, port);
        svc.host = host;
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
        if self.sheet == Some(Sheet::Welcome) {
            return self.finish_welcome(name, cx);
        }
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

    // ---- import and export ----

    /// Ask where to save, then write the current config there.
    pub fn export_config(&mut self, cx: &mut Context<Self>) {
        let config = self
            .node
            .as_ref()
            .map_or_else(|| self.config.clone(), |n| n.config());
        let text = export::export(&config) + "\n";
        let dir = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| ".".into());
        let rx = cx.prompt_for_new_path(&dir, Some(&export::default_file_name()));
        cx.spawn(async move |this, cx| {
            let path = match rx.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let _ = this.update(cx, |s, cx| {
                        s.show_error(format!("Could not open the save dialog: {e:#}"), cx)
                    });
                    return;
                }
            };
            let res = std::fs::write(&path, text);
            let _ = this.update(cx, |s, cx| match res {
                Ok(()) => s.show_toast(format!("Config exported to {}", path.display()), false, cx),
                Err(e) => s.show_error(format!("Could not write {}: {e}", path.display()), cx),
            });
        })
        .detach();
    }

    /// Pick a file, check it, and open the confirmation sheet.
    pub fn start_import(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match rx.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(p) => p,
                    None => return,
                },
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let _ = this.update(cx, |s, cx| {
                        s.show_error(format!("Could not open the file dialog: {e:#}"), cx)
                    });
                    return;
                }
            };
            let file_name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let res = std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| export::parse(&bytes));
            let _ = this.update(cx, |s, cx| match res {
                Ok(import) => {
                    s.pending_import = Some(PendingImport {
                        file_name,
                        import,
                        mode: ImportMode::Merge,
                    });
                    s.open_sheet(Sheet::Import, cx);
                }
                Err(e) => s.show_error(format!("Can't import {file_name}: {e:#}"), cx),
            });
        })
        .detach();
    }

    pub fn set_import_mode(&mut self, mode: ImportMode, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.pending_import {
            p.mode = mode;
        }
        cx.notify();
    }

    /// Apply the confirmed import to the running node.
    pub fn finish_import(&mut self, cx: &mut Context<Self>) {
        self.sheet = None;
        let Some(PendingImport { import, mode, .. }) = self.pending_import.take() else {
            return;
        };
        self.run_then(
            move |n| async move { n.apply_config(import.resolve(&n.config(), mode)).await },
            Msg::Imported,
        );
        cx.notify();
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

    /// Progress of the download / install of the announced release, if started.
    pub fn update_phase(&self) -> Option<&UpdatePhase> {
        let r = self.update.available.as_ref()?;
        let d = self.update.download.as_ref()?;
        (d.version == r.version).then_some(&d.phase)
    }

    /// The banner's / settings' Install button: download, verify, then install and restart.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.update.available.clone() else {
            return;
        };
        match self.update_phase().cloned() {
            Some(UpdatePhase::Ready(staged)) => self.apply_update(staged, cx),
            Some(UpdatePhase::Downloading { .. }) => {
                if let Some(d) = self.update.download.as_mut() {
                    d.install_when_ready = true;
                }
            }
            Some(UpdatePhase::Installing) => {}
            None | Some(UpdatePhase::Failed(_)) => self.start_update_download(&release, true, cx),
        }
    }

    /// With "Install updates automatically", fetch a newly found release in the background.
    /// A failed download is not retried until the next release or a click on Install.
    fn auto_download_update(&mut self, cx: &mut Context<Self>) {
        if !self.prefs.auto_install || !self.prefs.check_updates || self.update_phase().is_some() {
            return;
        }
        if let Some(release) = self.update.available.clone() {
            self.start_update_download(&release, false, cx);
        }
    }

    fn start_update_download(&mut self, release: &Release, install: bool, cx: &mut Context<Self>) {
        let Some(plan) = update::plan(release) else {
            return;
        };
        let version = release.version.clone();
        self.update.download = Some(UpdateDownload {
            version: version.clone(),
            phase: UpdatePhase::Downloading {
                done: 0,
                total: None,
            },
            install_when_ready: install,
        });
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let progress_tx = tx.clone();
            let progress_version = version.clone();
            let mut sent = 0u64;
            let progress = move |done: u64, total: Option<u64>| {
                // A message per 256 KiB is plenty for a progress line.
                if done == 0 || done - sent >= 256 * 1024 || Some(done) == total {
                    sent = done;
                    let _ = progress_tx.send(Msg::UpdateProgress {
                        version: progress_version.clone(),
                        done,
                        total,
                    });
                }
            };
            let result = update::prepare(plan, progress)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::UpdatePrepared { version, result });
        });
        cx.notify();
    }

    fn on_update_prepared(
        &mut self,
        version: String,
        result: Result<Staged, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self
            .update
            .download
            .as_mut()
            .filter(|d| d.version == version)
        else {
            return;
        };
        match result {
            Ok(staged) => {
                d.phase = UpdatePhase::Ready(staged.clone());
                if d.install_when_ready {
                    self.apply_update(staged, cx);
                }
            }
            Err(e) => {
                tracing::warn!("update to {version} failed: {e}");
                d.phase = UpdatePhase::Failed(e);
            }
        }
    }

    /// Put the update in place and quit; `main` relaunches it after the clean shutdown.
    pub fn apply_update(&mut self, staged: Staged, cx: &mut Context<Self>) {
        let phase = match update::commit(&staged) {
            Ok(()) => {
                self.pending_install = Some(staged);
                // Quit outside this entity update: the quit handler reads `Root`.
                cx.defer(|cx| cx.quit());
                UpdatePhase::Installing
            }
            Err(e) => {
                tracing::warn!("installing the update failed: {e:#}");
                UpdatePhase::Failed(format!("{e:#}"))
            }
        };
        if let Some(d) = self.update.download.as_mut() {
            d.phase = phase;
        }
        cx.notify();
    }

    pub fn toggle_auto_install(&mut self, cx: &mut Context<Self>) {
        self.prefs.auto_install = !self.prefs.auto_install;
        self.save_prefs(cx);
        self.auto_download_update(cx);
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
