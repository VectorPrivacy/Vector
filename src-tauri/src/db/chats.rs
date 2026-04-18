//! Chat database operations — delegates to vector-core.

pub use vector_core::db::chats::SlimChatDB;

pub async fn get_all_chats() -> Result<Vec<SlimChatDB>, String> {
    vector_core::db::chats::get_all_chats()
}

pub async fn save_slim_chat(slim_chat: SlimChatDB) -> Result<(), String> {
    vector_core::db::chats::save_slim_chat(&slim_chat)
}
