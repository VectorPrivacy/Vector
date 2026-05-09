//! Cooperative-hide application for inbound MLS deletion rumors.
//!
//! Shared between the live `group_handler` path and the bulk-sync
//! `service` path so authorization logic and side-effects live in one
//! place. A future change to the auth model (additional moderator
//! roles, late-binding queue, etc.) lands here once.
//!
//! Authorization model:
//!   1. Self-delete: deletion sender matches the original message's
//!      author. For MLS rumors, `Message.npub` carries the author's
//!      bech32 pubkey. If it's empty (regression / older row), fall
//!      back to comparing against MY_PUBLIC_KEY when the original
//!      message has `mine == true` — the user's own client deleting
//!      its own message must always work.
//!   2. Admin-hide: deletion sender is currently a member of the
//!      group's admin set (looked up live via MlsService).
//!   3. Anything else: silently dropped (logged at warn level).

use nostr_sdk::prelude::*;

/// Apply an inbound MLS cooperative-hide deletion if the sender is
/// authorized. Returns `true` iff the original message was actually
/// removed from local STATE/DB and the `message_removed` event was
/// emitted.
pub(crate) async fn apply_cooperative_hide(
    target_event_id: &str,
    sender: &PublicKey,
    group_id: &str,
) -> bool {
    let sender_npub = sender.to_bech32().unwrap_or_default();
    if sender_npub.is_empty() {
        crate::log_warn!(
            "[MLS hide] dropped — sender pubkey not bech32-encodable (target {})",
            target_event_id
        );
        return false;
    }

    // Look up the original to derive the author + `mine` flag in a
    // single STATE access.
    let original = {
        let state = crate::state::STATE.lock().await;
        state
            .find_message(target_event_id)
            .map(|(_, m)| (m.npub.clone().unwrap_or_default(), m.mine))
    };
    let (original_author_npub, original_is_mine) = match original {
        Some(pair) => pair,
        None => {
            // Target not in local STATE. Most common cause: we issued
            // the deletion ourselves and removed it optimistically, now
            // the relay is echoing our own cooperative-hide back — fully
            // expected, log at debug. Other causes (cold cache, late-
            // binding from a remote sender) get a warn since they're
            // worth noticing. The forthcoming generic deferred-event-
            // resolution feature will queue late-binding cases.
            let is_own_echo = crate::state::MY_PUBLIC_KEY
                .get()
                .map(|my_pk| my_pk == sender)
                .unwrap_or(false);
            if is_own_echo {
                crate::log_debug!(
                    "[MLS hide] target {} already gone — our own cooperative-hide echo (we removed it optimistically)",
                    target_event_id
                );
            } else {
                crate::log_warn!(
                    "[MLS hide] dropped — target {} not in local STATE (cold cache or late-binding from remote sender)",
                    target_event_id
                );
            }
            return false;
        }
    };
    crate::log_info!(
        "[MLS hide] received notice: target={} sender={} original_author={} original_mine={}",
        target_event_id,
        sender_npub,
        original_author_npub,
        original_is_mine
    );

    // Self-delete check. Primary path: npub-vs-npub comparison.
    // Fallback: when the stored npub is empty AND the original is our
    // own outbound message, accept any deletion sent by us — covers
    // regression rows where the author bech32 wasn't persisted.
    let is_self_delete = if !original_author_npub.is_empty() {
        original_author_npub == sender_npub
    } else if original_is_mine {
        match crate::state::MY_PUBLIC_KEY.get() {
            Some(my_pk) => my_pk == sender,
            None => false,
        }
    } else {
        false
    };

    // Admin-hide check (only consulted when self-delete didn't match).
    let is_admin_hide = if !is_self_delete {
        match crate::mls::MlsService::new_persistent_static() {
            Ok(svc) => match svc.get_group_members(group_id) {
                Ok((_wire, _members, admins)) => {
                    let matched = admins.iter().any(|a| a == &sender_npub);
                    crate::log_info!(
                        "[MLS hide] admin lookup for group {}: admins=[{}] sender={} matched={}",
                        group_id,
                        admins.join(", "),
                        sender_npub,
                        matched
                    );
                    matched
                }
                Err(e) => {
                    crate::log_warn!(
                        "[MLS hide] get_group_members failed for {}: {}",
                        group_id,
                        e
                    );
                    false
                }
            },
            Err(e) => {
                crate::log_warn!("[MLS hide] MlsService init failed: {}", e);
                false
            }
        }
    } else {
        false
    };

    if !is_self_delete && !is_admin_hide {
        crate::log_warn!(
            "[MLS cooperative-delete] unauthorized: sender {} not the author of {} (group {}) — self_delete={} admin_hide={}",
            sender_npub,
            target_event_id,
            group_id,
            is_self_delete,
            is_admin_hide
        );
        return false;
    }
    crate::log_info!(
        "[MLS hide] authorized via {} — attempting to remove {} from STATE",
        if is_self_delete { "self-delete" } else { "admin-hide" },
        target_event_id
    );

    let removed = {
        let mut state = crate::state::STATE.lock().await;
        state.remove_message(target_event_id)
    };
    let removed_msg = match removed {
        Some((chat_id, msg)) => {
            crate::log_info!(
                "[MLS hide] state.remove_message succeeded — id={} chat_id={} now emitting message_removed",
                target_event_id,
                chat_id
            );
            msg
        }
        None => {
            crate::log_warn!(
                "[MLS hide] state.remove_message returned None for {} — message wasn't in state by the time we tried to remove it (race or already-removed)",
                target_event_id
            );
            return false;
        }
    };

    // Refcount-aware local cache nuke: drop attachments still
    // referenced by sibling messages so we don't yank a cached file
    // shared via SHA-256 dedup.
    let unique = crate::deletion::filter_unreferenced_attachments(
        target_event_id,
        removed_msg.attachments,
    ).await;
    crate::deletion::delete_cached_attachment_files_pub(&unique);

    if let Err(e) = crate::db::events::delete_event(target_event_id).await {
        crate::log_warn!(
            "[MLS cooperative-delete] DB delete failed for {}: {}",
            target_event_id,
            e
        );
    }

    crate::traits::emit_event(
        "message_removed",
        &serde_json::json!({
            "id": target_event_id,
            "chat_id": group_id,
            "reason": "deleted-by-sender",
        }),
    );

    true
}
