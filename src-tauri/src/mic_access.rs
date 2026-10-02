//! Microphone access, asked for the moment a feature needs the microphone and never
//! before: the system's prompt belongs to the call or the voice message that wants it,
//! not to the login screen.

/// What a voice message or a mic test answers when access is refused; the UI turns it into
/// a prompt that leads to the system's privacy settings.
pub const DENIED: &str = "MIC_DENIED";

/// Whether the microphone may be used, asking the system first if it never has. A refusal
/// is remembered by the system, which won't ask again: the UI points to its settings.
pub async fn ask() -> bool {
    #[cfg(target_os = "macos")]
    {
        return tokio::task::spawn_blocking(mac::ask).await.unwrap_or(false);
    }
    #[cfg(target_os = "android")]
    {
        use crate::android::permissions::{check_audio_permission, request_audio_permission_blocking};
        if check_audio_permission().unwrap_or(false) {
            return true;
        }
        return request_audio_permission_blocking().unwrap_or(false);
    }
    #[allow(unreachable_code)]
    true
}

/// [`ask`], for a feature that can't go on without the microphone.
pub async fn require() -> Result<(), String> {
    if ask().await { Ok(()) } else { Err(DENIED.to_string()) }
}

/// Whether access is already granted, without ever prompting.
pub fn granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        return mac::status() == mac::Status::Granted;
    }
    #[cfg(target_os = "android")]
    {
        return crate::android::permissions::check_audio_permission().unwrap_or(false);
    }
    #[allow(unreachable_code)]
    true
}

/// Open the system's microphone privacy settings, where a refusal is undone.
pub fn open_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    #[cfg(target_os = "windows")]
    {
        return std::process::Command::new("explorer")
            .arg("ms-settings:privacy-microphone")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    #[allow(unreachable_code)]
    Err("Not available on this platform".to_string())
}

#[cfg(target_os = "macos")]
mod mac {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::NSString;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    #[derive(PartialEq, Clone, Copy)]
    pub enum Status {
        Undetermined,
        Granted,
        Denied,
    }

    /// AVMediaTypeAudio's value: built here so no framework symbol has to be linked.
    fn media_type() -> objc2::rc::Retained<NSString> {
        NSString::from_str("soun")
    }

    fn device_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"AVCaptureDevice")
    }

    pub fn status() -> Status {
        let Some(cls) = device_class() else { return Status::Granted };
        let media = media_type();
        let s: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
        match s {
            0 => Status::Undetermined,
            3 => Status::Granted,
            _ => Status::Denied,
        }
    }

    /// Blocks until the user answers the system prompt; only ever on a worker thread.
    pub fn ask() -> bool {
        match status() {
            Status::Granted => return true,
            Status::Denied => return false,
            Status::Undetermined => {}
        }
        let Some(cls) = device_class() else { return true };
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let handler = RcBlock::new(move |granted: Bool| {
            let _ = tx.send(granted.as_bool());
        });
        let media = media_type();
        let _: () = unsafe { msg_send![cls, requestAccessForMediaType: &*media, completionHandler: &*handler] };
        rx.recv().unwrap_or(false)
    }
}
