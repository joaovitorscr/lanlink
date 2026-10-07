#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! lanlink desktop app. Everything a user needs is in the GUI; no terminal required.

mod format;
mod state;
mod theme;
mod views;
mod widgets;

use std::panic::{catch_unwind, AssertUnwindSafe};

use gpui::{prelude::*, px, size, App, Application, Bounds, WindowBounds, WindowOptions};

use crate::state::Root;

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
    let _log_guard = init_logging();
    // Panics from core calls are caught and shown in the UI; log them instead of printing.
    std::panic::set_hook(Box::new(|info| {
        tracing::warn!(
            "panic: {} at {}",
            state::panic_message(info.payload()),
            info.location().map(|l| l.to_string()).unwrap_or_default()
        );
    }));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    // Keep the runtime alive on its own thread for the life of the process.
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));

    Application::new().run(move |cx: &mut App| {
        widgets::text_input::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(480.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(420.), px(560.))),
                is_resizable: true,
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Root::new(handle, cx)),
        )
        .expect("open window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
