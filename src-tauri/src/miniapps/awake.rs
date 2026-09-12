//! Keeps the process out of App Nap while a mini app window is open. Napping halves
//! a covered game's frame rate and stretches its timers even when WebKit itself is
//! not throttling the page.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    use std::sync::Mutex;

    struct Token(Retained<AnyObject>);
    // The token only ever goes back to NSProcessInfo, which accepts it from any thread.
    unsafe impl Send for Token {}

    static TOKEN: Mutex<Option<Token>> = Mutex::new(None);
    /// NSActivityUserInitiatedAllowingIdleSystemSleep: no App Nap, the machine may still sleep.
    const OPTIONS: usize = 0x00EF_FFFF;

    pub fn hold() {
        let mut slot = TOKEN.lock().unwrap();
        if slot.is_some() {
            return;
        }
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            let reason = NSString::from_str("Mini App window open");
            let token: *mut AnyObject = msg_send![info, beginActivityWithOptions: OPTIONS, reason: &*reason];
            if let Some(token) = Retained::retain(token) {
                *slot = Some(Token(token));
            }
        }
    }

    pub fn release() {
        let token = TOKEN.lock().unwrap().take();
        if let Some(Token(token)) = token {
            unsafe {
                let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                let _: () = msg_send![info, endActivity: &*token];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn hold() {}
    pub fn release() {}
}

pub use imp::{hold, release};
