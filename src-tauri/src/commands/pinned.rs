//! Pinned Chats commands — the account's favourite conversations, synced
//! across its own devices as a self-encrypted kind-30078 list.
//!
//! Ids are opaque: a DM's npub or a Community's id. Nothing here resolves one
//! to a chat, so a pin for a chat this device has not synced yet survives every
//! read and republish (see `vector_core::pinned_chats`).

use nostr_sdk::prelude::Event;
use vector_core::pinned_chats::{self, PinnedChats};

/// The pinned list as this device holds it. Reads the LOCAL mirror, so the chat
/// list can sort itself on the very first paint without waiting on a relay.
#[tauri::command]
pub async fn get_pinned_chats() -> Result<Vec<String>, String> {
    Ok(pinned_chats::load_local().chats)
}

/// Pin a chat and sync it to the account's other devices. `Err` past the cap —
/// a user action deserves a message, not a silent no-op.
#[tauri::command]
pub async fn pin_chat(chat_id: String) -> Result<Vec<String>, String> {
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let list = pinned_chats::pin_chat(&client, &chat_id).await?;
    emit_pinned(&list);
    Ok(list.chats)
}

/// Unpin a chat and sync it. Unpinning something already gone is success.
#[tauri::command]
pub async fn unpin_chat(chat_id: String) -> Result<Vec<String>, String> {
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let list = pinned_chats::unpin_chat(&client, &chat_id).await?;
    emit_pinned(&list);
    Ok(list.chats)
}

/// A sibling device changed the list: fold it in and tell the UI to re-sort.
/// Never republishes — the relay echoes our own publishes back on the same
/// subscription, and answering an echo with a publish loops forever.
pub async fn ingest_pinned_chats_update(event: Event) {
    let Some(my_pk) = vector_core::my_public_key() else { return };
    match pinned_chats::ingest_remote_event(&my_pk, &event).await {
        Ok(list) => emit_pinned(&list),
        Err(e) => eprintln!("[PinnedChats] ingest failed: {e}"),
    }
}

/// Through `emit_event` so a list belonging to an account that has since been
/// swapped away paints nothing.
fn emit_pinned(list: &PinnedChats) {
    vector_core::traits::emit_event_json("pinned_chats_updated", serde_json::json!(list.chats));
}
