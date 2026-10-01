//! Pointer lock for Mini App windows on macOS.
//!
//! WKWebView refuses every `requestPointerLock()` unless its UI delegate
//! implements the private `_webViewDidRequestPointerLock:completionHandler:`,
//! and wry's delegate does not, so mouse-look games lose the cursor. The method
//! is added at runtime to the class of the main webview's delegate (wry's, under
//! a version-suffixed runtime name). WebKit reads a delegate's methods when the
//! delegate is assigned, so `install` must run before a Mini App webview is built.

use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicBool, Ordering};

use block2::Block;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{msg_send, sel};

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Add the grant to wry's delegate class, found through the main window.
pub(crate) async fn install(app: &tauri::AppHandle) {
    use tauri::Manager;
    if INSTALLED.load(Ordering::Acquire) {
        return;
    }
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let queued = main.with_webview(move |wv| unsafe {
        let view = wv.inner() as *mut AnyObject;
        let delegate: *mut AnyObject = msg_send![view, UIDelegate];
        let _ = tx.send(!delegate.is_null() && add_grant(objc2::ffi::object_getClass(delegate)));
    });
    if queued.is_err() {
        return;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(true)) => INSTALLED.store(true, Ordering::Release),
        _ => log_warn!("[WEBXDC] pointer lock: could not reach the webview delegate class"),
    }
}

/// Returns true when the class now carries the grant (added now or before).
unsafe fn add_grant(class: *const AnyClass) -> bool {
    if class.is_null() {
        return false;
    }
    let selector = sel!(_webViewDidRequestPointerLock:completionHandler:);
    let imp = std::mem::transmute::<
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut Block<dyn Fn(Bool)>),
        Imp,
    >(did_request_pointer_lock);
    let added = objc2::ffi::class_addMethod(class as *mut AnyClass, selector, imp, c"v@:@@?".as_ptr());
    added.as_bool() || (*class).responds_to(selector)
}

unsafe extern "C-unwind" fn did_request_pointer_lock(
    _this: *mut AnyObject,
    _cmd: Sel,
    web_view: *mut AnyObject,
    handler: *mut Block<dyn Fn(Bool)>,
) {
    // Only the window the user is in may take the pointer.
    let allow = !web_view.is_null() && in_key_window(web_view) && is_miniapp_url(&current_url(web_view));
    if let Some(handler) = handler.as_ref() {
        handler.call((Bool::new(allow),));
    }
}

unsafe fn in_key_window(web_view: *mut AnyObject) -> bool {
    let window: *mut AnyObject = msg_send![web_view, window];
    if window.is_null() {
        return false;
    }
    let key: Bool = msg_send![window, isKeyWindow];
    key.as_bool()
}

unsafe fn current_url(web_view: *mut AnyObject) -> String {
    let url: *mut AnyObject = msg_send![web_view, URL];
    if url.is_null() {
        return String::new();
    }
    let text: *mut AnyObject = msg_send![url, absoluteString];
    if text.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![text, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

/// Mini App pages only: the custom scheme, or a live loopback host.
fn is_miniapp_url(url: &str) -> bool {
    let Ok(url) = tauri::Url::parse(url) else {
        return false;
    };
    match url.scheme() {
        "webxdc" => true,
        "http" => {
            url.host_str() == Some("localhost")
                && url.port().is_some_and(super::isolated::is_live_port)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_miniapp_pages_get_the_lock() {
        assert!(is_miniapp_url("webxdc://abc123.host/index.html"));
        assert!(!is_miniapp_url("tauri://localhost/index.html"));
        assert!(!is_miniapp_url("https://example.com/"));
        // A loopback port that is not serving a Mini App is refused.
        assert!(!is_miniapp_url("http://localhost:1/index.html"));
        assert!(!is_miniapp_url("not a url"));
    }
}
