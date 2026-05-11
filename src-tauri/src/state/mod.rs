mod globals;

pub use globals::{
    TAURI_APP, TauriEventEmitter, TauriSubscriptionRefresher,
    NOSTR_CLIENT, MY_SECRET_KEY, STATE,
    nostr_client, my_public_key,
    set_my_public_key,
    active_trusted_relays,
    MNEMONIC_SEED, ENCRYPTION_KEY, PENDING_NSEC,
    NOTIFIED_WELCOMES, WRAPPER_ID_CACHE,
    get_blossom_servers,
    PendingInviteAcceptance,
    pending_invite, set_pending_invite, clear_pending_invite,
    PENDING_EVENTS,
    is_processing_allowed, close_processing_gate, open_processing_gate,
    set_encryption_enabled, init_encryption_enabled,
};

pub use vector_core::ChatState;
