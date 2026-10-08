//! Settings pane: identity, network, appearance, files.

use gpui::{div, prelude::*, px, ClipboardItem, Context};
use lanlink_core::Config;

use crate::state::{MenuKind, Root};
use crate::views::peers::pane_header;
use crate::widgets::*;

impl Root {
    pub fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let my_id = self.node.as_ref().map(|n| n.id().to_string());

        let identity = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(&t, "Display name", Some("Shown to your peers")))
                    .child(
                        div()
                            .w(px(200.))
                            .flex()
                            .child(self.inputs.display_name.clone()),
                    )
                    .child(
                        button(&t, "save-name", None, "Save", ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.save_display_name(cx))),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Your ID",
                        Some(my_id.clone().unwrap_or_else(|| "Starting…".into())),
                    ))
                    .when_some(my_id, |el, id| {
                        el.child(
                            button(
                                &t,
                                "copy-id-settings",
                                Some("copy"),
                                "Copy",
                                ButtonKind::Secondary,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
                                    this.show_toast("ID copied", false, cx);
                                },
                            )),
                        )
                    }),
            );

        let relay_sub = if self.restart_required {
            "Restart lanlink to use the new relay"
        } else {
            "Used only when a direct path fails. Empty = public relays."
        };
        let network = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(&t, "Relay server", Some(relay_sub)))
                    .child(
                        div()
                            .w(px(200.))
                            .flex()
                            .child(self.inputs.relay_url.clone()),
                    )
                    .child(
                        button(&t, "save-relay", None, "Save", ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.save_relay_url(cx))),
                    ),
            )
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Detect Minecraft LAN worlds",
                        Some("Lists worlds opened to LAN on this computer under Hosting"),
                    ))
                    .child(
                        toggle(&t, "lan-detect", !self.config.disable_lan_detection).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.toggle_lan_detection();
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Log latency",
                        Some("Writes every ping to a daily CSV in the logs folder (last 7 days)"),
                    ))
                    .child(toggle(&t, "latency-log", self.config.latency_log).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.toggle_latency_log();
                            cx.notify();
                        }),
                    )),
            );

        let appearance = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(&t, "Appearance", None::<&str>))
                    .child(
                        popup(&t, "appearance", self.prefs.appearance.label()).on_click(
                            cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                                let at = this.menu_at(ev.position());
                                this.toggle_menu(MenuKind::Appearance, at, cx);
                            }),
                        ),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Transparency",
                        Some("Blur the desktop behind the window"),
                    ))
                    .child(
                        toggle(&t, "transparency", self.prefs.transparency).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.prefs.transparency = !this.prefs.transparency;
                                this.save_prefs(cx);
                            },
                        )),
                    ),
            );

        let files = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Logs",
                        Some(Config::logs_dir().display().to_string()),
                    ))
                    .child(
                        button(&t, "open-logs", None, "Open", ButtonKind::Secondary).on_click(
                            cx.listener(|this, _, _, cx| this.open_folder(Config::logs_dir(), cx)),
                        ),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Config",
                        Some(Config::dir().display().to_string()),
                    ))
                    .child(
                        button(&t, "open-config", None, "Open", ButtonKind::Secondary).on_click(
                            cx.listener(|this, _, _, cx| this.open_folder(Config::dir(), cx)),
                        ),
                    ),
            );

        col()
            .child(pane_header(
                &t,
                "Settings",
                &format!("lanlink {}", lanlink_core::build_info::describe()),
            ))
            .child(glabel(&t, "Identity"))
            .child(identity)
            .child(glabel(&t, "Network"))
            .child(network)
            .child(glabel(&t, "Appearance"))
            .child(appearance)
            .child(glabel(&t, "Files"))
            .child(files)
    }
}
