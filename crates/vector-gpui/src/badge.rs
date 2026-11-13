//! The unread count on the app's icon: the dock badge on macOS, an overlay on the Windows
//! taskbar button, and the Unity launcher count Linux docks and panels read.

use gpui::Window;

pub fn set(window: &Window, count: u32) {
    platform::set(window, count);
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    pub fn set(_: &gpui::Window, count: u32) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm).dockTile().setBadgeLabel(label.as_deref());
    }
}

#[cfg(windows)]
mod platform {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR};
    use windows::core::{PCWSTR, w};

    const DOT: &[u8] = include_bytes!("../../../src-tauri/icons/icon_badge_notification.png");

    pub fn set(window: &gpui::Window, count: u32) {
        let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
        let RawWindowHandle::Win32(raw) = handle.as_raw() else { return };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        // SAFETY: COM is up on GPUI's thread, and the window outlives the call.
        unsafe {
            let Ok(list) = CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else { return };
            if list.HrInit().is_err() {
                return;
            }
            if count == 0 {
                let _ = list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null());
                return;
            }
            let Ok(icon) = CreateIconFromResourceEx(DOT, true, 0x0003_0000, 16, 16, LR_DEFAULTCOLOR) else { return };
            let _ = list.SetOverlayIcon(hwnd, icon, w!("Unread messages"));
            let _ = DestroyIcon(icon);
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use gtk::gio;
    use gtk::glib::{self, ToVariant};

    pub fn set(_: &gpui::Window, count: u32) {
        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return };
        let props = glib::VariantDict::new(None);
        props.insert("count", i64::from(count));
        props.insert("count-visible", count > 0);
        let args = glib::Variant::tuple_from_iter(["application://Vector.desktop".to_variant(), props.end()]);
        let _ = bus.emit_signal(
            None,
            "/com/canonical/unity/launcherentry/vector",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            Some(&args),
        );
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    pub fn set(_: &gpui::Window, _: u32) {}
}
