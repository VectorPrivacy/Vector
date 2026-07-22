//! Inbound share handler.
//!
//! When another app shares files or text *into* Vector via the Android share
//! sheet (ACTION_SEND / ACTION_SEND_MULTIPLE), MainActivity forwards the
//! payload here. The frontend then lets the user pick a chat and sends it.
//!
//! Mirrors the deep-link pattern: store as pending (the frontend may not be
//! ready on a cold start) AND emit live (if it already is).

use serde::Serialize;
use std::sync::Mutex;

/// Pending inbound share, received before the frontend was ready.
static PENDING_SHARE: Mutex<Option<SharePayload>> = Mutex::new(None);

/// A share received from another app.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SharePayload {
    /// `content://` URIs of shared files (may be empty for a text-only share).
    pub uris: Vec<String>,
    /// Shared plain text (empty when only files were shared).
    pub text: String,
}

/// Store an inbound share and emit it to the frontend if it's running.
/// Fed only by the Android share-intent path; dead on desktop builds.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_pending_share(uris: Vec<String>, text: String) {
    if uris.is_empty() && text.is_empty() {
        return;
    }
    let payload = SharePayload { uris, text };

    if let Ok(mut pending) = PENDING_SHARE.lock() {
        *pending = Some(payload.clone());
    }

    if let Some(handle) = crate::TAURI_APP.get() {
        use tauri::Emitter;
        let _ = handle.emit("share_received", &payload);
    }
}

/// Get and clear any pending inbound share. The frontend polls this on init to
/// catch a cold-start share that arrived before its listener was attached.
#[tauri::command]
pub fn get_pending_share() -> Option<SharePayload> {
    PENDING_SHARE.lock().ok().and_then(|mut p| p.take())
}
