//! Event handler service for processing incoming Nostr events.
//!
//! Thin Tauri wrapper around vector-core's two-phase event pipeline.
//! - Processing gate: queues events during encryption migration
//! - TauriEventHandler: OS notifications, badge updates
//! - WebXDC: intercepted before vector-core (platform-specific)

use nostr_sdk::prelude::*;
use tauri::{Emitter, Manager};

use vector_core::event_handler as core_handler;
use vector_core::{Message, RumorProcessingResult};

use crate::{
    db, miniapps, commands,
    NotificationData, show_notification_generic,
    STATE, TAURI_APP, nostr_client, WRAPPER_ID_CACHE,
    util::get_file_type_description,
    state::{is_processing_allowed, PENDING_EVENTS},
};

/// If the inbound message lands in the chat the user is actively watching
/// (FE marks open + pinned + focused via `set_active_chat`), advance
/// `chat.last_read` BEFORE the badge recount so the unread count stays at
/// zero and the dock badge never bumps. Also pushes a `chat_mark_read`
/// event so the FE state and DB persistence catch up — the FE's own
/// `message_new` markAsRead still runs as a belt-and-braces second hop.
/// Returns true if the chat was the active one and was marked read here (so the caller can clear its
/// unread cache entry without a DB reconcile); false otherwise (the caller reconciles the chat).
async fn auto_mark_if_active(chat_id: &str, msg_id: &str) -> bool {
    let active = vector_core::state::get_active_chat();
    if active.as_deref() != Some(chat_id) { return false; }
    // A swap can land while awaiting the STATE lock; re-check inside so we never write account A's
    // last_read into account B's freshly-swapped chat list/DB.

    let slim = {
        let mut state = STATE.lock().await;
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            // A per-message "Mark as Unread" ends only by the user's own read, never a background one.
            if chat.unread_from != [0u8; 32] { return false; }
            chat.mark_read_at(vector_core::compact::encode_message_id(msg_id));
            state.get_chat(chat_id).map(|c| {
                vector_core::db::chats::SlimChatDB::from_chat(c, &state.interner)
            })
        } else {
            None
        }
    };
    let marked = slim.is_some();
    if let Some(slim) = slim {
        let _ = vector_core::db::chats::save_slim_chat(&slim);
    }

    if let Some(app) = TAURI_APP.get() {
        let _ = app.emit("chat_mark_read", serde_json::json!({
            "chat_id": chat_id,
            "last_read": msg_id,
        }));
    }
    marked
}

/// Update the unread cache for `chat_id` after a live inbound message: the active chat was just
/// marked read (clear, no DB), otherwise reconcile the one chat from the DB.
async fn refresh_chat_unread(chat_id: &str, was_marked_active: bool) {
    if was_marked_active {
        STATE.lock().await.unread_clear(chat_id);
    } else {
        commands::messaging::reconcile_chat_unread(chat_id).await;
    }
}

// ============================================================================
// TauriEventHandler — OS notifications + badge updates
// ============================================================================

/// Platform-specific event handler for the Tauri GUI.
/// Handles OS notifications, badge counter updates, and other desktop/mobile specifics.
pub(crate) struct TauriEventHandler;

