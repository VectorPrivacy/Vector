//! Event handler — gift wrap receive, unwrap, process, commit pipeline.
//!
//! Two-phase architecture:
//! - **Phase 1** (`prepare_event`): Parallel-safe — dedup, unwrap, process_rumor
//! - **Phase 2** (`commit_prepared_event`): Sequential — save DB, update STATE, emit
//!
//! Platform-specific behavior (notifications, MLS) handled by `InboundEventHandler` trait.

use nostr_sdk::prelude::*;

use crate::rumor::{RumorProcessingResult, RumorEvent, RumorContext, ConversationType, process_rumor};
use crate::types::Message;
use crate::state::WRAPPER_ID_CACHE;

/// Platform-specific callbacks for inbound event processing.
///
/// Same pattern as SendCallback/ProfileSyncHandler — trait with default no-ops.
/// Platforms implement only the hooks they need.
pub trait InboundEventHandler: Send + Sync {
    /// A DM text message was received and committed to STATE + DB.
    fn on_dm_received(&self, _chat_id: &str, _msg: &Message, _is_new: bool) {}

    /// A DM file attachment was received and committed to STATE + DB.
    fn on_file_received(&self, _chat_id: &str, _msg: &Message, _is_new: bool) {}

    /// A reaction was received and applied to a message.
    fn on_reaction_received(&self, _chat_id: &str, _msg: &Message) {}

    /// An MLS Welcome event — platform handles group join flow.
    fn on_mls_welcome(&self, _event: &Event, _rumor: &UnsignedEvent, _sender: &PublicKey, _contact: &str, _is_mine: bool, _is_new: bool) {}

    /// An MLS group message — platform handles decryption via MDK.
    fn on_mls_group_message(&self, _event: &Event) {}
}

/// No-op handler for CLI/tests.
pub struct NoOpEventHandler;
impl InboundEventHandler for NoOpEventHandler {}

/// Result of Phase 1 (prepare_event) — everything needed for sequential commit.
pub enum PreparedEvent {
    /// Fully processed DM rumor — ready for state commit.
    Processed {
        result: RumorProcessingResult,
        contact: String,
        sender: PublicKey,
        is_mine: bool,
        wrapper_event_id: String,
        wrapper_event_id_bytes: [u8; 32],
        wrapper_created_at: u64,
        /// Time spent on ECDH + ChaCha20Poly1305 decryption (nanoseconds)
        unwrap_ns: u64,
        /// Time spent on rumor parsing (nanoseconds)
        parse_ns: u64,
    },
    /// MLS Welcome — platform handles group join.
    MlsWelcome {
        event: Event,
        rumor: UnsignedEvent,
        contact: String,
        sender: PublicKey,
        is_mine: bool,
        wrapper_event_id: String,
        wrapper_event_id_bytes: [u8; 32],
        wrapper_created_at: u64,
        unwrap_ns: u64,
    },
    /// Duplicate event — just persist wrapper for negentropy.
    DedupSkip {
        wrapper_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
    /// Error during unwrap/processing — persist wrapper for negentropy.
    ErrorSkip {
        wrapper_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
}

/// Phase 1: Prepare an event for commit (parallel-safe, no state mutation).
///
/// Performs dedup check, gift wrap decryption, and rumor parsing.
/// Safe to call from multiple tokio tasks concurrently.
pub async fn prepare_event(
    event: Event,
    client: &Client,
    my_public_key: PublicKey,
) -> PreparedEvent {
    let wrapper_created_at = event.created_at.as_secs();
    let wrapper_event_id_bytes: [u8; 32] = event.id.to_bytes();
    let wrapper_event_id = event.id.to_hex();

    // Dedup: in-memory cache first, then DB fallback
    {
        let cache = WRAPPER_ID_CACHE.lock().await;
        if cache.contains(&wrapper_event_id_bytes) {
            return PreparedEvent::DedupSkip { wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at };
        }
    }

    if let Ok(true) = crate::db::events::wrapper_event_exists(&wrapper_event_id) {
        return PreparedEvent::DedupSkip { wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at };
    }

    // Unwrap gift wrap (CPU-bound ECDH + ChaCha20Poly1305)
    let unwrap_start = std::time::Instant::now();
    let (rumor, sender) = match client.unwrap_gift_wrap(&event).await {
        Ok(UnwrappedGift { rumor, sender }) => (rumor, sender),
        Err(_) => return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        },
    };

    let unwrap_ns = unwrap_start.elapsed().as_nanos() as u64;

    let is_mine = sender == my_public_key;
    let contact = if is_mine {
        rumor.tags.public_keys().next()
            .and_then(|pk| pk.to_bech32().ok())
            .unwrap_or_else(|| sender.to_bech32().unwrap_or_default())
    } else {
        sender.to_bech32().unwrap_or_default()
    };

    // Skip NIP-17 group messages (multiple p-tags) — Vector uses MLS
    if rumor.tags.public_keys().count() > 1 {
        return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        };
    }

