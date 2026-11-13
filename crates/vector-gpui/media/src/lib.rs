//! The desktop client's audio engine, device preferences, microphone access and call media,
//! compiled from `src-tauri` as they are, over a slice of Tauri's API (`tauri-shim`).

#[macro_use]
extern crate vector_core;

/// The desktop's `log_error!` also toasts; here it is a log line.
macro_rules! log_error {
    ($($t:tt)*) => { vector_core::log_warn!($($t)*) };
}

/// The desktop's global accessors, by the paths its modules name them.
pub use vector_core::{STATE, my_public_key};
pub mod db {
    pub use vector_core::db::Session;
    pub use vector_core::db::events::save_pivx_payment_event;
    pub use vector_core::db::miniapps::*;
    pub use vector_core::db::settings::{get_sql_setting, set_sql_setting};
    pub async fn get_pivx_payments_for_chat(id: &str) -> Result<Vec<vector_core::StoredEvent>, String> {
        vector_core::db::events::get_pivx_payments_for_chat(id)
    }
}
pub mod account_manager {
    pub use vector_core::db::get_db_connection_guard_static;
}
pub mod util {
    pub use vector_core::crypto::mime_from_magic_bytes;
    pub use vector_core::simd::hex::{bytes_to_hex_16, bytes_to_hex_string, hex_string_to_bytes};
}
pub use vector_core::{blossom, crypto, inbox_relays, nostr_client, stored_event};
#[path = "../../../../src-tauri/src/pivx.rs"]
#[allow(dead_code)]
pub mod pivx;
#[path = "../../../../src-tauri/src/commands/players.rs"]
pub mod players;
#[path = "../../../../src-tauri/src/net.rs"]
#[allow(dead_code)]
pub mod net;
pub mod image_cache;
/// What `listing_icon` asks of the desktop's image module, from the image crate.
pub mod shared {
    pub mod image {
        use image::DynamicImage;

        pub fn decode_image(bytes: &[u8], _min_long_side: u32) -> Result<DynamicImage, String> {
            image::load_from_memory(bytes).map_err(|e| e.to_string())
        }
        pub fn resize_fit(img: &DynamicImage, w: u32, h: u32, filter: image::imageops::FilterType) -> DynamicImage {
            img.resize(w, h, filter)
        }
        pub fn encode_png(pixels: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
            let mut out = Vec::new();
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(pixels, w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| e.to_string())?;
            Ok(out)
        }
        use image::ImageEncoder;
    }
}

#[allow(dead_code)]
pub mod audio;
#[path = "../../../../src-tauri/src/audio_codecs.rs"]
pub mod audio_codecs;
#[path = "../../../../src-tauri/src/audio_devices.rs"]
#[allow(dead_code, unused_imports)]
pub mod audio_devices;
#[path = "../../../../src-tauri/src/audio_engine.rs"]
#[allow(dead_code, clippy::all)]
pub mod audio_engine;
#[path = "../../../../src-tauri/src/awake.rs"]
#[allow(dead_code)]
pub mod awake;
#[allow(dead_code, unused_imports, clippy::all)]
pub mod calls;
#[path = "../../../../src-tauri/src/chapters.rs"]
#[allow(dead_code)]
pub mod chapters;
#[path = "../../../../src-tauri/src/lyrics.rs"]
#[allow(dead_code)]
pub mod lyrics;
pub mod tags;
#[path = "../../../../src-tauri/src/mic_access.rs"]
#[allow(dead_code)]
pub mod mic_access;
pub mod miniapps;
#[cfg(feature = "whisper")]
#[path = "../../../../src-tauri/src/whisper.rs"]
#[allow(dead_code)]
pub mod whisper;
#[path = "../../../../src-tauri/src/voice.rs"]
#[allow(dead_code, unexpected_cfgs)]
pub mod voice;

#[path = "../../../../src-tauri/src/simd/audio.rs"]
#[allow(dead_code)]
#[doc(hidden)]
pub mod simd_audio;

#[path = "../../../../src-tauri/src/simd/html_meta.rs"]
#[allow(dead_code)]
#[doc(hidden)]
pub mod simd_html_meta;

pub mod simd {
    pub use super::simd_audio as audio;
    pub use super::simd_html_meta as html_meta;
}

/// Where the engine's events go: set once by the host before any sound plays.
pub static TAURI_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

extern crate tauri as tauri_plugin_notification;

/// What the host does for a notification's buttons, by the paths the desktop's notifier names.
pub mod notify_host {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::OnceLock;

    pub type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

    pub struct Host {
        pub mark_read: fn(String) -> Fut<bool>,
        /// A mute of `ms`, in `set_notify_mute`'s units (negative is indefinite).
        pub mute: fn(String, i64) -> Fut<Result<(), String>>,
        pub reply: fn(String, String) -> Fut<Result<String, String>>,
        /// The chat, at the message if there is one, for the account it was shown for.
        pub open: fn(String, Option<String>, Option<String>),
        pub sound: fn(),
    }

    static HOST: OnceLock<Host> = OnceLock::new();

    pub fn set(host: Host) {
        let _ = HOST.set(host);
    }

    pub(crate) fn get() -> Option<&'static Host> {
        HOST.get()
    }
}

pub mod chat {
    pub async fn mark_as_read_headless(chat_id: &str) -> bool {
        match crate::notify_host::get() {
            Some(h) => (h.mark_read)(chat_id.to_string()).await,
            None => false,
        }
    }
}

pub mod commands {
    pub mod messaging {
        /// The host recounts on the read itself.
        pub async fn update_unread_counter(_: tauri::AppHandle) -> Result<(), String> {
            Ok(())
        }
    }
    pub mod notify {
        pub async fn set_notify_mute(scope_id: String, duration_ms: i64) -> Result<(), String> {
            match crate::notify_host::get() {
                Some(h) => (h.mute)(scope_id, duration_ms).await,
                None => Err("not ready".into()),
            }
        }
    }
}

pub mod message {
    pub async fn send_text_reply_headless(chat_id: &str, content: &str) -> Result<String, String> {
        match crate::notify_host::get() {
            Some(h) => (h.reply)(chat_id.to_string(), content.to_string()).await,
            None => Err("not ready".into()),
        }
    }
}

pub mod deep_link {
    pub fn open_chat_from_notification<R: tauri::Runtime>(
        _: &tauri::AppHandle<R>,
        chat_id: &str,
        message: Option<&str>,
        account: Option<&str>,
    ) {
        if let Some(h) = crate::notify_host::get() {
            (h.open)(chat_id.to_string(), message.map(str::to_string), account.map(str::to_string));
        }
    }
}

#[path = "../../../../src-tauri/src/services/notification_service.rs"]
#[allow(dead_code)]
#[doc(hidden)]
pub mod notification_service;
#[path = "../../../../src-tauri/src/services/native_notify/mod.rs"]
#[allow(dead_code)]
#[doc(hidden)]
pub mod native_notify;

/// The desktop's OS notifications: its notification service and one backend per platform.
pub mod services {
    pub use super::native_notify;
    pub use super::notification_service;
    pub use super::notification_service::{show_notification_generic, strip_content_for_preview, NotificationData};
}
