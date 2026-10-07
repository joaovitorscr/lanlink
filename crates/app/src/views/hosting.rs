//! Hosting tab: services we share, add form, detected Minecraft LAN worlds.

use gpui::{div, prelude::*, rgb, Context};
use lanlink_core::ServiceStatus;

use crate::state::Root;
use crate::theme::*;
use crate::views::peers::proto;
use crate::widgets::*;

impl Root {
    pub fn render_hosting(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let services = self.service_list();
        let nothing_listening = services
            .iter()
            .any(|s| s.service.enabled && s.reachable == Some(false));

        let list = card(Some("Shared with friends"))
            .when(!self.services.available, |el| {
                el.child(small("Live status not available"))
            })
            .when(services.is_empty(), |el| {
                el.child(muted("Nothing shared yet. Add a service below."))
            })
            .children(services.iter().map(|s| self.render_service(s, cx)))
            .when(nothing_listening, |el| {
                el.child(small(
                    "Red means no program is listening on that port. Start the game or server \
                     first (in Minecraft: Esc → Open to LAN).",
                ))
            });

        let add =
            card(Some("Add service"))
                .child(
                    row()
                        .child(self.inputs.svc_name.clone())
                        .child(div().w_20().flex().child(self.inputs.svc_port.clone())),
                )
                .child(
                    row()
                        .child(
                            button("proto", if self.svc_udp { "UDP" } else { "TCP" }, BORDER)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.svc_udp = !this.svc_udp;
                                    cx.notify();
                                })),
                        )
                        .child(switch("svc-mc", self.svc_minecraft, "Minecraft").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.svc_minecraft = !this.svc_minecraft;
                                cx.notify();
                            }),
                        ))
                        .child(div().flex_1())
                        .child(
                            button("add-svc", "Add", ACCENT)
                                .on_click(cx.listener(|this, _, _, cx| this.add_service(cx))),
                        ),
                );

        let worlds = self.worlds.value.clone();
        let worlds_card = card(Some("Detected Minecraft worlds"))
            .when(!self.worlds.available, |el| {
                el.child(muted("LAN world detection not available"))
            })
            .when(self.worlds.available && worlds.is_empty(), |el| {
                el.child(muted(if self.config.disable_lan_detection {
                    "Detection is turned off in Settings."
                } else {
                    "None right now. In Minecraft: Esc → Open to LAN."
                }))
            })
            .children(worlds.into_iter().map(|w| {
                let port = w.port;
                let shared = services.iter().any(|s| s.effective_port == port);
                row()
                    .child(div().flex_1().min_w_0().truncate().child(w.motd.clone()))
                    .child(muted(format!("port {port}")))
                    .child(if shared {
                        badge("Shared", GREEN).into_any_element()
                    } else {
                        button(eid("share-world", port), "Share", ACCENT)
                            .on_click(cx.listener(move |this, _, _, _| this.share_world(w.clone())))
                            .into_any_element()
                    })
            }));

        col().gap_3().child(list).child(add).child(worlds_card)
    }

    fn render_service(&mut self, s: &ServiceStatus, cx: &mut Context<Self>) -> impl IntoElement {
        let svc = s.service.clone();
        let name = svc.name.clone();
        let (dot_color, status) = match (svc.enabled, s.reachable) {
            (false, _) => (GREY, "Disabled".to_string()),
            (true, Some(true)) => (GREEN, "Ready".to_string()),
            (true, Some(false)) => (
                DANGER,
                format!("Nothing listening on port {}", s.effective_port),
            ),
            (true, None) => (GREY, String::new()),
        };
        let lan_note = (s.effective_port != svc.port)
            .then(|| format!("LAN world detected on {}", s.effective_port));
        let users = if s.connections > 0 {
            let names: Vec<String> = s.peers.iter().map(|p| self.peer_name(*p)).collect();
            Some(format!(
                "{} connection{} · {}",
                s.connections,
                if s.connections == 1 { "" } else { "s" },
                names.join(", ")
            ))
        } else {
            None
        };

        let toggle_enabled = {
            let name = name.clone();
            let enabled = !svc.enabled;
            cx.listener(move |this, _, _, _| {
                let name = name.clone();
                this.run(move |n| async move { n.set_service_enabled(&name, enabled).await })
            })
        };
        let toggle_mc = {
            let mut svc = svc.clone();
            svc.minecraft_lan = !svc.minecraft_lan;
            cx.listener(move |this, _, _, _| {
                let svc = svc.clone();
                this.run(move |n| async move { n.add_service(svc).await })
            })
        };
        let remove = {
            let name = name.clone();
            cx.listener(move |this, _, _, _| {
                let name = name.clone();
                this.run(move |n| async move { n.remove_service(&name).await })
            })
        };

        col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(rgb(BG))
            .child(
                row()
                    .child(dot(dot_color))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(muted(format!(
                        "{} {}",
                        proto(svc.protocol),
                        s.effective_port
                    ))),
            )
            .when(!status.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(if dot_color == DANGER { DANGER } else { MUTED }))
                        .child(status),
                )
            })
            .when_some(lan_note, |el, n| el.child(small(n)))
            .when_some(users, |el, u| el.child(small(u)))
            .child(
                row()
                    .child(
                        switch(eid("svc-on", &name), svc.enabled, "Enabled")
                            .on_click(toggle_enabled),
                    )
                    .child(
                        switch(eid("svc-mc", &name), svc.minecraft_lan, "Minecraft")
                            .on_click(toggle_mc),
                    )
                    .child(div().flex_1())
                    .child(button(eid("svc-rm", &name), "Remove", BORDER).on_click(remove)),
            )
    }
}
