//! Chat database operations — delegates to vector-core.

// Re-export types and sync functions
pub use vector_core::db::chats::SlimChatDB;

// Async wrappers for backward compatibility (callers use .await)
pub async fn get_all_chats() -> Result<Vec<SlimChatDB>, String> {
    vector_core::db::chats::get_all_chats()
}

pub async fn save_slim_chat(slim_chat: SlimChatDB) -> Result<(), String> {
    vector_core::db::chats::save_slim_chat(&slim_chat)
}

#[allow(dead_code)]
pub async fn delete_chat(chat_id: &str) -> Result<(), String> {
    vector_core::db::chats::delete_chat(chat_id)
}
