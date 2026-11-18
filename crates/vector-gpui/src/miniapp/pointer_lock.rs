//! Pointer lock for Mini App webviews on macOS. WKWebView refuses every `requestPointerLock()`
//! unless its UI delegate answers the private `_webViewDidRequestPointerLock:completionHandler:`,
//! which wry's doesn't, so mouse-look games lose the cursor. The method is added to the
//! delegate's class at run time.

use std::ffi::{CStr, c_char};

use block2::Block;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{msg_send, sel};

/// Lets `view`'s page take the pointer while its window is key. WebKit reads a delegate's
/// methods when the delegate is assigned, so it is assigned again once its class has the grant.
pub fn grant(view: &wry::WebView) {
    use wry::WebViewExtMacOS;
    let wk = view.webview();
    let web_view = objc2::rc::Retained::as_ptr(&wk) as *mut AnyObject;
    // SAFETY: a live WKWebView on the main thread; `UIDelegate` is a getter returning an
    // object or nil, and wry keeps that delegate alive for the webview's life.
    unsafe {
        let delegate: *mut AnyObject = msg_send![web_view, UIDelegate];
        if delegate.is_null() || !add_grant(objc2::ffi::object_getClass(delegate)) {
            vector_core::log_warn!("[MiniApp] pointer lock: no webview delegate to extend");
            return;
        }
        let none: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![web_view, setUIDelegate: none];
        let _: () = msg_send![web_view, setUIDelegate: delegate];
    }
}

/// True when the class now carries the grant (added now or before).
///
/// # Safety
/// `class` is null or a registered Objective-C class.
unsafe fn add_grant(class: *const AnyClass) -> bool {
    if class.is_null() {
        return false;
    }
    let selector = sel!(_webViewDidRequestPointerLock:completionHandler:);
    // SAFETY: an IMP takes the receiver and selector first, then the method's arguments;
    // "v@:@@?" (void; self, _cmd, WKWebView, block) is WebKit's signature for this selector.
    let imp = unsafe {
        std::mem::transmute::<unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut Block<dyn Fn(Bool)>), Imp>(
            did_request_pointer_lock,
        )
    };
    let added = unsafe { objc2::ffi::class_addMethod(class as *mut AnyClass, selector, imp, c"v@:@@?".as_ptr()) };
    added.as_bool() || unsafe { (*class).responds_to(selector) }
}

/// Not unwinding: a panic here aborts rather than unwind into WebKit.
unsafe extern "C" fn did_request_pointer_lock(
    _this: *mut AnyObject,
    _cmd: Sel,
    web_view: *mut AnyObject,
    handler: *mut Block<dyn Fn(Bool)>,
) {
    // Only the window the user is in, and only a Mini App page, may take the pointer.
    let allow = !web_view.is_null() && unsafe { in_key_window(web_view) } && is_miniapp_url(&unsafe { current_url(web_view) });
    // SAFETY: WebKit passes a live completion block and expects exactly one call.
    if let Some(handler) = unsafe { handler.as_ref() } {
        handler.call((Bool::new(allow),));
    }
}

/// # Safety
/// `web_view` is a live WKWebView.
unsafe fn in_key_window(web_view: *mut AnyObject) -> bool {
    let window: *mut AnyObject = unsafe { msg_send![web_view, window] };
    if window.is_null() {
        return false;
    }
    let key: Bool = unsafe { msg_send![window, isKeyWindow] };
    key.as_bool()
}

/// # Safety
/// `web_view` is a live WKWebView; NSURL and NSString results are checked for nil.
unsafe fn current_url(web_view: *mut AnyObject) -> String {
    let url: *mut AnyObject = unsafe { msg_send![web_view, URL] };
    if url.is_null() {
        return String::new();
    }
    let text: *mut AnyObject = unsafe { msg_send![url, absoluteString] };
    if text.is_null() {
        return String::new();
    }
    let utf8: *const c_char = unsafe { msg_send![text, UTF8String] };
    if utf8.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
}

/// Mini App pages only: the custom scheme, or a live loopback host.
fn is_miniapp_url(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    match url.scheme() {
        "webxdc" => true,
        "http" => url.host_str() == Some("localhost") && url.port().is_some_and(vector_media::miniapps::isolated::is_live_port),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_miniapp_pages_get_the_lock() {
        assert!(is_miniapp_url("webxdc://abc123.host/index.html"));
        assert!(!is_miniapp_url("https://example.com/"));
        assert!(!is_miniapp_url("http://localhost:1/index.html"), "a port serving no Mini App");
        assert!(!is_miniapp_url("not a url"));
    }
}
