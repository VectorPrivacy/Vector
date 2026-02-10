//! Global state management for the Vector application.
//!
//! This module contains:
//! - `globals`: Global static variables (TAURI_APP, NOSTR_CLIENT, STATE, etc.)
//! - `chat_state`: The ChatState struct and its methods
//! - `sync`: SyncMode enum for sync state management
//! - `stats`: Cache statistics and memory benchmarking

mod globals;
mod chat_state;
mod sync;
#[cfg(debug_assertions)]
pub mod stats;

pub use globals::{
    TAURI_APP, NOSTR_CLIENT, MY_KEYS, MY_PUBLIC_KEY, STATE,
    TRUSTED_RELAYS,
    MNEMONIC_SEED, ENCRYPTION_KEY, PENDING_NSEC,
    PENDING_INVITE, NOTIFIED_WELCOMES, WRAPPER_ID_CACHE,
    get_blossom_servers,
    PendingInviteAcceptance,
    // Processing gate for encryption migration
    PENDING_EVENTS,
    is_processing_allowed, close_processing_gate, open_processing_gate,
    // Cached encryption-enabled flag
    is_encryption_enabled_fast, set_encryption_enabled, init_encryption_enabled,
};

pub use chat_state::ChatState;
pub use sync::SyncMode;
