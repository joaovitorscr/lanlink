//! Peers pane: requests banner, your ID, grouped peer list with a ⋯ menu per peer.

use gpui::{div, prelude::*, px, ClipboardItem, Context, FontWeight};
use lanlink_core::{ConnState, PeerInfo};

use crate::format;
use crate::state::{MenuKind, Root};
use crate::theme::Theme;
use crate::widgets::*;

impl Root {
    pub fn render_peers(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let id = self.node.as_ref().map(|n| n.id().to_string());
        let peers = self.peers.value.clone();
        let n = peers.len();

        col()
            .child(pane_header(
                &t,
                "Peers",
                "People who can connect to you, and who you connect to.",
            ))
            .children(self.render_requests(&t, cx))
            .child(glabel(&t, "Your ID"))
            .child(
                group(&t).child(
                    group_row(&t, true)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .child(id.clone().unwrap_or_else(|| "Starting…".into())),
                        )
                        .when_some(id, |el, full| {
                            el.child(
                                button(
                                    &t,
                                    "copy-full-id",
                                    Some("copy"),
                                    "Copy",
                                    ButtonKind::Secondary,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            full.clone(),
                                        ));
                                        this.show_toast("ID copied", false, cx);
                                    },
                                )),
                            )
                        }),
                ),
            )
            .child(glabel(&t, "Peers"))
            .child(
                group(&t)
                    .when(!self.peers.available, |el| {
                        el.child(empty(&t, "Peer list not available"))
                    })
                    .when(peers.is_empty() && self.peers.available, |el| {
                        el.child(empty(
                            &t,
                            "No peers yet. Use Add Peer, or send your ID to a friend.",
                        ))
                    })
                    .children(
                        peers
                            .iter()
                            .enumerate()
                            .map(|(i, p)| self.render_peer(&t, p, i + 1 == n, cx)),
                    ),
            )
    }

    fn render_requests(&mut self, t: &Theme, cx: &mut Context<Self>) -> Vec<impl IntoElement> {
        self.requests
            .value
            .clone()
            .into_iter()
            .map(|r| {
                let key = r.id.to_string();
                let who = r.name.clone().unwrap_or_else(|| format::short_id(&key));
                let id = r.id;
                row()
                    .mt_3p5()
                    .px_3p5()
                    .py_2p5()
                    .rounded(px(10.))
                    .bg(t.blue.opacity(0.1))
                    .border_1()
                    .border_color(t.blue.opacity(0.3))
                    .child(avatar(t, &who, &key, 30.))
                    .child(title_sub(
                        t,
                        format!("{who} wants to connect"),
                        Some(format::short_id(&key)),
                    ))
                    .child(
                        button(t, eid("deny", id), None, "Ignore", ButtonKind::Secondary).on_click(
                            cx.listener(move |this, _, _, cx| this.respond_request(id, false, cx)),
                        ),
                    )
                    .child(
                        button(t, eid("allow", id), None, "Allow", ButtonKind::Primary).on_click(
                            cx.listener(move |this, _, _, cx| this.respond_request(id, true, cx)),
                        ),
                    )
            })
            .collect()
    }

    fn render_peer(
        &mut self,
        t: &Theme,
        p: &PeerInfo,
        last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = p.id;
        let key = id.to_string();
        let name = self.peer_name(id);
        let (label, color, dot_color) = match p.state {
            ConnState::Disconnected => ("Offline", t.fg2, t.fg3),
            ConnState::Connecting => ("Connecting", t.orange, t.orange),
            ConnState::Relayed => ("Relayed", t.orange, t.orange),
            ConnState::Direct => ("Direct", t.green, t.green),
        };
        let st = p.stats;
        let mut parts = vec![label.to_string()];
        if st.samples > 0 {
            parts.push(format!("{:.0} ms", st.last_ms));
            parts.push(format!("jitter {:.0} ms", st.jitter_ms));
        } else if let Some(ms) = p.latency_ms {
            parts.push(format!("{ms} ms"));
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

        let menu_open = self.menu_is(&MenuKind::Peer(id));
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
                        eid("reconnect", id),
                        None,
                        "Reconnect",
                        ButtonKind::Secondary,
                    )
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.run(move |n| async move { n.reconnect(id).await })
                    })),
                )
            })
            .when(p.state != ConnState::Disconnected, |el| {
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
                icon_button(t, eid("menu", id), "dots")
                    .when(menu_open, |el| el.bg(t.hover))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(MenuKind::Peer(id), at, cx);
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
