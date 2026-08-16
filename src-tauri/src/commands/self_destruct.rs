//! Self-Destruct Timer commands — per-chat NIP-40 message expiry.
//!
//! The per-chat lifespan is a DURATION in seconds (absent = permanent). The
//! send path resolves it to an absolute NIP-40 expiry at send time, so setting
//! the timer here makes every subsequent DM in that chat self-destruct.


/// Configured self-destruct duration (seconds) for a chat, or null for
/// permanent. Read by the picker + composer indicator.
#[tauri::command]
pub async fn get_self_destruct_timer(chat_id: String) -> Result<Option<u64>, String> {
    Ok(vector_core::self_destruct::chat_duration_secs(&chat_id))
}

/// Set (or clear, with null / 0) the self-destruct duration for a chat.
#[tauri::command]
pub async fn set_self_destruct_timer(chat_id: String, secs: Option<u64>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        // Per-account KV write — guard against a mid-call account swap.
        vector_core::self_destruct::set_chat_duration_secs(&chat_id, secs)
    })
    .await
}

// Handlers: get_self_destruct_timer, set_self_destruct_timer
