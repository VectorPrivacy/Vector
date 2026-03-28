use vector_core::compact::encode_message_id;

pub use vector_core::{ChatType, SerializableChat};
// ============================================================================

/// Headless mark-as-read: updates STATE + persists to DB without requiring TAURI_APP.
/// Used by both the Tauri command and the Android notification action (JNI).
/// If the Tauri app handle is available (backgrounded app mode), emits a
/// `chat_mark_read` event so the frontend can update its unread state.
#[allow(dead_code)]
pub async fn mark_as_read_headless(chat_id: &str) -> bool {
    let (slim, last_read_hex) = {
        let mut state = crate::STATE.lock().await;
        let idx = match state.chats.iter().position(|c| c.id == chat_id) {
            Some(i) => i,
            None => return false,
        };
        if !state.chats[idx].set_as_read() {
            return false;
        }
        let lr = vector_core::compact::decode_message_id(&state.chats[idx].last_read);
        (crate::db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner), lr)
    };
    let _ = crate::db::chats::save_slim_chat(slim).await;

    // Notify the frontend if TAURI_APP is available (backgrounded app mode)
    if let Some(handle) = crate::TAURI_APP.get() {
        use tauri::Emitter;
        let _ = handle.emit("chat_mark_read", serde_json::json!({
            "chat_id": chat_id,
            "last_read": last_read_hex,
        }));
    }

    true
}

/// Marks a specific message as read for a chat.
#[tauri::command]
pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    let handle = crate::TAURI_APP.get().unwrap();

    let (result, chat_id_for_save) = {
        let mut state = crate::STATE.lock().await;
        let mut result = false;
        let mut chat_id_for_save: Option<String> = None;

        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            if let Some(msg_id) = &message_id {
                chat.last_read = encode_message_id(msg_id);
                result = true;
                chat_id_for_save = Some(chat.id.clone());
            } else {
                result = chat.set_as_read();
                if result {
                    chat_id_for_save = Some(chat.id.clone());
                }
            }
        }

        (result, chat_id_for_save)
    };

    if result {
        crate::commands::messaging::update_unread_counter(handle.clone()).await;

        if let Some(chat_id) = chat_id_for_save {
            let slim = {
                let state = crate::STATE.lock().await;
                state.get_chat(&chat_id).map(|chat| {
                    crate::db::chats::SlimChatDB::from_chat(chat, &state.interner)
                })
            };
            if let Some(slim) = slim {
                let _ = crate::db::chats::save_slim_chat(slim).await;
            }
        }
    }

    result
}

/// Toggles the muted status of a chat (DM or group).
#[tauri::command]
pub async fn toggle_chat_mute(chat_id: String) -> bool {
    let handle = crate::TAURI_APP.get().unwrap();

    let (muted, slim) = {
        let mut state = crate::STATE.lock().await;
        let idx = match state.chats.iter().position(|c| c.id == chat_id) {
            Some(i) => i,
            None => return false,
        };
        state.chats[idx].muted = !state.chats[idx].muted;
        let m = state.chats[idx].muted;
        (m, crate::db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner))
    };

    let _ = crate::db::chats::save_slim_chat(slim).await;

    use tauri::Emitter;
    handle.emit("chat_muted", serde_json::json!({
        "chat_id": &chat_id,
        "value": muted
    })).ok();

    let _ = crate::commands::messaging::update_unread_counter(handle.clone()).await;
    muted
}
