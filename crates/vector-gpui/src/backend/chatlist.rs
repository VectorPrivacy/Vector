//! The chat list's own state: pinned rows, the archive, and Mark as Unread.

use std::collections::HashMap;

use serde_json::json;
use vector_core::synced_prefs::{self, Pref};
use vector_core::{STATE, db, pinned_chats};

/// The pinned rows as this device holds them, for the first paint.
pub fn pinned() -> Vec<String> {
    pinned_chats::load_local().chats
}

/// Pin or unpin a row and sync it; the list as it now stands. Past the cap, the reason.
pub async fn set_pinned(id: String, pin: bool) -> Result<Vec<String>, String> {
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let list = if pin {
        pinned_chats::pin_chat(&client, &id).await?
    } else {
        pinned_chats::unpin_chat(&client, &id).await?
    };
    super::selfsync::emit_pinned(&list.chats);
    Ok(list.chats)
}

/// Move a pin to just before `before`, or to the end.
pub async fn move_pinned(id: String, before: Option<String>) -> Result<Vec<String>, String> {
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let list = pinned_chats::move_pinned_chat(&client, &id, before.as_deref()).await?;
    super::selfsync::emit_pinned(&list.chats);
    Ok(list.chats)
}

/// Archived chats: chat id → the newest message time when archived.
pub fn archived() -> HashMap<String, u64> {
    synced_prefs::load_archive().times().into_iter().collect()
}

/// Archive a DM as of `at` (unix ms), or bring it back when `at` is absent.
pub fn set_archived(chat_id: &str, at: Option<u64>) -> Result<HashMap<String, u64>, String> {
    let map = synced_prefs::set_archived(chat_id, at)?;
    super::selfsync::emit_archive(&map);
    super::prefs::publish_projection(Pref::Archive);
    Ok(map.times().into_iter().collect())
}

/// A message at `active_at` outlived the chat's archive: forget it.
pub fn revoke_archive(chat_id: &str, active_at: u64) -> Result<HashMap<String, u64>, String> {
    let (map, changed) = synced_prefs::revoke_archive(chat_id, active_at)?;
    if changed {
        super::selfsync::emit_archive(&map);
    }
    Ok(map.times().into_iter().collect())
}

/// Retreat the read marker so the newest contact message surfaces again, from the full history.
/// The new marker (empty when never read), or None when there was nothing to surface.
pub async fn mark_unread(chat_id: String) -> Option<String> {
    use db::events::{UnreadMark, compute_unread_anchor};
    let (marker, hex, clear) = match compute_unread_anchor(&chat_id).await {
        Ok(UnreadMark::Anchor(id)) => (vector_core::compact::encode_message_id(&id), id, false),
        Ok(UnreadMark::Clear) => ([0u8; 32], String::new(), true),
        Ok(UnreadMark::NoOp) | Err(_) => return None,
    };
    let slim = {
        let mut state = STATE.lock().await;
        let idx = state.chats.iter().position(|c| c.id == chat_id)?;
        state.chats[idx].mark_read_at(marker);
        db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner)
    };
    let _ = db::chats::save_slim_chat(&slim);
    // The upsert refuses an empty marker, so a never-read chat takes the clear path.
    if clear {
        let _ = db::chats::clear_chat_last_read(&chat_id);
    }
    let count = db::events::unread_count_for_chat(&chat_id).await.unwrap_or(0);
    {
        let mut state = STATE.lock().await;
        if state.unread_seeded {
            state.unread_set(&chat_id, count);
        }
    }
    vector_core::emit_event("unread_counts_stale", &json!({ "chats": [chat_id] }));
    Some(hex)
}
