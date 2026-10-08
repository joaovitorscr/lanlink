#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! lanlink desktop app. Everything a user needs is in the GUI; no terminal required.

mod assets;
mod format;
mod glass;
mod lifecycle;
mod state;
mod theme;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod tray;
mod update;
mod views;
mod widgets;

use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, Entity, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions,
};

use crate::state::Root;
use crate::theme::{Prefs, Theme};

fn main() {
    // Send panics to the log file instead of printing them.
    std::panic::set_hook(Box::new(|info| {
        tracing::warn!(
            "panic: {} at {}",
            state::panic_message(info.payload()),
            info.location().map(|l| l.to_string()).unwrap_or_default()
        );
    }));
    let _log_guard = lanlink_core::init_logging("app");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    // Keep the runtime alive on its own thread for the life of the process.
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));

    let launched_hidden = std::env::args().any(|a| a == lifecycle::HIDDEN_ARG);

    let app = Application::new().with_assets(assets::Assets);
    // macOS: clicking the Dock icon brings back a window hidden by background mode.
    app.on_reopen(|cx| {
        if let Some(window) = cx.windows().into_iter().find_map(|w| w.downcast::<Root>()) {
            show(window, cx);
        }
    });
    app.run(move |cx: &mut App| {
        widgets::text_input::bind_keys(cx);
        let prefs = Prefs::load();
        cx.set_global(Theme::new(true, prefs.transparency));
        if prefs.launch_at_login {
            // Re-register so the login item follows the app if it was moved.
            if let Err(e) = lifecycle::set_launch_at_login(true) {
                tracing::warn!("launch at login: {e:#}");
            }
        }
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
        let start_hidden = launched_hidden && prefs.keep_in_background && tray_supported();
        let rt = handle.clone();
        let window: WindowHandle<Root> = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(640.), px(420.))),
                    is_resizable: true,
                    titlebar: Some(titlebar),
                    window_background: gpui::WindowBackgroundAppearance::Transparent,
                    show: !start_hidden,
                    focus: !start_hidden,
                    ..Default::default()
                },
                |window, cx| {
                    let root = cx.new(|cx| Root::new(rt, cx));
                    let r = root.clone();
                    // Background mode: hide instead of closing while the tray is up.
                    window.on_window_should_close(cx, move |window, cx| {
                        if r.read(cx).keep_running_on_close() && tray_installed(cx) {
                            lifecycle::hide_window(window, cx);
                            false
                        } else {
                            true
                        }
                    });
                    root
                },
            )
            .expect("open window");
        let root = window.entity(cx).expect("root view");

        install_tray(cx, window, &root);
        cx.on_window_closed(|cx| cx.quit()).detach();
        let rt = handle.clone();
        let quitting = root.clone();
        cx.on_app_quit(move |cx| {
            if let Some(node) = quitting.read(cx).node.clone() {
                lifecycle::shutdown_node(&rt, node);
            }
            remove_tray(cx);
            async {}
        })
        .detach();
        lifecycle::quit_on_stop_signal(&handle, cx);
        if !start_hidden {
            cx.activate(true);
        }
    });
}

fn show(window: WindowHandle<Root>, cx: &mut App) {
    let _ = window.update(cx, |_, window, cx| lifecycle::show_window(window, cx));
}

fn tray_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn tray_installed(cx: &App) -> bool {
    cx.has_global::<tray::Tray>()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn tray_installed(_cx: &App) -> bool {
    false
}

/// Tray icon with Show / Quit and a live connection summary. If it can't be created the
/// window is shown, since closing must then quit (there would be no way back).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn install_tray(cx: &mut App, window: WindowHandle<Root>, root: &Entity<Root>) {
    tray::Tray::install(cx, move |action, cx| match action {
        tray::TrayAction::Show => show(window, cx),
        tray::TrayAction::Quit => cx.quit(),
    });
    if !tray_installed(cx) {
        show(window, cx);
        return;
    }
    cx.observe(root, |root, cx| {
        let summary = root.read(cx).connection_summary();
        cx.global::<tray::Tray>().set_summary(&summary);
    })
    .detach();
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn install_tray(_cx: &mut App, _window: WindowHandle<Root>, _root: &Entity<Root>) {}

/// Drop the tray icon before exit so Windows doesn't leave a stale one behind.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn remove_tray(cx: &mut App) {
    if tray_installed(cx) {
        cx.remove_global::<tray::Tray>();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn remove_tray(_cx: &mut App) {}
