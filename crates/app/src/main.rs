#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! lanlink desktop app. Everything a user needs is in the GUI; no terminal required.

mod assets;
mod format;
mod glass;
mod state;
mod theme;
mod update;
mod views;
mod widgets;

use std::panic::{catch_unwind, AssertUnwindSafe};

use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use crate::state::Root;
use crate::theme::{Prefs, Theme};

fn init_logging() -> Option<lanlink_core::LogGuard> {
    match catch_unwind(AssertUnwindSafe(|| lanlink_core::init_logging("app"))) {
        Ok(guard) => Some(guard),
        Err(_) => {
            // Core logging not available yet: fall back to stderr.
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .try_init();
            None
        }
    }
}

fn main() {
    // Panics from core calls are caught and shown in the UI; log them instead of printing.
    // Installed before logging so a todo!() in init_logging stays quiet too.
    std::panic::set_hook(Box::new(|info| {
        tracing::warn!(
            "panic: {} at {}",
            state::panic_message(info.payload()),
            info.location().map(|l| l.to_string()).unwrap_or_default()
        );
    }));
    let _log_guard = init_logging();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    // Keep the runtime alive on its own thread for the life of the process.
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            widgets::text_input::bind_keys(cx);
            let prefs = Prefs::load();
            cx.set_global(Theme::new(true, prefs.transparency));
            let bounds = Bounds::centered(None, size(px(820.), px(540.)), cx);
            let titlebar = if cfg!(target_os = "macos") {
                TitlebarOptions {
                    title: Some("lanlink".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(16.))),
                }
            } else {
                TitlebarOptions {
                    title: Some("lanlink".into()),
                    ..Default::default()
                }
            };
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(640.), px(420.))),
                    is_resizable: true,
                    titlebar: Some(titlebar),
                    window_background: gpui::WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| Root::new(handle, cx)),
            )
            .expect("open window");
            cx.on_window_closed(|cx| cx.quit()).detach();
            cx.activate(true);
        });
}
