use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::sync::LazyLock;

// Submodules
mod maintenance;
pub mod settings;
// profiles: delegates to vector_core::db::profiles (no local file needed)
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
pub use vector_core::SlimProfile;
pub async fn get_all_profiles() -> Result<Vec<SlimProfile>, String> {
    vector_core::db::profiles::get_all_profiles()
}
pub async fn set_profile(profile: SlimProfile) -> Result<(), String> {
    vector_core::db::profiles::set_profile(&profile)
}
// MLS database functions
pub use mls::{
    save_mls_groups, save_mls_group, load_mls_groups, update_mls_group_avatar, clear_all_mls_group_avatar_cache,
    save_mls_keypackages, load_mls_keypackages,
    save_mls_event_cursors, load_mls_event_cursors,
    save_mls_device_id, load_mls_device_id,
    load_mls_negentropy_items,
    get_mls_engine_group_id,
};
// Mini Apps database functions
pub use miniapps::{
    MiniAppHistoryEntry,
    record_miniapp_opened, record_miniapp_opened_with_metadata,
    get_miniapps_history, toggle_miniapp_favorite, set_miniapp_favorite,
    remove_miniapp_from_history, update_miniapp_version, get_miniapp_installed_version,
    backfill_marketplace_ids,
    get_miniapp_granted_permissions, set_miniapp_permission, set_miniapp_permissions,
    has_miniapp_permission_prompt, revoke_all_miniapp_permissions, copy_miniapp_permissions,
    save_marketplace_cache, load_marketplace_cache,
    get_active_peer_advertisements,
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
    get_chat_messages_paginated, get_chat_message_count,
    get_messages_around_id, message_exists_in_db, wrapper_event_exists,
    update_wrapper_event_id, load_recent_wrapper_ids, save_processed_wrapper, load_processed_wrappers, load_negentropy_items, update_wrapper_timestamp,
    update_attachment_downloaded_status, backfill_attachment_downloaded_status, check_downloaded_attachments_integrity,
};
// Event database functions
pub use events::{
    save_event, save_pivx_payment_event, save_system_event_by_id,
    get_pivx_payments_for_chat, get_system_events_for_chat,
    save_reaction_event, save_edit_event, event_exists, delete_event,
    populate_reply_context, get_message_views, get_all_chats_last_messages,
};

/// In-memory cache for chat_identifier → integer ID mappings
/// This avoids database lookups on every message operation
static CHAT_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

/// In-memory cache for npub → integer ID mappings
/// This avoids database lookups on every message operation
static USER_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

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