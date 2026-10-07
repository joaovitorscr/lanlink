//! Peers tab: share our id, add a friend, peer cards with their services.

use std::time::SystemTime;

use gpui::{div, prelude::*, rgb, ClipboardItem, Context, Focusable};
use lanlink_core::{ConnState, PeerInfo};

use crate::format;
use crate::state::Root;
use crate::theme::*;
use crate::widgets::*;

impl Root {
    pub fn render_peers(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.node.as_ref().map(|n| n.id().to_string());
        let share = card(Some("Share your ID"))
            .child(small("Send this to your friend so they can add you."))
            .child(
                row()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .child(id.clone().unwrap_or_else(|| "Starting…".into())),
                    )
                    .when_some(id, |el, full| {
                        el.child(button("copy-full-id", "Copy", ACCENT).on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(full.clone()))
                            },
                        ))
                    }),
            );

        let add = card(Some("Add friend"))
            .child(row().child(self.inputs.peer_id.clone()))
            .child(
                row().child(self.inputs.peer_name.clone()).child(
                    button("add-peer", "Add", ACCENT)
                        .on_click(cx.listener(|this, _, _, cx| this.add_peer(cx))),
                ),
            );

        let peers = self.peers.value.clone();
        col()
            .gap_3()
            .child(share)
            .child(add)
            .when(!self.peers.available, |el| {
                el.child(muted("Peer list not available"))
            })
            .when(peers.is_empty() && self.peers.available, |el| {
                el.child(muted("No friends yet. Add one above."))
            })
            .children(peers.iter().map(|p| self.render_peer(p, cx)))
    }

    fn render_peer(&mut self, p: &PeerInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let id = p.id;
        let name = self.peer_name(id);
        let (label, color) = match p.state {
            ConnState::Disconnected => ("Disconnected", GREY),
            ConnState::Connecting => ("Connecting", YELLOW),
            ConnState::Relayed => ("Relayed", ORANGE),
            ConnState::Direct => ("Direct", GREEN),
        };
        let st = p.stats;
        let latency = if st.samples > 0 {
            format!("{:.1} ms", st.last_ms)
        } else {
            p.latency_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_default()
        };
        let stats_line = (st.samples > 0).then(|| {
            format!(
                "last {}s  min {:.1}  avg {:.1}  max {:.1}  jitter {:.1} ms",
                st.samples, st.min_ms, st.avg_ms, st.max_ms, st.jitter_ms
            )
        });
        let connected = p.connected_since.map(|since| {
            let d = SystemTime::now().duration_since(since).unwrap_or_default();
            format!("Connected for {}", format::duration(d))
        });
        let traffic = format!(
            "Sent {} · Received {}",
            format::bytes(p.bytes_sent),
            format::bytes(p.bytes_received)
        );

        let renaming = self
            .renaming
            .as_ref()
            .filter(|(rid, _)| *rid == id)
            .map(|(_, i)| i.clone());
        let confirming = self.confirm_remove == Some(id);

        let title =
            match renaming {
                Some(input) => row()
                    .flex_1()
                    .child(input)
                    .child(
                        button(eid("rename-save", id), "Save", ACCENT)
                            .on_click(cx.listener(|this, _, _, cx| this.finish_rename(cx))),
                    )
                    .child(button(eid("rename-cancel", id), "Cancel", BORDER).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.renaming = None;
                            cx.notify();
                        }),
                    )),
                None => row()
                    .flex_1()
                    .min_w_0()
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(muted(latency))
                    .child(badge(label, color)),
            };

        let actions = row()
            .child(
                button(eid("reconnect", id), "Reconnect", BORDER).on_click(cx.listener(
                    move |this, _, _, _| this.run(move |n| async move { n.reconnect(id).await }),
                )),
            )
            .child(
                button(eid("rename", id), "Rename", BORDER).on_click(cx.listener(
                    move |this, _, window, cx| {
                        let current = this.config.peer_names.get(&id.to_string()).cloned();
                        let input = this.start_rename(id, current.unwrap_or_default(), cx);
                        window.focus(&input.focus_handle(cx));
                        cx.notify();
                    },
                )),
            )
            .child(div().flex_1())
            .child(
                button(
                    eid("remove", id),
                    if confirming {
                        "Click again to remove"
                    } else {
                        "Remove"
                    },
                    if confirming { DANGER } else { BORDER },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.remove_peer(id, cx))),
            );

        let services = p.services.iter().filter(|s| s.enabled).map(|s| {
            let svc = s.clone();
            let open = self.open_tunnel(id, &s.name);
            let info = format!(
                "{} · {} {}{}",
                s.name,
                proto(s.protocol),
                s.port,
                if s.minecraft_lan { " · Minecraft" } else { "" }
            );
            let action = match open {
                Some(t) => {
                    let addr = t.local_addr.to_string();
                    row()
                        .child(small(format!("Open at {addr}")))
                        .child(
                            button(eid("copy-open", format!("{id}-{}", s.name)), "Copy", BORDER)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(addr.clone()))
                                }),
                        )
                        .into_any_element()
                }
                None => button(
                    eid("connect", format!("{id}-{}", s.name)),
                    "Connect",
                    ACCENT,
                )
                .on_click(cx.listener(move |this, _, _, _| this.connect_service(id, svc.clone())))
                .into_any_element(),
            };
            row()
                .pl_2()
                .border_l_2()
                .border_color(rgb(BORDER))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(MUTED))
                        .child(info),
                )
                .child(action)
        });

        card(None)
            .child(title)
            .when_some(stats_line, |el, l| el.child(small(l)))
            .when_some(connected, |el, c| el.child(small(c)))
            .child(small(traffic))
            .when_some(p.last_error.clone(), |el, e| {
                el.child(div().text_xs().text_color(rgb(AMBER)).child(e))
            })
            .child(actions)
            .children(services)
    }
}

pub fn proto(p: lanlink_core::Protocol) -> &'static str {
    match p {
        lanlink_core::Protocol::Tcp => "TCP",
        lanlink_core::Protocol::Udp => "UDP",
    }
}
