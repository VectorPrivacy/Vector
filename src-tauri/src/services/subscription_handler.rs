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
                    // Handle live MLS group message wrappers
                    let ev = (*event).clone();

                    // Extract group wire id from 'h' tag
                    let group_wire_id_opt = ev
                        .tags
                        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
                        .and_then(|t| t.content().map(|s| s.to_string()));

                    if let Some(group_wire_id) = group_wire_id_opt {
                        // Check if we are a member of this group (metadata check) without constructing MLS engine
                        let is_member: bool = if let Ok(groups) = db::load_mls_groups().await {
                            groups.iter().any(|g| {
                                g.group_id == group_wire_id || g.engine_group_id == group_wire_id
                            })
                        } else { false };

                        // Not a member - ignore this group message
                        if !is_member {
                            return Ok(false);
                        }
                        
                        // Resolve my pubkey for filtering and 'mine' flag
                        let (my_pubkey, my_pubkey_bech32) = {
                            if let Some(&pk) = crate::MY_PUBLIC_KEY.get() {
                                (Some(pk), pk.to_bech32().unwrap())
                            } else {
                                (None, String::new())
                            }
                        };
                        
                        // Skip processing our own events - they're already processed locally when sent
                        if let Some(my_pk) = my_pubkey {
                            if ev.pubkey == my_pk {
                                return Ok(false);
                            }
                        }

                        // Process with non-Send MLS engine on a blocking thread (no awaits in scope)
                        let app_handle = TAURI_APP.get().unwrap().clone();
                        let my_npub_for_block = my_pubkey_bech32.clone();
                        let group_id_for_persist = group_wire_id.clone();
                        let group_id_for_emit = group_wire_id.clone();
                        
                        // Process message and persist in one blocking operation to avoid Send issues
                        let emit_record = tokio::task::spawn_blocking(move || {
                            // Use runtime handle to drive async operations from blocking context
                            let rt = tokio::runtime::Handle::current();

                            // Acquire per-group lock to coordinate with sync
                            let group_lock = crate::mls::get_group_sync_lock(&group_id_for_persist);
                            let _guard = rt.block_on(group_lock.lock());

                            // EventTracker: Skip if already processed (pre-check before MDK call)
                            if crate::mls::is_mls_event_processed(&ev.id.to_hex()) {
                                // Already processed by sync or previous live handler - skip
                                return None;
                            }

                            // Create MLS service and process message
                            let svc = MlsService::new_persistent(&app_handle).ok()?;
                            let engine = svc.engine().ok()?;

                            match engine.process_message(&ev) {
                                Ok(res) => {
                                    // Use unified storage via process_rumor
                                    match res {
                                        mdk_core::prelude::MessageProcessingResult::ApplicationMessage(msg) => {
                                            // Convert to RumorEvent for protocol-agnostic processing
                                            let rumor_event = crate::rumor::RumorEvent {
                                                id: msg.id,
                                                kind: msg.kind,
                                                content: msg.content.clone(),
                                                tags: msg.tags.clone(),
                                                created_at: msg.created_at,
                                                pubkey: msg.pubkey,
                                            };
    
                                            let is_mine = !my_npub_for_block.is_empty() && msg.pubkey.to_bech32().unwrap() == my_npub_for_block;
    
                                            // Process through unified rumor processor
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
                                                                // Populate reply context for old messages not in frontend cache
                                                                if !message.replied_to.is_empty() {
                                                                    let _ = db::populate_reply_context(&mut message).await;
                                                                }

                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();

                                                                let (was_added, _active_typers, should_notify) = {
                                                                    let mut state = crate::STATE.lock().await;

                                                                    // Add message to chat
                                                                    let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                                    
                                                                    // Check if we should send notification (not muted, not mine)
                                                                    let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                        !chat.muted && !message.mine
                                                                    } else {
                                                                        false
                                                                    };
                                                                    
                                                                    // Clear typing indicator for sender
                                                                    let typers = state.update_typing_and_get_active(&group_id_for_persist, &sender_npub, 0);

                                                                    (added, typers, notify)
                                                                };

                                                                // Send OS notification for new group messages
                                                                if was_added && should_notify {
                                                                    // Get sender name and group name for notification
                                                                    let (sender_name, group_name) = {
                                                                        let state = crate::STATE.lock().await;

                                                                        let sender = if let Some(profile) = state.get_profile(&sender_npub) {
                                                                            if !profile.nickname.is_empty() {
                                                                                profile.nickname.to_string()
                                                                            } else if !profile.name.is_empty() {
                                                                                profile.name.to_string()
                                                                            } else {
                                                                                "Someone".to_string()
                                                                            }
                                                                        } else {
                                                                            "Someone".to_string()
                                                                        };

                                                                        let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                            chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                                        } else {
                                                                            "Group Chat".to_string()
                                                                        };

                                                                        (sender, group)
                                                                    };

                                                                    // Create notification for text message
                                                                    let notification = NotificationData::group_message(sender_name, group_name, message.content.clone());
                                                                    show_notification_generic(notification);
                                                                }

                                                                // Save to database if message was added
                                                                if was_added {
                                                                    // Save chat metadata + the single new message
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
                                                                // Populate reply context for old messages not in frontend cache
                                                                if !message.replied_to.is_empty() {
                                                                    let _ = db::populate_reply_context(&mut message).await;
                                                                }

                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                let is_file = true;

                                                                let (was_added, _active_typers, should_notify) = {
                                                                    let mut state = crate::STATE.lock().await;

                                                                    // Add message to chat
                                                                    let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                                    
                                                                    // Check if we should send notification (not muted, not mine)
                                                                    let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                        !chat.muted && !message.mine
                                                                    } else {
                                                                        false
                                                                    };
                                                                    
                                                                    // Clear typing indicator for sender
                                                                    let typers = state.update_typing_and_get_active(&group_id_for_persist, &sender_npub, 0);

                                                                    (added, typers, notify)
                                                                };

                                                                // Send OS notification for new group messages
                                                                if was_added && should_notify {
                                                                    // Get sender name and group name for notification
                                                                    let (sender_name, group_name) = {
                                                                        let state = crate::STATE.lock().await;

                                                                        let sender = if let Some(profile) = state.get_profile(&sender_npub) {
                                                                            if !profile.nickname.is_empty() {
                                                                                profile.nickname.to_string()
                                                                            } else if !profile.name.is_empty() {
                                                                                profile.name.to_string()
                                                                            } else {
                                                                                "Someone".to_string()
                                                                            }
                                                                        } else {
                                                                            "Someone".to_string()
                                                                        };

                                                                        let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                            chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                                        } else {
                                                                            "Group Chat".to_string()
                                                                        };

                                                                        (sender, group)
                                                                    };

                                                                    // Create appropriate notification (both text and files use group_message)
                                                                    let content = if is_file {
                                                                        let extension = message.attachments.first()
                                                                            .map(|att| att.extension.clone())
                                                                            .unwrap_or_else(|| String::from("file"));
                                                                        "Sent a ".to_string() + &get_file_type_description(&extension)
                                                                    } else {
                                                                        message.content.clone()
                                                                    };
                                                                    let notification = NotificationData::group_message(sender_name, group_name, content);

                                                                    show_notification_generic(notification);
                                                                }

                                                                // Save to database if message was added
                                                                if was_added {
                                                                    // Get chat and save it
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
                                                                // Handle reactions in real-time
                                                                let (was_added, chat_id_for_save) = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    // Use helper that handles interner access via split borrowing
                                                                    if let Some((chat_id, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
                                                                        (added, if added { Some(chat_id) } else { None })
                                                                    } else {
                                                                        (false, None)
                                                                    }
                                                                };
                                                                
                                                                // Save the updated message to database immediately (like DM reactions)
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

                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                                                // Handle typing indicators in real-time
                                                                let active_typers = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    state.update_typing_and_get_active(&group_id_for_persist, &profile_id, until)
                                                                };
                                                                
                                                                // Emit typing update event
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    let _ = handle.emit("typing-update", serde_json::json!({
                                                                        "conversation_id": group_id_for_persist,
                                                                        "typers": active_typers
                                                                    }));
                                                                }
                                                                
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::LeaveRequest { event_id, member_pubkey } => {
                                                                // Deduplicate by event ID - skip if already processed
                                                                if db::event_exists(&event_id).unwrap_or(false) {
                                                                    println!("[MLS] Live: Skipping duplicate leave request: {}", event_id);
                                                                    return None;
                                                                }

                                                                // A member is requesting to leave - if we're admin, auto-remove them
                                                                println!("[MLS] Live: Leave request received from {} in group {}", member_pubkey, group_id_for_persist);

                                                                // Get member's display name for the system event
                                                                let member_name = {
                                                                    let state = crate::STATE.lock().await;
                                                                    state.get_profile(&member_pubkey)
                                                                        .map(|p| {
                                                                            if !p.nickname.is_empty() { p.nickname.to_string() }
                                                                            else if !p.name.is_empty() { p.name.to_string() }
                                                                            else { member_pubkey.chars().take(12).collect::<String>() + "..." }
                                                                        })
                                                                };

                                                                // Check if we're admin for this group
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    let mls_svc = match MlsService::new_persistent(handle) {
                                                                        Ok(s) => s,
                                                                        Err(e) => {
                                                                            eprintln!("[MLS] Live: Failed to create MLS service: {}", e);
                                                                            return None;
                                                                        }
                                                                    };

                                                                    // Get my hex pubkey for comparison
                                                                    let my_hex = if let Some(&pk) = crate::MY_PUBLIC_KEY.get() {
                                                                        pk.to_hex()
                                                                    } else { String::new() };

                                                                    // Get group metadata to check admin status
                                                                    let am_i_admin = if let Ok(groups) = mls_svc.read_groups().await {
                                                                        if let Some(meta) = groups.iter().find(|g| g.group_id == group_id_for_persist) {
                                                                            println!("[MLS] Live: Found group metadata, creator={}", meta.creator_pubkey);
                                                                            // Compare with my pubkey (npub or hex)
                                                                            meta.creator_pubkey == my_npub_for_block || meta.creator_pubkey == my_hex
                                                                        } else {
                                                                            println!("[MLS] Live: Group metadata not found for {}", group_id_for_persist);
                                                                            false
                                                                        }
                                                                    } else {
                                                                        println!("[MLS] Live: Failed to read groups");
                                                                        false
                                                                    };

                                                                    println!("[MLS] Live: am_i_admin={}, my_npub={}, my_hex={}", am_i_admin, my_npub_for_block, my_hex);

                                                                    if am_i_admin {
                                                                        println!("[MLS] Live: I'm admin, auto-removing member: {}", member_pubkey);

                                                                        // Save system event - only emit if actually inserted (not duplicate)
                                                                        let was_inserted = db::save_system_event_by_id(
                                                                            &event_id,
                                                                            &group_id_for_persist,
                                                                            crate::db::SystemEventType::MemberLeft,
                                                                            &member_pubkey,
                                                                            member_name.as_deref(),
                                                                        ).await.unwrap_or(false);

                                                                        if was_inserted {
                                                                            // Emit event to frontend only if we saved it (not a duplicate)
                                                                            let _ = handle.emit("system_event", serde_json::json!({
                                                                                "conversation_id": group_id_for_persist,
                                                                                "event_id": event_id,
                                                                                "event_type": crate::db::SystemEventType::MemberLeft.as_u8(),
                                                                                "member_pubkey": member_pubkey,
                                                                                "member_name": member_name,
                                                                            }));

                                                                            // Remove the member
                                                                            if let Err(e) = mls_svc.remove_member_device(&group_id_for_persist, &member_pubkey, "").await {
                                                                                eprintln!("[MLS] Live: Failed to auto-remove member {}: {}", member_pubkey, e);
                                                                            } else {
                                                                                println!("[MLS] Live: Successfully removed member {} from group {}", member_pubkey, group_id_for_persist);
                                                                            }
                                                                        } else {
                                                                            println!("[MLS] Live: Skipping duplicate system event: {}", event_id);
                                                                        }
                                                                    } else {
                                                                        println!("[MLS] Live: Not admin, ignoring leave request from {}", member_pubkey);
                                                                    }
                                                                }

                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                                                                // Handle WebXDC peer advertisement - add peer to realtime channel
                                                                super::handle_webxdc_peer_advertisement(&topic_id, &node_addr).await;
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::UnknownEvent(mut event) => {
                                                                // Store unknown events for future compatibility
                                                                // Get chat_id and save the event
                                                                if let Ok(chat_id) = db::get_chat_id_by_identifier(&group_id_for_persist) {
                                                                    event.chat_id = chat_id;
                                                                    let _ = db::save_event(&event).await;
                                                                }
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::Ignored => None,
                                                            RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                                                // Save PIVX payment event and emit to frontend
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
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                                                // Skip if this edit event was already processed (deduplication)
                                                                if db::event_exists(&event.id).unwrap_or(false) {
                                                                    return None; // Already processed, skip
                                                                }

                                                                // Save edit event to database
                                                                if let Ok(chat_id) = db::get_chat_id_by_identifier(&group_id_for_persist) {
                                                                    let mut event_with_chat = event;
                                                                    event_with_chat.chat_id = chat_id;
                                                                    let _ = db::save_event(&event_with_chat).await;
                                                                }

                                                                // Update message in state and emit to frontend
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
                                                                None // Don't emit as message
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        eprintln!("[MLS][live] Failed to process rumor: {}", e);
                                                        None
                                                    }
                                                }
                                            });

                                            // EventTracker: Track as processed after successful handling
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());

                                            processed
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Commit { mls_group_id } => {
                                            // Commit processed - member list may have changed
                                            // Check if we're still a member of this group
                                            let my_pubkey_hex = my_npub_for_block.clone();
                                            
                                            // Only evict if we can POSITIVELY CONFIRM removal
                                            let membership_check = engine.get_members(&mls_group_id)
                                                .ok()
                                                .and_then(|members| {
                                                    nostr_sdk::PublicKey::from_bech32(&my_pubkey_hex)
                                                        .ok()
                                                        .map(|pk| members.contains(&pk))
                                                });
                                            
                                            match membership_check {
                                                Some(false) => {
                                                    // Successfully checked and confirmed NOT a member - evict!
                                                    eprintln!("[MLS] Eviction detected via Commit - group: {}", group_id_for_persist);
                                                    
                                                    // Perform full cleanup using the helper method
                                                    rt.block_on(async {
                                                        if let Err(e) = svc.cleanup_evicted_group(&group_id_for_persist).await {
                                                            eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
                                                        }
                                                    });
                                                }
                                                Some(true) => {
                                                    // Still a member, just update the UI
                                                    if let Some(handle) = TAURI_APP.get() {
                                                        handle.emit("mls_group_updated", serde_json::json!({
                                                            "group_id": group_id_for_persist
                                                        })).ok();
                                                    }
                                                }
                                                None => {
                                                    // Check failed - don't evict, just update UI
                                                    if let Some(handle) = TAURI_APP.get() {
                                                        handle.emit("mls_group_updated", serde_json::json!({
                                                            "group_id": group_id_for_persist
                                                        })).ok();
                                                    }
                                                }
                                            }
                                            // EventTracker: Track as processed
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Proposal(_update_result) => {
                                            // Proposal received (e.g., leave proposal)
                                            // Emit event to notify UI that group state may have changed
                                            if let Some(handle) = TAURI_APP.get() {
                                                handle.emit("mls_group_updated", serde_json::json!({
                                                    "group_id": group_id_for_persist
                                                })).ok();
                                            }
                                            // EventTracker: Track as processed
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::PendingProposal { .. } => {
                                            // Pending proposal - track as processed
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::IgnoredProposal { .. } => {
                                            // Ignored proposal - track as processed
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Unprocessable { mls_group_id: _ } => {
                                            // Unprocessable result - could be many reasons (out of order, can't decrypt, etc.)
                                            // Don't try to detect eviction here - wait for next message to trigger error-based detection
                                            // Note: We do NOT track Unprocessable events - they may succeed on retry
                                            None
                                        }
                                        // Other message types (ExternalJoinProposal) are not persisted as chat messages
                                        _ => {
                                            // EventTracker: Track as processed
                                            let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &group_id_for_persist, ev.created_at.as_secs());
                                            None
                                        }
                                    }
                                }
                                Err(e) => {
                                    let error_msg = e.to_string();
                                    
                                    // Check if this is an eviction error
                                    if error_msg.contains("evicted from it") ||
                                       error_msg.contains("after being evicted") ||
                                       error_msg.contains("own leaf not found") {
                                        eprintln!("[MLS] Eviction detected in live subscription - group: {}", group_id_for_persist);
                                        
                                        // Perform full cleanup using the helper method
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
                            // Emit UI event (no MLS operations here, just event emission)
                            if let Some(handle) = TAURI_APP.get() {
                                let _ = handle.emit("mls_message_new", serde_json::json!({
                                    "group_id": group_id_for_emit,
                                    "message": record
                                }));
                            }
                        }
                    }
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