    // MLS Welcome — defer to platform
    if rumor.kind == Kind::MlsWelcome {
        return PreparedEvent::MlsWelcome {
            event, rumor, contact, sender, is_mine,
            wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at, unwrap_ns,
        };
    }

    // Build RumorEvent for processing
    let Some(rumor_id) = rumor.id else {
        return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        };
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

    let parse_start = std::time::Instant::now();
    let download_dir = crate::db::get_download_dir();
    match process_rumor(rumor_event, rumor_context, &download_dir) {
        Ok(result) => {
            let parse_ns = parse_start.elapsed().as_nanos() as u64;
            PreparedEvent::Processed {
                result, contact, sender, is_mine,
                wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at,
                unwrap_ns, parse_ns,
            }
        }
        Err(e) => {
            log_warn!("[EventHandler] Failed to process rumor: {}", e);
            PreparedEvent::ErrorSkip {
                wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
            }
        }
    }
}

// ============================================================================
// Phase 2: Commit — sequential state mutation, DB save, emit
// ============================================================================

/// Phase 2: Commit a prepared event (sequential — not parallel-safe).
///
/// Saves to DB, updates STATE, emits to frontend, calls handler hooks.
/// Returns true if a new displayable message was committed.
pub async fn commit_prepared_event(
    prepared: PreparedEvent,
    is_new: bool,
    handler: &dyn InboundEventHandler,
) -> bool {
    match prepared {
        PreparedEvent::Processed { result, contact, sender, is_mine, wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at, .. } => {
            // Cache wrapper for session dedup
            {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                cache.insert(wrapper_event_id_bytes);
            }
            // Persist for cross-session dedup + negentropy
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_event_id_bytes, wrapper_created_at);

            // Blocked check — drop content from blocked contacts (wrapper still persisted for negentropy)
            if !is_mine {
                let state = crate::state::STATE.lock().await;
                if state.get_profile(&contact).map_or(false, |p| p.flags.is_blocked()) {
                    return false;
                }
            }

            match result {
                RumorProcessingResult::TextMessage(mut msg) => {
                    msg.wrapper_event_id = Some(wrapper_event_id.clone());
                    commit_dm_message(msg, &contact, is_mine, is_new, &wrapper_event_id, wrapper_event_id_bytes, handler, false).await
                }
                RumorProcessingResult::FileAttachment(mut msg) => {
                    msg.wrapper_event_id = Some(wrapper_event_id.clone());
                    commit_dm_message(msg, &contact, is_mine, is_new, &wrapper_event_id, wrapper_event_id_bytes, handler, true).await
                }
                RumorProcessingResult::Reaction(reaction) => {
                    commit_reaction(reaction, &contact, is_mine, &wrapper_event_id, handler).await
                }
                RumorProcessingResult::Edit { message_id, new_content, edited_at, mut event } => {
                    commit_edit(&mut event, &contact, &message_id, &new_content, edited_at, &wrapper_event_id).await
                }
                RumorProcessingResult::TypingIndicator { profile_id, until } => {
                    let active_typers = {
                        let mut state = crate::state::STATE.lock().await;
                        state.update_typing_and_get_active(&contact, &profile_id, until)
                    };
                    crate::traits::emit_event("typing-update", &serde_json::json!({
                        "conversation_id": contact,
                        "typers": active_typers,
                    }));
                    false
                }
                RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, mut event } => {
                    if crate::db::events::event_exists(&event.id).unwrap_or(false) {
                        return false;
                    }
                    event.wrapper_event_id = Some(wrapper_event_id.clone());
                    let ts = event.created_at;
                    let _ = crate::db::events::save_pivx_payment_event(&contact, event).await;
                    crate::traits::emit_event("pivx_payment_received", &serde_json::json!({
                        "conversation_id": contact,
                        "gift_code": gift_code, "amount_piv": amount_piv,
                        "address": address, "message_id": message_id,
                        "sender": sender.to_hex(), "is_mine": is_mine,
                        "at": ts * 1000,
                    }));
                    true
                }
                RumorProcessingResult::UnknownEvent(mut event) => {
                    event.wrapper_event_id = Some(wrapper_event_id.clone());
                    // Store unknown events for forward compatibility
                    if let Ok(chat_id) = crate::db::id_cache::get_or_create_chat_id(&contact) {
                        event.chat_id = chat_id;
                    }
                    let _ = crate::db::events::save_event(&event).await;
                    false
                }
                RumorProcessingResult::LeaveRequest { .. } => false,
                RumorProcessingResult::WebxdcPeerAdvertisement { .. } |
                RumorProcessingResult::WebxdcPeerLeft { .. } => {
                    // WebXDC is platform-specific — handled by src-tauri directly
                    false
                }
                RumorProcessingResult::Ignored => false,
            }
        }
        PreparedEvent::MlsWelcome { event, rumor, contact, sender, is_mine, wrapper_event_id_bytes, wrapper_created_at, .. } => {
            // Dedup: same welcome can arrive from multiple relays simultaneously.
            // Check-and-insert atomically (single lock scope) to close the race window.
            {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                if cache.contains(&wrapper_event_id_bytes) {
                    return false;
                }
                cache.insert(wrapper_event_id_bytes);
            }
            // MLS Welcome — delegate to platform handler
            handler.on_mls_welcome(&event, &rumor, &sender, &contact, is_mine, is_new);
            // Persist wrapper regardless of outcome
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_event_id_bytes, wrapper_created_at);
            false
        }
        PreparedEvent::DedupSkip { wrapper_id_bytes, wrapper_created_at } => {
            // Persist wrapper timestamp for negentropy backfill (skip no-op writes)
            if wrapper_created_at > 0 {
                let _ = crate::db::wrappers::update_wrapper_timestamp(&wrapper_id_bytes, wrapper_created_at);
            }
            false
        }
        PreparedEvent::ErrorSkip { wrapper_id_bytes, wrapper_created_at } => {
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_id_bytes, wrapper_created_at);
            false
        }
    }
}

