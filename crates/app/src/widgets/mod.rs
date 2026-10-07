//! Small building blocks shared by the views.

pub mod text_input;

use gpui::{div, prelude::*, px, rgb, ElementId, Hsla, SharedString};

use crate::theme::*;

pub fn row() -> gpui::Div {
    div().flex().items_center().gap_2()
}

pub fn col() -> gpui::Div {
    div().flex().flex_col().gap_2()
}

/// A panel card with an optional small uppercase title.
pub fn card(title: Option<&str>) -> gpui::Div {
    col()
        .p_3()
        .rounded_lg()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .when_some(title, |el, t| {
            el.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(t.to_uppercase()),
            )
        })
}

pub fn muted(text: impl Into<SharedString>) -> gpui::Div {
    div().text_color(rgb(MUTED)).child(text.into())
}

pub fn small(text: impl Into<SharedString>) -> gpui::Div {
    div().text_xs().text_color(rgb(MUTED)).child(text.into())
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: u32,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(color))
        .text_color(rgb(WHITE))
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .child(label.into())
}

/// Colored pill, e.g. connection state.
pub fn badge(label: impl Into<SharedString>, color: u32) -> gpui::Div {
    div()
        .flex_none()
        .px_2()
        .rounded_md()
        .text_xs()
        .bg(Hsla::from(rgb(color)).opacity(0.2))
        .text_color(rgb(color))
        .child(label.into())
}

pub fn dot(color: u32) -> gpui::Div {
    div().flex_none().size(px(8.)).rounded_full().bg(rgb(color))
}

/// On/off switch with a label to its right. Attach `.on_click`.
pub fn switch(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let track = div()
        .flex()
        .flex_none()
        .w(px(28.))
        .h(px(16.))
        .p(px(2.))
        .rounded_full()
        .bg(rgb(if on { ACCENT } else { BORDER }))
        .when(on, |el| el.justify_end())
        .child(div().size(px(12.)).rounded_full().bg(rgb(WHITE)));
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .child(track)
        .child(div().text_xs().text_color(rgb(MUTED)).child(label.into()))
}

/// Element id from a prefix and any key, e.g. a peer id or service name.
pub fn eid(prefix: &str, key: impl std::fmt::Display) -> ElementId {
    ElementId::Name(SharedString::from(format!("{prefix}-{key}")))
}
