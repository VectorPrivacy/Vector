//! Event handler service for processing incoming Nostr events.
//!
//! This module handles:
//! - Gift-wrapped DM/file message processing
//! - MLS Welcome processing
//! - Reactions, typing indicators, edits
//! - WebXDC peer advertisements
//! - Unknown event storage for future compatibility

use log::warn;
use nostr_sdk::prelude::*;
use tauri::{Emitter, Manager};

use crate::{
    db, miniapps, commands,
    Message, Reaction, StoredEvent,
    RumorEvent, RumorContext, ConversationType, RumorProcessingResult, process_rumor,
    MlsService, NotificationData, show_notification_generic,
    STATE, TAURI_APP, NOSTR_CLIENT, WRAPPER_ID_CACHE, NOTIFIED_WELCOMES, SyncMode,
    util::get_file_type_description,
    state::{is_processing_allowed, PENDING_EVENTS},
};

// Internal event handler - called by fetch_messages and real-time event stream
// Not exposed as a Tauri command to frontend
pub(crate) async fn handle_event(event: Event, is_new: bool) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let my_public_key = *crate::MY_PUBLIC_KEY.get().expect("Public key not initialized");
    handle_event_with_context(event, is_new, client, my_public_key).await
}

/// Full event processing implementation — accepts dependencies as parameters.
/// This enables headless (background service) callers to provide their own client/key
/// without relying on globals that may not be initialized.
pub(crate) async fn handle_event_with_context(
    event: Event,
    is_new: bool,
    client: &Client,
    my_public_key: PublicKey,
) -> bool {
    // Check processing gate - queue events during encryption migration
    if !is_processing_allowed() {
        let mut queue = PENDING_EVENTS.lock().await;
        // Re-check after acquiring lock: drain_pending_events opens the gate
        // INSIDE this same lock, so if the gate is now open, drain already
        // finished and we should process normally instead of queuing (TOCTOU fix).
        if !is_processing_allowed() {
            queue.push((event, is_new));
            return false;
        }
        drop(queue);
    }

    // Get the wrapper (giftwrap) event ID for duplicate detection
    // Use bytes for cache (memory efficient), hex string for DB operations
    let wrapper_event_id_bytes: [u8; 32] = event.id.to_bytes();
    let wrapper_event_id = event.id.to_hex();

    // For historical sync events (is_new = false), use the wrapper_id cache for fast duplicate detection
    // For real-time new events (is_new = true), skip cache checks - they're guaranteed to be new
    if !is_new {
        // Check in-memory cache first (O(1) lookup, no SQL overhead)
        // This cache is only populated during init and cleared after sync finishes
        {
            let cache = WRAPPER_ID_CACHE.lock().await;
            if cache.contains(&wrapper_event_id_bytes) {
                // Already processed this giftwrap, skip (cache hit)
                return false;
            }
        }

        // Cache miss - check database as fallback (for events older than cache window)
        if let Ok(exists) = db::wrapper_event_exists(&wrapper_event_id).await {
            if exists {
                // Already processed this giftwrap, skip (DB hit)
                return false;
            }
        }
    }

    // Unwrap the gift wrap
    match client.unwrap_gift_wrap(&event).await {
        Ok(UnwrappedGift { rumor, sender }) => {
            // Check if it's mine
            let is_mine = sender == my_public_key;

            // Attempt to get contact public key (bech32)
            let contact: String = if is_mine {
                // Try to get the first public key from tags
                match rumor.tags.public_keys().next() {
                    Some(pub_key) => pub_key.to_bech32()
                        .or_else(|_| sender.to_bech32())
                        .unwrap_or_default(),
                    None => sender.to_bech32().unwrap_or_default(),
                }
            } else {
                sender.to_bech32().unwrap_or_default()
            };

            // Special handling for MLS Welcomes (not processed by rumor processor)
            if rumor.kind == Kind::MlsWelcome {
                // Dedup: the same welcome can arrive from multiple relays simultaneously
                {
                    let cache = WRAPPER_ID_CACHE.lock().await;
                    if cache.contains(&wrapper_event_id_bytes) {
                        return false;
                    }
                }

                // Convert rumor Event -> UnsignedEvent
                let unsigned_opt = serde_json::to_string(&rumor)
                    .ok()
                    .and_then(|s| nostr_sdk::UnsignedEvent::from_json(s.as_bytes()).ok());

                if let Some(unsigned) = unsigned_opt {
                    // Outer giftwrap id is our wrapper id for dedup/logs
                    let wrapper_id = event.id;

                    // Use blocking thread for non-Send MLS engine.
                    // Returns the group name on success (for notification).
                    let welcome_result: Option<String> = tokio::task::spawn_blocking(move || {
                        let svc = MlsService::new_persistent_static();
                        if let Ok(mls) = svc {
                            if let Ok(engine) = mls.engine() {
                                match engine.process_welcome(&wrapper_id, &unsigned) {
                                    Ok(_) => {
                                        // Read back the welcome to get the group name
                                        if let Ok(welcomes) = engine.get_pending_welcomes(None) {
                                            let group_name = welcomes.iter()
                                                .find(|w| w.wrapper_event_id == wrapper_id)
                                                .map(|w| w.group_name.clone())
                                                .unwrap_or_default();
                                            return Some(group_name);
                                        }
                                        return Some(String::new());
                                    }
                                    Err(e) => {
                                        eprintln!("[MLS] Failed to process welcome: {}", e);
                                    }
                                }
                            }
                        }
                        None
                    })
                    .await
                    .unwrap_or(None);

                    if let Some(group_name) = welcome_result {
                        // Mark this wrapper as processed to prevent duplicates from other relays
                        {
                            let mut cache = WRAPPER_ID_CACHE.lock().await;
                            cache.insert(wrapper_event_id_bytes);
                        }
                        // Only notify UI after initial sync is complete
                        // During initial sync, invites are processed but not emitted to avoid UI updates before chats are loaded
                        let should_emit = {
                            let state = STATE.lock().await;
                            state.sync_mode == SyncMode::Finished || !state.is_syncing
                        };

                        if should_emit {
                            if let Some(app) = TAURI_APP.get() {
                                let _ = app.emit("mls_invite_received", serde_json::json!({
                                    "wrapper_event_id": wrapper_id.to_hex()
                                }));
                            }

                            // OS notification for group invites
                            if !is_mine && is_new {
                                let display_info = {
                                    let state = STATE.lock().await;
                                    match state.get_profile(&contact) {
                                        Some(profile) => {
                                            let name = if !profile.nickname.is_empty() {
                                                profile.nickname.to_string()
                                            } else if !profile.name.is_empty() {
                                                profile.name.to_string()
                                            } else {
                                                String::from("Someone")
                                            };
                                            let avatar = if !profile.avatar_cached.is_empty() {
                                                Some(profile.avatar_cached.to_string())
                                            } else {
                                                None
                                            };
                                            (name, avatar)
                                        }
                                        None => (String::from("Someone"), None),
                                    }
                                };
                                let notif_group_name = if group_name.is_empty() {
                                    String::from("Group Chat")
                                } else {
                                    group_name.clone()
                                };
                                let notification = NotificationData::group_invite(
                                    notif_group_name,
                                    display_info.0,
                                    display_info.1,
                                );
                                show_notification_generic(notification);

                                // Mark as notified to prevent list_pending_mls_welcomes from double-notifying
                                let mut notified = NOTIFIED_WELCOMES.lock().await;
                                notified.insert(wrapper_event_id.clone());
                            }
                        }
                        return true;
                    } else {
                        return false;
                    }
                } else {
                    eprintln!("[MLS] Failed to convert rumor to UnsignedEvent");
                    return false;
                }
            }

            // Convert rumor to RumorEvent for protocol-agnostic processing
            // Move content and tags instead of cloning (rumor is owned and not used after this)
            let Some(rumor_id) = rumor.id else {
                eprintln!("Unwrapped rumor missing event ID, skipping");
                return false;
            };
            let rumor_event = RumorEvent {
                id: rumor_id,
                kind: rumor.kind,
                content: rumor.content,
                tags: rumor.tags,
                created_at: rumor.created_at,
                pubkey: rumor.pubkey,
            };

            let rumor_context = RumorContext {
                sender,
                is_mine,
                conversation_id: contact.clone(),
                conversation_type: ConversationType::DirectMessage,
            };

            // Process the rumor using our protocol-agnostic processor
            match process_rumor(rumor_event, rumor_context).await {
                Ok(result) => {
                    match result {
                        RumorProcessingResult::TextMessage(mut msg) => {
                            // Set the wrapper event ID for database storage
                            msg.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_text_message(msg, &contact, is_mine, is_new, &wrapper_event_id, wrapper_event_id_bytes).await
                        }
                        RumorProcessingResult::FileAttachment(mut msg) => {
                            // Set the wrapper event ID for database storage
                            msg.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_file_attachment(msg, &contact, is_mine, is_new, &wrapper_event_id, wrapper_event_id_bytes).await
                        }
                        RumorProcessingResult::Reaction(reaction) => {
                            handle_reaction(reaction, &contact).await
                        }
                        RumorProcessingResult::TypingIndicator { profile_id, until } => {
                            // Update the chat's typing participants
                            let active_typers = {
                                let mut state = STATE.lock().await;
                                state.update_typing_and_get_active(&contact, &profile_id, until)
                            };
                            
                            // Emit typing update event to frontend
                            if let Some(handle) = TAURI_APP.get() {
                                let _ = handle.emit("typing-update", serde_json::json!({
                                    "conversation_id": contact,
                                    "typers": active_typers,
                                }));
                            }
                            
                            true
                        }
                        RumorProcessingResult::LeaveRequest { .. } => {
                            // Leave requests only apply to MLS groups, not DMs
                            true
                        }
                        RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                            // Handle WebXDC peer advertisement - add peer to realtime channel
                            handle_webxdc_peer_advertisement(&topic_id, &node_addr).await
                        }
                        RumorProcessingResult::UnknownEvent(mut event) => {
                            // Store unknown events for future compatibility
                            event.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_unknown_event(event, &contact).await
                        }
                        RumorProcessingResult::Ignored => false,
                        RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                            // Save PIVX payment event to database
                            let event_timestamp = event.created_at;
                            let _ = db::save_pivx_payment_event(&contact, event).await;

                            // Emit PIVX payment event to frontend for DMs
                            if let Some(handle) = TAURI_APP.get() {
                                let _ = handle.emit("pivx_payment_received", serde_json::json!({
                                    "conversation_id": contact,
                                    "gift_code": gift_code,
                                    "amount_piv": amount_piv,
                                    "address": address,
                                    "message_id": message_id,
                                    "sender": sender,
                                    "is_mine": is_mine,
                                    "at": event_timestamp * 1000,
                                }));
                            }
                            true
                        }
                        RumorProcessingResult::Edit { message_id, new_content, edited_at, mut event } => {
                            // Skip if this edit event was already processed (deduplication)
                            if db::event_exists(&event.id).unwrap_or(false) {
                                return true; // Already processed, skip
                            }

                            // Save edit event to database with proper chat_id
                            if let Ok(chat_id) = db::get_chat_id_by_identifier(&contact) {
                                event.chat_id = chat_id;
                            }
                            event.wrapper_event_id = Some(wrapper_event_id.clone());
                            let _ = db::save_event(&event).await;

                            // Update message in state and emit to frontend
                            let msg_for_emit = {
                                let mut state = STATE.lock().await;
                                state.update_message_in_chat(&contact, &message_id, |msg| {
                                    msg.apply_edit(new_content, edited_at);
                                })
                            };

                            if let Some(msg) = msg_for_emit {
                                if let Some(handle) = TAURI_APP.get() {
                                    let _ = handle.emit("message_update", serde_json::json!({
                                        "old_id": &message_id,
                                        "message": msg,
                                        "chat_id": &contact
                                    }));
                                }
                            }
                            true
                        }
                    }
                }
                Err(e) => {
                    warn!("[EventHandler] Failed to process rumor: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            warn!("[EventHandler] Failed to unwrap gift wrap: {:?}", e);
            false
        }
    }
}

/// Handle a processed text message
async fn handle_text_message(mut msg: Message, contact: &str, is_mine: bool, is_new: bool, wrapper_event_id: &str, wrapper_event_id_bytes: [u8; 32]) -> bool {
    // Check if message already exists in database (important for sync with partial message loading)
    if let Ok(exists) = db::message_exists_in_db(&msg.id).await {
        if exists {
            // Message already in DB but we got here (wrapper check passed)
            // Try to backfill the wrapper_event_id for future fast lookups
            // If backfill fails (message already has a different wrapper), add this wrapper to cache
            // to prevent repeated processing of duplicate giftwraps
            if let Ok(updated) = db::update_wrapper_event_id(&msg.id, wrapper_event_id).await {
                if !updated {
                    // Message has a different wrapper_id - add this duplicate wrapper to cache
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    cache.insert(wrapper_event_id_bytes);
                }
            }
            return false;
        }
    }

    // Populate reply context before emitting (for replies to old messages not in frontend cache)
    if !msg.replied_to.is_empty() {
        let _ = db::populate_reply_context(&mut msg).await;
    }

    // Add the message to the state and handle database save in one operation to avoid multiple locks
    let was_msg_added_to_state = {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(contact, msg.clone())
    };
    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Send OS notification for incoming messages (only after confirming message is new)
        if !is_mine && is_new {
            let display_info = {
                let state = STATE.lock().await;
                // Check chat-level mute (covers both DM and group mutes)
                let chat_muted = state.get_chat(contact)
                    .map_or(false, |c| c.muted);
                if chat_muted {
                    None
                } else {
                    match state.get_profile(contact) {
                        Some(profile) => {
                            let display_name = if !profile.nickname.is_empty() {
                                profile.nickname.to_string()
                            } else if !profile.name.is_empty() {
                                profile.name.to_string()
                            } else {
                                String::from("New Message")
                            };
                            let avatar = if !profile.avatar_cached.is_empty() {
                                Some(profile.avatar_cached.to_string())
                            } else {
                                None
                            };
                            Some((display_name, msg.content.clone(), avatar))
                        }
                        None => Some((String::from("New Message"), msg.content.clone(), None)),
                    }
                }
            };
            if let Some((display_name, content, avatar)) = display_info {
                let notification = NotificationData::direct_message(display_name, content, avatar, contact.to_string());
                show_notification_generic(notification);
            }
        }

        // Save the new message to DB (chat_id = contact npub for DMs)
        let _ = db::save_message(contact, &msg).await;
        // Ensure OS badge is updated immediately after accepting the message
        if let Some(handle) = TAURI_APP.get() {
            let _ = commands::messaging::update_unread_counter(handle.clone()).await;
        }
    }

    was_msg_added_to_state
}

