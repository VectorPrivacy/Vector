use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;

// Submodules
mod maintenance;
pub mod settings;
mod profiles;
mod mls;
mod miniapps;
pub mod chats;
mod messages;
mod attachments;
mod events;

// Re-exports
pub use maintenance::check_and_vacuum_if_needed;
// Settings functions used internally (not just as Tauri commands)
pub use settings::{get_sql_setting, set_sql_setting, get_seed, set_seed, get_pkey, set_pkey, remove_setting};
// Profile types and functions
pub use profiles::{SlimProfile, get_all_profiles, set_profile};
// MLS database functions
pub use mls::{
    save_mls_groups, save_mls_group, load_mls_groups, update_mls_group_avatar,
    save_mls_keypackages, load_mls_keypackages,
    save_mls_event_cursors, load_mls_event_cursors,
    save_mls_device_id, load_mls_device_id,
};
// Mini Apps database functions
pub use miniapps::{
    MiniAppHistoryEntry,
    record_miniapp_opened, record_miniapp_opened_with_metadata,
    get_miniapps_history, toggle_miniapp_favorite, set_miniapp_favorite,
    remove_miniapp_from_history, update_miniapp_version, get_miniapp_installed_version,
    get_miniapp_granted_permissions, set_miniapp_permission, set_miniapp_permissions,
    has_miniapp_permission_prompt, revoke_all_miniapp_permissions, copy_miniapp_permissions,
};
// Chat database functions
pub use chats::{
    get_chat_id_by_identifier,
    preload_id_caches, clear_id_caches,
    get_all_chats, delete_chat,
};
// Internal chat helpers used by messages/events
pub(crate) use chats::{get_or_create_chat_id, get_or_create_user_id};
// Message database functions
pub use messages::{save_message, save_chat_messages};
// Attachment database functions
pub use attachments::{
    lookup_attachment_cached, warm_file_hash_cache,
    get_chat_messages_paginated, get_chat_message_count,
    get_messages_around_id, message_exists_in_db, wrapper_event_exists,
    update_wrapper_event_id, load_recent_wrapper_ids, update_attachment_downloaded_status,
    check_downloaded_attachments_integrity,
};
// Event database functions
pub use events::{
    save_event, save_pivx_payment_event, save_system_event_by_id,
    get_pivx_payments_for_chat, get_system_events_for_chat,
    save_reaction_event, save_edit_event, event_exists,
    populate_reply_context, get_message_views, get_all_chats_last_messages,
};

/// In-memory cache for chat_identifier → integer ID mappings
/// This avoids database lookups on every message operation
static CHAT_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// In-memory cache for npub → integer ID mappings
/// This avoids database lookups on every message operation
static USER_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// System event types for MLS groups (stored as integers for efficiency)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SystemEventType {
    MemberLeft = 0,
    MemberJoined = 1,
    MemberRemoved = 2,
}

impl SystemEventType {
    /// Get the display message for this event type
    pub fn display_message(&self, display_name: &str) -> String {
        match self {
            SystemEventType::MemberLeft => format!("{} has left", display_name),
            SystemEventType::MemberJoined => format!("{} has joined", display_name),
            SystemEventType::MemberRemoved => format!("{} was removed", display_name),
        }
    }

    /// Convert to integer for storage/serialization
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}