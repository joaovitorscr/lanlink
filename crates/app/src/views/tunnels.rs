//! Tunnels tab: services of friends we are connected to, with live counters.

use gpui::{div, prelude::*, rgb, ClipboardItem, Context};

use crate::format;
use crate::state::Root;
use crate::theme::*;
use crate::views::peers::proto;
use crate::widgets::*;

impl Root {
    pub fn render_tunnels(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tunnels = self.tunnel_list();
        col()
            .gap_3()
            .when(!self.tunnels.available, |el| {
                el.child(small("Live traffic counters not available"))
            })
            .when(tunnels.is_empty(), |el| {
                el.child(muted(
                    "No open tunnels. Go to Peers and press Connect on one of your friend's services.",
                ))
            })
            .children(tunnels.into_iter().map(|t| {
                let peer = t.tunnel.peer;
                let service = t.tunnel.service.clone();
                let key = format!("{peer}-{service}");
                let addr = t.tunnel.local_addr.to_string();
                let port = t.tunnel.local_addr.port();
                let (up, down) = self.rate(peer, &service);
                let minecraft = self
                    .peers
                    .value
                    .iter()
                    .find(|p| p.id == peer)
                    .and_then(|p| p.services.iter().find(|s| s.name == service))
                    .is_some_and(|s| s.minecraft_lan)
                    || service.to_lowercase().contains("minecraft");

                let toggle_saved = {
                    let service = service.clone();
                    let saved = t.saved;
                    cx.listener(move |this, _, _, _| {
                        let service = service.clone();
                        if saved {
                            this.run(move |n| async move { n.forget_tunnel(peer, &service).await });
                        } else {
                            this.run(move |n| async move {
                                n.save_tunnel(peer, &service, port, true).await.map(|_| ())
                            });
                        }
                    })
                };
                let close = {
                    let tunnel = t.tunnel.clone();
                    cx.listener(move |this, _, _, cx| {
                        let tunnel = tunnel.clone();
                        this.plain_tunnels
                            .retain(|x| !crate::state::same_tunnel(x, &tunnel));
                        this.run(move |n| async move { n.close_tunnel(&tunnel).await });
                        cx.notify();
                    })
                };

                card(None)
                    .child(
                        row()
                            .child(div().flex_1().min_w_0().truncate().child(format!(
                                "{} · {}",
                                self.peer_name(peer),
                                service
                            )))
                            .child(muted(proto(t.protocol))),
                    )
                    .child(
                        row()
                            .child(div().flex_1().child(addr.clone()))
                            .child(button(eid("copy-addr", &key), "Copy", BORDER).on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(addr.clone()))
                                },
                            )),
                    )
                    .child(small(format!(
                        "{} connection{} · ↑ {} · ↓ {} · total ↑ {} ↓ {}",
                        t.connections,
                        if t.connections == 1 { "" } else { "s" },
                        format::rate(up),
                        format::rate(down),
                        format::bytes(t.bytes_up),
                        format::bytes(t.bytes_down)
                    )))
                    .when(minecraft, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(rgb(ACCENT))
                                .child(format!(
                                    "Join localhost:{port} in Multiplayer, or look for it in the LAN list"
                                )),
                        )
                    })
                    .child(
                        row()
                            .child(
                                switch(eid("auto-open", &key), t.saved, "Open automatically")
                                    .on_click(toggle_saved),
                            )
                            .child(div().flex_1())
                            .child(button(eid("close", &key), "Close", DANGER).on_click(close)),
                    )
            }))
    }
}
