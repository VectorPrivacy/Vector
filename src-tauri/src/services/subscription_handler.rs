//! Live subscription handler for real-time Nostr events.
//!
//! This module handles:
//! - GiftWrap subscription (DMs, files, MLS welcomes)
//! - MLS group message subscription
//!
//! ## MLS Live Subscriptions Overview (using Marmot/MDK)
//!
//! ### GiftWrap Subscription (Kind::GiftWrap)
//! - Carries DMs/files and also MLS Welcomes
//! - Welcomes are detected after unwrap in handle_event() when rumor.kind == Kind::MlsWelcome
//! - We immediately persist via the MDK engine on a blocking thread (spawn_blocking)
//! - Emits "mls_invite_received" so the frontend can refresh list_pending_mls_welcomes
//!
//! ### MLS Group Messages Subscription (Kind::MlsGroupMessage)
//! - Subscribed live in parallel to GiftWraps
//! - We extract the wire group id from the 'h' tag and check membership using encrypted metadata
//! - If a message is for a group we belong to, we process via MDK engine on a blocking thread
//! - Persists to "mls_messages_{group_id}" and "mls_timeline_{group_id}"
//! - Emits "mls_message_new" for immediate UI updates
//! - For non-members: We attempt to process as a Welcome message
//!
//! ### Deduplication
//! - Real-time path uses the same keys as sync (inner_event_id, wrapper_event_id)
//! - Only insert if inner_event_id is not present in the group messages map
//! - This prevents duplicates when subsequent explicit sync covers the same events
//!
//! ### Send-boundary
//! - All MDK engine interactions occur inside tokio::task::spawn_blocking
//! - We avoid awaits while holding the engine to respect non-Send constraints
//!
//! ### Privacy & Logging
//! - We do not log plaintext message content
//! - Logs are limited to ids, counts, kinds, and outcomes

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::{
    db,
    MlsService, NotificationData, show_notification_generic,
    TAURI_APP, NOSTR_CLIENT,
    util::get_file_type_description,
};

