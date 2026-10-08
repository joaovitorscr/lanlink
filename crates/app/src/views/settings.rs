//! Settings tab.

use gpui::{div, prelude::*, rgb, Context};
use lanlink_core::Config;

use crate::state::Root;
use crate::theme::*;
use crate::widgets::*;

impl Root {
    pub fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = card(Some("Your name"))
            .child(small("Shown to your friends when you connect."))
            .child(
                row().child(self.inputs.display_name.clone()).child(
                    button("save-name", "Save", ACCENT)
                        .on_click(cx.listener(|this, _, _, cx| this.save_display_name(cx))),
                ),
            );

        let relay = card(Some("Custom relay"))
            .child(small("Leave empty to use the free public relays."))
            .child(
                row().child(self.inputs.relay_url.clone()).child(
                    button("save-relay", "Save", ACCENT)
                        .on_click(cx.listener(|this, _, _, cx| this.save_relay_url(cx))),
                ),
            )
            .when(self.restart_required, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(AMBER))
                        .child("Restart lanlink to use the new relay."),
                )
            });

        let lan = card(Some("Minecraft")).child(
            switch(
                "lan-detect",
                !self.config.disable_lan_detection,
                "Detect \"Open to LAN\" worlds on this computer",
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_lan_detection();
                cx.notify();
            })),
        );

        let folders = card(Some("Files"))
            .child(
                row()
                    .child(button("open-logs", "Open logs folder", BORDER).on_click(
                        cx.listener(|this, _, _, cx| this.open_folder(Config::logs_dir(), cx)),
                    ))
                    .child(
                        button("open-config", "Open config folder", BORDER).on_click(
                            cx.listener(|this, _, _, cx| this.open_folder(Config::dir(), cx)),
                        ),
                    ),
            )
            .child(small(format!(
                "Latency log: {}",
                Config::dir().join("latency.csv").display()
            )));

        col()
            .gap_3()
            .child(name)
            .child(relay)
            .child(lan)
            .child(folders)
            .child(small(format!(
                "lanlink {}",
                lanlink_core::build_info::describe()
            )))
    }
}
