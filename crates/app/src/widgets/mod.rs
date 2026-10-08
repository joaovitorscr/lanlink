//! Small building blocks shared by the views, styled after macOS grouped lists.

pub mod text_input;

use gpui::{div, prelude::*, px, svg, App, ElementId, FontWeight, Hsla, SharedString};

use crate::assets::icon;
use crate::theme::Theme;

pub fn theme(cx: &App) -> Theme {
    *cx.global::<Theme>()
}

pub fn row() -> gpui::Div {
    div().flex().items_center().gap_2()
}

pub fn col() -> gpui::Div {
    div().flex().flex_col()
}

/// Tabler icon, colored like text.
pub fn ic(name: &str, size: f32, color: Hsla) -> gpui::Svg {
    svg()
        .path(icon(name))
        .flex_none()
        .size(px(size))
        .text_color(color)
}

/// Small uppercase label above a group.
pub fn glabel(t: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .mt_4()
        .mb_1p5()
        .ml_0p5()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.fg2)
        .child(text.into().to_uppercase())
}

/// Inset grouped list container. Put [`group_row`]s inside.
pub fn group(t: &Theme) -> gpui::Div {
    col()
        .rounded(px(10.))
        .bg(t.card)
        .border_1()
        .border_color(t.card_stroke)
        .overflow_hidden()
}

/// One row of a group: `[leading] title/subtitle [trailing...]`.
pub fn group_row(t: &Theme, last: bool) -> gpui::Div {
    row()
        .gap_2p5()
        .px_3p5()
        .py_2p5()
        .when(!last, |el| el.border_b_1().border_color(t.sep))
}

pub fn title_sub(
    t: &Theme,
    title: impl Into<SharedString>,
    sub: Option<impl Into<SharedString>>,
) -> gpui::Div {
    col()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(title.into()),
        )
        .when_some(sub, |el, s| {
            el.child(
                div()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(t.fg2)
                    .child(s.into()),
            )
        })
}

pub fn small(t: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(11.5))
        .text_color(t.fg2)
        .child(text.into())
}

pub fn empty(t: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .p_6()
        .text_center()
        .text_size(px(12.))
        .text_color(t.fg2)
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Danger,
}

/// Push button. Optional leading icon.
pub fn button(
    t: &Theme,
    id: impl Into<ElementId>,
    icon: Option<&str>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
) -> gpui::Stateful<gpui::Div> {
    let fg = match kind {
        ButtonKind::Primary | ButtonKind::Danger => t.white,
        ButtonKind::Secondary => t.fg,
    };
    let base = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .px_3()
        .py_1()
        .rounded(px(7.))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when_some(icon, |el, name| el.child(ic(name, 13., fg)))
        .child(label.into());
    match kind {
        ButtonKind::Primary => base
            .bg(t.blue)
            .text_color(t.white)
            .shadow_xs()
            .hover(|s| s.opacity(0.9)),
        ButtonKind::Danger => base
            .bg(t.red)
            .text_color(t.white)
            .shadow_xs()
            .hover(|s| s.opacity(0.9)),
        ButtonKind::Secondary => base
            .bg(t.btn)
            .border_1()
            .border_color(t.btn_stroke)
            .text_color(t.fg)
            .hover(|s| s.opacity(0.85)),
    }
}

/// Icon-only button (e.g. the ⋯ menu).
pub fn icon_button(t: &Theme, id: impl Into<ElementId>, name: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .child(ic(name, 15., t.fg2))
}

/// Colored pill, e.g. connection state.
pub fn pill(label: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .bg(color.opacity(0.16))
        .text_color(color)
        .child(label.into())
}

pub fn dot(color: Hsla) -> gpui::Div {
    div().flex_none().size(px(8.)).rounded_full().bg(color)
}

/// Round avatar with initials.
pub fn avatar(t: &Theme, name: &str, key: &str, size: f32) -> gpui::Div {
    let initial: String = name
        .split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(t.avatar_color(key))
        .text_color(t.white)
        .text_size(px(size * 0.4))
        .font_weight(FontWeight::SEMIBOLD)
        .child(initial)
}

/// Square tinted icon tile, used for services and sidebar items.
pub fn tile(name: &str, color: Hsla, size: f32, white: Hsla) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(size * 0.24))
        .bg(color)
        .child(ic(name, size * 0.6, white))
}

/// macOS-style switch. Attach `.on_click`.
pub fn toggle(t: &Theme, id: impl Into<ElementId>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(px(36.))
        .h(px(21.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { t.green } else { t.fg3 })
        .when(on, |el| el.justify_end())
        .cursor_pointer()
        .child(div().size(px(17.)).rounded_full().bg(t.white).shadow_xs())
}

/// Popup button (NSPopUpButton look). Attach `.on_click` to open a menu.
pub fn popup(
    t: &Theme,
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .min_w(px(110.))
        .pl_2p5()
        .pr_1()
        .py_1()
        .rounded(px(6.))
        .bg(t.btn)
        .border_1()
        .border_color(t.btn_stroke)
        .text_size(px(12.))
        .cursor_pointer()
        .child(value.into())
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded(px(4.))
                .bg(t.blue)
                .child(ic("chevron-down", 11., t.white)),
        )
}

/// Checkbox with label. Attach `.on_click`.
pub fn checkbox(
    t: &Theme,
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(px(12.5))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(15.))
                .rounded(px(4.))
                .when(on, |el| el.bg(t.blue))
                .when(!on, |el| {
                    el.bg(t.field).border_1().border_color(t.btn_stroke)
                })
                .when(on, |el| el.child(ic("check", 11., t.white))),
        )
        .child(label.into())
}

/// Element id from a prefix and any key, e.g. a peer id or service name.
pub fn eid(prefix: &str, key: impl std::fmt::Display) -> ElementId {
    ElementId::Name(SharedString::from(format!("{prefix}-{key}")))
}