/// Process an MLS group message event through the full pipeline.
/// Shared between live subscriptions (full app) and standalone sync (background service).
/// Handles: membership check, MDK decryption, rumor processing, DB persistence,
/// OS notifications, and frontend emissions (guarded by TAURI_APP).
///
/// Returns true if the event was processed, false if skipped.
pub(crate) async fn handle_mls_group_message(event: Event, my_public_key: PublicKey) -> bool {
    // Extract group wire id from 'h' tag
    let group_wire_id = match event
        .tags
        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
        .and_then(|t| t.content().map(|s| s.to_string()))
    {
        Some(id) => id,
        None => return false,
    };

    // Check membership without constructing MLS engine
    let is_member = if let Ok(groups) = db::load_mls_groups().await {
        groups.iter().any(|g| g.group_id == group_wire_id || g.engine_group_id == group_wire_id)
    } else {
        false
    };
    if !is_member {
        return false;
    }

    // Skip own events
    if event.pubkey == my_public_key {
        return false;
    }

    let my_npub = my_public_key.to_bech32().unwrap_or_default();
    let group_id_for_persist = group_wire_id.clone();
    let group_id_for_emit = group_wire_id.clone();
    let ev = event;

    // Process message and persist in one blocking operation (MLS engine is non-Send)
    let emit_record = tokio::task::spawn_blocking(move || {
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
                    mdk_core::prelude::MessageProcessingResult::ApplicationMessage(msg) => {
                        let rumor_event = crate::rumor::RumorEvent {
                            id: msg.id,
                            kind: msg.kind,
                            content: msg.content.clone(),
                            tags: msg.tags.clone(),
                            created_at: msg.created_at,
                            pubkey: msg.pubkey,
                        };

                        let is_mine = !my_npub.is_empty() && msg.pubkey.to_bech32().unwrap() == my_npub;

                        let processed = rt.block_on(async {
                            use crate::rumor::{process_rumor, RumorContext, ConversationType, RumorProcessingResult};

                            let rumor_context = RumorContext {
                                sender: msg.pubkey,
                                is_mine,
                                conversation_id: group_id_for_persist.clone(),
                                conversation_type: ConversationType::MlsGroup,
                            };

                            match process_rumor(rumor_event, rumor_context).await {
                                Ok(result) => {
                                    match result {
                                        RumorProcessingResult::TextMessage(mut message) => {
                                            if !message.replied_to.is_empty() {
                                                let _ = db::populate_reply_context(&mut message).await;
                                            }

                                            let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();

                                            let (was_added, _active_typers, should_notify) = {
                                                let mut state = crate::STATE.lock().await;
                                                let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                    !chat.muted && !message.mine
                                                } else {
                                                    false
                                                };
                                                let typers = state.update_typing_and_get_active(&group_id_for_persist, &sender_npub, 0);
                                                (added, typers, notify)
                                            };

                                            if was_added && should_notify {
                                                let (sender_name, group_name, avatar, group_avatar) = {
                                                    let state = crate::STATE.lock().await;

                                                    let (sender, av) = if let Some(profile) = state.get_profile(&sender_npub) {
                                                        let name = if !profile.nickname.is_empty() {
                                                            profile.nickname.to_string()
                                                        } else if !profile.name.is_empty() {
                                                            profile.name.to_string()
                                                        } else {
                                                            "Someone".to_string()
                                                        };
                                                        let cached = if !profile.avatar_cached.is_empty() {
                                                            Some(profile.avatar_cached.to_string())
                                                        } else {
                                                            None
                                                        };
                                                        (name, cached)
                                                    } else {
                                                        ("Someone".to_string(), None)
                                                    };

                                                    let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                        chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                    } else {
                                                        "Group Chat".to_string()
                                                    };

                                                    (sender, group, av, None::<String>)
                                                };

                                                // Fetch group avatar from MLS metadata (outside STATE lock)
                                                let group_avatar = if group_avatar.is_none() {
                                                    db::load_mls_groups().await.ok().and_then(|groups| {
                                                        groups.into_iter()
                                                            .find(|g| g.group_id == group_id_for_persist)
                                                            .and_then(|g| g.avatar_cached)
                                                    })
                                                } else {
                                                    group_avatar
                                                };

                                                let notification = NotificationData::group_message(sender_name, group_name, message.content.clone(), avatar, group_avatar, group_id_for_persist.clone());
                                                show_notification_generic(notification);
                                            }

                                            if was_added {
                                                let slim = {
                                                    let state = crate::STATE.lock().await;
                                                    state.get_chat(&group_id_for_persist).map(|c| {
                                                        crate::db::chats::SlimChatDB::from_chat(c, &state.interner)
                                                    })
                                                };
                                                if let Some(slim) = slim {
                                                    let _ = crate::db::chats::save_slim_chat(slim).await;
                                                    let _ = db::save_message(&group_id_for_persist, &message).await;
                                                }
                                                Some(message)
                                            } else {
                                                None
                                            }
                                        }
                                        RumorProcessingResult::FileAttachment(mut message) => {
                                            if !message.replied_to.is_empty() {
                                                let _ = db::populate_reply_context(&mut message).await;
                                            }

                                            let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();

                                            let (was_added, _active_typers, should_notify) = {
                                                let mut state = crate::STATE.lock().await;
                                                let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                    !chat.muted && !message.mine
                                                } else {
                                                    false
                                                };
                                                let typers = state.update_typing_and_get_active(&group_id_for_persist, &sender_npub, 0);
                                                (added, typers, notify)
                                            };

                                            if was_added && should_notify {
                                                let (sender_name, group_name, avatar, group_avatar) = {
                                                    let state = crate::STATE.lock().await;

                                                    let (sender, av) = if let Some(profile) = state.get_profile(&sender_npub) {
                                                        let name = if !profile.nickname.is_empty() {
                                                            profile.nickname.to_string()
                                                        } else if !profile.name.is_empty() {
                                                            profile.name.to_string()
                                                        } else {
                                                            "Someone".to_string()
                                                        };
                                                        let cached = if !profile.avatar_cached.is_empty() {
                                                            Some(profile.avatar_cached.to_string())
                                                        } else {
                                                            None
                                                        };
                                                        (name, cached)
                                                    } else {
                                                        ("Someone".to_string(), None)
                                                    };

                                                    let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                        chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                    } else {
                                                        "Group Chat".to_string()
                                                    };

                                                    (sender, group, av, None::<String>)
                                                };

                                                // Fetch group avatar from MLS metadata (outside STATE lock)
                                                let group_avatar = if group_avatar.is_none() {
                                                    db::load_mls_groups().await.ok().and_then(|groups| {
                                                        groups.into_iter()
                                                            .find(|g| g.group_id == group_id_for_persist)
                                                            .and_then(|g| g.avatar_cached)
                                                    })
                                                } else {
                                                    group_avatar
                                                };

                                                let content = {
                                                    let extension = message.attachments.first()
                                                        .map(|att| att.extension.clone())
                                                        .unwrap_or_else(|| String::from("file"));
                                                    "Sent a ".to_string() + &get_file_type_description(&extension)
                                                };
                                                let notification = NotificationData::group_message(sender_name, group_name, content, avatar, group_avatar, group_id_for_persist.clone());
                                                show_notification_generic(notification);
                                            }

                                            if was_added {
                                                let slim = {
                                                    let state = crate::STATE.lock().await;
                                                    state.get_chat(&group_id_for_persist).map(|c| {
                                                        crate::db::chats::SlimChatDB::from_chat(c, &state.interner)
                                                    })
                                                };
                                                if let Some(slim) = slim {
                                                    let _ = crate::db::chats::save_slim_chat(slim).await;
                                                    let _ = db::save_message(&group_id_for_persist, &message).await;
                                                }
                                                Some(message)
                                            } else {
                                                None
                                            }
                                        }
                                        RumorProcessingResult::Reaction(reaction) => {
                                            let (was_added, chat_id_for_save) = {
                                                let mut state = crate::STATE.lock().await;
                                                if let Some((chat_id, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
                                                    (added, if added { Some(chat_id) } else { None })
                                                } else {
                                                    (false, None)
                                                }
                                            };

                                            if was_added {
                                                if let Some(chat_id) = chat_id_for_save {
                                                    let updated_message = {
                                                        let state = crate::STATE.lock().await;
                                                        state.find_message(&reaction.reference_id)
                                                            .map(|(_, msg)| msg.clone())
                                                    };
                                                    if let Some(msg) = updated_message {
                                                        let _ = db::save_message(&chat_id, &msg).await;
                                                        if let Some(handle) = TAURI_APP.get() {
                                                            let _ = handle.emit("message_update", serde_json::json!({
                                                                "old_id": &reaction.reference_id,
                                                                "message": &msg,
                                                                "chat_id": &chat_id
                                                            }));
                                                        }
                                                    }
                                                }
                                            }
                                            None
                                        }
                                        RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                            let active_typers = {
                                                let mut state = crate::STATE.lock().await;
                                                state.update_typing_and_get_active(&group_id_for_persist, &profile_id, until)
                                            };
                                            if let Some(handle) = TAURI_APP.get() {
                                                let _ = handle.emit("typing-update", serde_json::json!({
                                                    "conversation_id": group_id_for_persist,
                                                    "typers": active_typers
                                                }));
                                            }
                                            None
                                        }
                                        RumorProcessingResult::LeaveRequest { event_id, member_pubkey } => {
                                            if db::event_exists(&event_id).unwrap_or(false) {
                                                return None;
                                            }

                                            let member_name = {
                                                let state = crate::STATE.lock().await;
                                                state.get_profile(&member_pubkey)
                                                    .map(|p| {
                                                        if !p.nickname.is_empty() { p.nickname.to_string() }
                                                        else if !p.name.is_empty() { p.name.to_string() }
                                                        else { member_pubkey.chars().take(12).collect::<String>() + "..." }
                                                    })
                                            };

                                            if let Some(handle) = TAURI_APP.get() {
                                                let mls_svc = match MlsService::new_persistent_static() {
                                                    Ok(s) => s,
                                                    Err(_) => return None,
                                                };

                                                let my_hex = my_public_key.to_hex();

                                                let am_i_admin = if let Ok(groups) = mls_svc.read_groups().await {
                                                    if let Some(meta) = groups.iter().find(|g| g.group_id == group_id_for_persist) {
                                                        meta.creator_pubkey == my_npub || meta.creator_pubkey == my_hex
                                                    } else {
                                                        false
                                                    }
                                                } else {
                                                    false
                                                };

                                                if am_i_admin {
                                                    let was_inserted = db::save_system_event_by_id(
                                                        &event_id,
                                                        &group_id_for_persist,
                                                        crate::db::SystemEventType::MemberLeft,
                                                        &member_pubkey,
                                                        member_name.as_deref(),
                                                    ).await.unwrap_or(false);

                                                    if was_inserted {
                                                        let _ = handle.emit("system_event", serde_json::json!({
                                                            "conversation_id": group_id_for_persist,
                                                            "event_id": event_id,
                                                            "event_type": crate::db::SystemEventType::MemberLeft.as_u8(),
                                                            "member_pubkey": member_pubkey,
                                                            "member_name": member_name,
                                                        }));

                                                        if let Err(e) = mls_svc.remove_member_device(&group_id_for_persist, &member_pubkey, "").await {
                                                            eprintln!("[MLS] Live: Failed to auto-remove member {}: {}", member_pubkey, e);
                                                        }
                                                    }
                                                }
                                            }
                                            None
                                        }
                                        RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                                            super::handle_webxdc_peer_advertisement(&topic_id, &node_addr).await;
                                            None
                                        }
                                        RumorProcessingResult::UnknownEvent(mut event) => {
                                            if let Ok(chat_id) = db::get_chat_id_by_identifier(&group_id_for_persist) {
                                                event.chat_id = chat_id;
                                                let _ = db::save_event(&event).await;
                                            }
                                            None
                                        }
                                        RumorProcessingResult::Ignored => None,
                                        RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                            let event_timestamp = event.created_at;
                                            let _ = db::save_pivx_payment_event(&group_id_for_persist, event).await;
                                            if let Some(handle) = TAURI_APP.get() {
                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                let _ = handle.emit("pivx_payment_received", serde_json::json!({
                                                    "conversation_id": group_id_for_persist,
                                                    "gift_code": gift_code,
                                                    "amount_piv": amount_piv,
                                                    "address": address,
                                                    "message_id": message_id,
                                                    "sender": sender_npub,
                                                    "is_mine": is_mine,
                                                    "at": event_timestamp * 1000,
                                                }));
                                            }
                                            None
                                        }
                                        RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                            if db::event_exists(&event.id).unwrap_or(false) {
                                                return None;
                                            }
                                            if let Ok(chat_id) = db::get_chat_id_by_identifier(&group_id_for_persist) {
                                                let mut event_with_chat = event;
                                                event_with_chat.chat_id = chat_id;
                                                let _ = db::save_event(&event_with_chat).await;
                                            }
                                            let msg_for_emit = {
                                                let mut state = crate::STATE.lock().await;
                                                state.update_message_in_chat(&group_id_for_persist, &message_id, |msg| {
                                                    msg.apply_edit(new_content, edited_at);
                                                })
                                            };
                                            if let Some(msg) = msg_for_emit {
                                                if let Some(handle) = TAURI_APP.get() {
                                                    let _ = handle.emit("message_update", serde_json::json!({
                                                        "old_id": &message_id,
                                                        "message": &msg,
                                                        "chat_id": &group_id_for_persist
                                                    }));
                                                }
                                            }
                                            None
                                        }
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
                    mdk_core::prelude::MessageProcessingResult::Commit { mls_group_id } => {
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
                            Some(true) => {
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("mls_group_updated", serde_json::json!({
                                        "group_id": group_id_for_persist
                                    })).ok();
                                }
                            }
                            None => {
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("mls_group_updated", serde_json::json!({
                                        "group_id": group_id_for_persist
                                    })).ok();
                                }
                            }
                        }
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    mdk_core::prelude::MessageProcessingResult::Proposal(_) => {
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_group_updated", serde_json::json!({
                                "group_id": group_id_for_persist
                            })).ok();
                        }
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    mdk_core::prelude::MessageProcessingResult::PendingProposal { .. } |
                    mdk_core::prelude::MessageProcessingResult::IgnoredProposal { .. } => {
                        let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                        None
                    }
                    mdk_core::prelude::MessageProcessingResult::Unprocessable { .. } => {
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
                    eprintln!("[MLS] Eviction detected in live subscription - group: {}", group_id_for_persist);
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

    if let Some(record) = emit_record {
        if let Some(handle) = TAURI_APP.get() {
            let _ = handle.emit("mls_message_new", serde_json::json!({
                "group_id": group_id_for_emit,
                "message": record
            }));
        }
    }

    true
}