/// Commit a DM text or file message (shared logic for both).
async fn commit_dm_message(
    mut msg: Message,
    contact: &str,
    _is_mine: bool,
    is_new: bool,
    wrapper_event_id: &str,
    wrapper_event_id_bytes: [u8; 32],
    handler: &dyn InboundEventHandler,
    is_file: bool,
) -> bool {
    // Dedup: check if message already in DB
    if let Ok(true) = crate::db::events::message_exists_in_db(&msg.id) {
        // Already in DB — try to backfill wrapper_event_id
        if let Ok(updated) = crate::db::events::update_wrapper_event_id(&msg.id, wrapper_event_id) {
            if !updated {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                cache.insert(wrapper_event_id_bytes);
            }
        }
        return false;
    }

    // Populate reply context
    if !msg.replied_to.is_empty() {
        let _ = crate::db::events::populate_reply_context(&mut msg).await;
    }

    // Add to STATE (+ clear typing indicator for file senders)
    let added = {
        let mut state = crate::state::STATE.lock().await;
        let added = state.add_message_to_participant(contact, msg.clone());
        if is_file && added {
            state.update_typing_and_get_active(contact, contact, 0);
        }
        added
    };

    if added {
        // Emit to frontend
        crate::traits::emit_event("message_new", &serde_json::json!({
            "message": &msg,
            "chat_id": contact
        }));

        // Platform callback (notifications, badge, etc.)
        if is_file {
            handler.on_file_received(contact, &msg, is_new);
        } else {
            handler.on_dm_received(contact, &msg, is_new);
        }

        // Save to DB
        let _ = crate::db::events::save_message(contact, &msg).await;
    }

    added
}

