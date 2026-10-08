//! Networks pane: request banners, one card per network with its members, direct peers,
//! and your ID. Each member and network has a ⋯ menu.

use gpui::{div, prelude::*, px, ClipboardItem, Context, FontWeight, Hsla};
use lanlink_core::{ConnState, Network, NetworkColor, NodeId, PeerInfo, PeerRequest};

use crate::format;
use crate::state::{MenuKind, Root, Sheet};
use crate::theme::Theme;
use crate::widgets::*;

/// Badge color of a network.
pub fn network_color(t: &Theme, c: NetworkColor) -> Hsla {
    match c {
        NetworkColor::Blue => t.blue,
        NetworkColor::Green => t.green,
        NetworkColor::Orange => t.orange,
        NetworkColor::Purple => t.purple,
        NetworkColor::Pink => gpui::rgb(0xff375f).into(),
        NetworkColor::Red => t.red,
        NetworkColor::Teal => t.teal,
        NetworkColor::Gray => t.fg2,
    }
}

/// Label, text color and dot color of a connection state.
fn state_style(t: &Theme, s: ConnState) -> (&'static str, Hsla, Hsla) {
    match s {
        ConnState::Disconnected => ("Offline", t.fg2, t.fg3),
        ConnState::Connecting => ("Connecting", t.orange, t.orange),
        ConnState::Relayed => ("Relayed", t.orange, t.orange),
        ConnState::Direct => ("Direct", t.green, t.green),
    }
}

