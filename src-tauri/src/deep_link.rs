//! Deep links from the OS: taken, held until the frontend has an account, and handed over.
//!
//! The grammar lives in `vector_core::deep_link`, which Vector Web parses with too.

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};

pub use vector_core::deep_link::DeepLinkAction;

/// Global storage for pending deep link action (received before frontend is ready)
static PENDING_DEEP_LINK: Mutex<Option<DeepLinkAction>> = Mutex::new(None);

/// The URLs last handed to `handle_deep_link`, from any source.
static LAST_URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What a link inside the app opens, so it goes there in place rather than through the browser.
#[tauri::command]
pub fn parse_deep_link(url: String) -> Option<DeepLinkAction> {
    vector_core::deep_link::parse_deep_link(&url)
}

/// Handle incoming deep link URLs
///
/// This function parses the URLs, stores them for later retrieval, and emits events to the frontend.
/// It should be called when the app receives deep link URLs from the OS.
///
/// # Arguments
/// * `handle` - The Tauri app handle
/// * `urls` - A vector of URL strings to process
pub fn handle_deep_link<R: Runtime>(handle: &AppHandle<R>, urls: Vec<String>) {
    if let Ok(mut last) = LAST_URLS.lock() {
        last.clone_from(&urls);
    }
    for url in urls {
        // The fragment carries invite secrets and go-link ids: never into a log.
        let locator = url.split('#').next().unwrap_or("");
        println!("[DeepLink] Received URL: {}", locator);

        if let Some(action) = vector_core::deep_link::parse_deep_link(&url) {
            println!("[DeepLink] Parsed action: {}", action.action_type);
            
            // Store the action for later retrieval (in case frontend isn't ready yet)
            if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
                *pending = Some(action.clone());
                println!("[DeepLink] Stored pending action for later retrieval");
            }
            
            // Also emit event to frontend (in case it's already listening)
            if let Err(e) = handle.emit("deep_link_action", &action) {
                println!("[DeepLink] Failed to emit event: {:?}", e);
            }
        } else {
            println!("[DeepLink] Failed to parse URL: {}", locator);
        }
    }
}

/// Handle the plugin's `get_current()` URLs on a page load.
///
/// The plugin keeps the last URL for the life of the process, so every reload (an account swap)
/// would replay it; only a URL not already handled is new.
pub fn handle_current_deep_link<R: Runtime>(handle: &AppHandle<R>, urls: Vec<String>) {
    if LAST_URLS.lock().is_ok_and(|last| *last == urls) {
        return;
    }
    handle_deep_link(handle, urls);
}

/// Store a pending notification tap action (called from Android JNI when user taps a notification)
#[allow(dead_code)]
pub fn set_pending_notification_action(chat_id: &str) {
    if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
        *pending = Some(DeepLinkAction {
            action_type: "chat".to_string(),
            target: chat_id.to_string(),
            ..Default::default()
        });
        println!("[DeepLink] Stored pending notification action for chat: {}", &chat_id[..chat_id.len().min(20)]);
    }
}

/// Get and clear any pending deep link action
///
/// This should be called by the frontend after login to check if there's a pending
/// deep link action that was received before the frontend was ready.
///
/// # Returns
/// * `Some(DeepLinkAction)` if there was a pending action (clears it)
/// * `None` if there was no pending action
#[tauri::command]
pub fn get_pending_deep_link() -> Option<DeepLinkAction> {
    // Called once the frontend has an account, which is also when a reply typed into a
    // notification before login can go out.
    #[cfg(not(target_os = "android"))]
    crate::services::native_notify::flush_pending_reply();

    let action = PENDING_DEEP_LINK.lock().ok()?.take()?;
    println!("[DeepLink] Retrieved and cleared pending action");
    if let Some(account) = &action.account {
        use nostr_sdk::prelude::ToBech32;
        let live = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok());
        if live.as_deref() != Some(account.as_str()) {
            return None;
        }
    }
    Some(action)
}

/// Open a chat from a desktop notification, at `message` when given. Held as the pending
/// action so a notification that launched Vector still lands once its account logs in.
#[cfg(not(target_os = "android"))]
pub fn open_chat_from_notification<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    message: Option<&str>,
    account: Option<&str>,
) {
    let action = DeepLinkAction {
        action_type: "chat".to_string(),
        target: chat_id.to_string(),
        message: message.map(str::to_string),
        account: account.filter(|a| !a.is_empty()).map(str::to_string),
    };
    if let Ok(mut pending) = PENDING_DEEP_LINK.lock() {
        *pending = Some(action.clone());
    }
    let _ = handle.emit("deep_link_action", &action);
}
