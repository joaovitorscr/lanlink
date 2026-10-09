//! Settings pane: identity, network, appearance, updates, files.

use gpui::{div, prelude::*, px, ClipboardItem, Context};
use lanlink_core::Config;

use crate::format;
use crate::lifecycle;
use crate::state::{MenuKind, Root, UpdatePhase};
use crate::update;
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
                group_row(&t, false)
                    .child(
                        col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(gpui::FontWeight::MEDIUM).child("Your ID"))
                            .child(match &my_id {
                                Some(id) => self.id_line(&t, id, cx).into_any_element(),
                                None => small(&t, "Starting…").into_any_element(),
                            }),
                    )
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
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Hide IDs",
                        Some("Mask your ID and peer IDs on screen, e.g. while streaming"),
                    ))
                    .child(
                        toggle(&t, "hide-ids", self.prefs.hide_ids)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_hide_ids(cx))),
                    ),
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

        let background = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Keep running in the background when the window is closed",
                        Some(format!(
                            "lanlink stays in the {} so friends can still reach you",
                            lifecycle::TRAY_PLACE
                        )),
                    ))
                    .child(
                        toggle(&t, "keep-in-background", self.prefs.keep_in_background).on_click(
                            cx.listener(|this, _, _, cx| this.toggle_keep_in_background(cx)),
                        ),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Launch at login",
                        Some(format!("Starts hidden in the {}", lifecycle::TRAY_PLACE)),
                    ))
                    .child(
                        toggle(&t, "launch-at-login", self.prefs.launch_at_login).on_click(
                            cx.listener(|this, _, _, cx| this.toggle_launch_at_login(cx)),
                        ),
                    ),
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

        let updates = self.render_updates(cx);

        let transfer = group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Export config",
                        Some("Peers, services, saved tunnels and settings. Your identity key is not included."),
                    ))
                    .child(
                        button(&t, "export-config", None, "Export…", ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.export_config(cx))),
                    ),
            )
            .child(
                group_row(&t, true)
                    .child(title_sub(
                        &t,
                        "Import config",
                        Some("Merge or replace with a file exported from lanlink"),
                    ))
                    .child(
                        button(&t, "import-config", None, "Import…", ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.start_import(cx))),
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
            .child(glabel(&t, "Background"))
            .child(background)
            .child(glabel(&t, "Appearance"))
            .child(appearance)
            .child(glabel(&t, "Updates"))
            .child(updates)
            .child(glabel(&t, "Import and export"))
            .child(transfer)
            .child(glabel(&t, "Files"))
            .child(files)
    }

    fn render_updates(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        if !update::supported() {
            return group(&t).child(group_row(&t, true).child(title_sub(
                &t,
                "Updates",
                Some("Development build: update checks are off"),
            )));
        }

        let u = &self.update;
        let phase = self.update_phase().cloned();
        let status = match (&phase, &u.available) {
            (Some(UpdatePhase::Downloading { done, total }), Some(r)) => format!(
                "Downloading lanlink {}: {}",
                r.version,
                format::progress(*done, *total)
            ),
            (Some(UpdatePhase::Ready(_)), Some(r)) => {
                format!("lanlink {} is ready to install", r.version)
            }
            (Some(UpdatePhase::Installing), Some(r)) => {
                format!("Installing lanlink {}…", r.version)
            }
            (Some(UpdatePhase::Failed(_)), Some(r)) => {
                format!("Couldn't update to lanlink {}", r.version)
            }
            _ if u.checking => "Checking for updates…".to_string(),
            (_, Some(r)) => format!("lanlink {} is available", r.version),
            _ if u.failed => "Couldn't check for updates".to_string(),
            _ if u.last_check.is_some() => "You're up to date".to_string(),
            _ => "Not checked yet".to_string(),
        };
        let sub = match &phase {
            Some(UpdatePhase::Failed(e)) => Some(e.clone()),
            _ => u
                .last_check
                .map(|at| format!("Last checked {}", format::ago(at.elapsed()))),
        };
        let installable = u.available.as_ref().and_then(update::plan).is_some();
        let download = u.available.as_ref().map(|r| r.url.clone());
        let action = match &phase {
            Some(UpdatePhase::Ready(_)) => Some("Restart to update"),
            None | Some(UpdatePhase::Failed(_)) if installable => Some("Install"),
            _ => None,
        };
        let download = download.filter(|_| !installable);

        let auto = if update::install_target().is_some() {
            group_row(&t, false)
                .child(title_sub(
                    &t,
                    "Install updates automatically",
                    Some(
                        "Downloads and verifies new versions in the background, \
                         then asks you to restart. Nothing installs until you do.",
                    ),
                ))
                .child(
                    toggle(&t, "auto-install", self.prefs.auto_install)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_install(cx))),
                )
        } else {
            group_row(&t, false).child(title_sub(
                &t,
                "Install updates automatically",
                Some(update::manual_reason()),
            ))
        };

        group(&t)
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Check for updates",
                        Some(format!(
                            "Looks for new {} releases on GitHub every 6 hours",
                            self.update_channel().name()
                        )),
                    ))
                    .child(
                        toggle(&t, "check-updates", self.prefs.check_updates)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_update_checks(cx))),
                    ),
            )
            .child(
                group_row(&t, false)
                    .child(title_sub(
                        &t,
                        "Update channel",
                        Some("Nightly builds come out daily and may be unstable"),
                    ))
                    .child(
                        popup(&t, "update-channel", self.update_channel().label()).on_click(
                            cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                                let at = this.menu_at(ev.position());
                                this.toggle_menu(MenuKind::UpdateChannel, at, cx);
                            }),
                        ),
                    ),
            )
            .child(auto)
            .child(
                group_row(&t, true)
                    .child(title_sub(&t, status, sub))
                    .when_some(action, |el, label| {
                        el.child(
                            button(
                                &t,
                                "update-install-settings",
                                None,
                                label,
                                ButtonKind::Primary,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.install_update(cx))),
                        )
                    })
                    .when_some(download, |el, url| {
                        el.child(
                            button(&t, "update-open", None, "Download", ButtonKind::Primary)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_url(url.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        button(&t, "check-now", None, "Check now", ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(cx))),
                    ),
            )
    }
}
