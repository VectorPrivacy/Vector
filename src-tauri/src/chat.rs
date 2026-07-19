use vector_core::compact::encode_message_id;

pub use vector_core::{ChatType, SerializableChat};
// ============================================================================

/// Headless mark-as-read: updates STATE + persists to DB without requiring TAURI_APP.
/// Used by both the Tauri command and the Android notification action (JNI).
/// If the Tauri app handle is available (backgrounded app mode), emits a
/// `chat_mark_read` event so the frontend can update its unread state.
#[allow(dead_code)]
pub async fn mark_as_read_headless(chat_id: &str) -> bool {
    // A swap can land while awaiting the STATE lock; re-check inside so an answered-elsewhere read
    // never writes account A's last_read into account B's freshly-swapped DB.
    let session = vector_core::state::SessionGuard::capture();
    let (slim, last_read_hex) = {
        let mut state = crate::STATE.lock().await;
        if !session.is_valid() { return false; }
        let idx = match state.chats.iter().position(|c| c.id == chat_id) {
            Some(i) => i,
            None => return false,
        };
        if !state.chats[idx].set_as_read() {
            return false;
        }
        state.unread_clear(chat_id);
        let lr = vector_core::compact::decode_message_id(&state.chats[idx].last_read);
        (crate::db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner), lr)
    };
    let _ = crate::db::chats::save_slim_chat(slim).await;

    // Read clears any lingering OS notification for this chat (in-app open / another device).
    crate::services::notification_service::cancel_chat_notification(chat_id);

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
        // Persist the new last_read to the DB FIRST — update_unread_counter now counts unread from
        // the DB (not in-memory state), so the badge would otherwise be computed against the stale,
        // pre-read last_read and never clear.
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

        // Reconcile from the DB: a message-scoped mark ("read up to here") can leave a non-zero
        // remainder, so a blind clear would be wrong. The full mark-all-read case reconciles to 0.
        crate::commands::messaging::reconcile_chat_unread(&chat_id).await;

        // Read clears any lingering OS notification for this chat (in-app open / another device).
        crate::services::notification_service::cancel_chat_notification(&chat_id);

        crate::commands::messaging::update_unread_counter(handle.clone()).await;
    }

    result
}

/// Retreat a chat's read marker so its newest contact message re-surfaces as unread. The anchor is
/// computed from the full DB history — a community row may hold only a preview message in RAM, so
/// the frontend can't pick it locally. Returns the new read marker as hex (empty string = never-read)
/// so the frontend can sync its cached `last_read`, or `None` when nothing was marked (we spoke last
/// / nothing to surface).
#[tauri::command]
pub async fn mark_as_unread(chat_id: String) -> Option<String> {
    use vector_core::db::events::{compute_unread_anchor, UnreadMark};
    // The DB read + STATE write straddle an await; re-check the session so a mid-flight account swap
    // never writes account A's last_read into account B's chat.
    let session = vector_core::state::SessionGuard::capture();

    let (last_read, last_read_hex) = match compute_unread_anchor(&chat_id).await {
        Ok(UnreadMark::Anchor(id)) => (encode_message_id(&id), id),
        Ok(UnreadMark::Clear) => ([0u8; 32], String::new()),
        Ok(UnreadMark::NoOp) | Err(_) => return None,
    };

    let slim = {
        let mut state = crate::STATE.lock().await;
        if !session.is_valid() { return None; }
        let idx = match state.chats.iter().position(|c| c.id == chat_id) {
            Some(i) => i,
            None => return None,
        };
        state.chats[idx].last_read = last_read;
        crate::db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner)
    };
    let _ = crate::db::chats::save_slim_chat(slim).await;

    // The retreat surfaces the newest contact message again, so reconcile the exact remainder.
    crate::commands::messaging::reconcile_chat_unread(&chat_id).await;

    if let Some(handle) = crate::TAURI_APP.get() {
        let _ = crate::commands::messaging::update_unread_counter(handle.clone()).await;
    }
    Some(last_read_hex)
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
