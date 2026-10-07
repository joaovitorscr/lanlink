//! lanlink desktop UI. A tokio runtime on a background thread owns the `Node`; results and
//! `NodeEvent`s flow back to the single `AppState` view over an unbounded channel.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, UdpSocket};

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, ClipboardItem, Context, ElementId,
    FocusHandle, Hsla, KeyDownEvent, MouseButton, SharedString, Window, WindowBounds,
    WindowOptions,
};
use lanlink_core::{
    ActiveTunnel, Config, ConnState, Node, NodeEvent, NodeId, PeerInfo, Protocol, Service,
};
use tokio::sync::{broadcast, mpsc};

const BG: u32 = 0x16181d;
const PANEL: u32 = 0x1f2228;
const BORDER: u32 = 0x2e323a;
const TEXT: u32 = 0xe6e8eb;
const MUTED: u32 = 0x8b919c;
const ACCENT: u32 = 0x3b82f6;
const DANGER: u32 = 0xdc4b4b;

/// Messages from the tokio side to the UI.
enum Msg {
    Started(Node),
    StartFailed(String),
    Event(NodeEvent),
    Resync,
    Error(String),
}

/// Minimal single-line text input state; rendered and fed keys by `AppState`.
struct TextInput {
    text: String,
    placeholder: &'static str,
    focus: FocusHandle,
}

impl TextInput {
    fn new(placeholder: &'static str, cx: &mut App) -> Self {
        Self {
            text: String::new(),
            placeholder,
            focus: cx.focus_handle(),
        }
    }

    fn handle_key(&mut self, ev: &KeyDownEvent, cx: &mut App) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || ks.modifiers.control {
            if ks.key == "v" {
                if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.text.extend(t.chars().filter(|c| !c.is_control()));
                }
            }
            return;
        }
        match ks.key.as_str() {
            "backspace" => {
                self.text.pop();
            }
            "enter" | "tab" | "escape" => {}
            _ => {
                if let Some(c) = &ks.key_char {
                    self.text.extend(c.chars().filter(|c| !c.is_control()));
                }
            }
        }
    }

    fn take(&mut self) -> String {
        std::mem::take(&mut self.text).trim().to_string()
    }
}

#[derive(Clone, Copy)]
enum Field {
    PeerId,
    PeerName,
    SvcName,
    SvcPort,
}

struct AppState {
    rt: tokio::runtime::Handle,
    tx: mpsc::UnboundedSender<Msg>,
    node: Option<Node>,
    status: Option<String>,
    fatal: Option<String>,
    peers: Vec<PeerInfo>,
    tunnels: Vec<ActiveTunnel>,
    config: Config,
    peer_id: TextInput,
    peer_name: TextInput,
    svc_name: TextInput,
    svc_port: TextInput,
    svc_udp: bool,
}