impl vector_core::InboundEventHandler for TauriEventHandler {
    fn on_dm_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        if !is_new { return; }
        if msg.mine {
            // Answered from another device: mark this chat read here too so the unread clears and any
            // pending OS notification is revoked, even when backgrounded. This hook runs in both the
            // foreground handler and the background-sync commit path.
            let chat_id = chat_id.to_string();
            vector_core::db::spawn_bound(async move {
                crate::chat::mark_as_read_headless(&chat_id).await;
                if let Some(handle) = TAURI_APP.get() {
                    let _ = commands::messaging::update_unread_counter(handle.clone()).await;
                }
            });
            return;
        }
        let chat_id = chat_id.to_string();
        let content = msg.content.clone();
        let msg_id = msg.id.clone();
        let expires = msg.expiration;
        vector_core::db::spawn_bound(async move {
            // If the user is actively watching this chat, advance last_read
            // before the badge recount so the message never counts as unread.
            // The FE's own markAsRead still runs on the message_new event for
            // DB persistence, but this avoids the racey badge bump in between.
            let marked = auto_mark_if_active(&chat_id, &msg_id).await;
            refresh_chat_unread(&chat_id, marked).await;
            // A DM has no mention tier to fall back to, so anything short of "ring for
            // everything" is silence.
            let rings = {
                let state = STATE.lock().await;
                // No row yet means nobody has asked for quiet, so it rings.
                state.get_chat(&chat_id).is_none_or(|c| {
                    vector_core::notify::ring_for_chat(c) == vector_core::notify::NotifyLevel::All
                })
            };
            if rings {
                let gate = vector_core::notify::StreamGate::load();
                let (name, body, avatar) = {
                    let state = STATE.lock().await;
                    get_dm_notification_info(&state, &gate, &chat_id, &content)
                };
                show_notification_generic(NotificationData::direct_message(name, body, avatar, chat_id.clone()).with_message_id(msg_id.clone()).with_expiry(expires));
            }
            // Update badge
            if let Some(handle) = TAURI_APP.get() {
                let _ = commands::messaging::update_unread_counter(handle.clone()).await;
            }
        });
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        if !is_new { return; }
        if msg.mine {
            // Answered from another device: mark read + revoke any notification (see on_dm_received).
            let chat_id = chat_id.to_string();
            vector_core::db::spawn_bound(async move {
                crate::chat::mark_as_read_headless(&chat_id).await;
                if let Some(handle) = TAURI_APP.get() {
                    let _ = commands::messaging::update_unread_counter(handle.clone()).await;
                }
            });
            return;
        }
        let chat_id = chat_id.to_string();
        let extension = msg.attachments.first()
            .map(|att| att.extension.clone())
            .unwrap_or_else(|| String::from("file"));
        let msg_id = msg.id.clone();
        let expires = msg.expiration;
        vector_core::db::spawn_bound(async move {
            let marked = auto_mark_if_active(&chat_id, &msg_id).await;
            refresh_chat_unread(&chat_id, marked).await;
            // A DM has no mention tier to fall back to, so anything short of "ring for
            // everything" is silence.
            let rings = {
                let state = STATE.lock().await;
                // No row yet means nobody has asked for quiet, so it rings.
                state.get_chat(&chat_id).is_none_or(|c| {
                    vector_core::notify::ring_for_chat(c) == vector_core::notify::NotifyLevel::All
                })
            };
            if rings {
                let gate = vector_core::notify::StreamGate::load();
                let (name, body, avatar) = {
                    let state = STATE.lock().await;
                    get_file_notification_info(&state, &gate, &chat_id, &extension)
                };
                show_notification_generic(NotificationData::direct_message(name, body, avatar, chat_id.clone()).with_message_id(msg_id.clone()).with_expiry(expires));
            }
            // Update badge
            if let Some(handle) = TAURI_APP.get() {
                let _ = commands::messaging::update_unread_counter(handle.clone()).await;
            }
        });
    }

    fn on_community_invite(&self, community_id: &str) {
        // vector-core parked the invite for consent (no join, no relay connect). Just
        // surface it so the frontend can refresh its pending-invite list; the actual
        // join + subscription refresh happens on the explicit accept command.
        if let Some(app) = TAURI_APP.get() {
            let _ = app.emit("community_invite_received", serde_json::json!({
                "community_id": community_id,
            }));
        }
    }

    // --- Community realtime (vector-core's `community::realtime::dispatch_event` already
    // ingested + persisted message-path events; these hooks own UI + notifications + the
    // richer presence/teardown side-effects the GUI needs). ---

    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        // Realtime community messages are genuinely live: the back-paging fetch is a SEPARATE one-shot
        // batch (process_channel_batch), not this stream, so a suppression flag here only ever hides
        // live messages (saved-but-never-surfaced → permanently stuck unread). Always surface.
        vector_core::traits::emit_message_new(chat_id, msg);
        let chat_id = chat_id.to_string();
        let msg = msg.clone();
        // Snapshot the session BEFORE the spawn (multi-account rule 1): a swap can land before the
        // body runs, and auto_mark below writes last_read — guard it from landing in account B's DB.
        vector_core::db::spawn_bound(async move {
            // Advance last_read first if this is the chat the user is actively watching, so a message
            // in the open community never counts as unread on the badge recount below (mirrors the DM
            // path; without it the badge bumps to 1 and races the FE's markAsRead, leaving it stuck).
            let marked = auto_mark_if_active(&chat_id, &msg.id).await;
            // Reconcile (not a blind +1): this handler also sees stale re-deliveries and bulk
            // back-sync, so the one-chat DB recount is the only correct update.
            refresh_chat_unread(&chat_id, marked).await;
            // Ping only for genuinely-live messages. A bulk back-sync (jump-to-unread) or a relay
            // re-delivering already-saved history pushes OLD events through this handler; by inner
            // timestamp they're stale, so skip the notification. They still surface + count via the
            // message_new emit above — only the SFX/OS-ping is muted, so nothing goes sticky.
            if vector_core::community::is_realtime_fresh(msg.at) {
                crate::services::subscription_handler::show_community_notification(&chat_id, &msg).await;
            }
            if let Some(handle) = TAURI_APP.get() {
                let _ = commands::messaging::update_unread_counter(handle.clone()).await;
            }
        });
    }

    fn on_community_update(&self, chat_id: &str, target_id: &str, msg: &Message) {
        vector_core::traits::emit_message_replaced(chat_id, target_id, msg);
    }

    fn on_community_removed(&self, chat_id: &str, target_id: &str) {
        vector_core::emit_event("message_removed", &serde_json::json!({
            "id": target_id, "chat_id": chat_id, "reason": "deleted",
        }));
    }

    fn on_community_presence(
        &self,
        chat_id: &str,
        npub: &str,
        joined: bool,
        event_id: &str,
        created_at: u64,
        invited_by: Option<&str>,
        invited_label: Option<&str>,
    ) {
        let (chat_id, npub, event_id) = (chat_id.to_string(), npub.to_string(), event_id.to_string());
        let invited_by = invited_by.map(str::to_string);
        let invited_label = invited_label.map(str::to_string);
        vector_core::db::spawn_bound(async move {
            crate::commands::community::apply_community_presence(
                &chat_id, &npub, joined, &event_id, created_at,
                invited_by.as_deref(), invited_label.as_deref(),
            ).await;
        });
    }

    fn on_community_typing(&self, chat_id: &str, npub: &str, until: u64) {
        let (chat_id, npub) = (chat_id.to_string(), npub.to_string());
        vector_core::db::spawn_bound(async move {
            let typers = {
                let mut state = crate::STATE.lock().await;
                state.update_typing_and_get_active(&chat_id, &npub, until)
            };
            vector_core::emit_event("typing-update", &serde_json::json!({
                "conversation_id": chat_id, "typers": typers,
            }));
        });
    }

    fn on_community_webxdc(
        &self,
        chat_id: &str,
        npub: &str,
        topic_id: &str,
        node_addr: Option<&str>,
        event_id: &str,
        created_at: u64,
    ) {
        let (chat_id, npub, topic_id, event_id) =
            (chat_id.to_string(), npub.to_string(), topic_id.to_string(), event_id.to_string());
        let node_addr = node_addr.map(str::to_string);
        vector_core::db::spawn_bound(async move {
            match node_addr {
                Some(addr) => handle_webxdc_peer_advertisement(&event_id, &topic_id, &addr, &npub, created_at, &chat_id).await,
                None => handle_webxdc_peer_left(&event_id, &topic_id, &npub, created_at, &chat_id).await,
            }
        });
    }

    fn on_community_self_removed(&self, community_id: &str) {
        let community_id = community_id.to_string();
        vector_core::db::spawn_bound(async move {
            crate::commands::community::self_remove_from_community(&community_id, false).await;
        });
    }

    fn on_community_refreshed(&self, community_id: &str) {
        let session = vector_core::db::current_session();
        let community_id = community_id.to_string();
        vector_core::db::spawn_bound(async move {
            let id = vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(&community_id));
            match vector_core::db::community::community_protocol(&id).ok().flatten() {
                Some(vector_core::community::ConcordProtocol::V2) => {
                    // A control fold may have revealed new channels — surface them as
                    // chat rows so the list grows without a reboot.
                    if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) {
                        vector_core::VectorCore.register_v2_chats(&c, &session).await;
                    }
                }
                _ => {
                    if let Ok(Some(c)) = vector_core::db::community::load_community(&id) {
                        crate::commands::community::sync_community_chats(&c).await;
                    }
                }
            }
            vector_core::emit_event("community_refreshed", &serde_json::json!({ "community_id": community_id }));
        });
    }

    fn on_community_dissolved(&self, community_id: &str) {
        // CORD-02 §9: the community is sealed read-only (the row is already
        // flagged by the fold) — nudge the frontend to re-render it as such.
        vector_core::emit_event("community_refreshed", &serde_json::json!({ "community_id": community_id }));
    }
}

