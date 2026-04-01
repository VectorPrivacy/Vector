// Submodules
mod maintenance;
pub mod settings;
// profiles: delegates to vector_core::db::profiles (no local file needed)
mod mls;
mod miniapps;
pub mod chats;
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
// ID cache — delegates to vector-core
pub use vector_core::db::id_cache::{
    get_chat_id_by_identifier, get_or_create_chat_id,
    clear_id_caches,
};
pub async fn preload_id_caches() -> Result<(), String> {
    vector_core::db::id_cache::preload_id_caches()
}
// Chat database functions
pub use chats::{get_all_chats, delete_chat};
// Message database functions (delegates to vector-core)
pub use vector_core::db::events::{save_message, save_chat_messages};
// Event queries — async wrappers around sync vector-core functions
pub async fn message_exists_in_db(id: &str) -> Result<bool, String> {
    vector_core::db::events::message_exists_in_db(id)
}
pub async fn wrapper_event_exists(id: &str) -> Result<bool, String> {
    vector_core::db::events::wrapper_event_exists(id)
}
pub async fn update_wrapper_event_id(event_id: &str, wrapper_id: &str) -> Result<bool, String> {
    vector_core::db::events::update_wrapper_event_id(event_id, wrapper_id)
}
pub async fn get_chat_message_count(chat_id: &str) -> Result<usize, String> {
    let chat_int_id = vector_core::db::id_cache::get_chat_id_by_identifier(chat_id)?;
    vector_core::db::events::get_chat_message_count(chat_int_id)
}
// Wrapper tracking — sync functions re-exported directly
pub use vector_core::db::wrappers::{
    save_processed_wrapper, load_processed_wrappers, load_negentropy_items,
    update_wrapper_timestamp,
};
pub async fn load_recent_wrapper_ids(days: u64) -> Result<Vec<[u8; 32]>, String> {
    vector_core::db::wrappers::load_recent_wrapper_ids(days)
}
// Attachment database functions (remain in src-tauri)
pub use attachments::{
    get_chat_messages_paginated,
    get_messages_around_id,
    update_attachment_downloaded_status, backfill_attachment_downloaded_status, check_downloaded_attachments_integrity,
};
// Event database functions
pub use events::{
    save_event, save_pivx_payment_event, save_system_event_by_id,
    save_reaction_event, save_edit_event, event_exists, delete_event,
    populate_reply_context, get_message_views, get_all_chats_last_messages,
};
// Async wrappers for sync vector-core read functions
pub async fn get_pivx_payments_for_chat(id: &str) -> Result<Vec<vector_core::StoredEvent>, String> {
    vector_core::db::events::get_pivx_payments_for_chat(id)
}
pub async fn get_system_events_for_chat(id: &str) -> Result<Vec<vector_core::StoredEvent>, String> {
    vector_core::db::events::get_system_events_for_chat(id)
}


// SystemEventType moved to vector-core::stored_event
pub use vector_core::SystemEventType;