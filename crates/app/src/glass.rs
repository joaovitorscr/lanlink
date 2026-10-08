//! Window backdrop. On macOS 26+ a native Liquid Glass view (`NSGlassEffectView`) sits
//! behind gpui's content; the system applies the user's Appearance (Clear/Tinted) and
//! Accessibility (Reduce Transparency) choices to it. Elsewhere, or on older macOS, gpui's
//! own blurred background is used (NSVisualEffectView on macOS, acrylic on Windows).

use gpui::{Window, WindowBackgroundAppearance};

/// Apply the backdrop for the current preference. Returns the appearance gpui should use.
pub fn apply(window: &Window, transparent: bool) -> WindowBackgroundAppearance {
    if !transparent {
        mac::remove(window);
        return WindowBackgroundAppearance::Opaque;
    }
    if mac::install(window) {
        // The glass view is the backdrop; gpui only needs to leave the window transparent.
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Blurred
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use gpui::Window;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSGlassEffectView, NSView, NSWindowOrderingMode,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::RefCell;

    thread_local! {
        static GLASS: RefCell<Option<Retained<NSGlassEffectView>>> = const { RefCell::new(None) };
    }

    fn content_view(window: &Window) -> Option<Retained<NSView>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        // SAFETY: gpui hands out the NSView it created for this window; it outlives the call.
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let ns_window = view.window()?;
        ns_window.contentView()
    }

    /// Insert the glass view below gpui's content. Idempotent. False when unavailable.
    pub fn install(window: &Window) -> bool {
        if GLASS.with(|g| g.borrow().is_some()) {
            return true;
        }
        if AnyClass::get(c"NSGlassEffectView").is_none() {
            tracing::info!("NSGlassEffectView unavailable, using blur");
            return false;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some(content) = content_view(window) else {
            return false;
        };
        let glass =
            NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), content.bounds());
        glass.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, None);
        GLASS.with(|g| *g.borrow_mut() = Some(glass));
        tracing::info!("liquid glass backdrop installed");
        true
    }

    pub fn remove(_window: &Window) {
        GLASS.with(|g| {
            if let Some(glass) = g.borrow_mut().take() {
                glass.removeFromSuperview();
            }
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    use gpui::Window;
    pub fn install(_: &Window) -> bool {
        false
    }
    pub fn remove(_: &Window) {}
}
