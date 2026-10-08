//! Single-line text input: cursor, selection (shift-arrows, select-all), clipboard, Enter to
//! submit, Tab to move between inputs. Editing keys are gpui actions bound in the
//! `TextInput` key context; printable characters arrive as unhandled key-downs.

use std::ops::Range;

use gpui::{
    actions, div, prelude::*, px, AnyElement, App, ClipboardItem, Context, CursorStyle,
    EventEmitter, FocusHandle, Focusable, KeyBinding, KeyDownEvent, MouseButton, SharedString,
    Window,
};

use crate::widgets::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Copy,
        Cut,
        Paste,
        Submit,
        FocusNext,
        FocusPrev,
    ]
);

const CONTEXT: &str = "TextInput";

/// Register the key bindings. `secondary` is Cmd on macOS and Ctrl elsewhere.
pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("secondary-a", SelectAll, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("secondary-c", Copy, c),
        KeyBinding::new("secondary-x", Cut, c),
        KeyBinding::new("secondary-v", Paste, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("tab", FocusNext, c),
        KeyBinding::new("shift-tab", FocusPrev, c),
    ]);
}

pub enum InputEvent {
    Submit,
}

pub struct TextInput {
    focus: FocusHandle,
    text: String,
    placeholder: SharedString,
    /// Byte offsets; the selection is between `anchor` and `cursor`.
    cursor: usize,
    anchor: usize,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle().tab_stop(true),
            text: String::new(),
            placeholder: placeholder.into(),
            cursor: 0,
            anchor: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = clean(&text.into());
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        cx.notify();
    }

    /// Trimmed contents; clears the input.
    pub fn take(&mut self, cx: &mut Context<Self>) -> String {
        let t = self.text.trim().to_string();
        self.set_text("", cx);
        t
    }

    fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| i + c.len_utf8())
    }

    fn move_to(&mut self, i: usize, cx: &mut Context<Self>) {
        self.cursor = i;
        self.anchor = i;
        cx.notify();
    }

    fn select_to(&mut self, i: usize, cx: &mut Context<Self>) {
        self.cursor = i;
        cx.notify();
    }

    fn replace_selection(&mut self, s: &str, cx: &mut Context<Self>) {
        let r = self.selection();
        let s = clean(s);
        self.text.replace_range(r.clone(), &s);
        self.move_to(r.start + s.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor == self.anchor {
            self.anchor = self.prev(self.cursor);
        }
        self.replace_selection("", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor == self.anchor {
            self.anchor = self.next(self.cursor);
        }
        self.replace_selection("", cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.selection();
        let to = if r.is_empty() {
            self.prev(self.cursor)
        } else {
            r.start
        };
        self.move_to(to, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.selection();
        let to = if r.is_empty() {
            self.next(self.cursor)
        } else {
            r.end
        };
        self.move_to(to, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.prev(self.cursor), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next(self.cursor), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.anchor = 0;
        self.select_to(self.text.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let r = self.selection();
        if !r.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[r].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, w: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, w, cx);
        self.replace_selection("", cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.replace_selection(t.trim(), cx);
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, _: &mut Context<Self>) {
        window.focus_next();
    }

    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, _: &mut Context<Self>) {
        window.focus_prev();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        // Ctrl+Alt is AltGr on Windows keyboards and still types characters.
        if m.platform || m.function || (m.control && !m.alt) {
            return;
        }
        if let Some(ch) = &ev.keystroke.key_char {
            if !clean(ch).is_empty() {
                self.replace_selection(ch, cx);
                cx.stop_propagation();
            }
        }
    }
}

/// Single line: drop newlines and other control characters.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let t = theme(cx);
        let caret = move || div().flex_none().w(px(1.5)).h(px(16.)).bg(t.fg);
        let span = |s: &str| div().flex_none().child(s.to_string());

        let mut parts: Vec<AnyElement> = Vec::new();
        if self.text.is_empty() {
            if focused {
                parts.push(caret().into_any_element());
            }
            parts.push(
                div()
                    .text_color(t.fg3)
                    .child(self.placeholder.clone())
                    .into_any_element(),
            );
        } else {
            let r = self.selection();
            if r.is_empty() {
                parts.push(span(&self.text[..self.cursor]).into_any_element());
                if focused {
                    parts.push(caret().into_any_element());
                }
                parts.push(span(&self.text[self.cursor..]).into_any_element());
            } else {
                parts.push(span(&self.text[..r.start]).into_any_element());
                parts.push(
                    span(&self.text[r.clone()])
                        .bg(if focused {
                            t.blue.opacity(0.35)
                        } else {
                            t.fg3.opacity(0.35)
                        })
                        .into_any_element(),
                );
                parts.push(span(&self.text[r.end..]).into_any_element());
            }
        }

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus);
                    this.move_to(this.text.len(), cx);
                }),
            )
            .flex_1()
            .min_w_0()
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded(px(6.))
            .bg(t.field)
            .border_1()
            .border_color(if focused { t.blue } else { t.card_stroke })
            .text_color(t.fg)
            .children(parts)
    }
}