/// Name, preview and avatar for a DM text notification.
fn get_dm_notification_info(
    state: &crate::state::ChatState,
    gate: &vector_core::notify::StreamGate,
    contact: &str,
    content: &str,
) -> (String, String, Option<String>) {
    let sender = vector_core::notify::sender(state, gate, contact, "New Message");
    let body = crate::services::strip_content_for_preview(
        &vector_core::notify::resolve_mentions(content, state, gate),
    );
    (sender.name, body, sender.avatar)
}

/// Name, preview and avatar for a DM attachment notification.
fn get_file_notification_info(
    state: &crate::state::ChatState,
    gate: &vector_core::notify::StreamGate,
    contact: &str,
    extension: &str,
) -> (String, String, Option<String>) {
    let sender = vector_core::notify::sender(state, gate, contact, "New Message");
    (sender.name, "Sent a ".to_string() + &get_file_type_description(extension), sender.avatar)
}

// ============================================================================
// Event processing entry points
// ============================================================================

/// Internal event handler — called by subscription handler and encryption drain.
///
/// Returns `false` if no session is active or the session has been swapped
/// out from under us. The subscription loop treats `false` as "drop this
/// event"; the encryption-drain loop tolerates the same outcome.
pub(crate) async fn handle_event(event: Event, is_new: bool) -> bool {
    let Some(client) = nostr_client() else { return false; };
    let Some(my_public_key) = crate::my_public_key() else { return false; };
    handle_event_with_context(event, is_new, &client, my_public_key).await
}

