//! Window chrome: floating sidebar, title bar actions, scrolling content, toast, overlays.

use gpui::{
    div, point, prelude::*, px, ClipboardItem, Context, FontWeight, MouseButton, SharedString,
    Window,
};

use crate::state::{MenuKind, Root, Sheet, Tab};
use crate::theme::Theme;
use crate::widgets::*;

/// Height of the drag strip at the top of the content area (traffic lights live in it).
const TITLEBAR_H: f32 = 48.;
const SIDEBAR_W: f32 = 200.;

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::resolve(&self.prefs, window.appearance());
        cx.set_global(t);
        self.apply_background(window);

        let font: SharedString = if cfg!(target_os = "macos") {
            ".SystemUIFont".into()
        } else {
            "Segoe UI".into()
        };

        let root = div()
            .id("root")
            .relative()
            .size_full()
            .flex()
            .bg(t.content)
            .text_color(t.fg)
            .text_size(px(13.))
            .font_family(font);

        if let Some(err) = &self.fatal {
            return root
                .p_6()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child("lanlink failed to start"),
                )
                .child(div().text_color(t.red).child(err.clone()));
        }

        let pane = match self.tab {
            Tab::Peers => self.render_peers(cx).into_any_element(),
            Tab::Hosting => self.render_hosting(cx).into_any_element(),
            Tab::Tunnels => self.render_tunnels(cx).into_any_element(),
            Tab::Settings => self.render_settings(cx).into_any_element(),
        };

        root.child(self.render_sidebar(&t, cx))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_titlebar(&t, cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("pane-{}", self.tab as u8)))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .pl(px(SIDEBAR_W + 30.))
                            .pr_6()
                            .pb_6()
                            .child(pane),
                    ),
            )
            .children(self.render_toast(&t, cx))
            .children(self.render_menu(&t, window, cx))
            .children(self.render_sheet(&t, cx))
    }
}

impl Root {
    fn render_sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let me = self
            .config
            .display_name
            .clone()
            .unwrap_or_else(|| "You".into());
        let my_id = self.node.as_ref().map(|n| n.id().to_string());
        let online = self.node.is_some() && self.network.value.online;
        let status = if self.node.is_none() {
            "Starting…"
        } else if online {
            "Online"
        } else {
            "Offline"
        };
        let requests = self.requests.value.len();

        let tabs = [
            (Tab::Peers, "Peers", "users", t.blue),
            (Tab::Hosting, "Hosting", "broadcast", t.green),
            (Tab::Tunnels, "Tunnels", "arrows-exchange", t.purple),
            (Tab::Settings, "Settings", "settings", t.fg2),
        ];

        let top = if cfg!(target_os = "macos") { 42. } else { 10. };
        col()
            .absolute()
            .left(px(10.))
            .top(px(top))
            .bottom(px(10.))
            .w(px(SIDEBAR_W))
            .p_2()
            .rounded(px(16.))
            .bg(t.sidebar)
            .border_1()
            .border_color(t.sidebar_stroke)
            .shadow_lg()
            .child(
                row()
                    .px_2()
                    .py_1p5()
                    .mb_2()
                    .child(avatar(t, &me, my_id.as_deref().unwrap_or("me"), 30.))
                    .child(
                        col()
                            .min_w_0()
                            .child(div().truncate().font_weight(FontWeight::SEMIBOLD).child(me))
                            .child(
                                row()
                                    .gap_1()
                                    .text_size(px(11.))
                                    .text_color(t.fg2)
                                    .child(dot(if online { t.green } else { t.fg3 }))
                                    .child(status),
                            ),
                    ),
            )
            .children(tabs.into_iter().map(|(tab, label, icon, color)| {
                let active = self.tab == tab;
                let badge = (tab == Tab::Peers && requests > 0).then_some(requests);
                div()
                    .id(label)
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .when(active, |el| {
                        el.bg(t.nav_sel).border_1().border_color(t.sidebar_stroke)
                    })
                    .when(!active, |el| el.hover(|s| s.bg(t.hover)))
                    .child(tile(icon, color, 20., t.white))
                    .child(div().flex_1().child(label))
                    .when_some(badge, |el, n| {
                        el.child(
                            div()
                                .flex_none()
                                .min_w(px(16.))
                                .h(px(16.))
                                .px_1()
                                .rounded_full()
                                .bg(t.red)
                                .text_color(t.white)
                                .text_size(px(10.))
                                .font_weight(FontWeight::BOLD)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(n.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        this.menu = None;
                        cx.notify();
                    }))
            }))
    }

    fn render_titlebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let my_id = self.node.as_ref().map(|n| n.id().to_string());
        row()
            .id("titlebar")
            .flex_none()
            .h(px(TITLEBAR_H))
            .justify_end()
            .gap_1p5()
            .pr_4()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .when_some(my_id, |el, id| {
                el.child(
                    button(
                        t,
                        "copy-my-id",
                        Some("copy"),
                        "Copy my ID",
                        ButtonKind::Secondary,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
                        this.show_toast("ID copied", false, cx);
                    })),
                )
            })
            .child(
                button(t, "add-peer", Some("plus"), "Add Peer", ButtonKind::Primary)
                    .on_click(cx.listener(|this, _, _, cx| this.open_sheet(Sheet::AddPeer, cx))),
            )
    }

    fn render_toast(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let toast = self.toast.as_ref()?;
        let color = if toast.error { t.red } else { t.blue };
        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("toast")
                        .px_3p5()
                        .py_2()
                        .rounded(px(10.))
                        .bg(t.menu)
                        .border_1()
                        .border_color(color.opacity(0.5))
                        .shadow_lg()
                        .text_size(px(12.5))
                        .cursor_pointer()
                        .child(toast.text.clone())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        })),
                ),
        )
    }

    /// Position a menu just under an element, from a click event's position.
    pub fn menu_at(&self, position: gpui::Point<gpui::Pixels>) -> gpui::Point<gpui::Pixels> {
        point(position.x - px(150.), position.y + px(12.))
    }

    pub fn menu_is(&self, kind: &MenuKind) -> bool {
        self.menu.as_ref().is_some_and(|m| &m.kind == kind)
    }
}
