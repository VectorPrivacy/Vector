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

use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::{
    commands, db,
    MlsService, NotificationData, show_notification_generic,
    TAURI_APP, NOSTR_CLIENT,
    util::get_file_type_description,
};

/// Current MLS group message subscription ID. Updated by `refresh_mls_subscription()`
/// when groups are joined or left — single subscription, never accumulates.
static MLS_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));

/// Process an MLS group message event through the full pipeline.
///
/// Delegates business logic to vector-core's handle_mls_group_message,
/// then adds Tauri-specific notifications and badge updates on top.
pub(crate) async fn handle_mls_group_message(event: Event, my_public_key: PublicKey) -> bool {
    // Extract group_id for notification context (before event is moved into vector-core)
    let group_id = event
        .tags
        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
        .and_then(|t| t.content().map(|s| s.to_string()));

    // Delegate all business logic to vector-core
    let message = vector_core::mls::handle_mls_group_message(event, my_public_key).await;

    // Tauri-specific: notifications + badge for new messages
    if let Some(ref msg) = message {
        if let Some(group_id) = &group_id {
            // Check sender blocked status
            let sender_blocked = {
                let state = crate::STATE.lock().await;
                msg.npub.as_ref().and_then(|npub| state.get_profile(npub))
                    .map_or(false, |p| p.flags.is_blocked())
            };

            // Show OS notification (unless blocked/muted/mine)
            if !msg.mine && !sender_blocked {
                show_mls_group_notification(group_id, msg).await;
            }

            // Update badge (unless sender is blocked)
            if !sender_blocked {
                if let Some(handle) = TAURI_APP.get() {
                    let _ = commands::messaging::update_unread_counter(handle.clone()).await;
                }
            }
        }
    }

    message.is_some()
}

/// Show an OS notification for an MLS group message.
/// Handles mention detection, @everyone admin validation, and mute checks.
async fn show_mls_group_notification(group_id: &str, msg: &vector_core::Message) {
    let sender_npub = msg.npub.as_deref().unwrap_or_default();

    // Determine if we should notify (mentions bypass group mute, blocked senders never notify)
    let should_notify = {
        let state = crate::STATE.lock().await;

        let mentions_me = crate::MY_PUBLIC_KEY.get()
            .and_then(|pk| pk.to_bech32().ok())
            .map_or(false, |my_npub| msg.content.contains(&format!("@{}", my_npub)));

        let everyone_ping = if msg.content.contains("@everyone") {
            // Only admin @everyone pings count, and user can opt out
            let is_admin_sender = db::get_mls_engine_group_id(group_id).ok().flatten()
                .and_then(|eid| {
                    let svc = MlsService::new_persistent_static().ok()?;
                    let engine = svc.engine().ok()?;
                    engine.get_groups().ok()?.into_iter()
                        .find(|g| vector_core::simd::hex::bytes_to_hex_string(g.mls_group_id.as_slice()) == eid)
                        .map(|g| g.admin_pubkeys.iter().any(|pk| pk.to_bech32().ok().as_deref() == Some(sender_npub)))
                })
                .unwrap_or(false);
            is_admin_sender && !vector_core::db::settings::get_sql_setting("notif_mute_everyone".to_string())
                .ok().flatten().map_or(false, |v| v == "true")
        } else {
            false
        };

        let sender_dm_muted = state.get_chat(sender_npub).map_or(false, |c| c.muted);
        let sender_blocked = state.get_profile(sender_npub).map_or(false, |p| p.flags.is_blocked());

        if sender_blocked {
            false
        } else if mentions_me || everyone_ping {
            !sender_dm_muted
        } else {
            state.get_chat(group_id).map_or(false, |c| !c.muted)
        }
    };

    if !should_notify { return; }

    // Build notification display info
    let is_file = !msg.attachments.is_empty();
    let (sender_name, group_name, avatar, content) = {
        let state = crate::STATE.lock().await;

        let (sender, av) = if let Some(profile) = state.get_profile(sender_npub) {
            let name = if !profile.nickname.is_empty() { profile.nickname.to_string() }
                else if !profile.name.is_empty() { profile.name.to_string() }
                else { "Someone".to_string() };
            let cached = if !profile.avatar_cached.is_empty() { Some(profile.avatar_cached.to_string()) } else { None };
            (name, cached)
        } else {
            ("Someone".to_string(), None)
        };

        let group = state.get_chat(group_id)
            .and_then(|c| c.metadata.get_name().map(|n| n.to_string()))
            .unwrap_or_else(|| "Group Chat".to_string());

        let content = if is_file {
            let ext = msg.attachments.first().map(|a| a.extension.clone()).unwrap_or_else(|| "file".into());
            "Sent a ".to_string() + &get_file_type_description(&ext)
        } else {
            crate::services::strip_content_for_preview(
                &crate::services::resolve_mention_display_names(&msg.content, &state)
            )
        };

        (sender, group, av, content)
    };

    // Fetch group avatar from MLS metadata (outside STATE lock)
    let group_avatar = db::load_mls_groups().await.ok().and_then(|groups| {
        groups.into_iter()
            .find(|g| g.group_id == group_id)
            .and_then(|g| g.profile.avatar_cached)
    });

    let notification = NotificationData::group_message(sender_name, group_name, content, avatar, group_avatar, group_id.to_string());
    show_notification_generic(notification);
}

