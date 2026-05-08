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
            // Target not in local STATE (cold cache, late-binding,
            // already locally removed). For the late-binding case the
            // forthcoming generic deferred-event-resolution feature
            // will queue this; for now we drop silently.
            return false;
        }
    };

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
                Ok((_wire, _members, admins)) => admins.iter().any(|a| a == &sender_npub),
                Err(_) => false,
            },
            Err(_) => false,
        }
    } else {
        false
    };

    if !is_self_delete && !is_admin_hide {
        crate::log_warn!(
            "[MLS cooperative-delete] unauthorized: sender {} not the author of {} (group {})",
            sender_npub,
            target_event_id,
            group_id
        );
        return false;
    }

    let removed = {
        let mut state = crate::state::STATE.lock().await;
        state.remove_message(target_event_id)
    };
    if removed.is_none() {
        return false;
    }

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