impl AppState {
    fn new(rt: tokio::runtime::Handle, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                if this.update(cx, |s, cx| s.handle(msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Start the node. A panic inside core (e.g. todo!()) surfaces as a JoinError.
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

        Self {
            rt,
            tx,
            node: None,
            status: Some("Starting node…".into()),
            fatal: None,
            peers: Vec::new(),
            tunnels: Vec::new(),
            config: Config::default(),
            peer_id: TextInput::new("Peer NodeId", cx),
            peer_name: TextInput::new("Name (optional)", cx),
            svc_name: TextInput::new("Service name", cx),
            svc_port: TextInput::new("Port", cx),
            svc_udp: false,
        }
    }

    fn handle(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Started(node) => self.on_started(node),
            Msg::StartFailed(e) => {
                self.fatal = Some(e);
                self.status = None;
            }
            Msg::Event(ev) => match ev {
                NodeEvent::PeerStateChanged(p) => self.upsert_peer(p),
                NodeEvent::TunnelOpened(t) => self.add_tunnel(t),
                NodeEvent::TunnelClosed(t) => self.tunnels.retain(|x| !same_tunnel(x, &t)),
                NodeEvent::Error(e) => self.status = Some(e),
            },
            Msg::Resync => self.resync(),
            Msg::Error(e) => self.status = Some(e),
        }
        cx.notify();
    }

    fn on_started(&mut self, node: Node) {
        self.status = None;
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
        self.node = Some(node.clone());
        self.resync();
        for id in self.config.allowed_peers.clone() {
            if let Ok(peer) = id.parse::<NodeId>() {
                self.run(move |n| async move { n.connect(peer).await.map(|_| ()) });
            }
        }
    }

    fn resync(&mut self) {
        if let Some(node) = &self.node {
            self.config = node.config();
            self.peers = node.peers();
            self.tunnels = node.tunnels();
        }
    }

    fn upsert_peer(&mut self, p: PeerInfo) {
        match self.peers.iter_mut().find(|x| x.id == p.id) {
            Some(slot) => *slot = p,
            None => self.peers.push(p),
        }
    }

    fn add_tunnel(&mut self, t: ActiveTunnel) {
        if !self.tunnels.iter().any(|x| same_tunnel(x, &t)) {
            self.tunnels.push(t);
        }
    }

    /// Run an async node operation on the tokio runtime, reporting errors to the status line.
    fn run<F, Fut>(&self, f: F)
    where
        F: FnOnce(Node) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Some(node) = self.node.clone() else {
            return;
        };
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let res = tokio::spawn(f(node)).await;
            let msg = match res {
                Ok(Ok(())) => Msg::Resync,
                Ok(Err(e)) => Msg::Error(format!("{e:#}")),
                Err(e) => Msg::Error(panic_text(e)),
            };
            let _ = tx.send(msg);
        });
    }

    fn save_config(&mut self, config: Config) {
        self.config = config.clone();
        self.run(move |n| async move { n.update_config(config).await });
    }

    fn connect_service(&mut self, peer: NodeId, svc: Service) {
        let local = if port_free(svc.port, svc.protocol) {
            svc.port
        } else {
            0
        };
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, local));
        let tx = self.tx.clone();
        self.run(move |n| async move {
            let t = n.open_tunnel(peer, &svc.name, addr).await?;
            let _ = tx.send(Msg::Event(NodeEvent::TunnelOpened(t)));
            Ok(())
        });
    }

    fn close_tunnel(&mut self, t: ActiveTunnel) {
        self.tunnels.retain(|x| !same_tunnel(x, &t));
        self.run(move |n| async move { n.close_tunnel(&t).await });
    }

    fn add_peer(&mut self) {
        let id = self.peer_id.take();
        let name = self.peer_name.take();
        let peer = match id.parse::<NodeId>() {
            Ok(p) => p,
            Err(e) => {
                self.status = Some(format!("Invalid NodeId: {e}"));
                return;
            }
        };
        let mut config = self.config.clone();
        let key = peer.to_string();
        if !config.allowed_peers.contains(&key) {
            config.allowed_peers.push(key.clone());
        }
        if !name.is_empty() {
            config.peer_names.insert(key, name);
        }
        self.config = config.clone();
        self.run(move |n| async move {
            n.update_config(config).await?;
            n.connect(peer).await.map(|_| ())
        });
    }

    fn add_service(&mut self) {
        let port = match self.svc_port.text.trim().parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => {
                self.status = Some("Port must be 1-65535".into());
                return;
            }
        };
        let name = self.svc_name.take();
        if name.is_empty() {
            self.status = Some("Service name required".into());
            return;
        }
        self.svc_port.take();
        let protocol = if self.svc_udp {
            Protocol::Udp
        } else {
            Protocol::Tcp
        };
        let mut config = self.config.clone();
        config.services.retain(|s| s.name != name);
        config.services.push(Service {
            name,
            protocol,
            port,
        });
        self.save_config(config);
    }

    fn remove_service(&mut self, name: String) {
        let mut config = self.config.clone();
        config.services.retain(|s| s.name != name);
        self.save_config(config);
    }

    fn input_mut(&mut self, f: Field) -> &mut TextInput {
        match f {
            Field::PeerId => &mut self.peer_id,
            Field::PeerName => &mut self.peer_name,
            Field::SvcName => &mut self.svc_name,
            Field::SvcPort => &mut self.svc_port,
        }
    }

    // ---- rendering ----

    fn render_input(
        &mut self,
        f: Field,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let input = self.input_mut(f);
        let focused = input.focus.is_focused(window);
        let focus = input.focus.clone();
        let (content, color) = if input.text.is_empty() {
            (input.placeholder.to_string(), MUTED)
        } else {
            (input.text.clone(), TEXT)
        };
        div()
            .track_focus(&input.focus)
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(BG))
            .border_1()
            .border_color(rgb(if focused { ACCENT } else { BORDER }))
            .text_color(rgb(color))
            .on_mouse_down(MouseButton::Left, move |_, window, _| focus.focus(window))
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "enter" {
                    match f {
                        Field::PeerId | Field::PeerName => this.add_peer(),
                        Field::SvcName | Field::SvcPort => this.add_service(),
                    }
                } else {
                    this.input_mut(f).handle_key(ev, cx);
                }
                cx.notify();
            }))
            .child(format!("{content}{}", if focused { "|" } else { "" }))
    }

    fn render_peer(&self, ix: usize, p: &PeerInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let key = p.id.to_string();
        let name = p
            .name
            .clone()
            .or_else(|| self.config.peer_names.get(&key).cloned())
            .unwrap_or_else(|| short(&key));
        let (label, color) = match p.state {
            ConnState::Disconnected => ("Disconnected", 0x6b7280),
            ConnState::Connecting => ("Connecting", 0xeab308),
            ConnState::Relayed => ("Relayed", 0xf97316),
            ConnState::Direct => ("Direct", 0x22c55e),
        };
        let st = p.stats;
        let latency = if st.samples > 0 {
            format!("{:.1} ms", st.last_ms)
        } else {
            p.latency_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_default()
        };
        // Rolling stats over the last minute of 1s pings.
        let stats_line = (st.samples > 0).then(|| {
            format!(
                "last {}s  min {:.1}  avg {:.1}  max {:.1}  jitter {:.1} ms",
                st.samples, st.min_ms, st.avg_ms, st.max_ms, st.jitter_ms
            )
        });
        let peer = p.id;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(rgb(BG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(name))
                    .child(div().text_color(rgb(MUTED)).child(latency))
                    .child(
                        div()
                            .px_2()
                            .rounded_md()
                            .text_xs()
                            .bg(Hsla::from(rgb(color)).opacity(0.2))
                            .text_color(rgb(color))
                            .child(label),
                    ),
            )
            .when_some(stats_line, |el, line| {
                el.child(div().text_xs().text_color(rgb(MUTED)).child(line))
            })
            .children(p.services.iter().enumerate().map(|(si, s)| {
                let svc = s.clone();
                row()
                    .text_color(rgb(MUTED))
                    .child(div().flex_1().child(format!(
                        "{} · {} {}",
                        s.name,
                        proto(s.protocol),
                        s.port
                    )))
                    .child(
                        button(id("connect", ix * 1000 + si), "Connect", ACCENT).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.connect_service(peer, svc.clone());
                                cx.notify();
                            }),
                        ),
                    )
            }))
    }
}

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm();

        if let Some(err) = &self.fatal {
            return root
                .child(div().text_lg().child("lanlink failed to start"))
                .child(div().text_color(rgb(DANGER)).child(err.clone()));
        }

        // Header
        let id_text = self.node.as_ref().map(|n| n.id().to_string());
        let header = row()
            .child(div().text_lg().flex_1().child("lanlink"))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .child(id_text.as_deref().map(short).unwrap_or_else(|| "…".into())),
            )
            .when_some(id_text, |el, full| {
                el.child(button("copy-id", "Copy", BORDER).on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(full.clone()))
                }))
            });

        // Peers
        let peers = section("Peers").children(
            self.peers
                .clone()
                .iter()
                .enumerate()
                .map(|(ix, p)| self.render_peer(ix, p, cx)),
        );
        let peers = if self.peers.is_empty() {
            peers.child(empty("No peers yet"))
        } else {
            peers
        };

        // Add peer
        let add_peer = section("Add peer")
            .child(row().child(self.render_input(Field::PeerId, window, cx)))
            .child(
                row()
                    .child(self.render_input(Field::PeerName, window, cx))
                    .child(button("add-peer", "Add", ACCENT).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.add_peer();
                            cx.notify();
                        },
                    ))),
            );

        // My services
        let services = section("My services")
            .children(self.config.services.iter().enumerate().map(|(ix, s)| {
                let name = s.name.clone();
                row()
                    .child(div().flex_1().child(s.name.clone()))
                    .child(div().text_color(rgb(MUTED)).child(format!(
                        "{} {}",
                        proto(s.protocol),
                        s.port
                    )))
                    .child(
                        button(id("rm-svc", ix), "Remove", DANGER).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.remove_service(name.clone());
                                cx.notify();
                            },
                        )),
                    )
            }))
            .child(
                row()
                    .child(self.render_input(Field::SvcName, window, cx))
                    .child(div().w(px(70.)).flex().child(self.render_input(
                        Field::SvcPort,
                        window,
                        cx,
                    )))
                    .child(
                        button("proto", if self.svc_udp { "UDP" } else { "TCP" }, BORDER).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.svc_udp = !this.svc_udp;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(button("add-svc", "Add", ACCENT).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.add_service();
                            cx.notify();
                        },
                    ))),
            );

        // Active tunnels
        let tunnels =
            section("Active tunnels").children(self.tunnels.iter().enumerate().map(|(ix, t)| {
                let tunnel = t.clone();
                row()
                    .child(div().flex_1().child(t.service.clone()))
                    .child(div().text_color(rgb(MUTED)).child(t.local_addr.to_string()))
                    .child(
                        button(id("close", ix), "Close", DANGER).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.close_tunnel(tunnel.clone());
                                cx.notify();
                            },
                        )),
                    )
            }));
        let tunnels = if self.tunnels.is_empty() {
            tunnels.child(empty("No active tunnels"))
        } else {
            tunnels
        };

        root.child(header)
            .when_some(self.status.clone(), |el, s| {
                el.child(div().text_color(rgb(MUTED)).child(s))
            })
            .child(peers)
            .child(add_peer)
            .child(services)
            .child(tunnels)
    }
}

fn row() -> gpui::Div {
    div().flex().items_center().gap_2()
}

fn section(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(title.to_uppercase()),
        )
}

fn empty(text: &'static str) -> impl IntoElement {
    div().text_color(rgb(MUTED)).child(text)
}

fn button(id: impl Into<ElementId>, label: &'static str, color: u32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(color))
        .text_color(rgb(0xffffff))
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .child(label)
}

fn id(prefix: &str, ix: usize) -> ElementId {
    ElementId::Name(SharedString::from(format!("{prefix}-{ix}")))
}

fn proto(p: Protocol) -> &'static str {
    match p {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    }
}

fn short(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…{}", &id[..6], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

fn same_tunnel(a: &ActiveTunnel, b: &ActiveTunnel) -> bool {
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

fn panic_text(e: tokio::task::JoinError) -> String {
    match e.try_into_panic() {
        Ok(p) => p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .map(|s| format!("panic: {s}"))
            .unwrap_or_else(|| "panic".into()),
        Err(e) => e.to_string(),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    // Keep the runtime alive on its own thread for the life of the process.
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(420.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| AppState::new(handle, cx)),
        )
        .expect("open window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
