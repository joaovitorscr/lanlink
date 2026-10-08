//! Tray (Windows notification area) / menu bar (macOS) icon. gpui has no tray API, so this
//! uses `tray-icon`, which only needs the platform event loop gpui already runs on the main
//! thread. Menu and click events are forwarded to a gpui task.

use std::cell::RefCell;

use gpui::{App, Global};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the user asked for from the tray.
pub enum TrayAction {
    Show,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    summary: MenuItem,
    last: RefCell<String>,
}

impl Global for Tray {}

impl Tray {
    /// Create the icon and call `on_action` on the main thread for each request.
    pub fn install(cx: &mut App, on_action: impl Fn(TrayAction, &mut App) + 'static) {
        match Self::build() {
            Ok((tray, show_id, quit_id)) => {
                cx.set_global(tray);
                Self::forward_events(cx, show_id, quit_id, on_action);
            }
            Err(e) => tracing::warn!("tray icon unavailable: {e:#}"),
        }
    }

    fn build() -> anyhow::Result<(Tray, MenuId, MenuId)> {
        let show = MenuItem::new("Show lanlink", true, None);
        let summary = MenuItem::new("Starting…", false, None);
        let quit = MenuItem::new("Quit lanlink", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &summary,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("lanlink");
        // macOS: a template image recoloured for the menu bar; the menu opens on click.
        #[cfg(target_os = "macos")]
        let builder =
            builder.with_icon_templated(icon(include_bytes!("../../../assets/tray-template.png"))?);
        // Windows: the app icon; left click shows the window, right click opens the menu.
        #[cfg(not(target_os = "macos"))]
        let builder = builder
            .with_icon(icon(include_bytes!("../../../assets/tray-color.png"))?)
            .with_menu_on_left_click(false);
        let icon = builder.build()?;

        let tray = Tray {
            icon,
            summary,
            last: RefCell::new(String::new()),
        };
        Ok((tray, show.id().clone(), quit.id().clone()))
    }

    fn forward_events(
        cx: &mut App,
        show_id: MenuId,
        quit_id: MenuId,
        on_action: impl Fn(TrayAction, &mut App) + 'static,
    ) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TrayAction>();
        let menu_tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let action = if ev.id == show_id {
                TrayAction::Show
            } else if ev.id == quit_id {
                TrayAction::Quit
            } else {
                return;
            };
            let _ = menu_tx.send(action);
        }));
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            let left_up = matches!(
                ev,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            );
            let double = matches!(
                ev,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            );
            // On macOS a left click opens the menu, so only a double click shows the window.
            let show = double || (left_up && !cfg!(target_os = "macos"));
            if show {
                let _ = tx.send(TrayAction::Show);
            }
        }));
        cx.spawn(async move |cx| {
            while let Some(action) = rx.recv().await {
                if cx.update(|cx| on_action(action, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Update the connection line in the menu and the tooltip. Cheap when unchanged.
    pub fn set_summary(&self, summary: &str) {
        if *self.last.borrow() == summary {
            return;
        }
        self.summary.set_text(summary);
        if let Err(e) = self.icon.set_tooltip(Some(format!("lanlink: {summary}"))) {
            tracing::debug!("tray tooltip: {e}");
        }
        *self.last.borrow_mut() = summary.to_string();
    }
}

fn icon(png: &[u8]) -> anyhow::Result<Icon> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Icon::from_rgba(img.into_raw(), w, h)?)
}
