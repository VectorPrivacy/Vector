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
async fn auto_mark_if_active(chat_id: &str, msg_id: &str) {
    let active = vector_core::state::get_active_chat();
    if active.as_deref() != Some(chat_id) { return; }

    let slim = {
        let mut state = STATE.lock().await;
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.last_read = vector_core::compact::encode_message_id(msg_id);
            state.get_chat(chat_id).map(|c| {
                vector_core::db::chats::SlimChatDB::from_chat(c, &state.interner)
            })
        } else {
            None
        }
    };
    if let Some(slim) = slim {
        let _ = vector_core::db::chats::save_slim_chat(&slim);
    }

    if let Some(app) = TAURI_APP.get() {
        let _ = app.emit("chat_mark_read", serde_json::json!({
            "chat_id": chat_id,
            "last_read": msg_id,
        }));
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
        if msg.mine || !is_new { return; }
        let chat_id = chat_id.to_string();
        let content = msg.content.clone();
        let msg_id = msg.id.clone();
        tokio::spawn(async move {
            // If the user is actively watching this chat, advance last_read
            // before the badge recount so the message never counts as unread.
            // The FE's own markAsRead still runs on the message_new event for
            // DB persistence, but this avoids the racey badge bump in between.
            auto_mark_if_active(&chat_id, &msg_id).await;
            // Check muted
            let is_muted = {
                let state = STATE.lock().await;
                state.get_chat(&chat_id).map_or(false, |c| c.muted)
            };
            if !is_muted {
                let display_info = {
                    let state = STATE.lock().await;
                    get_dm_notification_info(&state, &chat_id, &content)
                };
                if let Some((name, body, avatar)) = display_info {
                    let notification = NotificationData::direct_message(name, body, avatar, chat_id.clone());
                    show_notification_generic(notification);
                }
            }
            // Update badge
            if let Some(handle) = TAURI_APP.get() {
                let _ = commands::messaging::update_unread_counter(handle.clone()).await;
            }
        });
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        if msg.mine || !is_new { return; }
        let chat_id = chat_id.to_string();
        let extension = msg.attachments.first()
            .map(|att| att.extension.clone())
            .unwrap_or_else(|| String::from("file"));
        let msg_id = msg.id.clone();
        tokio::spawn(async move {
            auto_mark_if_active(&chat_id, &msg_id).await;
            // Check muted
            let is_muted = {
                let state = STATE.lock().await;
                state.get_chat(&chat_id).map_or(false, |c| c.muted)
            };
            if !is_muted {
                let display_info = {
                    let state = STATE.lock().await;
                    get_file_notification_info(&state, &chat_id, &extension)
                };
                if let Some((name, body, avatar)) = display_info {
                    let notification = NotificationData::direct_message(name, body, avatar, chat_id.clone());
                    show_notification_generic(notification);
                }
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
}

/// Extract display info for a DM text notification.
fn get_dm_notification_info(
    state: &crate::state::ChatState,
    contact: &str,
    content: &str,
) -> Option<(String, String, Option<String>)> {
    let (name, avatar) = match state.get_profile(contact) {
        Some(profile) => {
            let name = if !profile.nickname.is_empty() {
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
            (name, avatar)
        }
        None => (String::from("New Message"), None),
    };
    let resolved = crate::services::strip_content_for_preview(
        &crate::services::resolve_mention_display_names(content, state),
    );
    Some((name, resolved, avatar))
}

/// Extract display info for a file attachment notification.
fn get_file_notification_info(
    state: &crate::state::ChatState,
    contact: &str,
    extension: &str,
) -> Option<(String, String, Option<String>)> {
    let (name, avatar) = match state.get_profile(contact) {
        Some(profile) => {
            let name = if !profile.nickname.is_empty() {
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
            (name, avatar)
        }
        None => (String::from("New Message"), None),
    };
    let body = "Sent a ".to_string() + &get_file_type_description(extension);
    Some((name, body, avatar))
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
/// Intercepts WebXDC and MLS Welcome events (deeply platform-specific),
/// then delegates everything else to vector-core's commit pipeline.
pub(crate) async fn tauri_commit_prepared_event(
    prepared: vector_core::PreparedEvent,
    is_new: bool,
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

    // MLS Welcomes + everything else: vector-core handles processing.
    // TauriEventHandler hooks fire callbacks for notifications/badges.
    static HANDLER: TauriEventHandler = TauriEventHandler;
    core_handler::commit_prepared_event(prepared, is_new, &HANDLER).await
}

// ============================================================================
// WebXDC peer management — Tauri + Iroh specific
// ============================================================================

/// Handle a WebXDC peer advertisement - persist to SQLite and add the peer to our realtime channel
pub(crate) async fn handle_webxdc_peer_advertisement(
    event_id: &str,
    topic_id: &str,
    node_addr_encoded: &str,
    sender_npub: &str,
    created_at: u64,
    conversation_id: &str,
) -> bool {
    use crate::miniapps::realtime::{decode_topic_id, decode_node_addr};

    log_info!("[WEBXDC] Received peer advertisement for topic {}", topic_id);

    // Validate BEFORE persisting — both fields are sender-controlled, and a garbage
    // row would otherwise sit in the events table forever.
    let topic = match decode_topic_id(topic_id) {
        Ok(t) => t,
        Err(e) => {
            log_warn!("Failed to decode topic ID in peer advertisement: {}", e);
            return false;
        }
    };
    let node_addr = match decode_node_addr(node_addr_encoded) {
        Ok(addr) => addr,
        Err(e) => {
            log_warn!("Failed to decode node address in peer advertisement: {}", e);
            return false;
        }
    };
    // Sender-claimed timestamp: clamp into the near future so a forged far-future ad
    // can't outrank every later genuine peer-left forever.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let created_at = created_at.min(now_secs + 300);

    // Persist to SQLite for offline->online peer discovery
    if !db::event_exists(event_id).unwrap_or(true) {
        if let Ok(chat_id) = db::get_or_create_chat_id(conversation_id) {
            let tags = vec![
                vec!["webxdc-topic".to_string(), topic_id.to_string()],
                vec!["webxdc-node-addr".to_string(), node_addr_encoded.to_string()],
                vec!["d".to_string(), "vector-webxdc-peer".to_string()],
            ];
            let event = crate::stored_event::StoredEvent {
                id: event_id.to_string(),
                kind: crate::stored_event::event_kind::APPLICATION_SPECIFIC,
                chat_id,
                user_id: None,
                content: "peer-advertisement".to_string(),
                tags,
                reference_id: Some(topic_id.to_string()),
                created_at,
                received_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                mine: false,
                pending: false,
                failed: false,
                wrapper_event_id: None,
                npub: Some(sender_npub.to_string()),
                preview_metadata: None,
            };
            if let Err(e) = db::save_event(&event).await {
                log_warn!("[WEBXDC] Failed to persist peer advertisement: {}", e);
            }
        }
    }

    // Recency gate for everything LIVE below (session peers, Iroh feed, lobby emit):
    // a replayed or historically-synced ad that's no longer this sender's latest word
    // (a newer signal exists, or a same-second left — tombstone wins ties) must not
    // resurrect a departed player. History stays persisted; the lobby follows the present.
    if !db::peer_signal_is_current(topic_id, sender_npub, created_at, true).unwrap_or(false) {
        log_info!("[WEBXDC] Stale peer advertisement for topic {} from {} — persisted, not surfaced", topic_id, sender_npub);
        return true;
    }

    // Get the MiniApps state and add the peer
    if let Some(handle) = TAURI_APP.get() {
        let state = handle.state::<miniapps::state::MiniAppsState>();

        // Check if we have an active realtime channel for this topic
        let has_channel = {
            let channels = state.realtime_channels.read().await;
            log_info!("[WEBXDC] Checking {} active channels for topic match", channels.len());
            for (label, ch) in channels.iter() {
                log_info!("[WEBXDC]   Channel '{}': topic={}, active={}",
                    label,
                    crate::miniapps::realtime::encode_topic_id(&ch.topic),
                    ch.active);
            }
            channels.values().any(|ch| ch.topic == topic && ch.active)
        };

        log_info!("[WEBXDC] has_channel for topic {}: {}", topic_id, has_channel);

        if has_channel {
            log_info!("[WEBXDC] Found active channel for topic {}, adding peer", topic_id);
            state.add_session_peer(topic, sender_npub.to_string()).await;
            // Get the realtime manager and add the peer
            match state.realtime.get_or_init().await {
                Ok(iroh) => {
                    match iroh.add_peer(topic, node_addr.clone()).await {
                        Ok(_) => {
                            log_info!("[WEBXDC] Successfully added peer {} to realtime channel topic {}",
                                node_addr.id, topic_id);
                        }
                        Err(e) => {
                            log_error!("[WEBXDC] Failed to add peer to realtime channel: {}", e);
                        }
                    }
                }
                Err(e) => {
                    log_error!("[WEBXDC] Failed to get realtime manager: {}", e);
                }
            }

            // Emit status update so the frontend shows the new peer's avatar
            let peer_npubs = state.get_session_peers(&topic).await;
            let peer_count = peer_npubs.len();
            if let Some(main_window) = handle.get_webview_window("main") {
                use tauri::Emitter;
                let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                    "topic": topic_id,
                    "peer_count": peer_count,
                    "peers": peer_npubs,
                    "is_active": true,
                    "has_pending_peers": true,
                }));
                log_info!("[WEBXDC] Emitted miniapp_realtime_status: topic={}, peer_count={}", topic_id, peer_count);
            }
            return true;
        } else {
            // Cache addr for QUIC connection when we join, track npub for lobby UI
            log_info!("[WEBXDC] Caching peer addr for topic {} (no active channel yet)", topic_id);
            state.cache_peer_addr(topic, node_addr).await;
            state.add_session_peer(topic, sender_npub.to_string()).await;

            // Emit event to frontend so it can update the UI (show "Click to Join" and player avatars)
            let peer_npubs = state.get_session_peers(&topic).await;
            let peer_count = peer_npubs.len();
            if let Some(main_window) = handle.get_webview_window("main") {
                use tauri::Emitter;
                let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                    "topic": topic_id,
                    "peer_count": peer_count,
                    "peers": peer_npubs,
                    "is_active": false,
                    "has_pending_peers": peer_count > 0,
                }));
                log_info!("[WEBXDC] Emitted miniapp_realtime_status event: topic={}, peer_count={}", topic_id, peer_count);
            }

            return true;
        }
    }

    false
}