/// Full event processing — accepts dependencies as parameters.
/// Enables headless (background service) callers to provide their own client/key.
pub(crate) async fn handle_event_with_context(
    event: Event,
    is_new: bool,
    client: &Client,
    my_public_key: PublicKey,
) -> bool {
    // Processing gate — queue events during encryption migration
    if !is_processing_allowed() {
        let mut queue = PENDING_EVENTS.lock().await;
        if !is_processing_allowed() {
            queue.push((event, is_new));
            return false;
        }
        drop(queue);
    }

    // Phase 1: parallel-safe prepare (dedup, unwrap, parse)
    let prepared = core_handler::prepare_event(event, client, my_public_key).await;

    // Phase 2: sequential commit with Tauri-specific handling
    tauri_commit_prepared_event(prepared, is_new).await
}

// ============================================================================
// Tauri commit wrapper — intercepts platform-specific events
// ============================================================================

/// Commit a prepared event with Tauri-specific handling.
///
/// Intercepts WebXDC events (deeply platform-specific),
/// then delegates everything else to vector-core's commit pipeline.
pub(crate) async fn tauri_commit_prepared_event(
    prepared: vector_core::PreparedEvent,
    is_new: bool,
) -> bool {
    static HANDLER: TauriEventHandler = TauriEventHandler;
    tauri_commit_prepared_event_with(prepared, is_new, &HANDLER).await
}

