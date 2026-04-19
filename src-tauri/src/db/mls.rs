//! MLS database operations — async wrappers around vector-core's sync functions.

use vector_core::mls::types::MlsGroupFull;

pub async fn load_mls_groups() -> Result<Vec<MlsGroupFull>, String> {
    vector_core::db::mls::load_mls_groups()
}

pub fn update_mls_group_avatar(group_id: &str, avatar_cached: &str, avatar_ref: Option<&str>) -> Result<(), String> {
    vector_core::db::mls::update_mls_group_avatar(group_id, avatar_cached, avatar_ref)
}

pub fn clear_all_mls_group_avatar_cache() -> Result<u64, String> {
    vector_core::db::mls::clear_all_mls_group_avatar_cache()
}

pub fn get_mls_engine_group_id(group_id: &str) -> Result<Option<String>, String> {
    vector_core::db::mls::get_mls_engine_group_id(group_id)
}

pub async fn load_mls_keypackages() -> Result<Vec<serde_json::Value>, String> {
    vector_core::db::mls::load_mls_keypackages()
}

pub fn load_mls_negentropy_items(since: Option<u64>) -> Result<Vec<(nostr_sdk::EventId, nostr_sdk::Timestamp)>, String> {
    vector_core::db::mls::load_mls_negentropy_items(since)
}

pub async fn load_mls_device_id() -> Result<Option<String>, String> {
    vector_core::db::mls::load_mls_device_id()
}
