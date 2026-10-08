//! Tunnels pane: services your peers share, grouped by peer, with the local address.

use gpui::{div, prelude::*, px, ClipboardItem, Context, FontWeight};
use lanlink_core::{ConnState, NodeId, Service};

use crate::format;
use crate::state::Root;
use crate::theme::Theme;
use crate::views::overlays::proto_label;
use crate::views::peers::pane_header;
use crate::widgets::*;

impl Root {
    pub fn render_tunnels(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let peers = self.peers.value.clone();

        let mut out = col().child(pane_header(
            &t,
            "Tunnels",
            "Services your peers share. Connect to the local address as if it were running on your own computer.",
        ));

        if peers.is_empty() {
            return out
                .child(glabel(&t, "From peers"))
                .child(group(&t).child(empty(
                    &t,
                    "Add a peer first. Their shared services show up here.",
                )));
        }

        for p in peers {
            let name = self.peer_name(p.id);
            let services: Vec<Service> = p.services.iter().filter(|s| s.enabled).cloned().collect();
            let online = p.state != ConnState::Disconnected;
            out = out.child(glabel(&t, format!("From {name}")));
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
            .tunnel_list()
            .into_iter()
            .find(|x| x.tunnel.peer == peer && x.tunnel.service == svc.name);
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
                                this.plain_tunnels
                                    .retain(|x| !crate::state::same_tunnel(x, &tunnel));
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