/// Commit a reaction event.
async fn commit_reaction(
    reaction: crate::types::Reaction,
    contact: &str,
    is_mine: bool,
    wrapper_event_id: &str,
    handler: &dyn InboundEventHandler,
) -> bool {
    // Add to STATE
    let msg_for_emit = {
        let mut state = crate::state::STATE.lock().await;
        if let Some((chat_id, was_added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
            if was_added {
                state.find_message(&reaction.reference_id)
                    .map(|(_, msg)| (chat_id, msg))
            } else { None }
        } else { None }
    };

    if let Some((chat_id, msg)) = msg_for_emit {
        crate::traits::emit_event("message_update", &serde_json::json!({
            "old_id": &reaction.reference_id,
            "message": &msg,
            "chat_id": &chat_id
        }));
        let _ = crate::db::events::save_message(&chat_id, &msg).await;
        handler.on_reaction_received(&chat_id, &msg);
    }

    // Always save reaction event with wrapper for dedup
    if let Ok(chat_id) = crate::db::id_cache::get_chat_id_by_identifier(contact) {
        let _ = crate::db::events::save_reaction_event(
            &reaction, chat_id, None, is_mine, Some(wrapper_event_id.to_string())
        ).await;
    }

    true
}

/// Commit a message edit.
async fn commit_edit(
    event: &mut crate::stored_event::StoredEvent,
    contact: &str,
    message_id: &str,
    new_content: &str,
    edited_at: u64,
    wrapper_event_id: &str,
) -> bool {
    if crate::db::events::event_exists(&event.id).unwrap_or(false) {
        return false;
    }
    if let Ok(chat_id) = crate::db::id_cache::get_chat_id_by_identifier(contact) {
        event.chat_id = chat_id;
    }
    event.wrapper_event_id = Some(wrapper_event_id.to_string());
    let _ = crate::db::events::save_event(event).await;

    let msg_for_emit = {
        let mut state = crate::state::STATE.lock().await;
        state.update_message_in_chat(contact, message_id, |msg| {
            msg.apply_edit(new_content.to_string(), edited_at);
        })
    };
    if let Some(msg) = msg_for_emit {
        crate::traits::emit_event("message_update", &serde_json::json!({
            "old_id": message_id,
            "message": msg,
            "chat_id": contact
        }));
    }
    true
}

// ============================================================================
// Convenience: single-call event processing
// ============================================================================

/// Process a single event through the full pipeline (prepare + commit).
///
/// Gets client and public key from globals. For callers that manage their
/// own notification loop but want the full vector-core processing pipeline.
pub async fn process_event(
    event: Event,
    is_new: bool,
    handler: &dyn InboundEventHandler,
) -> std::result::Result<bool, String> {
    let client = crate::state::NOSTR_CLIENT.get()
        .ok_or_else(|| "Nostr client not initialized".to_string())?;
    let my_pk = crate::state::MY_PUBLIC_KEY.get()
        .copied()
        .ok_or_else(|| "Public key not initialized".to_string())?;
    let prepared = prepare_event(event, client, my_pk).await;
    Ok(commit_prepared_event(prepared, is_new, handler).await)
}