/// `tauri_commit_prepared_event` with a caller-chosen handler — the bulk-sync loops pass a
/// `BatchingPersist` wrapper here so committed messages land in batched transactions.
pub(crate) async fn tauri_commit_prepared_event_with(
    prepared: vector_core::PreparedEvent,
    is_new: bool,
    handler: &dyn vector_core::InboundEventHandler,
) -> bool {
    // Intercept WebXDC events — requires Iroh/MiniApps (Tauri-only)
    if let vector_core::PreparedEvent::Processed {
        ref result, ref contact,
        ref wrapper_event_id_bytes, wrapper_created_at, ..
    } = prepared {
        match result {
            RumorProcessingResult::WebxdcPeerAdvertisement { event_id, topic_id, node_addr, sender_npub, created_at } => {
                // Cache + persist wrapper (same as commit would do)
                {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    cache.insert(*wrapper_event_id_bytes);
                }
                let _ = db::save_processed_wrapper(wrapper_event_id_bytes, wrapper_created_at, vector_core::db::wrappers::TRANSPORT_NIP17);
                return handle_webxdc_peer_advertisement(event_id, topic_id, node_addr, sender_npub, *created_at, contact).await;
            }
            RumorProcessingResult::CallSignal { call_id, signal, node_addr, sender_npub, created_at, video, media, .. } => {
                {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    cache.insert(*wrapper_event_id_bytes);
                }
                let _ = db::save_processed_wrapper(wrapper_event_id_bytes, wrapper_created_at, vector_core::db::wrappers::TRANSPORT_NIP17);
                // Only the DM's other party may signal; a group member cannot ring us through a channel.
                if contact == sender_npub {
                    crate::calls::session::on_signal(sender_npub, call_id, signal, node_addr.as_deref(), *created_at, video.as_deref(), media.as_deref()).await;
                }
                return true;
            }
            RumorProcessingResult::WebxdcPeerLeft { event_id, topic_id, sender_npub, created_at } => {
                {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    cache.insert(*wrapper_event_id_bytes);
                }
                let _ = db::save_processed_wrapper(wrapper_event_id_bytes, wrapper_created_at, vector_core::db::wrappers::TRANSPORT_NIP17);
                return handle_webxdc_peer_left(event_id, topic_id, sender_npub, *created_at, contact).await;
            }
            _ => {}
        }
    }

    // Everything else: vector-core handles processing.
    // TauriEventHandler hooks fire callbacks for notifications/badges.
    core_handler::commit_prepared_event(prepared, is_new, handler).await
}

// ============================================================================
// WebXDC peer management — Tauri + Iroh specific
// ============================================================================

/// A WebXDC peer advertisement: vector-core persists it and dials the peer into
/// any session we have on the topic; the lobby here shows who is playing.
pub(crate) async fn handle_webxdc_peer_advertisement(
    event_id: &str,
    topic_id: &str,
    node_addr_encoded: &str,
    sender_npub: &str,
    created_at: u64,
    conversation_id: &str,
) -> bool {
    on_peer_signal(event_id, topic_id, Some(node_addr_encoded), sender_npub, created_at, conversation_id).await
}

/// A WebXDC peer-left signal: a peer closed their Mini App.
pub(crate) async fn handle_webxdc_peer_left(
    event_id: &str,
    topic_id: &str,
    sender_npub: &str,
    created_at: u64,
    conversation_id: &str,
) -> bool {
    on_peer_signal(event_id, topic_id, None, sender_npub, created_at, conversation_id).await
}

async fn on_peer_signal(
    event_id: &str,
    topic_id: &str,
    node_addr: Option<&str>,
    sender_npub: &str,
    created_at: u64,
    conversation_id: &str,
) -> bool {
    let Some(sig) = vector_core::xdc::on_signal(conversation_id, sender_npub, topic_id, node_addr, event_id, created_at).await else {
        log_warn!("[WEBXDC] Dropped a malformed peer signal for topic {}", topic_id);
        return false;
    };
    // History stays persisted; the lobby follows the present. A replayed or
    // out-of-order signal must not resurrect a departed player or evict an active one.
    if !sig.current {
        return true;
    }
    let Ok(topic) = crate::miniapps::realtime::decode_topic_id(topic_id) else { return false };
    let Some(handle) = TAURI_APP.get() else { return false };
    let state = handle.state::<miniapps::state::MiniAppsState>();
    if node_addr.is_some() {
        state.add_session_peer(topic, sender_npub.to_string()).await;
    } else {
        state.remove_session_peer(&topic, sender_npub).await;
    }
    let we_are_playing = state.has_realtime_channel_for_topic(&topic).await;
    let peers = state.get_session_peers(&topic).await;
    crate::miniapps::commands::emit_lobby(&sig.topic, peers, we_are_playing);
    true
}
