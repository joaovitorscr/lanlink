//! Sheets (modal, drop from the title bar) and popover menus.

use gpui::{
    anchored, deferred, div, prelude::*, px, AnyElement, ClipboardItem, Context, Corner,
    FontWeight, MouseButton, SharedString, Window,
};
use lanlink_core::export::ImportMode;
use lanlink_core::Protocol;

use crate::state::{MenuKind, Root, Sheet};
use crate::theme::{Appearance, Theme};
use crate::widgets::*;

impl Root {
    // ---- sheets ----

    pub fn render_sheet(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet.clone()?;
        let body: AnyElement = match sheet {
            Sheet::AddPeer => self.sheet_add_peer(t, cx).into_any_element(),
            Sheet::AddService => self.sheet_add_service(t, cx).into_any_element(),
            Sheet::RenamePeer(id) => self.sheet_rename(t, id, cx).into_any_element(),
            Sheet::RemovePeer(id) => self.sheet_remove_peer(t, id, cx).into_any_element(),
            Sheet::RemoveService(name) => self.sheet_remove_service(t, name, cx).into_any_element(),
            Sheet::Import => self.sheet_import(t, cx)?.into_any_element(),
        };
        Some(
            deferred(
                div()
                    .id("scrim")
                    .absolute()
                    .inset_0()
                    .bg(t.scrim)
                    .flex()
                    .justify_center()
                    .items_start()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_overlays(cx)),
                    )
                    .child(
                        col()
                            .id("sheet")
                            .occlude()
                            .w(px(440.))
                            .px_5()
                            .pt_5()
                            .pb_4()
                            .rounded_b(px(14.))
                            .bg(t.sheet)
                            .border_1()
                            .border_color(t.card_stroke)
                            .shadow_2xl()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(body),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn sheet_head(&self, t: &Theme, title: &str, lede: &str) -> impl IntoElement {
        col()
            .mb_3()
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(title.to_string()),
            )
            .child(small(t, lede.to_string()))
    }

    fn frow(&self, t: &Theme, label: &str, field: impl IntoElement) -> impl IntoElement {
        row()
            .my_1p5()
            .gap_2p5()
            .child(
                div()
                    .w(px(100.))
                    .flex_none()
                    .text_right()
                    .text_size(px(12.))
                    .text_color(t.fg2)
                    .child(label.to_string()),
            )
            .child(div().flex_1().min_w_0().flex().child(field))
    }

    fn actions(
        &self,
        t: &Theme,
        cx: &mut Context<Self>,
        primary: (&str, ButtonKind, Action),
    ) -> impl IntoElement {
        let (label, kind, f) = primary;
        row()
            .justify_end()
            .mt_4()
            .child(
                button(t, "sheet-cancel", None, "Cancel", ButtonKind::Secondary)
                    .on_click(cx.listener(|this, _, _, cx| this.close_overlays(cx))),
            )
            .child(
                button(t, "sheet-ok", None, label.to_string(), kind)
                    .on_click(cx.listener(move |this, _, _, cx| f(this, cx))),
            )
    }

    fn sheet_add_peer(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        col()
            .child(self.sheet_head(
                t,
                "Add a peer",
                "Paste the ID your friend copied from their lanlink.",
            ))
            .child(self.frow(t, "Their ID", self.inputs.peer_id.clone()))
            .child(self.frow(t, "Name", self.inputs.peer_name.clone()))
            .child(
                div()
                    .ml(px(110.))
                    .child(small(t, "They have to allow you on their side too.")),
            )
            .child(self.actions(
                t,
                cx,
                ("Add", ButtonKind::Primary, Box::new(|r, cx| r.add_peer(cx))),
            ))
    }

    fn sheet_add_service(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let proto = proto_label(self.svc_protocol);
        col()
            .child(self.sheet_head(
                t,
                "Share a service",
                "Anything listening on a port on this computer: a game, a server, a tool.",
            ))
            .child(self.frow(t, "Name", self.inputs.svc_name.clone()))
            .child(self.frow(
                t,
                "Port",
                div().w(px(110.)).flex().child(self.inputs.svc_port.clone()),
            ))
            .child(self.frow(t, "Address", self.inputs.svc_host.clone()))
            .child(self.frow(
                t,
                "Protocol",
                popup(t, "svc-proto", proto).on_click(cx.listener(
                    |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(MenuKind::Protocol, at, cx);
                    },
                )),
            ))
            .child(self.frow(
                t,
                "",
                checkbox(t, "svc-mc", self.svc_minecraft, "Minecraft world or server").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.svc_minecraft = !this.svc_minecraft;
                        cx.notify();
                    }),
                ),
            ))
            .child(self.actions(
                t,
                cx,
                (
                    "Share",
                    ButtonKind::Primary,
                    Box::new(|r, cx| r.add_service(cx)),
                ),
            ))
    }

    fn sheet_rename(
        &mut self,
        t: &Theme,
        id: lanlink_core::NodeId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let who = self.peer_name(id);
        col()
            .child(self.sheet_head(
                t,
                &format!("Rename {who}"),
                "Only you see this name. Leave it empty to use the name they send.",
            ))
            .child(self.frow(t, "Name", self.inputs.rename.clone()))
            .child(self.actions(
                t,
                cx,
                (
                    "Save",
                    ButtonKind::Primary,
                    Box::new(|r, cx| r.finish_rename(cx)),
                ),
            ))
    }

    fn sheet_remove_peer(
        &mut self,
        t: &Theme,
        id: lanlink_core::NodeId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let who = self.peer_name(id);
        col()
            .child(
                row()
                    .items_start()
                    .gap_3p5()
                    .child(avatar(t, &who, &id.to_string(), 44.))
                    .child(self.sheet_head(
                        t,
                        &format!("Remove {who}?"),
                        "They lose access to everything you share, and your tunnels to them close. You can add them again later.",
                    )),
            )
            .child(self.actions(
                t,
                cx,
                ("Remove", ButtonKind::Danger, Box::new(move |r, cx| r.remove_peer(id, cx))),
            ))
    }

    fn sheet_remove_service(
        &mut self,
        t: &Theme,
        name: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let n = name.clone();
        col()
            .child(self.sheet_head(
                t,
                &format!("Stop sharing {name}?"),
                "Open connections from peers to it are closed.",
            ))
            .child(self.actions(
                t,
                cx,
                (
                    "Stop sharing",
                    ButtonKind::Danger,
                    Box::new(move |r, cx| r.remove_service(n.clone(), cx)),
                ),
            ))
    }

    fn sheet_import(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = self.pending_import.as_ref()?;
        let mut lede = format!("Contains {}.", p.import.summary().describe());
        if let Some(v) = &p.import.app_version {
            lede.push_str(&format!(" Exported by lanlink {v}"));
            if let Some(at) = &p.import.exported_at {
                lede.push_str(&format!(" on {}", at.get(..10).unwrap_or(at)));
            }
            lede.push('.');
        }
        let mode = p.mode;
        let choice = |id: &'static str, m: ImportMode, label: &'static str, sub: &'static str| {
            col()
                .gap_0p5()
                .child(
                    checkbox(t, id, mode == m, label)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_import_mode(m, cx))),
                )
                .child(div().ml(px(23.)).child(small(t, sub)))
        };
        let (label, kind) = match mode {
            ImportMode::Merge => ("Merge", ButtonKind::Primary),
            ImportMode::Replace => ("Replace", ButtonKind::Danger),
        };
        Some(
            col()
                .child(self.sheet_head(t, &format!("Import {}?", p.file_name), &lede))
                .child(
                    col()
                        .gap_2p5()
                        .child(choice(
                            "import-merge",
                            ImportMode::Merge,
                            "Merge with my config",
                            "Adds its peers, services and tunnels. Where both have the same one, the file wins.",
                        ))
                        .child(choice(
                            "import-replace",
                            ImportMode::Replace,
                            "Replace my config",
                            "Peers, services and tunnels not in the file are removed.",
                        )),
                )
                .child(div().mt_3().child(small(
                    t,
                    "Your current config is saved as config.json.bak first. Your identity is not part of the file and does not change.",
                )))
                .child(self.actions(
                    t,
                    cx,
                    (label, kind, Box::new(|r, cx| r.finish_import(cx))),
                )),
        )
    }

    // ---- menus ----

    pub fn render_menu(
        &mut self,
        t: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let at = menu.at;
        let items: Vec<MenuItem> = match menu.kind.clone() {
            MenuKind::Peer(id) => {
                let key = id.to_string();
                vec![
                    MenuItem::action("pencil", "Rename…", move |r, cx| r.start_rename(id, cx)),
                    MenuItem::action("copy", "Copy ID", move |r, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(key.clone()));
                        r.show_toast("ID copied", false, cx);
                    }),
                    MenuItem::action("refresh", "Reconnect", move |r, _| {
                        r.run(move |n| async move { n.reconnect(id).await })
                    }),
                    MenuItem::Sep,
                    MenuItem::danger("trash", "Remove…", move |r, cx| {
                        r.open_sheet(Sheet::RemovePeer(id), cx)
                    }),
                ]
            }
            MenuKind::Service(name) => {
                let svc = self
                    .services
                    .iter()
                    .find(|s| s.service.name == name)
                    .cloned();
                let svc = svc?;
                let enabled = svc.service.enabled;
                let mc = svc.service.minecraft_lan;
                let n1 = name.clone();
                let n2 = name.clone();
                let mut s2 = svc.service.clone();
                s2.minecraft_lan = !mc;
                vec![
                    MenuItem::action(
                        if enabled { "x" } else { "check" },
                        if enabled {
                            "Pause sharing"
                        } else {
                            "Resume sharing"
                        },
                        move |r, _| {
                            let n = n1.clone();
                            r.run(
                                move |n2| async move { n2.set_service_enabled(&n, !enabled).await },
                            )
                        },
                    ),
                    MenuItem::check(mc, "Minecraft world or server", move |r, _| {
                        let s = s2.clone();
                        r.run(move |n| async move { n.add_service(s).await })
                    }),
                    MenuItem::Sep,
                    MenuItem::danger("trash", "Stop sharing…", move |r, cx| {
                        r.open_sheet(Sheet::RemoveService(n2.clone()), cx)
                    }),
                ]
            }
            MenuKind::Protocol => [Protocol::Tcp, Protocol::Udp]
                .into_iter()
                .map(|p| {
                    MenuItem::check(self.svc_protocol == p, proto_label(p), move |r, cx| {
                        r.svc_protocol = p;
                        cx.notify();
                    })
                })
                .collect(),
            MenuKind::Appearance => Appearance::ALL
                .into_iter()
                .map(|a| {
                    MenuItem::check(self.prefs.appearance == a, a.label(), move |r, cx| {
                        r.prefs.appearance = a;
                        r.save_prefs(cx);
                    })
                })
                .collect(),
        };

        let theme = *t;
        let menu_el = col()
            .id("menu")
            .occlude()
            .min_w(px(190.))
            .p_1()
            .rounded(px(10.))
            .bg(t.menu)
            .border_1()
            .border_color(t.card_stroke)
            .shadow_2xl()
            .children(items.into_iter().enumerate().map(|(i, item)| {
                match item {
                    MenuItem::Sep => div()
                        .h(px(1.))
                        .my_1()
                        .mx_1p5()
                        .bg(theme.sep)
                        .into_any_element(),
                    MenuItem::Item {
                        icon,
                        checked,
                        label,
                        danger,
                        action,
                    } => {
                        let color = if danger { theme.red } else { theme.fg };
                        div()
                            .id(eid("mi", i))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(px(5.))
                            .text_color(color)
                            .cursor_pointer()
                            .hover(move |s| {
                                s.bg(if danger { theme.red } else { theme.blue })
                                    .text_color(theme.white)
                            })
                            .child(match (icon, checked) {
                                (Some(name), _) => ic(name, 14., color).into_any_element(),
                                (None, Some(true)) => ic("check", 13., color).into_any_element(),
                                (None, _) => div().size(px(13.)).into_any_element(),
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                action(this, cx);
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                }
            }));

        Some(
            deferred(
                div()
                    .id("menu-scrim")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }),
                    )
                    .child(
                        anchored()
                            .position(at)
                            .anchor(Corner::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu_el),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

type Action = Box<dyn Fn(&mut Root, &mut Context<Root>)>;

enum MenuItem {
    Sep,
    Item {
        icon: Option<&'static str>,
        checked: Option<bool>,
        label: SharedString,
        danger: bool,
        action: Action,
    },
}

impl MenuItem {
    fn action(
        icon: &'static str,
        label: impl Into<SharedString>,
        f: impl Fn(&mut Root, &mut Context<Root>) + 'static,
    ) -> Self {
        MenuItem::Item {
            icon: Some(icon),
            checked: None,
            label: label.into(),
            danger: false,
            action: Box::new(f),
        }
    }

    fn danger(
        icon: &'static str,
        label: impl Into<SharedString>,
        f: impl Fn(&mut Root, &mut Context<Root>) + 'static,
    ) -> Self {
        MenuItem::Item {
            icon: Some(icon),
            checked: None,
            label: label.into(),
            danger: true,
            action: Box::new(f),
        }
    }

    fn check(
        on: bool,
        label: impl Into<SharedString>,
        f: impl Fn(&mut Root, &mut Context<Root>) + 'static,
    ) -> Self {
        MenuItem::Item {
            icon: None,
            checked: Some(on),
            label: label.into(),
            danger: false,
            action: Box::new(f),
        }
    }
}

pub fn proto_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    }
}
