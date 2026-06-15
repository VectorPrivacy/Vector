//! Event handler — gift wrap receive, unwrap, process, commit pipeline.
//!
//! Two-phase architecture:
//! - **Phase 1** (`prepare_event`): Parallel-safe — dedup, unwrap, process_rumor
//! - **Phase 2** (`commit_prepared_event`): Sequential — save DB, update STATE, emit
//!
//! Platform-specific behavior (notifications) handled by `InboundEventHandler` trait.

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

    /// A previously-stored message was deleted by its sender (Layer 2
    /// cooperative hide via NIP-09 over NIP-17). Frontend drops the row.
    fn on_message_deleted(&self, _chat_id: &str, _message_id: &str) {}

    /// A Community invite was received over a gift wrap and the local user was
    /// joined (member-view Community persisted). Platform refreshes the Community
    /// subscription so messages start flowing, and surfaces the new Community in the UI.
    fn on_community_invite(&self, _community_id: &str) {}

    // --- Community realtime (Concord channel events; `chat_id` is the channel id hex) ---

    /// A new Community channel message was received, ingested into STATE, and persisted.
    fn on_community_message(&self, _chat_id: &str, _msg: &Message, _is_new: bool) {}

    /// A reaction or edit was applied to an existing Community message. `target_id` is the
    /// affected message; `msg` is the live-updated view.
    fn on_community_update(&self, _chat_id: &str, _target_id: &str, _msg: &Message) {}

    /// A Community message was removed (cooperative delete / moderation tombstone).
    fn on_community_removed(&self, _chat_id: &str, _target_id: &str) {}

    /// A join/leave presence announcement. `created_at` is the authenticated inner timestamp;
    /// `invited_by`/`invited_label` carry invite attribution when present.
    #[allow(clippy::too_many_arguments)]
    fn on_community_presence(
        &self,
        _chat_id: &str,
        _npub: &str,
        _joined: bool,
        _event_id: &str,
        _created_at: u64,
        _invited_by: Option<&str>,
        _invited_label: Option<&str>,
    ) {}

    /// A Community typing indicator (ephemeral). `until` is the unix-secs the typer stops being active.
    fn on_community_typing(&self, _chat_id: &str, _npub: &str, _until: u64) {}

    /// A WebXDC realtime peer signal. `node_addr` = `Some` advertises an Iroh node, `None` = peer-left.
    #[allow(clippy::too_many_arguments)]
    fn on_community_webxdc(
        &self,
        _chat_id: &str,
        _npub: &str,
        _topic_id: &str,
        _node_addr: Option<&str>,
        _event_id: &str,
        _created_at: u64,
    ) {}

    /// The local user was removed from a Community (kick / ban / a leave authored on another device).
    /// Local data is torn down (epoch keys retained); the platform surfaces it + refreshes subs.
    fn on_community_self_removed(&self, _community_id: &str) {}

    /// A Community's control plane was refreshed in realtime (banlist/roles/metadata/mode change,
    /// or a re-founding followed). The platform re-reads display state.
    fn on_community_refreshed(&self, _community_id: &str) {}
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
    /// Community invite bundle (kind 3304) — parked for explicit user consent.
    CommunityInvite {
        invite: crate::community::invite::CommunityInvite,
        /// Inviter's npub (bech32) — shown in the pending-invite UI.
        inviter: String,
        is_mine: bool,
        wrapper_event_id_bytes: [u8; 32],
        wrapper_created_at: u64,
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

    // Skip NIP-17 group messages (multiple p-tags) — Vector DMs are 1:1
    if rumor.tags.public_keys().count() > 1 {
        return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        };
    }

    // Community invite (carrier) — a join, not a chat message. Recognized before
    // process_rumor so it never lands as an UnknownEvent in the DM thread.
    if rumor.kind == Kind::Custom(crate::stored_event::event_kind::COMMUNITY_INVITE_BUNDLE) {
        return match crate::community::invite::parse_invite_rumor(rumor.kind, &rumor.content) {
            Some(invite) => PreparedEvent::CommunityInvite {
                invite, inviter: contact.clone(), is_mine, wrapper_event_id_bytes, wrapper_created_at,
            },
            None => PreparedEvent::ErrorSkip { wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at },
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

/// Phase 2: commit a prepared event (sequential — not parallel-safe).
/// Saves to DB, updates STATE, emits to frontend, calls handler hooks.
/// Returns true if a new displayable message was committed.
///
/// Session-safety: captures the generation at the first line. If a swap
/// occurred between `prepare_event()` and here (e.g. long-running
/// negentropy fetch queued events for commit), bail before any STATE /
/// DB write. Centralized so individual spawn sites (sync.rs fetch_messages,
/// archive task, sync_dms, subscription_handler) don't have to wrap.
pub async fn commit_prepared_event(
    prepared: PreparedEvent,
    is_new: bool,
    handler: &dyn InboundEventHandler,
) -> bool {
    let session = crate::state::SessionGuard::capture();
    if !session.is_valid() {
        return false;
    }
    match prepared {
        PreparedEvent::Processed { result, contact, sender, is_mine, wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at, .. } => {
            // Cache wrapper for session dedup
            {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                cache.insert(wrapper_event_id_bytes);
            }
            // Persist for cross-session dedup + negentropy
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_event_id_bytes, wrapper_created_at, crate::db::wrappers::TRANSPORT_NIP17);

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
                    // If the sender's client (e.g. 0xChat) didn't ship `size` in
                    // the imeta tag, probe the URL via Content-Length so the
                    // frontend's auto-download gate has accurate metadata to
                    // decide on.
                    //
                    // Skip for self-echoes (is_mine): we just uploaded these
                    // files, the local Attachment.size is authoritative, and
                    // probing our own blossom URL right after upload is a
                    // correlation-fingerprint privacy regression.
                    //
                    // Each probe is bounded by a 3s outer timeout so a slow or
                    // dead server can't stall the inbound rumor pipeline. If
                    // the probe times out, we ship size=0 and the frontend
                    // falls back to a manual "Click to Download" affordance.
                    if !is_mine {
                        for att in &mut msg.attachments {
                            if att.size == 0
                                && (att.url.starts_with("https://") || att.url.starts_with("http://"))
                            {
                                if let Ok(Some(size)) = tokio::time::timeout(
                                    std::time::Duration::from_secs(3),
                                    crate::net::get_remote_file_size(&att.url),
                                ).await {
                                    att.size = size;
                                }
                            }
                        }
                    }
                    commit_dm_message(msg, &contact, is_mine, is_new, &wrapper_event_id, wrapper_event_id_bytes, handler, true).await
                }
                RumorProcessingResult::Reaction(reaction) => {
                    commit_reaction(reaction, &contact, is_mine, &wrapper_event_id, handler).await
                }
                RumorProcessingResult::Edit { message_id, new_content, edited_at, emoji_tags, mut event } => {
                    commit_edit(&mut event, &contact, &message_id, &new_content, edited_at, emoji_tags, &wrapper_event_id).await
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
                RumorProcessingResult::WallpaperChanged {
                    sender_npub, created_at, url, decryption_key, decryption_nonce,
                    plaintext_hash, mime, blur, dim, event_id,
                } => {
                    let _ = crate::wallpaper::apply_received_wallpaper(
                        &contact, &sender_npub, created_at, &url,
                        &decryption_key, &decryption_nonce,
                        plaintext_hash.as_deref(), mime.as_deref(),
                        blur, dim,
                        &event_id,
                    ).await;
                    // System event is saved inside apply_received_wallpaper.
                    // Return true so the caller treats this as a stored event.
                    true
                }
                RumorProcessingResult::DeletionRequest { target_event_id } => {
                    commit_deletion(&target_event_id, &contact, &sender, handler).await
                }
                RumorProcessingResult::Ignored => false,
            }
        }
        PreparedEvent::CommunityInvite { invite, inviter, is_mine, wrapper_event_id_bytes, wrapper_created_at } => {
            // Negentropy bookkeeping regardless of outcome (the outer wrapper id is
            // attacker-controlled, so it can't be the join-idempotency key — see below).
            {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                cache.insert(wrapper_event_id_bytes);
            }
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_event_id_bytes, wrapper_created_at, crate::db::wrappers::TRANSPORT_NIP17);

            // Never park our own echoed invite.
            if is_mine {
                return false;
            }

            // Cap-check before touching the DB (a hostile bundle can declare an
            // unbounded channel/relay list).
            if let Err(e) = invite.validate() {
                log_warn!("[community] invite rejected: {}", e);
                return false;
            }

            // Idempotency on the INNER identity (community_id), NOT the wrapper id: a
            // replayed bundle re-wrapped under fresh ephemeral keys must not re-notify
            // or churn. If we already hold this Community, or already have it parked,
            // drop silently.
            let community_id = invite.community_id.clone();
            // PUBLIC input: a signature-valid invite can still carry a malformed id, so decode through
            // the SIMD-validated path (rejects non-hex / wrong length in-register).
            let already_held = crate::community::CommunityId(
                match crate::simd::hex::hex_to_bytes_32_checked(&community_id) {
                    Some(b) => b,
                    None => { log_warn!("[community] invite has malformed id"); return false; }
                },
            );
            if crate::db::community::community_exists(&already_held).unwrap_or(false) {
                return false;
            }
            if crate::db::community::pending_invite_exists(&community_id).unwrap_or(false) {
                return false;
            }

            // Supersession: a decline/leave tombstone suppresses any invite no newer than the
            // decision (so the un-deletable 3304 can't re-nag, and a sibling's decline propagated via
            // the synced list silences this device too). A STRICTLY-newer invite falls through and
            // parks — a deliberate re-invite resurfaces. `wrapper_created_at` is outer-send seconds.
            if crate::community::list::tombstone_suppresses(&community_id, wrapper_created_at) {
                return false;
            }

            // Park for explicit consent — do NOT join, subscribe, or dial the bundle's
            // relays here. The user accepts via the command layer.
            let bundle_json = match invite.to_json() {
                Ok(j) => j,
                Err(e) => { log_warn!("[community] invite re-serialize failed: {}", e); return false; }
            };
            match crate::db::community::save_pending_invite(&community_id, &bundle_json, &inviter) {
                Ok(true) => {
                    handler.on_community_invite(&community_id);
                    // Warm the community's first page in the background so a subsequent Accept opens
                    // populated instead of paying the join sync. RAM-only + best-effort; promotion on
                    // Join re-validates freshness. SessionGuard'd so a mid-flight swap is a no-op.
                    let invite_warm = invite.clone();
                    let bg = crate::state::SessionGuard::capture();
                    tokio::spawn(async move {
                        if !bg.is_valid() {
                            return;
                        }
                        crate::community::service::preload_community(&invite_warm).await;
                    });
                }
                Ok(false) => {} // raced — already parked
                Err(e) => log_warn!("[community] invite park failed: {}", e),
            }
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
            let _ = crate::db::wrappers::save_processed_wrapper(&wrapper_id_bytes, wrapper_created_at, crate::db::wrappers::TRANSPORT_NIP17);
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
    emoji_tags: Vec<crate::types::EmojiTag>,
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
            msg.apply_edit(new_content.to_string(), edited_at, emoji_tags.clone());
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

/// Commit a NIP-09 cooperative deletion request.
///
/// Authorization: only the original message's author can delete it
/// (matches NIP-09's `event.pubkey == deletion.pubkey` rule applied to
/// the inner rumor). For DMs, that means either the sender is `MY` (we
/// deleted from another device) or the sender is the chat counterpart
/// who originally sent that message. Anyone else's deletion is silently
/// ignored.
///
/// On success: drops the message from in-memory STATE, removes the row
/// from the events table, and emits `message_removed` so the frontend
/// can fade the row out — same code path as failed-message cleanup.
async fn commit_deletion(
    target_event_id: &str,
    contact: &str,
    sender: &PublicKey,
    handler: &dyn InboundEventHandler,
) -> bool {
    // Look up the original. If not present locally there's nothing to
    // delete — the deletion notice arrived before the original (rare),
    // or we never had it. Either way, no-op.
    //
    // KNOWN LIMITATION: late-binding deletions are not handled. If
    // the deletion arrives BEFORE the original (cold sync, out-of-order
    // relay delivery), we drop the deletion silently here, and when the
    // original arrives later it shows up unhidden. A future enhancement
    // would persist a `pending_deletions` table keyed by target id and
    // apply queued deletions when the target is committed in
    // commit_dm_message. The common case (deletion arrives after the
    // original) works correctly today.
    //
    // For DM rumors the `npub` field is intentionally empty: the chat
    // is between two parties, so the author is implicit from `mine`
    // (me if true, chat counterpart if false). We derive the original
    // author from that, since the rumor pubkey isn't stored.
    let (mine, chat_id) = {
        let state = crate::state::STATE.lock().await;
        match state.find_message(target_event_id) {
            Some((chat, msg)) => (msg.mine, chat.id.clone()),
            None => return false,
        }
    };

    // Authorization: deletion sender must match the original author.
    // For DMs:
    //   - mine == true:  original author == us (MY_PUBLIC_KEY).
    //                    Authorized if the deletion sender is also us
    //                    (i.e. came in via our own self-wrap from
    //                    another device, multi-device sync).
    //   - mine == false: original author == chat counterpart. Chat id
    //                    for a DM is the counterpart's npub, so we
    //                    parse it and compare against the deletion
    //                    sender.
    let authorized = if mine {
        match crate::state::my_public_key() {
            Some(my_pk) => *sender == my_pk,
            None => false,
        }
    } else {
        match nostr_sdk::PublicKey::from_bech32(&chat_id) {
            Ok(counterpart) => sender == &counterpart,
            Err(_) => false, // chat id wasn't an npub (shouldn't happen for DMs)
        }
    };
    if !authorized {
        eprintln!(
            "[NIP-17 cooperative-delete] unauthorized: sender {} not the author of target {} (mine={}, chat={})",
            sender.to_hex(), target_event_id, mine, chat_id
        );
        return false;
    }

    // Drop from in-memory state.
    let removed = {
        let mut state = crate::state::STATE.lock().await;
        state.remove_message(target_event_id)
    };
    let removed_msg = match removed {
        Some((_chat_id, msg)) => msg,
        None => return false,
    };

    // Nuke any cached attachment files for this message — sender asked
    // for the message to disappear, and a downloaded file the receiver
    // never moved out of Vector's cache should go with it.
    //
    // Refcount filter: drop attachments still referenced by sibling
    // messages so we don't yank a cached file from messages that
    // still need it (Vector dedupes by SHA-256, so the same file
    // can back multiple messages). User-managed paths are also left
    // alone (canonicalize + starts_with check).
    let unique = crate::deletion::filter_unreferenced_attachments(
        target_event_id,
        removed_msg.attachments,
    ).await;
    crate::deletion::delete_cached_attachment_files_pub(&unique);

    // Drop from the events table.
    if let Err(e) = crate::db::events::delete_event(target_event_id).await {
        eprintln!(
            "[NIP-17 cooperative-delete] DB delete failed for {}: {}",
            target_event_id, e
        );
    }

    // Tell the frontend to fade the row out. Reuses the existing
    // message_removed event handled in main.js, so no new wiring.
    crate::traits::emit_event(
        "message_removed",
        &serde_json::json!({
            "id": target_event_id,
            "chat_id": &chat_id,
            "reason": "deleted-by-sender",
        }),
    );

    handler.on_message_deleted(&chat_id, target_event_id);
    let _ = contact;
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
    let client = crate::state::nostr_client()
        .ok_or_else(|| "Nostr client not initialized".to_string())?;
    let my_pk = crate::state::my_public_key()
        .ok_or_else(|| "Public key not initialized".to_string())?;
    let prepared = prepare_event(event, &client, my_pk).await;
    Ok(commit_prepared_event(prepared, is_new, handler).await)
}
