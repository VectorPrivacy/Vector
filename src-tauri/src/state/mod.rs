//! Global state management for the Vector application.
//!
//! This module contains:
//! - `globals`: Global static variables (TAURI_APP, NOSTR_CLIENT, STATE, etc.)
//! - `chat_state`: The ChatState struct and its methods
//! - `sync`: SyncMode enum for sync state management

mod globals;
mod chat_state;
mod sync;

pub use globals::{
    TAURI_APP, NOSTR_CLIENT, STATE,
    TRUSTED_RELAYS,
    MNEMONIC_SEED, ENCRYPTION_KEY,
    PENDING_INVITE, NOTIFIED_WELCOMES, WRAPPER_ID_CACHE,
    get_blossom_servers,
    PendingInviteAcceptance,
};

pub use chat_state::ChatState;
pub use sync::SyncMode;