/// Start live subscriptions for GiftWraps and MLS group messages.
/// Called once after login to begin receiving real-time events.
pub(crate) async fn start_subscriptions() -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let pubkey = *crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?;

    // Live GiftWraps to us (DMs, files, MLS welcomes)
    let giftwrap_filter = Filter::new()
        .pubkey(pubkey)
        .kind(Kind::GiftWrap)
        .limit(0);

    // Live MLS group wrappers (Kind::MlsGroupMessage). Broad subscribe; we'll filter by membership in handler.
    let mls_msg_filter = Filter::new()
        .kind(Kind::MlsGroupMessage)
        .limit(0);

    // Subscribe to both filters
    let gift_sub_id = match client.subscribe(giftwrap_filter, None).await {
        Ok(id) => id.val,
        Err(e) => return Err(e.to_string()),
    };
    let mls_sub_id = match client.subscribe(mls_msg_filter, None).await {
        Ok(id) => id.val,
        Err(e) => return Err(e.to_string()),
    };

    // Begin watching for notifications from our subscriptions
    match client
        .handle_notifications(|notification| async {
            if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                if subscription_id == gift_sub_id {
                    // Handle DMs/files/vector-specific + MLS welcomes inside giftwrap
                    super::handle_event(*event, true).await;
                } else if subscription_id == mls_sub_id {
                    // Handle live MLS group message via shared handler
                    let my_pk = crate::MY_PUBLIC_KEY.get()
                        .copied()
                        .unwrap_or(PublicKey::from_slice(&[0u8; 32]).unwrap());
                    handle_mls_group_message((*event).clone(), my_pk).await;
                }
            }
            Ok(false)
        })
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}