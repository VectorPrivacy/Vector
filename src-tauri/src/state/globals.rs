use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

pub static TAURI_APP: OnceLock<AppHandle> = OnceLock::new();

/// Bridges vector-core's EventEmitter to Tauri's AppHandle.emit().
pub struct TauriEventEmitter;

impl vector_core::EventEmitter for TauriEventEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = handle.emit(event, payload) {
                log_warn!("[EventEmitter] Failed to emit '{}': {}", event, e);
            }
        }
    }
}

pub use vector_core::state::{
    NOSTR_CLIENT, MY_SECRET_KEY, STATE,
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
