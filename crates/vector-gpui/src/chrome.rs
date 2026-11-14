//! The window's own title strip on every OS, as the desktop client draws it (`decorations: false`).

/// macOS: a titled window under transparent chrome, since GPUI's untitled one can't resize,
/// minimise or close. Elsewhere none: the strip is all there is.
pub fn titlebar() -> Option<gpui::TitlebarOptions> {
    if cfg!(target_os = "macos") {
        Some(gpui::TitlebarOptions { title: None, appears_transparent: true, traffic_light_position: None })
    } else {
        None
    }
}

/// The traffic lights go: the strip's own buttons stand in for them.
#[cfg(target_os = "macos")]
pub fn hide_native_controls(window: &gpui::Window) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    // SAFETY: AppKit's handle is the window's live content view, used on the main thread.
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else { return };
    for kind in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = ns_window.standardWindowButton(kind) {
            button.setHidden(true);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_native_controls(_: &gpui::Window) {}
