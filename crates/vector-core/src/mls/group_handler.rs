//! MLS group message handler — processes live MLS group message events.
//!
//! Core processing pipeline: membership check → MDK decryption → rumor processing →
//! STATE + DB persistence → event emission. Returns the processed message (if any)
//! for the caller to handle notifications, badge updates, etc.

use nostr_sdk::prelude::*;
use mdk_core::prelude::*;

use crate::mls::MlsService;
use crate::types::Message;

/// Process an MLS group message event through the full pipeline.
///
/// Handles: membership check, MDK decryption, rumor processing, DB persistence,
/// commit/proposal/eviction detection, and metadata sync.
///
/// Returns `Some(Message)` if a new displayable message was processed,
/// `None` if the event was a commit/proposal/skipped/error.
/// The caller is responsible for notifications and badge updates.
pub async fn handle_mls_group_message(event: Event, my_public_key: PublicKey) -> Option<Message> {
    // Extract group wire id from 'h' tag
    let group_wire_id = event
        .tags
        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
        .and_then(|t| t.content().map(|s| s.to_string()))?;

    // Check membership without constructing MLS engine
    let is_member = if let Ok(groups) = crate::db::mls::load_mls_groups() {
        groups.iter().any(|g| g.group.group_id == group_wire_id || g.group.engine_group_id == group_wire_id)
    } else {
        false
    };
    if !is_member { return None; }

    // Skip own events
    if event.pubkey == my_public_key { return None; }

    let my_npub = my_public_key.to_bech32().unwrap_or_default();
    let group_id_for_persist = group_wire_id.clone();
    let group_id_for_emit = group_wire_id.clone();
    let ev = event;

    // Process message and persist in one blocking operation (MLS engine is non-Send)
    let emit_record: Option<Message> = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();

        // Per-group lock to coordinate with sync
        let group_lock = crate::mls::get_group_sync_lock(&group_id_for_persist);
        let _guard = rt.block_on(group_lock.lock());

        // Skip if already processed
        if crate::mls::is_mls_event_processed(&ev.id.to_hex()) {
            return None;
        }

        let svc = MlsService::new_persistent_static().ok()?;
        let engine = svc.engine().ok()?;

        match engine.process_message(&ev) {
            Ok(res) => {
                match res {
                    MessageProcessingResult::ApplicationMessage(msg) => {
                        let rumor_event = crate::rumor::RumorEvent {
                            id: msg.id,
                            kind: msg.kind,
                            content: msg.content.clone(),
                            tags: msg.tags.clone(),
                            created_at: msg.created_at,
                            pubkey: msg.pubkey,
                        };

                        let is_mine = !my_npub.is_empty() && msg.pubkey.to_bech32().ok().as_deref() == Some(my_npub.as_str());

                        let processed = rt.block_on(async {
                            use crate::rumor::{RumorContext, ConversationType, RumorProcessingResult};

                            let rumor_context = RumorContext {
                                sender: msg.pubkey,
                                is_mine,
                                conversation_id: group_id_for_persist.clone(),
                                conversation_type: ConversationType::MlsGroup,
                            };

                            let download_dir = crate::db::get_download_dir();
                            match crate::mls::process_rumor_with_mls(&rumor_event, &rumor_context, &download_dir).await {
                                Ok(result) => {
                                    match result {
                                        RumorProcessingResult::TextMessage(mut message) | RumorProcessingResult::FileAttachment(mut message) => {
                                            if !message.replied_to.is_empty() {
                                                let _ = crate::db::events::populate_reply_context(&mut message).await;
                                            }

                                            let was_added = {
                                                let mut state = crate::state::STATE.lock().await;
                                                let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                state.update_typing_and_get_active(&group_id_for_persist, &sender_npub, 0);
                                                added
                                            };

                                            if was_added {
                                                let slim = {
                                                    let state = crate::state::STATE.lock().await;
                                                    state.get_chat(&group_id_for_persist).map(|c| {
                                                        crate::db::chats::SlimChatDB::from_chat(c, &state.interner)
                                                    })
                                                };
                                                if let Some(slim) = slim {
                                                    let _ = crate::db::chats::save_slim_chat(&slim);
                                                    let _ = crate::db::events::save_message(&group_id_for_persist, &message).await;
                                                }
                                                Some(message)
                                            } else {
                                                None
                                            }
                                        }
                                        RumorProcessingResult::Reaction(reaction) => {
                                            let (was_added, chat_id_for_save) = {
                                                let mut state = crate::state::STATE.lock().await;
                                                if let Some((chat_id, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
                                                    (added, if added { Some(chat_id) } else { None })
                                                } else {
                                                    (false, None)
                                                }
                                            };

                                            if was_added {
                                                if let Some(chat_id) = chat_id_for_save {
                                                    let updated = {
                                                        let state = crate::state::STATE.lock().await;
                                                        state.find_message(&reaction.reference_id).map(|(_, msg)| msg.clone())
                                                    };
                                                    if let Some(msg) = updated {
                                                        let _ = crate::db::events::save_message(&chat_id, &msg).await;
                                                        crate::traits::emit_event("message_update", &serde_json::json!({
                                                            "old_id": &reaction.reference_id, "message": &msg, "chat_id": &chat_id
                                                        }));
                                                    }
                                                }
                                            }
                                            None
                                        }
                                        RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                            let active_typers = {
                                                let mut state = crate::state::STATE.lock().await;
                                                state.update_typing_and_get_active(&group_id_for_persist, &profile_id, until)
                                            };
                                            crate::traits::emit_event("typing-update", &serde_json::json!({
                                                "conversation_id": group_id_for_persist, "typers": active_typers
                                            }));
                                            None
                                        }
                                        RumorProcessingResult::LeaveRequest { event_id, member_pubkey } => {
                                            if crate::db::events::event_exists(&event_id).unwrap_or(false) {
                                                return None;
                                            }

                                            let member_name = {
                                                let state = crate::state::STATE.lock().await;
                                                state.get_profile(&member_pubkey).map(|p| {
                                                    if !p.nickname.is_empty() { p.nickname.to_string() }
                                                    else if !p.name.is_empty() { p.name.to_string() }
                                                    else { member_pubkey.chars().take(12).collect::<String>() + "..." }
                                                })
                                            };

                                            let mls_svc = match MlsService::new_persistent_static() {
                                                Ok(s) => s,
                                                Err(_) => return None,
                                            };

                                            let my_hex = my_public_key.to_hex();
                                            let am_i_admin = if let Ok(groups) = mls_svc.read_groups() {
                                                if let Some(meta) = groups.iter().find(|g| g.group.group_id == group_id_for_persist) {
                                                    meta.group.creator_pubkey == my_npub || meta.group.creator_pubkey == my_hex
                                                } else { false }
                                            } else { false };

                                            if am_i_admin {
                                                let was_inserted = crate::db::events::save_system_event_by_id(
                                                    &event_id, &group_id_for_persist,
                                                    crate::stored_event::SystemEventType::MemberLeft,
                                                    &member_pubkey, member_name.as_deref(),
                                                ).await.unwrap_or(false);

                                                if was_inserted {
                                                    crate::traits::emit_event("system_event", &serde_json::json!({
                                                        "conversation_id": group_id_for_persist,
                                                        "event_id": event_id,
                                                        "event_type": crate::stored_event::SystemEventType::MemberLeft.as_u8(),
                                                        "member_pubkey": member_pubkey,
                                                        "member_name": member_name,
                                                    }));

                                                    if let Err(e) = mls_svc.remove_member_device(&group_id_for_persist, &member_pubkey, "").await {
                                                        eprintln!("[MLS] Live: Failed to auto-remove member {}: {}", member_pubkey, e);
                                                    }
                                                }
                                            }
                                            None
                                        }
                                        RumorProcessingResult::UnknownEvent(mut event) => {
                                            if let Ok(chat_id) = crate::db::id_cache::get_chat_id_by_identifier(&group_id_for_persist) {
                                                event.chat_id = chat_id;
                                                let _ = crate::db::events::save_event(&event).await;
                                            }
                                            None
                                        }
                                        RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                            let event_timestamp = event.created_at;
                                            let _ = crate::db::events::save_pivx_payment_event(&group_id_for_persist, event).await;
                                            crate::traits::emit_event("pivx_payment_received", &serde_json::json!({
                                                "conversation_id": group_id_for_persist,
                                                "gift_code": gift_code, "amount_piv": amount_piv,
                                                "address": address, "message_id": message_id,
                                                "sender": msg.pubkey.to_bech32().unwrap_or_default(),
                                                "is_mine": is_mine,
                                                "at": event_timestamp * 1000,
                                            }));
                                            None
                                        }
                                        RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                            if crate::db::events::event_exists(&event.id).unwrap_or(false) {
                                                return None;
                                            }
                                            if let Ok(chat_id) = crate::db::id_cache::get_chat_id_by_identifier(&group_id_for_persist) {
                                                let mut event_with_chat = event;
                                                event_with_chat.chat_id = chat_id;
                                                let _ = crate::db::events::save_event(&event_with_chat).await;
                                            }
                                            let msg_for_emit = {
                                                let mut state = crate::state::STATE.lock().await;
                                                state.update_message_in_chat(&group_id_for_persist, &message_id, |msg| {
                                                    msg.apply_edit(new_content, edited_at);
                                                })
                                            };
                                            if let Some(msg) = msg_for_emit {
                                                crate::traits::emit_event("message_update", &serde_json::json!({
                                                    "old_id": &message_id, "message": &msg, "chat_id": &group_id_for_persist
                                                }));
                                            }
                                            None
                                        }
                                        RumorProcessingResult::WebxdcPeerAdvertisement { .. } |
                                        RumorProcessingResult::WebxdcPeerLeft { .. } => {
                                            // WebXDC handled by platform layer
                                            None
                                        }
                                        RumorProcessingResult::Ignored => None,
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[MLS][live] Failed to process rumor: {}", e);
                                    None
                                }
                            }
                        });

                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        processed
                    }
                    MessageProcessingResult::Commit { mls_group_id } => {
                        let membership_check = engine.get_members(&mls_group_id)
                            .ok()
                            .and_then(|members| {
                                nostr_sdk::PublicKey::from_bech32(&my_npub)
                                    .ok()
                                    .map(|pk| members.contains(&pk))
                            });

                        match membership_check {
                            Some(false) => {
                                eprintln!("[MLS] Eviction detected via Commit - group: {}", group_id_for_persist);
                                rt.block_on(async {
                                    if let Err(e) = svc.cleanup_evicted_group(&group_id_for_persist).await {
                                        eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
                                    }
                                });
                            }
                            Some(true) | None => {
                                let _ = engine.sync_group_metadata_from_mls(&mls_group_id);
                                if let Ok(Some(group)) = engine.get_group(&mls_group_id) {
                                    let new_name = group.name.clone();
                                    let new_desc = group.description.clone();
                                    let gid = group_id_for_persist.clone();
                                    rt.block_on(async {
                                        if let Ok(mut groups) = svc.read_groups() {
                                            let mut changed = false;
                                            if let Some(meta) = groups.iter_mut().find(|g| g.group.group_id == gid || g.group.engine_group_id == gid) {
                                                if meta.profile.name != new_name {
                                                    meta.profile.name = new_name.clone();
                                                    changed = true;
                                                }
                                                if meta.profile.description.as_deref().unwrap_or("") != new_desc {
                                                    meta.profile.description = if new_desc.is_empty() { None } else { Some(new_desc) };
                                                    changed = true;
                                                }
                                                if changed {
                                                    meta.group.updated_at = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs();
                                                }
                                            }
                                            if changed {
                                                let updated_meta = groups.iter().find(|g| g.group.group_id == gid || g.group.engine_group_id == gid).cloned();
                                                let _ = svc.write_groups(&groups);
                                                if let Some(meta) = updated_meta {
                                                    crate::mls::emit_group_metadata_event(&meta);
                                                }
                                                let mut state = crate::state::STATE.lock().await;
                                                if let Some(chat) = state.get_chat_mut(&gid) {
                                                    if !new_name.is_empty() {
                                                        chat.metadata.set_name(new_name);
                                                    }
                                                }
                                            }
                                        }
                                    });
                                }
                                // Sync participants
                                rt.block_on(async {
                                    if let Err(e) = svc.sync_group_participants(&group_id_for_persist).await {
                                        eprintln!("[MLS] Live: Failed to sync participants after commit: {}", e);
                                    }
                                });
                                crate::traits::emit_event("mls_group_updated", &serde_json::json!({
                                    "group_id": group_id_for_persist
                                }));
                            }
                        }
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    MessageProcessingResult::Proposal(_) => {
                        crate::traits::emit_event("mls_group_updated", &serde_json::json!({
                            "group_id": group_id_for_persist
                        }));
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    MessageProcessingResult::PendingProposal { .. } |
                    MessageProcessingResult::IgnoredProposal { .. } => {
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    MessageProcessingResult::Unprocessable { .. } => {
                        None // Don't track — may succeed on retry
                    }
                    _ => {
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                }
            }
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("evicted from it") ||
                   error_msg.contains("after being evicted") ||
                   error_msg.contains("own leaf not found") {
                    eprintln!("[MLS] Eviction detected in live handler - group: {}", group_id_for_persist);
                    rt.block_on(async {
                        if let Err(e) = svc.cleanup_evicted_group(&group_id_for_persist).await {
                            eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
                        }
                    });
                } else if !error_msg.contains("group not found") {
                    eprintln!("[MLS] live process_message failed (id={}): {}", ev.id, error_msg);
                }
                None
            }
        }
    })
    .await
    .unwrap_or(None);

    // Emit the processed message for frontend rendering
    if let Some(ref record) = emit_record {
        crate::traits::emit_event("mls_message_new", &serde_json::json!({
            "group_id": group_id_for_emit,
            "message": record
        }));
    }

    emit_record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_no_h_tag_returns_none() {
        let keys = Keys::generate();
        // Event without an 'h' tag
        let event = EventBuilder::new(Kind::MlsGroupMessage, "test")
            .build(keys.public_key())
            .sign(&keys)
            .await
            .unwrap();
        let result = handle_mls_group_message(event, keys.public_key()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn handle_own_event_returns_none() {
        let keys = Keys::generate();
        // Event with h tag but from ourselves
        let event = EventBuilder::new(Kind::MlsGroupMessage, "test")
            .tag(Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)), vec!["a".repeat(64)]))
            .build(keys.public_key())
            .sign(&keys)
            .await
            .unwrap();
        let result = handle_mls_group_message(event, keys.public_key()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn handle_non_member_returns_none() {
        let sender_keys = Keys::generate();
        let my_keys = Keys::generate();
        // Event from another user, with h tag, but we're not a member of this group
        let event = EventBuilder::new(Kind::MlsGroupMessage, "test")
            .tag(Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)), vec!["b".repeat(64)]))
            .build(sender_keys.public_key())
            .sign(&sender_keys)
            .await
            .unwrap();
        let result = handle_mls_group_message(event, my_keys.public_key()).await;
        assert!(result.is_none());
    }
}