/// Handle a processed file attachment
async fn handle_file_attachment(mut msg: Message, contact: &str, is_mine: bool, is_new: bool, wrapper_event_id: &str, wrapper_event_id_bytes: [u8; 32]) -> bool {
    // Check if message already exists in database (important for sync with partial message loading)
    if let Ok(exists) = db::message_exists_in_db(&msg.id).await {
        if exists {
            // Message already in DB but we got here (wrapper check passed)
            // Try to backfill the wrapper_event_id for future fast lookups
            // If backfill fails (message already has a different wrapper), add this wrapper to cache
            // to prevent repeated processing of duplicate giftwraps
            if let Ok(updated) = db::update_wrapper_event_id(&msg.id, wrapper_event_id).await {
                if !updated {
                    // Message has a different wrapper_id - add this duplicate wrapper to cache
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    cache.insert(wrapper_event_id_bytes);
                }
            }
            return false;
        }
    }

    // Populate reply context before emitting (for replies to old messages not in frontend cache)
    if !msg.replied_to.is_empty() {
        let _ = db::populate_reply_context(&mut msg).await;
    }

    // Get file extension for notification
    let extension = msg.attachments.first()
        .map(|att| att.extension.clone())
        .unwrap_or_else(|| String::from("file"));

    // Add the message to the state and clear typing indicator for sender
    let (was_msg_added_to_state, _active_typers) = {
        let mut state = STATE.lock().await;
        let added = state.add_message_to_participant(contact, msg.clone());

        // Clear typing indicator for the sender (they just sent a message)
        let typers = state.update_typing_and_get_active(contact, contact, 0);

        (added, typers)
    };

    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Send OS notification for incoming files (only after confirming message is new)
        if !is_mine && is_new {
            let display_info = {
                let state = STATE.lock().await;
                // Check chat-level mute (covers both DM and group mutes)
                let chat_muted = state.get_chat(contact)
                    .map_or(false, |c| c.muted);
                if chat_muted {
                    None
                } else {
                    match state.get_profile(contact) {
                        Some(profile) => {
                            let display_name = if !profile.nickname.is_empty() {
                                profile.nickname.to_string()
                            } else if !profile.name.is_empty() {
                                profile.name.to_string()
                            } else {
                                String::from("New Message")
                            };
                            let avatar = if !profile.avatar_cached.is_empty() {
                                Some(profile.avatar_cached.to_string())
                            } else {
                                None
                            };
                            Some((display_name, extension.clone(), avatar))
                        }
                        None => Some((String::from("New Message"), extension.clone(), None)),
                    }
                }
            };
            if let Some((display_name, file_extension, avatar)) = display_info {
                let file_description = "Sent a ".to_string() + &get_file_type_description(&file_extension);
                let notification = NotificationData::direct_message(display_name, file_description, avatar, contact.to_string());
                show_notification_generic(notification);
            }
        }

        // Save the new message to DB (chat_id = contact npub for DMs)
        let _ = db::save_message(contact, &msg).await;
        // Ensure OS badge is updated immediately after accepting the attachment
        if let Some(handle) = TAURI_APP.get() {
            let _ = commands::messaging::update_unread_counter(handle.clone()).await;
        }
    }

    was_msg_added_to_state
}

