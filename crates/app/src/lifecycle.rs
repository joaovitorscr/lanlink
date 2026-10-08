//! App lifecycle: clean node shutdown on quit and on Ctrl-C / SIGTERM, hiding the window
//! instead of closing it (background mode), and launch at login.

use std::time::Duration;

use gpui::{App, Context, Window};
use lanlink_core::{ConnState, Node};

use crate::state::Root;

/// Passed by the login item so lanlink starts hidden in the tray / menu bar.
pub const HIDDEN_ARG: &str = "--hidden";

/// How long quitting waits for peers to be told we are leaving.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Close the node's connections so peers see us leave now instead of after the QUIC idle
/// timeout. Blocks the calling (UI) thread for at most [`SHUTDOWN_TIMEOUT`].
pub fn shutdown_node(rt: &tokio::runtime::Handle, node: Node) {
    // The timer must be created inside the runtime, so build it in the async block.
    let res =
        rt.block_on(async move { tokio::time::timeout(SHUTDOWN_TIMEOUT, node.shutdown()).await });
    match res {
        Ok(Ok(())) => tracing::info!("node shut down"),
        Ok(Err(e)) => tracing::warn!("node shutdown failed: {e:#}"),
        Err(_) => tracing::warn!("node shutdown timed out after {SHUTDOWN_TIMEOUT:?}"),
    }
}

/// Quit the app (running the normal shutdown) on Ctrl-C or SIGTERM. A second signal exits
/// immediately.
pub fn quit_on_stop_signal(rt: &tokio::runtime::Handle, cx: &mut App) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    rt.spawn(async move {
        stop_signal().await;
        tracing::info!("stop signal received, quitting");
        let _ = tx.send(());
        stop_signal().await;
        std::process::exit(130);
    });
    cx.spawn(async move |cx| {
        if rx.recv().await.is_some() {
            let _ = cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// Resolves on Ctrl-C, or SIGTERM on Unix. Never resolves if the handlers can't be
/// installed (e.g. no console on Windows), so that never reads as a quit request.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                res = tokio::signal::ctrl_c() => {
                    if res.is_ok() {
                        return;
                    }
                }
                _ = term.recv() => return,
            }
        }
    }
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

impl Root {
    /// Whether closing the window should hide it and keep the node running.
    pub fn keep_running_on_close(&self) -> bool {
        self.prefs.keep_in_background && self.fatal.is_none()
    }

    /// One line for the tray menu: node state and how many peers are connected.
    pub fn connection_summary(&self) -> String {
        if self.fatal.is_some() {
            return "Not running".into();
        }
        if self.node.is_none() {
            return "Starting…".into();
        }
        let n = self
            .peers
            .iter()
            .filter(|p| matches!(p.state, ConnState::Direct | ConnState::Relayed))
            .count();
        match n {
            0 => "No peers connected".into(),
            1 => "1 peer connected".into(),
            n => format!("{n} peers connected"),
        }
    }

    pub fn toggle_keep_in_background(&mut self, cx: &mut Context<Self>) {
        self.prefs.keep_in_background = !self.prefs.keep_in_background;
        self.save_prefs(cx);
    }

    pub fn toggle_hide_ids(&mut self, cx: &mut Context<Self>) {
        self.prefs.hide_ids = !self.prefs.hide_ids;
        self.save_prefs(cx);
    }

    pub fn toggle_launch_at_login(&mut self, cx: &mut Context<Self>) {
        let enable = !self.prefs.launch_at_login;
        match set_launch_at_login(enable) {
            Ok(()) => {
                self.prefs.launch_at_login = enable;
                self.save_prefs(cx);
            }
            Err(e) => self.show_error(format!("Could not change launch at login: {e:#}"), cx),
        }
    }
}

/// Where the tray icon lives, in the platform's words.
pub const TRAY_PLACE: &str = if cfg!(target_os = "macos") {
    "menu bar"
} else {
    "notification area"
};

/// Hide the window without closing it; the app keeps running.
pub fn hide_window(window: &Window, cx: &mut App) {
    if let Some(hide) = native::hide(window) {
        defer_native(cx, hide);
    }
}

/// Bring a hidden (or minimized) window back to the front.
pub fn show_window(window: &Window, cx: &mut App) {
    if let Some(show) = native::show(window) {
        defer_native(cx, show);
    }
    cx.activate(true);
}

/// Run a native window call from the event loop rather than inside a gpui update: showing
/// or hiding a window synchronously calls back into gpui, which would then be re-entered
/// (gpui defers its own `activate_window` the same way).
fn defer_native(cx: &mut App, f: impl FnOnce() + 'static) {
    cx.foreground_executor().spawn(async move { f() }).detach();
}

#[cfg(target_os = "macos")]
mod native {
    use gpui::Window;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSView, NSWindow};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        // SAFETY: gpui hands out the NSView it created for this window; it outlives the call.
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        view.window()
    }

    pub fn hide(window: &Window) -> Option<impl FnOnce() + 'static> {
        let w = ns_window(window)?;
        Some(move || w.orderOut(None))
    }

    /// Also deminiaturizes: `makeKeyAndOrderFront:` restores a minimized window.
    pub fn show(window: &Window) -> Option<impl FnOnce() + 'static> {
        let w = ns_window(window)?;
        Some(move || w.makeKeyAndOrderFront(None))
    }
}

#[cfg(target_os = "windows")]
mod native {
    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW,
    };

    fn hwnd(window: &Window) -> Option<HWND> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(h) = handle.as_raw() else {
            return None;
        };
        Some(HWND(h.hwnd.get() as *mut core::ffi::c_void))
    }

    pub fn hide(window: &Window) -> Option<impl FnOnce() + 'static> {
        let hwnd = hwnd(window)?;
        // SAFETY: gpui keeps the HWND alive while the window exists, and lanlink never
        // closes its window without quitting.
        Some(move || unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        })
    }

    pub fn show(window: &Window) -> Option<impl FnOnce() + 'static> {
        let hwnd = hwnd(window)?;
        // SAFETY: as above.
        Some(move || unsafe {
            let cmd = if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            let _ = ShowWindow(hwnd, cmd);
            let _ = SetForegroundWindow(hwnd);
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod native {
    pub fn hide(window: &gpui::Window) -> Option<fn()> {
        window.minimize_window();
        None
    }

    pub fn show(_window: &gpui::Window) -> Option<fn()> {
        None
    }
}

/// Register or remove the login item: a LaunchAgent on macOS, the per-user Run key on
/// Windows. It starts this executable with [`HIDDEN_ARG`].
pub fn set_launch_at_login(enable: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let auto = auto_launch::AutoLaunchBuilder::new()
        .set_app_name("lanlink")
        .set_app_path(&exe.to_string_lossy())
        .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
        .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
        .set_args(&[HIDDEN_ARG])
        .build()?;
    if enable {
        auto.enable()?;
    } else if auto.is_enabled()? {
        auto.disable()?;
    }
    Ok(())
}
