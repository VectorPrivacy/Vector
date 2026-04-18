mod globals;

pub use globals::{
    TAURI_APP, TauriEventEmitter,
    NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE,
    active_trusted_relays,
    MNEMONIC_SEED, ENCRYPTION_KEY, PENDING_NSEC,
    PENDING_INVITE, NOTIFIED_WELCOMES, WRAPPER_ID_CACHE,
    get_blossom_servers,
    PendingInviteAcceptance,
    PENDING_EVENTS,
    is_processing_allowed, close_processing_gate, open_processing_gate,
    set_encryption_enabled, init_encryption_enabled,
};

pub use vector_core::ChatState;
