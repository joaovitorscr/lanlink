//! Tunnels pane: services your friends share, grouped by network (then direct peers), with
//! the local address.

use gpui::{div, prelude::*, px, ClipboardItem, Context, FontWeight};
use lanlink_core::{ConnState, Network, NodeId, PeerInfo, Service};

use crate::format;
use crate::state::Root;
use crate::theme::Theme;
use crate::views::overlays::proto_label;
use crate::views::peers::pane_header;
use crate::widgets::*;

impl Root {
    pub fn render_tunnels(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let me = self.my_id();
        let networks: Vec<Network> = self
            .config
            .networks
            .iter()
            .filter(|n| n.active())
            .cloned()
            .collect();

        let mut out = col().child(pane_header(
            &t,
            "Tunnels",
            "Services your friends share. Connect to the local address as if it were running on your own computer.",
        ));

        if networks.is_empty() && self.peers.is_empty() {
            return out
                .child(glabel(&t, "From peers"))
                .child(group(&t).child(empty(
                    &t,
                    "Join a network or add a peer first. Their shared services show up here.",
                )));
        }

        for net in &networks {
            let members: Vec<PeerInfo> = net
                .member_ids()
                .filter(|m| Some(*m) != me)
                .filter_map(|m| self.peers.iter().find(|p| p.id == m).cloned())
                .collect();
            let online = members.iter().any(|p| p.state != ConnState::Disconnected);
            let rows: Vec<(NodeId, Service)> = members
                .iter()
                .filter(|p| p.state != ConnState::Disconnected)
                .flat_map(|p| {
                    p.services
                        .iter()
                        .filter(|s| s.enabled && self.tunnel_networks(p.id, s).contains(&net.id))
                        .map(|s| (p.id, s.clone()))
                        .collect::<Vec<_>>()
                })
                .collect();
            let n = rows.len();
            out = out.child(glabel(&t, net.name.clone()));
            let g = group(&t)
                .when(!online, |el| {
                    el.child(empty(&t, "Nobody in this network is online."))
                })
                .when(online && rows.is_empty(), |el| {
                    el.child(empty(
                        &t,
                        "Nobody in this network is sharing anything right now.",
                    ))
                })
                .children(rows.into_iter().enumerate().map(|(i, (peer, s))| {
                    let name = self.member_name(net, peer);
                    self.render_tunnel_row(&t, peer, &name, s, i + 1 == n, cx)
                }));
            out = out.child(g);
        }

        // Peers added by ID: services shared with everyone.
        let direct: Vec<PeerInfo> = self
            .peers
            .iter()
            .filter(|p| self.config.allowed_peers.contains(&p.id.to_string()))
            .cloned()
            .collect();
        for p in direct {
            let name = self.peer_name(p.id);
            let services: Vec<Service> = p
                .services
                .iter()
                .filter(|s| s.enabled && self.tunnel_networks(p.id, s).is_empty())
                .cloned()
                .collect();
            let in_network = networks.iter().any(|n| n.has_member(&p.id));
            if in_network && services.is_empty() {
                continue;
            }
            let online = p.state != ConnState::Disconnected;
            out = out.child(glabel(&t, format!("Direct · from {name}")));
            let n = services.len();
            let g =
                group(&t)
                    .when(!online, |el| {
                        el.child(empty(
                            &t,
                            format!("{name} is offline. Tunnels come back when they reconnect."),
                        ))
                    })
                    .when(online && services.is_empty(), |el| {
                        el.child(empty(
                            &t,
                            format!("{name} is not sharing anything right now."),
                        ))
                    })
                    .when(online, |el| {
                        el.children(services.into_iter().enumerate().map(|(i, s)| {
                            self.render_tunnel_row(&t, p.id, &name, s, i + 1 == n, cx)
                        }))
                    });
            out = out.child(g);
        }
        out
    }

    /// Networks a peer's service is listed under. Empty = with the direct peers. A service
    /// shared with everyone goes to the direct peers if the peer is one, else to the first
    /// network we share with it.
    fn tunnel_networks(&self, peer: NodeId, s: &Service) -> Vec<String> {
        let ours = |id: &String| {
            self.config
                .networks
                .iter()
                .any(|n| &n.id == id && n.active() && n.has_member(&peer))
        };
        if !s.networks.is_empty() {
            // Listed once, under the first network.
            return s
                .networks
                .iter()
                .filter(|id| ours(id))
                .take(1)
                .cloned()
                .collect();
        }
        if self.config.allowed_peers.contains(&peer.to_string()) {
            return Vec::new();
        }
        self.config
            .networks
            .iter()
            .find(|n| n.active() && n.has_member(&peer))
            .map(|n| vec![n.id.clone()])
            .unwrap_or_default()
    }

    fn render_tunnel_row(
        &mut self,
        t: &Theme,
        peer: NodeId,
        peer_name: &str,
        svc: Service,
        last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self
            .tunnels
            .iter()
            .find(|x| x.tunnel.peer == peer && x.tunnel.service == svc.name)
            .cloned();
        let key = format!("{peer}-{}", svc.name);
        let icon = if svc.minecraft_lan {
            "pick"
        } else {
            "arrows-exchange"
        };
        let mut sub = vec![
            format!("Shared by {peer_name}"),
            proto_label(svc.protocol).into(),
        ];

        let row_el = group_row(t, last).child(tile(
            icon,
            if svc.minecraft_lan { t.green } else { t.purple },
            30.,
            t.white,
        ));

        match open {
            Some(info) => {
                let addr = info.tunnel.local_addr.to_string();
                let (up, down) = self.rate(peer, &svc.name);
                if info.connections > 0 || up > 0. || down > 0. {
                    sub.push(format!(
                        "{} active · ↑ {} ↓ {}",
                        info.connections,
                        format::rate(up),
                        format::rate(down)
                    ));
                }
                let tunnel = info.tunnel.clone();
                let saved = info.saved;
                let service = svc.name.clone();
                let port = info.tunnel.local_addr.port();
                row_el
                    .child(title_sub(t, svc.name.clone(), Some(sub.join(" · "))))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.blue)
                            .child(addr.clone()),
                    )
                    .child(
                        button(
                            t,
                            eid("copy-addr", &key),
                            Some("copy"),
                            "Copy",
                            ButtonKind::Secondary,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(addr.clone()));
                            this.show_toast("Address copied", false, cx);
                        })),
                    )
                    .child(toggle(t, eid("auto", &key), saved).on_click(cx.listener(
                        move |this, _, _, _| {
                            let service = service.clone();
                            if saved {
                                this.run(
                                    move |n| async move { n.forget_tunnel(peer, &service).await },
                                );
                            } else {
                                this.run(move |n| async move {
                                    n.save_tunnel(peer, &service, port, true).await.map(|_| ())
                                });
                            }
                        },
                    )))
                    .child(
                        icon_button(t, eid("close", &key), "x").on_click(cx.listener(
                            move |this, _, _, cx| {
                                let tunnel = tunnel.clone();
                                this.tunnels
                                    .retain(|x| !crate::state::same_tunnel(&x.tunnel, &tunnel));
                                this.run(move |n| async move { n.close_tunnel(&tunnel).await });
                                cx.notify();
                            },
                        )),
                    )
            }
            None => row_el
                .child(title_sub(t, svc.name.clone(), Some(sub.join(" · "))))
                .child(
                    button(
                        t,
                        eid("connect", &key),
                        None,
                        "Connect",
                        ButtonKind::Primary,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, _| this.connect_service(peer, svc.clone())),
                    ),
                ),
        }
    }
}