/// Handle a processed reaction
async fn handle_reaction(reaction: Reaction, _contact: &str) -> bool {
    // Find the chat containing the referenced message and add the reaction
    // Use a single lock scope to avoid nested locks
    let (reaction_added, chat_id_for_save) = {
        let mut state = STATE.lock().await;
        // Use helper that handles interner access via split borrowing
        if let Some((chat_id, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
            (added, if added { Some(chat_id) } else { None })
        } else {
            // Message not found in any chat - this can happen during sync
            // TODO: track these "ahead" reactions and re-apply them once sync has finished
            (false, None)
        }
    };

    // Save the updated message with the new reaction to our DB (outside of state lock)
    if let Some(chat_id) = chat_id_for_save {
        // Get only the message that was updated
        let updated_message = {
            let state = STATE.lock().await;
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

    reaction_added
}

/// Handle an unknown event type - store for future compatibility
async fn handle_unknown_event(mut event: StoredEvent, contact: &str) -> bool {
    // Get the chat_id for this contact
    match db::get_chat_id_by_identifier(contact) {
        Ok(chat_id) => {
            event.chat_id = chat_id;
            // Save the event to the database
            if let Err(e) = db::save_event(&event).await {
                eprintln!("Failed to save unknown event: {}", e);
                return false;
            }
            // Emit event to frontend (it can render as "Unknown Event" placeholder)
            if let Some(handle) = TAURI_APP.get() {
                let _ = handle.emit("event_new", serde_json::json!({
                    "event": event,
                    "chat_id": contact
                }));
            }
            true
        }
        Err(_) => {
            // Chat doesn't exist yet, skip this event
            eprintln!("Cannot save unknown event: chat not found for {}", contact);
            false
        }
    }
}

/// Handle a WebXDC peer advertisement - add the peer to our realtime channel
pub(crate) async fn handle_webxdc_peer_advertisement(topic_id: &str, node_addr_encoded: &str) -> bool {
    use crate::miniapps::realtime::{decode_topic_id, decode_node_addr};
    
    println!("[WEBXDC] Received peer advertisement for topic {}", topic_id);
    
    // Decode the topic ID
    let topic = match decode_topic_id(topic_id) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Failed to decode topic ID in peer advertisement: {}", e);
            return false;
        }
    };
    
    // Decode the node address
    let node_addr = match decode_node_addr(node_addr_encoded) {
        Ok(addr) => addr,
        Err(e) => {
            log::warn!("Failed to decode node address in peer advertisement: {}", e);
            return false;
        }
    };
    
    // Get the MiniApps state and add the peer
    if let Some(handle) = TAURI_APP.get() {
        let state = handle.state::<miniapps::state::MiniAppsState>();
        
        // Check if we have an active realtime channel for this topic
        // We need to find any instance that has this topic active
        let has_channel = {
            let channels = state.realtime_channels.read().await;
            println!("[WEBXDC] Checking {} active channels for topic match", channels.len());
            for (label, ch) in channels.iter() {
                println!("[WEBXDC]   Channel '{}': topic={}, active={}",
                    label,
                    crate::miniapps::realtime::encode_topic_id(&ch.topic),
                    ch.active);
            }
            channels.values().any(|ch| ch.topic == topic && ch.active)
        };
        
        println!("[WEBXDC] has_channel for topic {}: {}", topic_id, has_channel);
        
        if has_channel {
            println!("[WEBXDC] Found active channel for topic {}, adding peer", topic_id);
            // Get the realtime manager and add the peer
            match state.realtime.get_or_init().await {
                Ok(iroh) => {
                    match iroh.add_peer(topic, node_addr.clone()).await {
                        Ok(_) => {
                            println!("[WEBXDC] Successfully added peer {} to realtime channel topic {}",
                                node_addr.node_id, topic_id);
                            return true;
                        }
                        Err(e) => {
                            println!("[WEBXDC] ERROR: Failed to add peer to realtime channel: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("[WEBXDC] ERROR: Failed to get realtime manager: {}", e);
                }
            }
        } else {
            // Store as pending peer - we'll add them when we join the channel
            println!("[WEBXDC] Storing pending peer for topic {} (no active channel yet)", topic_id);
            state.add_pending_peer(topic, node_addr).await;
            
            // Emit event to frontend so it can update the UI (show "Click to Join" and player count)
            let pending_count = state.get_pending_peer_count(&topic).await;
            if let Some(main_window) = handle.get_webview_window("main") {
                use tauri::Emitter;
                let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                    "topic": topic_id,
                    "peer_count": pending_count,
                    "is_active": false,
                    "has_pending_peers": true,
                }));
                println!("[WEBXDC] Emitted miniapp_realtime_status event: topic={}, pending_count={}", topic_id, pending_count);
            }
            
            return true;
        }
    }
    
    false
}