impl Root {
    pub fn render_peers(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let id = self.my_id().map(|id| id.to_string());
        let networks: Vec<Network> = self
            .config
            .networks
            .iter()
            .filter(|n| !n.leaving)
            .cloned()
            .collect();
        let direct = self.direct_peers();
        let n = direct.len();

        col()
            .child(pane_header(
                &t,
                "Networks",
                "Each network is a group of friends. Anyone in a network can reach the services the others share.",
            ))
            .children(self.render_requests(&t, cx))
            .when(networks.is_empty(), |el| {
                el.child(glabel(&t, "Networks")).child(group(&t).child(empty(
                    &t,
                    "No networks yet. Create one with New Network, or join a friend's with their invite code.",
                )))
            })
            .children(networks.iter().map(|net| self.render_network(&t, net, cx)))
            .child(
                row()
                    .child(glabel(&t, "Direct peers").flex_1())
                    .child(
                        button(&t, "add-peer", Some("user-plus"), "Add peer", ButtonKind::Secondary)
                            .mt_3()
                            .on_click(cx.listener(|this, _, _, cx| this.open_sheet(Sheet::AddPeer, cx))),
                    ),
            )
            .child(
                group(&t)
                    .when(direct.is_empty(), |el| {
                        el.child(empty(&t, "Friends you add by their ID, outside any network, show up here."))
                    })
                    .children(direct.iter().enumerate().map(|(i, p)| {
                        let name = self.peer_name(p.id);
                        self.render_peer(&t, p, name, MenuKind::Peer(p.id), false, i + 1 == n, cx)
                    })),
            )
            .child(glabel(&t, "Your ID"))
            .child(
                group(&t).child(
                    group_row(&t, true)
                        .child(
                            col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.))
                                        .child(id.clone().unwrap_or_else(|| "Starting…".into())),
                                )
                                .child(small(&t, "Friends can also add you directly, outside any network.")),
                        )
                        .when_some(id, |el, full| {
                            el.child(
                                button(&t, "copy-full-id", Some("copy"), "Copy", ButtonKind::Secondary)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
                                        this.show_toast("ID copied", false, cx);
                                    })),
                            )
                        }),
                ),
            )
    }

    /// Peers allowed directly (by ID) that share no network with us.
    pub fn direct_peers(&self) -> Vec<PeerInfo> {
        let in_network = |id: &NodeId| {
            self.config
                .networks
                .iter()
                .any(|n| n.active() && n.has_member(id))
        };
        self.peers
            .iter()
            .filter(|p| self.config.allowed_peers.contains(&p.id.to_string()) && !in_network(&p.id))
            .cloned()
            .collect()
    }

    fn render_requests(&mut self, t: &Theme, cx: &mut Context<Self>) -> Vec<impl IntoElement> {
        self.requests
            .clone()
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let key = r.id.to_string();
                let who = r.name.clone().unwrap_or_else(|| format::short_id(&key));
                let title = match &r.network {
                    Some(net) => {
                        let net = self
                            .network(net)
                            .map_or("your network".into(), |n| n.name.clone());
                        format!("{who} wants to join {net}")
                    }
                    None => format!("{who} wants to connect"),
                };
                let (deny, allow): (PeerRequest, PeerRequest) = (r.clone(), r);
                row()
                    .mt_3p5()
                    .px_3p5()
                    .py_2p5()
                    .rounded(px(10.))
                    .bg(t.blue.opacity(0.1))
                    .border_1()
                    .border_color(t.blue.opacity(0.3))
                    .child(avatar(t, &who, &key, 30.))
                    .child(title_sub(t, title, Some(format::short_id(&key))))
                    .child(
                        button(t, eid("deny", i), None, "Ignore", ButtonKind::Secondary).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.respond_request(deny.clone(), false, cx)
                            }),
                        ),
                    )
                    .child(
                        button(t, eid("allow", i), None, "Allow", ButtonKind::Primary).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.respond_request(allow.clone(), true, cx)
                            }),
                        ),
                    )
            })
            .collect()
    }

    fn render_network(
        &mut self,
        t: &Theme,
        net: &Network,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let me = self.my_id();
        let owner = me.is_some_and(|me| net.is_owner(&me));
        let id = net.id.clone();
        let owner_name = net
            .owner_id()
            .map(|o| self.member_name(net, o))
            .unwrap_or_default();
        let others: Vec<PeerInfo> = net
            .member_ids()
            .filter(|m| Some(*m) != me)
            .map(|m| self.peer_info(m))
            .collect();
        let online = others
            .iter()
            .filter(|p| p.state != ConnState::Disconnected)
            .count();
        let best = others
            .iter()
            .map(|p| p.state)
            .find(|s| *s == ConnState::Direct)
            .or_else(|| {
                others
                    .iter()
                    .map(|p| p.state)
                    .find(|s| *s == ConnState::Relayed)
            });
        let members = net.members.len();
        let sub = if net.pending {
            format!("Waiting for {owner_name} to approve you")
        } else {
            let who = if owner {
                "you own this network".to_string()
            } else {
                format!("owned by {owner_name}")
            };
            let on = if online == 0 {
                "nobody else online".to_string()
            } else {
                format!("{} online", online + usize::from(self.network.online))
            };
            format!(
                "{members} member{} · {on} · {who}",
                if members == 1 { "" } else { "s" }
            )
        };
        let code = (!net.pending).then(|| net.invite_code()).flatten();
        let collapsed = self.collapsed.contains(&net.id);
        let menu_open = self.menu_is(&MenuKind::Network(id.clone()));
        let n_rows = net.members.len() + usize::from(owner);

        let head = row()
            .mt_4()
            .mb_1p5()
            .ml_0p5()
            .gap_2p5()
            .child(tile("users", network_color(t, net.color), 26., t.white))
            .child(title_sub(t, net.name.clone(), Some(sub)))
            .when_some(best, |el, s| {
                let (label, color, _) = state_style(t, s);
                el.child(pill(label, color))
            })
            .when(owner || code.is_some(), |el| {
                let id = id.clone();
                el.child(
                    button(
                        t,
                        eid("invite", &id),
                        Some("link"),
                        "Invite code",
                        ButtonKind::Secondary,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_sheet(Sheet::Invite(id.clone()), cx)
                    })),
                )
            })
            .child({
                let id = id.clone();
                icon_button(t, eid("net-menu", &id), "dots")
                    .when(menu_open, |el| el.bg(t.hover))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(MenuKind::Network(id.clone()), at, cx);
                    }))
            });

        let body = (!collapsed).then(|| {
            if net.pending {
                return group(t).child(empty(
                    t,
                    format!("You're in once {owner_name} approves your request. lanlink connects to them automatically."),
                ));
            }
            let mut rows: Vec<gpui::AnyElement> = Vec::new();
            for (i, m) in net.members.iter().enumerate() {
                let Ok(mid) = m.id.parse::<NodeId>() else { continue };
                let last = i + 1 == n_rows;
                let is_owner = net.is_owner(&mid);
                if Some(mid) == me {
                    rows.push(self.render_me(t, net, is_owner, last).into_any_element());
                } else {
                    let p = self.peer_info(mid);
                    let name = self.member_name(net, mid);
                    rows.push(
                        self.render_peer(t, &p, name, MenuKind::Member(id.clone(), mid), is_owner, last, cx)
                            .into_any_element(),
                    );
                }
            }
            if owner {
                let id = id.clone();
                rows.push(
                    group_row(t, true)
                        .id(eid("invite-row", &id))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.hover))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(30.))
                                .rounded_full()
                                .border_1()
                                .border_color(t.blue)
                                .child(ic("user-plus", 15., t.blue)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_color(t.blue)
                                .font_weight(FontWeight::MEDIUM)
                                .child("Invite a friend…"),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_sheet(Sheet::Invite(id.clone()), cx)
                        }))
                        .into_any_element(),
                );
            }
            group(t).children(rows)
        });

        col().child(head).children(body)
    }

    /// Our own row in a network.
    fn render_me(&self, t: &Theme, net: &Network, owner: bool, last: bool) -> impl IntoElement {
        let me = self
            .config
            .display_name
            .clone()
            .unwrap_or_else(|| "You".into());
        let key = self.my_id().map(|i| i.to_string()).unwrap_or_default();
        let sharing = self
            .config
            .services
            .iter()
            .filter(|s| s.enabled && (s.networks.is_empty() || s.networks.contains(&net.id)))
            .count();
        let mut sub = vec![if self.network.online {
            "Online"
        } else {
            "Offline"
        }
        .to_string()];
        if sharing > 0 && (owner || net.policy.members_share) {
            sub.push(format!(
                "sharing {sharing} service{}",
                if sharing == 1 { "" } else { "s" }
            ));
        }
        group_row(t, last)
            .child(avatar(t, &me, &key, 30.))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        row()
                            .gap_1p5()
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).child(me))
                            .child(pill("you", t.fg2)),
                    )
                    .child(
                        row()
                            .gap_1p5()
                            .text_size(px(11.5))
                            .text_color(t.fg2)
                            .child(dot(if self.network.online { t.green } else { t.fg3 }))
                            .child(div().truncate().child(sub.join(" · "))),
                    ),
            )
            .when(owner, |el| el.child(pill("Owner", t.blue)))
    }

    /// Live info of a peer, or an offline placeholder.
    fn peer_info(&self, id: NodeId) -> PeerInfo {
        self.peers
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .unwrap_or(PeerInfo {
                id,
                name: None,
                state: ConnState::Disconnected,
                latency_ms: None,
                stats: Default::default(),
                last_error: None,
                connected_since: None,
                inbound: false,
                bytes_sent: 0,
                bytes_received: 0,
                services: Vec::new(),
            })
    }

    /// Our local name for a member, else the name it has in the network.
    pub fn member_name(&self, net: &Network, id: NodeId) -> String {
        let key = id.to_string();
        if let Some(n) = self.config.peer_names.get(&key) {
            return n.clone();
        }
        net.members
            .iter()
            .find(|m| m.id == key && !m.name.is_empty())
            .map(|m| m.name.clone())
            .unwrap_or_else(|| self.peer_name(id))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_peer(
        &mut self,
        t: &Theme,
        p: &PeerInfo,
        name: String,
        menu: MenuKind,
        owner: bool,
        last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = p.id;
        let key = id.to_string();
        let (label, color, dot_color) = state_style(t, p.state);
        let st = p.stats;
        let mut parts = vec![label.to_string()];
        if st.samples > 0 {
            parts.push(format!("{:.0} ms", st.last_ms));
            parts.push(format!("jitter {:.0} ms", st.jitter_ms));
        } else if let Some(ms) = p.latency_ms {
            parts.push(format!("{ms} ms"));
        }
        let shared = p.services.iter().filter(|s| s.enabled).count();
        if p.state != ConnState::Disconnected && shared > 0 {
            parts.push(format!(
                "sharing {shared} service{}",
                if shared == 1 { "" } else { "s" }
            ));
        }
        if p.state != ConnState::Disconnected && (p.bytes_sent > 0 || p.bytes_received > 0) {
            parts.push(format!(
                "↑ {} ↓ {}",
                format::bytes(p.bytes_sent),
                format::bytes(p.bytes_received)
            ));
        }
        if let Some(e) = &p.last_error {
            parts.push(e.clone());
        }
        let sub = parts.join(" · ");
        let row_key = match &menu {
            MenuKind::Member(net, _) => format!("{net}-{key}"),
            _ => key.clone(),
        };

        let menu_open = self.menu_is(&menu);
        group_row(t, last)
            .child(avatar(t, &name, &key, 30.))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(name))
                    .child(
                        row()
                            .gap_1p5()
                            .text_size(px(11.5))
                            .text_color(t.fg2)
                            .child(dot(dot_color))
                            .child(div().truncate().child(sub)),
                    ),
            )
            .when(p.state == ConnState::Disconnected, |el| {
                el.child(
                    button(
                        t,
                        eid("reconnect", &row_key),
                        None,
                        "Reconnect",
                        ButtonKind::Secondary,
                    )
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.run(move |n| async move { n.reconnect(id).await })
                    })),
                )
            })
            .when(owner, |el| el.child(pill("Owner", t.blue)))
            .when(!owner && p.state != ConnState::Disconnected, |el| {
                el.child(pill(
                    if p.state == ConnState::Direct {
                        "Connected"
                    } else {
                        label
                    },
                    color,
                ))
            })
            .child(
                icon_button(t, eid("menu", &row_key), "dots")
                    .when(menu_open, |el| el.bg(t.hover))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(menu.clone(), at, cx);
                    })),
            )
    }
}

/// Large title plus one line of explanation at the top of a pane.
pub fn pane_header(t: &Theme, title: &str, lede: &str) -> impl IntoElement {
    col()
        .mt_1()
        .child(
            div()
                .text_size(px(22.))
                .font_weight(FontWeight::BOLD)
                .child(title.to_string()),
        )
        .child(small(t, lede.to_string()))
}