/// Handle a WebXDC peer-left signal — a peer closed their Mini App.
pub(crate) async fn handle_webxdc_peer_left(
    event_id: &str,
    topic_id: &str,
    sender_npub: &str,
    created_at: u64,
    conversation_id: &str,
) -> bool {
    use crate::miniapps::realtime::decode_topic_id;

    log_info!("[WEBXDC] Received peer-left from {} for topic {}", sender_npub, topic_id);

    // Validate BEFORE persisting (sender-controlled field).
    let topic = match decode_topic_id(topic_id) {
        Ok(t) => t,
        Err(e) => {
            log_warn!("Failed to decode topic ID in peer-left: {}", e);
            return false;
        }
    };
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let created_at = created_at.min(now_secs + 300);

    // Persist to SQLite
    if !db::event_exists(event_id).unwrap_or(true) {
        if let Ok(chat_id) = db::get_or_create_chat_id(conversation_id) {
            let tags = vec![
                vec!["webxdc-topic".to_string(), topic_id.to_string()],
                vec!["d".to_string(), "vector-webxdc-peer".to_string()],
            ];
            let event = crate::stored_event::StoredEvent {
                id: event_id.to_string(),
                kind: crate::stored_event::event_kind::APPLICATION_SPECIFIC,
                chat_id,
                user_id: None,
                content: "peer-left".to_string(),
                tags,
                reference_id: Some(topic_id.to_string()),
                created_at,
                received_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                mine: false,
                pending: false,
                failed: false,
                wrapper_event_id: None,
                npub: Some(sender_npub.to_string()),
                preview_metadata: None,
            };
            if let Err(e) = db::save_event(&event).await {
                log_warn!("[WEBXDC] Failed to persist peer-left: {}", e);
            }
        }
    }

    // Recency gate (mirror of the advertisement side): a STALE left — one already
    // superseded by a newer ad — must not evict an actively-playing peer from the
    // lobby. Out-of-order page syncs deliver exactly this shape.
    if !db::peer_signal_is_current(topic_id, sender_npub, created_at, false).unwrap_or(false) {
        log_info!("[WEBXDC] Stale peer-left for topic {} from {} — persisted, not surfaced", topic_id, sender_npub);
        return true;
    }

    if let Some(handle) = TAURI_APP.get() {
        let state = handle.state::<miniapps::state::MiniAppsState>();

        // Remove from session peers
        state.remove_session_peer(&topic, sender_npub).await;

        // Check if we're actively playing
        let we_are_playing = {
            let channels = state.realtime_channels.read().await;
            channels.values().any(|ch| ch.topic == topic && ch.active)
        };

        // Emit updated status
        let peer_npubs = state.get_session_peers(&topic).await;
        let peer_count = peer_npubs.len();
        if let Some(main_window) = handle.get_webview_window("main") {
            use tauri::Emitter;
            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                "topic": topic_id,
                "peer_count": peer_count,
                "peers": peer_npubs,
                "is_active": we_are_playing,
                "has_pending_peers": peer_count > 0,
            }));
            log_info!("[WEBXDC] Peer-left status update: topic={}, remaining={}", topic_id, peer_count);
        }

        return true;
    }

    false
}
