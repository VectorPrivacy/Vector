//! Message actions beyond sending: reactions, edits, deletes and retries (DMs; a Community
//! channel's go through its own service).

use std::path::Path;
use std::sync::Arc;

use nostr_sdk::prelude::{EventId, PublicKey};
use serde_json::json;
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{db, VectorCore, STATE};

use super::messaging::Callback;

fn is_dm(chat_id: &str) -> bool {
    chat_id.starts_with("npub1")
}

fn emit_removed(id: &str, chat_id: &str) {
    vector_core::emit_event("message_removed", &json!({ "id": id, "chat_id": chat_id, "reason": "deleted" }));
}

pub async fn react(chat_id: String, reference: String, emoji: String) -> Result<(), String> {
    if !is_dm(&chat_id) {
        return Err("Reactions in Community channels are not yet supported".into());
    }
    VectorCore.send_reaction(&chat_id, &reference, &emoji, None).await.map(|_| ()).map_err(|e| e.to_string())
}

pub async fn edit(chat_id: String, message_id: String, content: String) -> Result<(), String> {
    if !is_dm(&chat_id) {
        return Err("Editing Community messages isn't available in this client yet".into());
    }
    VectorCore.edit_dm(&chat_id, &message_id, &content).await.map(|_| ()).map_err(|e| e.to_string())
}

async fn reconcile_unread(chat_id: &str) {
    let count = db::events::unread_count_for_chat(chat_id).await.unwrap_or(0);
    {
        let mut state = STATE.lock().await;
        if state.unread_seeded {
            state.unread_set(chat_id, count);
        }
    }
    vector_core::emit_event("unread_counts_stale", &json!({ "chats": [chat_id] }));
}

/// Deletes a DM from the relays (ours by our key, theirs by asking their clients).
pub async fn delete(message_id: String) -> Result<(), String> {
    let (chat_id, msg) = VectorCore.get_message(&message_id).await.ok_or_else(|| format!("Message not found (id: {message_id})"))?;
    if !is_dm(&chat_id) {
        return Err("Community channel messages are deleted via the Community service".into());
    }
    let rumor_id = EventId::from_hex(&message_id).map_err(|e| format!("Invalid message id: {e}"))?;
    if msg.mine {
        vector_core::delete_own_dm(&rumor_id).await?;
    } else {
        vector_core::delete_received_dm(&rumor_id).await?;
    }
    STATE.lock().await.remove_message(&message_id);
    // Before the row delete: an unflushed self-echo consults this and drops itself.
    vector_core::state::note_message_deleted(&message_id);
    let _ = db::events::delete_event(&message_id).await;
    emit_removed(&message_id, &chat_id);
    reconcile_unread(&chat_id).await;
    Ok(())
}

/// A failed send never reached the wire: local cleanup, and its uploads taken back.
pub async fn delete_failed(message_id: String) -> Result<(), String> {
    let removed = {
        let mut state = STATE.lock().await;
        match state.find_message(&message_id) {
            Some((_, m)) if m.failed => state.remove_message(&message_id),
            Some(_) => return Err("Message is not failed or does not exist".into()),
            None => None,
        }
    };
    let (chat_id, msg) = match removed {
        Some(found) => found,
        None => match VectorCore.get_message(&message_id).await {
            Some((c, m)) if m.failed => (c, m),
            _ => return Err("Message is not failed or does not exist".into()),
        },
    };
    let download_dir = db::get_download_dir();
    for att in &msg.attachments {
        if att.path.is_empty() || !Path::new(&att.path).starts_with(&download_dir) {
            continue;
        }
        if db::attachments::hash_referenced_elsewhere(&att.id, &message_id) == Ok(false) {
            let _ = tokio::fs::remove_file(&att.path).await;
        }
    }
    let urls: Vec<String> = msg.attachments.iter().flat_map(|a| a.all_urls().map(str::to_string)).collect();
    let urls = db::attachments::urls_unreferenced_elsewhere(&urls, &message_id).unwrap_or_default();
    if !urls.is_empty() {
        if let Ok(signer) = vector_core::signer::active_signer() {
            vector_core::blossom::delete_blobs_best_effort(signer, urls);
        }
    }
    let _ = db::events::delete_event(&message_id).await;
    emit_removed(&message_id, &chat_id);
    Ok(())
}

pub async fn retry(receiver: String, message_id: String) -> Result<bool, String> {
    let failed = VectorCore.get_message(&message_id).await.is_some_and(|(_, m)| m.failed);
    if !failed {
        return Err("Message is not failed or does not exist".into());
    }
    let callback: Arc<dyn SendCallback> = Arc::new(Callback);
    vector_core::sending::resend_failed_dm(&receiver, &message_id, &SendConfig::gui(), callback).await
}

/// Takes back one of our reactions.
pub async fn unreact(reaction_id: String) -> Result<(), String> {
    let found = STATE.lock().await.find_reaction(&reaction_id);
    let (chat_id, message_id, author, is_community) = found.ok_or_else(|| format!("Reaction not found (id: {reaction_id})"))?;
    if author != db::get_current_account()? {
        return Err("Cannot revoke a reaction that isn't yours".into());
    }
    if is_community {
        return Err("Community reactions aren't available in this client yet".into());
    }
    let updated = STATE.lock().await.remove_reaction_from_message(&message_id, &reaction_id);
    let _ = db::events::delete_event(&reaction_id).await;
    if let Some((_, message)) = updated {
        vector_core::emit_event("message_update", &json!({ "old_id": message_id, "message": message, "chat_id": chat_id }));
    }
    let rid = EventId::from_hex(&reaction_id).map_err(|e| format!("Invalid reaction id: {e}"))?;
    let recipient = PublicKey::parse(&chat_id).map_err(|e| format!("Invalid DM counterpart: {e}"))?;
    vector_core::deletion::delete_own_reaction(&rid, recipient).await.map(|_| ())
}

/// The account's stock emoji by decayed use, for the picker's recents.
pub fn emoji_usage() -> std::collections::HashMap<String, f64> {
    vector_core::emoji_usage::ranked(Some(256)).into_iter().filter(|u| u.kind == "unicode").map(|u| (u.id, u.score)).collect()
}

pub fn bump_emoji(emoji: &str) {
    let _ = vector_core::emoji_usage::bump("unicode", emoji, None);
}

const KAOMOJI_RECENTS: &str = "kaomoji_recents";

pub fn kaomoji_recents() -> Vec<String> {
    vector_core::db::settings::get_sql_setting(KAOMOJI_RECENTS.into()).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn set_kaomoji_recents(recents: Vec<String>) {
    if let Ok(json) = serde_json::to_string(&recents) {
        let _ = vector_core::db::settings::set_sql_setting(KAOMOJI_RECENTS.into(), json);
    }
}

/// A sent message's distinct emoji, in one record.
pub fn bump_emoji_batch(emoji: Vec<String>) {
    let uses: Vec<_> = emoji.into_iter().map(|id| vector_core::emoji_usage::EmojiUse { kind: "unicode".into(), id, url: None }).collect();
    let _ = vector_core::emoji_usage::bump_batch(&uses);
}
