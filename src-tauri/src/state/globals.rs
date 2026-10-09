use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

pub static TAURI_APP: OnceLock<AppHandle> = OnceLock::new();

/// Bridges vector-core's EventEmitter to Tauri's AppHandle.emit().
pub struct TauriEventEmitter;

/// Every way a message leaves a chat (deleted, unsent, hidden, self-destructed) ends in
/// `message_removed`, so this is where its OS notification goes too.
const MESSAGE_REMOVED: &str = "message_removed";

fn retract_removed_message(payload: &serde_json::Value) {
    #[cfg(not(target_os = "android"))]
    if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
        crate::services::native_notify::remove_message(id);
    }
    #[cfg(target_os = "android")]
    let _ = payload;
}

impl vector_core::EventEmitter for TauriEventEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if event == MESSAGE_REMOVED {
            retract_removed_message(&payload);
        }
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = handle.emit(event, payload) {
                log_warn!("[EventEmitter] Failed to emit '{}': {}", event, e);
            }
        }
    }

    fn emit_json(&self, event: &str, payload: &serde_json::value::RawValue) {
        if event == MESSAGE_REMOVED {
            if let Ok(value) = serde_json::from_str(payload.get()) {
                retract_removed_message(&value);
            }
        }
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = handle.emit(event, payload) {
                log_warn!("[EventEmitter] Failed to emit '{}': {}", event, e);
            }
        }
    }

    fn prefers_json(&self) -> bool {
        true
    }
}

pub use vector_core::state::{
    MY_SECRET_KEY, STATE,
    nostr_client, my_public_key,
    set_my_public_key,
    active_trusted_relays,
    get_blossom_servers,
    MNEMONIC_SEED, PENDING_NSEC,
    ENCRYPTION_KEY,
    set_encryption_enabled, init_encryption_enabled,
    PendingInviteAcceptance,
    pending_invite, set_pending_invite, clear_pending_invite,
    WRAPPER_ID_CACHE,
    PENDING_EVENTS,
    is_processing_allowed, close_processing_gate, open_processing_gate,
};
