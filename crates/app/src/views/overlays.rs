//! Sheets (modal, centered in the window) and popover menus.

use gpui::{
    anchored, deferred, div, prelude::*, px, AnyElement, ClipboardItem, Context, Corner,
    FontWeight, MouseButton, SharedString, Window,
};
use lanlink_core::export::ImportMode;
use lanlink_core::{Approval, InviteExpiry, NetworkColor, NodeId, Protocol};

use crate::state::{
    approval_label, expiry_label, MenuKind, Root, Sheet, WhoCanJoin, EXPIRY_CHOICES,
};
use crate::theme::{Appearance, Theme};
use crate::views::peers::network_color;
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
            Sheet::NewNetwork => self.sheet_new_network(t, cx).into_any_element(),
            Sheet::JoinNetwork => self.sheet_join(t, cx).into_any_element(),
            Sheet::Invite(id) => self.sheet_invite(t, id, cx).into_any_element(),
            Sheet::RenameNetwork(id) => self.sheet_rename_network(t, id, cx).into_any_element(),
            Sheet::RemoveMember(id, peer) => {
                self.sheet_remove_member(t, id, peer, cx).into_any_element()
            }
            Sheet::LeaveNetwork(id) => self.sheet_leave(t, id, cx).into_any_element(),
            Sheet::DeleteNetwork(id) => self.sheet_delete(t, id, cx).into_any_element(),
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
                    .items_center()
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
                            .pb_5()
                            .rounded(px(14.))
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

    fn sheet_new_network(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &self.new_network;
        let (color, who, expires, policy) = (f.color, f.who, f.expires, f.policy);
        let theme = *t;
        let swatches = row()
            .gap_1p5()
            .children(NetworkColor::ALL.into_iter().map(|c| {
                let on = c == color;
                div()
                    .id(eid("swatch", format!("{c:?}")))
                    .size(px(20.))
                    .rounded_full()
                    .bg(network_color(&theme, c))
                    .cursor_pointer()
                    .when(on, |el| el.border_2().border_color(theme.fg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.new_network.color = c;
                        cx.notify();
                    }))
            }));
        col()
            .child(self.sheet_head(
                t,
                "New network",
                "A network is a group of friends who can reach each other's shared services.",
            ))
            .child(self.frow(t, "Name", self.inputs.net_name.clone()))
            .child(self.frow(t, "Color", swatches))
            .child(self.frow(
                t,
                "Who can join",
                popup(t, "new-who", who.label()).on_click(cx.listener(
                    |this, ev: &gpui::ClickEvent, _, cx| {
                        let at = this.menu_at(ev.position());
                        this.toggle_menu(MenuKind::NewNetWho, at, cx);
                    },
                )),
            ))
            .when(who != WhoCanJoin::NoCode, |el| {
                el.child(self.frow(
                    t,
                    "Code expires",
                    popup(t, "new-expiry", expiry_label(expires)).on_click(cx.listener(
                        |this, ev: &gpui::ClickEvent, _, cx| {
                            let at = this.menu_at(ev.position());
                            this.toggle_menu(MenuKind::NewNetExpiry, at, cx);
                        },
                    )),
                ))
            })
            .child(
                self.frow(
                    t,
                    "",
                    checkbox(
                        t,
                        "new-share",
                        policy.members_share,
                        "Let members share services with the network",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = &mut this.new_network.policy;
                        p.members_share = !p.members_share;
                        cx.notify();
                    })),
                ),
            )
            .child(
                self.frow(
                    t,
                    "",
                    checkbox(
                        t,
                        "new-invite",
                        policy.members_invite,
                        "Let members invite others",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = &mut this.new_network.policy;
                        p.members_invite = !p.members_invite;
                        cx.notify();
                    })),
                ),
            )
            .child(self.actions(
                t,
                cx,
                (
                    "Create",
                    ButtonKind::Primary,
                    Box::new(|r, cx| r.create_network(cx)),
                ),
            ))
    }

    fn sheet_join(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        col()
            .child(self.sheet_head(
                t,
                "Join a network",
                "Paste the invite code your friend sent you.",
            ))
            .child(self.frow(t, "Invite code", self.inputs.join_code.clone()))
            .child(self.frow(t, "Your name", self.inputs.join_name.clone()))
            .child(div().ml(px(110.)).child(small(
                t,
                "The owner may have to approve you before you can reach anything.",
            )))
            .child(self.actions(
                t,
                cx,
                (
                    "Join",
                    ButtonKind::Primary,
                    Box::new(|r, cx| r.join_network(cx)),
                ),
            ))
    }

    fn sheet_invite(&mut self, t: &Theme, id: String, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(net) = self.network(&id).cloned() else {
            return col().child(self.sheet_head(t, "Invite", "This network is gone."));
        };
        let owner = self.my_id().is_some_and(|me| net.is_owner(&me));
        let code = net.invite_code();
        let current = net.current_invite().cloned();
        let (expires, approval) = current
            .as_ref()
            .map_or((InviteExpiry::Never, Approval::AskMe), |i| {
                (i.expires, i.approval)
            });
        let mono = if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "Consolas"
        };
        let lede = if owner {
            "Send this code to a friend. They enter it under Join Network."
        } else {
            "The owner lets members pass this code on. Your friend enters it under Join Network."
        };
        let code_box =
            match code.clone() {
                Some(code) => col()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .py_2p5()
                            .rounded(px(8.))
                            .bg(t.field)
                            .border_1()
                            .border_color(t.btn_stroke)
                            .font_family(mono)
                            .text_size(px(12.5))
                            .child(code.clone()),
                    )
                    .child(
                        row()
                            .justify_center()
                            .gap_1p5()
                            .child(
                                button(
                                    t,
                                    "invite-copy",
                                    Some("copy"),
                                    "Copy",
                                    ButtonKind::Secondary,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.copy_invite(code.clone(), cx),
                                )),
                            )
                            .when(owner, |el| {
                                let id = id.clone();
                                el.child(
                                    button(
                                        t,
                                        "invite-new",
                                        Some("refresh"),
                                        "New code",
                                        ButtonKind::Secondary,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, _| {
                                            this.new_invite(id.clone(), expires, approval)
                                        },
                                    )),
                                )
                            }),
                    ),
                None => col()
                    .gap_2()
                    .child(small(
                        t,
                        "There is no working invite code. Make a new one to invite someone.",
                    ))
                    .when(owner, |el| {
                        let id = id.clone();
                        el.child(
                            row().justify_center().child(
                                button(
                                    t,
                                    "invite-new",
                                    Some("plus"),
                                    "New code",
                                    ButtonKind::Primary,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, _| {
                                        this.new_invite(
                                            id.clone(),
                                            InviteExpiry::Never,
                                            Approval::AskMe,
                                        )
                                    },
                                )),
                            ),
                        )
                    }),
            };
        let has_code = code.is_some();
        col()
            .child(self.sheet_head(t, &format!("Invite to {}", net.name), lede))
            .child(code_box)
            .when(owner && has_code, |el| {
                let (i1, i2) = (id.clone(), id.clone());
                el.child(div().mt_3())
                    .child(self.frow(
                        t,
                        "Expires",
                        popup(t, "invite-expiry", expiry_label(expires)).on_click(cx.listener(
                            move |this, ev: &gpui::ClickEvent, _, cx| {
                                let at = this.menu_at(ev.position());
                                this.toggle_menu(MenuKind::InviteExpiry(i1.clone()), at, cx);
                            },
                        )),
                    ))
                    .child(self.frow(
                        t,
                        "Approval",
                        popup(t, "invite-approval", approval_label(approval)).on_click(
                            cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                                let at = this.menu_at(ev.position());
                                this.toggle_menu(MenuKind::InviteApproval(i2.clone()), at, cx);
                            }),
                        ),
                    ))
            })
            .child(
                row()
                    .mt_4()
                    .when(owner && has_code, |el| {
                        let id = id.clone();
                        el.child(
                            div()
                                .id("invite-revoke")
                                .px_3()
                                .py_1()
                                .rounded(px(7.))
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.red)
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .child("Revoke code")
                                .on_click(cx.listener(move |this, _, _, _| {
                                    let id = id.clone();
                                    this.run_then(
                                        move |n| async move { n.revoke_invites(&id).await },
                                        |()| crate::state::Msg::Info("Invite code revoked".into()),
                                    )
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        button(t, "sheet-ok", None, "Done", ButtonKind::Primary)
                            .on_click(cx.listener(|this, _, _, cx| this.close_overlays(cx))),
                    ),
            )
    }

    fn sheet_rename_network(
        &mut self,
        t: &Theme,
        id: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = self
            .network(&id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        col()
            .child(self.sheet_head(
                t,
                &format!("Rename {name}"),
                "Everyone in the network sees the new name.",
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

    fn sheet_remove_member(
        &mut self,
        t: &Theme,
        id: String,
        peer: NodeId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(net) = self.network(&id).cloned() else {
            return col();
        };
        let who = self.member_name(&net, peer);
        col()
            .child(
                row()
                    .items_start()
                    .gap_3p5()
                    .child(avatar(t, &who, &peer.to_string(), 44.))
                    .child(self.sheet_head(
                        t,
                        &format!("Remove {who} from {}?", net.name),
                        &format!("{who} will lose access to every service shared in this network. Tunnels on their side close immediately. You can invite them again later."),
                    )),
            )
            .child(self.actions(
                t,
                cx,
                (
                    "Remove",
                    ButtonKind::Danger,
                    Box::new(move |r, cx| {
                        let id = id.clone();
                        r.sheet = None;
                        r.run(move |n| async move { n.remove_member(&id, peer).await });
                        cx.notify();
                    }),
                ),
            ))
    }

    fn sheet_leave(&mut self, t: &Theme, id: String, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(net) = self.network(&id).cloned() else {
            return col();
        };
        let (title, lede, label) = if net.pending {
            (
                format!("Cancel your request to join {}?", net.name),
                "You can ask again later with an invite code.".to_string(),
                "Cancel request",
            )
        } else {
            (
                format!("Leave {}?", net.name),
                "You lose access to everything shared in this network, and your tunnels to its members close. You need a new invite to come back.".to_string(),
                "Leave",
            )
        };
        col()
            .child(self.sheet_head(t, &title, &lede))
            .child(self.actions(
                t,
                cx,
                (
                    label,
                    ButtonKind::Danger,
                    Box::new(move |r, cx| {
                        let id = id.clone();
                        r.sheet = None;
                        r.run(move |n| async move { n.leave_network(&id).await });
                        cx.notify();
                    }),
                ),
            ))
    }

    fn sheet_delete(&mut self, t: &Theme, id: String, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .network(&id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        col()
            .child(self.sheet_head(
                t,
                &format!("Delete {name}?"),
                "Every member loses access to everything shared in it, and its invite codes stop working. This cannot be undone.",
            ))
            .child(self.actions(
                t,
                cx,
                (
                    "Delete",
                    ButtonKind::Danger,
                    Box::new(move |r, cx| {
                        let id = id.clone();
                        r.sheet = None;
                        r.run(move |n| async move { n.delete_network(&id).await });
                        cx.notify();
                    }),
                ),
            ))
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
                let mut items = peer_items(id);
                items.push(MenuItem::Sep);
                items.push(MenuItem::danger("trash", "Remove…", move |r, cx| {
                    r.open_sheet(Sheet::RemovePeer(id), cx)
                }));
                items
            }
            MenuKind::Member(net, id) => {
                let mut items = peer_items(id);
                let owner = self
                    .network(&net)
                    .zip(self.my_id())
                    .is_some_and(|(n, me)| n.is_owner(&me));
                if owner {
                    let others = self.owned_networks(Some(&net));
                    if !others.is_empty() {
                        items.push(MenuItem::Sep);
                        items.push(MenuItem::Header("Move to".into()));
                        for to in others {
                            let from = net.clone();
                            items.push(MenuItem::action("users", to.name.clone(), move |r, _| {
                                let (from, to) = (from.clone(), to.id.clone());
                                r.run(move |n| async move { n.move_member(&from, &to, id).await })
                            }));
                        }
                    }
                    items.push(MenuItem::Sep);
                    items.push(MenuItem::danger(
                        "trash",
                        "Remove from network…",
                        move |r, cx| r.open_sheet(Sheet::RemoveMember(net.clone(), id), cx),
                    ));
                }
                items
            }
            MenuKind::Network(id) => {
                let net = self.network(&id).cloned()?;
                let owner = self.my_id().is_some_and(|me| net.is_owner(&me));
                let collapsed = self.collapsed.contains(&id);
                let mut items = Vec::new();
                if owner {
                    let i = id.clone();
                    items.push(MenuItem::action("pencil", "Rename…", move |r, cx| {
                        r.start_rename_network(i.clone(), cx)
                    }));
                }
                if owner || net.invite_code().is_some() {
                    let i = id.clone();
                    items.push(MenuItem::action("link", "Invite code…", move |r, cx| {
                        r.open_sheet(Sheet::Invite(i.clone()), cx)
                    }));
                }
                if !items.is_empty() {
                    items.push(MenuItem::Sep);
                }
                let i = id.clone();
                items.push(MenuItem::check(collapsed, "Collapse", move |r, cx| {
                    r.toggle_collapsed(i.clone(), cx)
                }));
                items.push(MenuItem::Sep);
                if owner {
                    items.push(MenuItem::danger(
                        "trash",
                        "Delete network…",
                        move |r, cx| r.open_sheet(Sheet::DeleteNetwork(id.clone()), cx),
                    ));
                } else {
                    let label = if net.pending {
                        "Cancel request…"
                    } else {
                        "Leave network…"
                    };
                    items.push(MenuItem::danger("x", label, move |r, cx| {
                        r.open_sheet(Sheet::LeaveNetwork(id.clone()), cx)
                    }));
                }
                items
            }
            MenuKind::NewNetWho => WhoCanJoin::ALL
                .into_iter()
                .map(|w| {
                    MenuItem::check(self.new_network.who == w, w.label(), move |r, cx| {
                        r.new_network.who = w;
                        cx.notify();
                    })
                })
                .collect(),
            MenuKind::NewNetExpiry => EXPIRY_CHOICES
                .into_iter()
                .map(|e| {
                    MenuItem::check(
                        self.new_network.expires == e,
                        expiry_label(e),
                        move |r, cx| {
                            r.new_network.expires = e;
                            cx.notify();
                        },
                    )
                })
                .collect(),
            MenuKind::InviteExpiry(id) => {
                let inv = self.network(&id)?.current_invite()?.clone();
                EXPIRY_CHOICES
                    .into_iter()
                    .map(|e| {
                        let id = id.clone();
                        MenuItem::check(inv.expires == e, expiry_label(e), move |r, _| {
                            r.update_invite(id.clone(), e, inv.approval)
                        })
                    })
                    .collect()
            }
            MenuKind::InviteApproval(id) => {
                let inv = self.network(&id)?.current_invite()?.clone();
                [Approval::AskMe, Approval::Auto]
                    .into_iter()
                    .map(|a| {
                        let id = id.clone();
                        MenuItem::check(inv.approval == a, approval_label(a), move |r, _| {
                            r.update_invite(id.clone(), inv.expires, a)
                        })
                    })
                    .collect()
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
                let mut items = vec![
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
                ];
                let networks: Vec<_> = self
                    .config
                    .networks
                    .iter()
                    .filter(|n| n.active())
                    .cloned()
                    .collect();
                if !networks.is_empty() {
                    let base = svc.service.clone();
                    items.push(MenuItem::Sep);
                    items.push(MenuItem::Header("Share with".into()));
                    let everyone = base.networks.is_empty();
                    let s = base.clone();
                    items.push(MenuItem::check(
                        everyone,
                        "Everyone I'm connected to",
                        move |r, _| {
                            let mut s = s.clone();
                            s.networks.clear();
                            r.run(move |n| async move { n.add_service(s).await })
                        },
                    ));
                    for net in networks {
                        let on = base.networks.contains(&net.id);
                        let s = base.clone();
                        items.push(MenuItem::check(on, net.name.clone(), move |r, cx| {
                            // Unchecking the last network would share it with everyone.
                            if on && s.networks.len() == 1 {
                                r.show_error("Pick another network first, or choose Everyone.", cx);
                                return;
                            }
                            let mut s = s.clone();
                            if on {
                                s.networks.retain(|n| n != &net.id);
                            } else {
                                s.networks.push(net.id.clone());
                            }
                            r.run(move |n| async move { n.add_service(s).await })
                        }));
                    }
                }
                items.push(MenuItem::Sep);
                items.push(MenuItem::danger(
                    "trash",
                    "Stop sharing…",
                    move |r, cx| r.open_sheet(Sheet::RemoveService(n2.clone()), cx),
                ));
                items
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
                    MenuItem::Header(text) => div()
                        .px_2()
                        .pt_1()
                        .pb_0p5()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.fg2)
                        .child(text)
                        .into_any_element(),
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
    /// Section title, not clickable.
    Header(SharedString),
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

/// Rename, Copy ID, Reconnect: the first items of every peer menu.
fn peer_items(id: NodeId) -> Vec<MenuItem> {
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
    ]
}

pub fn proto_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    }
}
