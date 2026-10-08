//! Hosting pane: detected Minecraft worlds, shared services, add via sheet.

use gpui::{prelude::*, Context};
use lanlink_core::ServiceStatus;

use crate::state::{MenuKind, Root, Sheet};
use crate::theme::Theme;
use crate::views::overlays::proto_label;
use crate::views::peers::pane_header;
use crate::widgets::*;

impl Root {
    pub fn render_hosting(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let services = self.services.clone();
        let worlds = self.worlds.clone();
        let n = services.len();

        col()
            .child(pane_header(
                &t,
                "Hosting",
                "Services on this computer that your peers can reach. Games, servers, anything that listens on a port.",
            ))
            .when(!worlds.is_empty(), |el| {
                let nw = worlds.len();
                el.child(glabel(&t, "Detected"))
                    .child(group(&t).children(worlds.into_iter().enumerate().map(|(i, w)| {
                        let port = w.port;
                        let shared = services.iter().any(|s| s.effective_port == port);
                        group_row(&t, i + 1 == nw)
                            .child(tile("pick", t.green, 30., t.white))
                            .child(title_sub(
                                &t,
                                w.motd.clone(),
                                Some(format!("Minecraft · open to LAN on port {port}")),
                            ))
                            .child(if shared {
                                pill("Shared", t.green).into_any_element()
                            } else {
                                button(&t, eid("share-world", port), None, "Share", ButtonKind::Primary)
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.share_world(w.clone())
                                    }))
                                    .into_any_element()
                            })
                    })))
            })
            .child(
                row()
                    .child(glabel(&t, "Shared").flex_1())
                    .child(
                        button(&t, "add-svc", Some("plus"), "Add service", ButtonKind::Secondary)
                            .mt_3()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_sheet(Sheet::AddService, cx)
                            })),
                    ),
            )
            .child(
                group(&t)
                    .when(services.is_empty(), |el| {
                        el.child(empty(&t, "Nothing shared yet."))
                    })
                    .children(
                        services
                            .iter()
                            .enumerate()
                            .map(|(i, s)| self.render_service(&t, s, i + 1 == n, cx)),
                    ),
            )
            .child(
                small(
                    &t,
                    "Some games pick a new port every launch. If a shared service stops working, check that the port still matches.",
                )
                .mt_2p5()
                .ml_0p5(),
            )
    }

    fn render_service(
        &mut self,
        t: &Theme,
        s: &ServiceStatus,
        last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let svc = s.service.clone();
        let name = svc.name.clone();
        let mut parts = vec![format!(
            "{} {}",
            proto_label(svc.protocol),
            s.effective_port
        )];
        if s.effective_port != svc.port {
            parts.push(format!("LAN world found, configured port {}", svc.port));
        }
        let (status_color, status) = match (svc.enabled, s.reachable) {
            (false, _) => (t.fg2, Some("Paused".to_string())),
            (true, Some(false)) => (t.red, Some("Nothing is listening on this port".into())),
            _ => (t.fg2, None),
        };
        if s.connections > 0 {
            let names: Vec<String> = s.peers.iter().map(|p| self.peer_name(*p)).collect();
            parts.push(format!(
                "{} connection{} · {}",
                s.connections,
                if s.connections == 1 { "" } else { "s" },
                names.join(", ")
            ));
        } else if svc.enabled {
            parts.push("nobody connected".into());
        }
        if let Some(st) = status {
            parts.push(st);
        }

        let icon = if svc.minecraft_lan {
            "pick"
        } else {
            "broadcast"
        };
        let tile_color = if !svc.enabled {
            t.fg3
        } else if svc.minecraft_lan {
            t.green
        } else {
            t.purple
        };
        let toggle_name = name.clone();
        let enabled = svc.enabled;
        let menu_open = self.menu_is(&MenuKind::Service(name.clone()));
        let menu_name = name.clone();

        group_row(t, last)
            .child(tile(icon, tile_color, 30., t.white))
            .child(
                title_sub(t, name.clone(), Some(parts.join(" · ")))
                    .text_color(if status_color == t.red { t.red } else { t.fg }),
            )
            .when(svc.enabled && s.reachable != Some(false), |el| {
                el.child(pill("Sharing", t.green))
            })
            .child(
                toggle(t, eid("svc-on", &name), enabled).on_click(cx.listener(
                    move |this, _, _, _| {
                        let n = toggle_name.clone();
                        this.run(
                            move |node| async move { node.set_service_enabled(&n, !enabled).await },
                        )
                    },
                )),
            )
            .child(
                icon_button(t, eid("svc-menu", &name), "dots")
                    .when(menu_open, |el| el.bg(t.hover))
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(MenuKind::Service(menu_name.clone()), at, cx);
                    })),
            )
    }
}
