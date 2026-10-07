//! Window chrome: header, pending request banners, tab bar, toast.

use gpui::{div, prelude::*, rgb, ClipboardItem, Context, SharedString, Window};

use crate::format;
use crate::state::{Root, Tab};
use crate::theme::*;
use crate::widgets::*;

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm();

        if let Some(err) = &self.fatal {
            return root
                .p_4()
                .gap_2()
                .child(div().text_lg().child("lanlink failed to start"))
                .child(div().text_color(rgb(DANGER)).child(err.clone()));
        }

        let panel = match self.tab {
            Tab::Peers => self.render_peers(cx).into_any_element(),
            Tab::Hosting => self.render_hosting(cx).into_any_element(),
            Tab::Tunnels => self.render_tunnels(cx).into_any_element(),
            Tab::Settings => self.render_settings(cx).into_any_element(),
        };

        root.child(self.render_header(cx))
            .children(self.render_requests(cx))
            .child(self.render_tabs(cx))
            .child(
                div()
                    .id(SharedString::from(format!("panel-{}", self.tab as u8)))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(panel),
            )
            .when_some(self.toast.as_ref(), |el, t| {
                let color = if t.error { DANGER } else { ACCENT };
                el.child(
                    div()
                        .id("toast")
                        .m_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(color))
                        .cursor_pointer()
                        .child(t.text.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        })),
                )
            })
    }
}

impl Root {
    fn render_header(&mut self, _cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .config
            .display_name
            .clone()
            .unwrap_or_else(|| "lanlink".into());
        let id = self.node.as_ref().map(|n| n.id().to_string());
        let net = &self.network;
        let (pill, color) = if self.node.is_none() {
            ("Starting…".to_string(), GREY)
        } else if !net.available {
            ("Network status not available".to_string(), GREY)
        } else if net.value.online {
            let host = net
                .value
                .home_relay
                .as_deref()
                .map(format::relay_host)
                .unwrap_or_else(|| "relay".into());
            (format!("Online via {host}"), GREEN)
        } else {
            ("Offline".to_string(), DANGER)
        };
        row()
            .px_3()
            .py_2()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                col()
                    .gap_0()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_lg().truncate().child(name))
                    .child(
                        row()
                            .child(small(
                                id.as_deref()
                                    .map(format::short_id)
                                    .unwrap_or_else(|| "…".into()),
                            ))
                            .when_some(id, |el, full| {
                                el.child(button("copy-id", "Copy", BORDER).text_xs().on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            full.clone(),
                                        ))
                                    },
                                ))
                            }),
                    ),
            )
            .child(badge(pill, color))
    }

    fn render_requests(&mut self, cx: &mut Context<Self>) -> Vec<impl IntoElement> {
        self.requests
            .value
            .clone()
            .into_iter()
            .map(|r| {
                let who = r
                    .name
                    .clone()
                    .unwrap_or_else(|| format::short_id(&r.id.to_string()));
                let id = r.id;
                col()
                    .mx_3()
                    .mt_2()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(AMBER))
                    .child(div().child(format!("{who} wants to connect")))
                    .child(
                        row()
                            .when_some(self.request_names.get(&id).cloned(), |el, input| {
                                el.child(input)
                            })
                            .child(
                                button(eid("allow", id), "Allow", GREEN).on_click(cx.listener(
                                    move |this, _, _, cx| this.respond_request(id, true, cx),
                                )),
                            )
                            .child(
                                button(eid("deny", id), "Deny", BORDER).on_click(cx.listener(
                                    move |this, _, _, cx| this.respond_request(id, false, cx),
                                )),
                            ),
                    )
            })
            .collect()
    }

    fn render_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            (Tab::Peers, "Peers"),
            (Tab::Hosting, "Hosting"),
            (Tab::Tunnels, "Tunnels"),
            (Tab::Settings, "Settings"),
        ];
        row()
            .gap_0()
            .mt_2()
            .px_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .children(tabs.into_iter().map(|(tab, label)| {
                let active = self.tab == tab;
                div()
                    .id(label)
                    .flex_1()
                    .flex()
                    .justify_center()
                    .py_2()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(rgb(if active { ACCENT } else { BG }))
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
            }))
    }
}