/// Start live subscriptions for GiftWraps and MLS group messages.
/// Refresh the MLS group message subscription with current group IDs from the DB.
/// Unsubscribes the old subscription (if any) and creates a new one scoped to our groups.
/// Called at boot and whenever groups change (join/leave/evict).
pub(crate) async fn refresh_mls_subscription() {
    let Some(client) = NOSTR_CLIENT.get() else { return; };

    let group_ids: Vec<String> = crate::db::load_mls_groups().await
        .unwrap_or_default()
        .into_iter()
        .filter(|g| !g.evicted)
        .map(|g| g.group_id.clone())
        .collect();

    let mut sub_guard = MLS_SUB_ID.lock().await;

    // Unsubscribe the old MLS subscription
    if let Some(old_id) = sub_guard.take() {
        client.unsubscribe(&old_id).await;
    }

    // Subscribe with current group IDs (skip if no groups)
    if !group_ids.is_empty() {
        let filter = Filter::new()
            .kind(Kind::MlsGroupMessage)
            .custom_tags(SingleLetterTag::lowercase(Alphabet::H), group_ids)
            .limit(0);
        match client.subscribe(filter, None).await {
            Ok(output) => { *sub_guard = Some(output.val); }
            Err(e) => eprintln!("[MLS] Failed to subscribe: {:?}", e),
        }
    }
}

/// Called once after login to begin receiving real-time events.
///
/// Uses vector-core's `subscribe_dms()` for the GiftWrap subscription,
/// then layers on MLS group subscription (Tauri-specific MDK dependency).
pub(crate) async fn start_subscriptions() -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // GiftWrap subscription via vector-core (DMs, files, MLS welcomes)
    let core = vector_core::VectorCore;
    let gift_sub_id = core.subscribe_dms().await.map_err(|e| e.to_string())?;

    // MLS group subscription (Tauri-specific — scoped to current groups)
    refresh_mls_subscription().await;

    // Notification loop: dispatch GiftWraps through Tauri's event handler,
    // MLS group messages through the MDK handler
    match client
        .handle_notifications(|notification| async {
            if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                if subscription_id == gift_sub_id {
                    // DMs/files/reactions/edits + MLS welcomes (via tauri_commit_prepared_event)
                    super::handle_event(*event, true).await;
                } else if MLS_SUB_ID.lock().await.as_ref() == Some(&subscription_id) {
                    // MLS group messages via MDK engine
                    if let Some(&my_pk) = crate::MY_PUBLIC_KEY.get() {
                        handle_mls_group_message((*event).clone(), my_pk).await;
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