//! MLS group invite handling — list pending invites, accept, decline.
//!
//! When a user is invited to an MLS group, they receive a NIP-59 GiftWrap
//! containing an MLS Welcome. The Welcome is stored locally by MDK as
//! "pending" until the user explicitly accepts it by joining the group.
//!
//! This module provides client-agnostic invite operations. Client-specific
//! concerns (notifications, avatar caching) are handled by callbacks or
//! outside this module.

use serde::{Deserialize, Serialize};
use nostr_sdk::prelude::*;

use crate::simd::hex::bytes_to_hex_string;
use super::{MlsService, MlsError, MlsGroup, MlsGroupFull, MlsGroupProfile, emit_group_metadata_event};

/// A pending MLS group invite (unaccepted welcome).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInvite {
    /// Welcome event ID (the rumor ID — pass this to accept_invite).
    pub welcome_event_id: String,
    /// Wrapper event ID (the gift-wrap event).
    pub wrapper_event_id: String,
    /// Wire group ID (the `h` tag used on relays).
    pub group_id: String,
    pub group_name: String,
    pub group_description: Option<String>,
    pub image_hash: Option<String>,
    pub image_key: Option<String>,
    pub image_nonce: Option<String>,
    /// Admin npubs (bech32).
    pub admin_npubs: Vec<String>,
    /// Group relay URLs.
    pub relays: Vec<String>,
    /// Inviter's npub (bech32).
    pub welcomer_npub: String,
    pub member_count: u32,
    /// Invite sent timestamp (from the welcome event).
    pub created_at: u64,
}

/// List all pending MLS invites. Deduplicated by group_id (most recent kept).
pub async fn list_invites() -> Result<Vec<PendingInvite>, MlsError> {
    let result = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static()?;
            let engine = mls.engine()?;

            let pending = engine.get_pending_welcomes(None)
                .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;

            let mut out: Vec<PendingInvite> = Vec::with_capacity(pending.len());
            for w in pending {
                let image_hash = w.group_image_hash.map(|h| bytes_to_hex_string(&h));
                let image_key = w.group_image_key.as_ref().map(|k| bytes_to_hex_string(k.as_ref()));
                let image_nonce = w.group_image_nonce.as_ref().map(|n| bytes_to_hex_string(n.as_ref()));
                let welcomer_npub = w.welcomer.to_bech32()
                    .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;

                out.push(PendingInvite {
                    welcome_event_id: w.id.to_hex(),
                    wrapper_event_id: w.wrapper_event_id.to_hex(),
                    group_id: bytes_to_hex_string(&w.nostr_group_id),
                    group_name: w.group_name.clone(),
                    group_description: if w.group_description.is_empty() {
                        None
                    } else {
                        Some(w.group_description.clone())
                    },
                    image_hash,
                    image_key,
                    image_nonce,
                    admin_npubs: w.group_admin_pubkeys.iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect(),
                    relays: w.group_relays.iter().map(|r| r.to_string()).collect(),
                    welcomer_npub,
                    member_count: w.member_count,
                    created_at: w.event.created_at.as_secs(),
                });
            }

            // Dedup by group_id — keep most recent
            let mut deduped: std::collections::HashMap<String, PendingInvite> =
                std::collections::HashMap::new();
            for invite in out {
                let gid = invite.group_id.clone();
                match deduped.get(&gid) {
                    Some(existing) if existing.created_at >= invite.created_at => {}
                    _ => { deduped.insert(gid, invite); }
                }
            }

            Ok::<Vec<PendingInvite>, MlsError>(deduped.into_values().collect())
        })
    })
    .await
    .map_err(|e| MlsError::NostrMlsError(format!("Task join error: {}", e)))??;

    Ok(result)
}

/// Accept a pending MLS invite. Returns the wire group_id.
///
/// Flow:
/// 1. MDK `accept_welcome()` — joins the group in the MLS engine
/// 2. Persist group metadata to DB
/// 3. Create chat in STATE
/// 4. Sync participants from engine
/// 5. Initial message sync (48h window via sync_group_since_cursor)
pub async fn accept_invite(welcome_event_id: &str) -> Result<String, MlsError> {
    let welcome_id_str = welcome_event_id.to_string();

    let nostr_group_id = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static()?;

            // Phase 1: MDK accept (engine in no-await scope since it's non-Send)
            let (nostr_group_id, engine_group_id, group_name, group_description,
                 welcomer_hex, invite_sent_at) = {
                let engine = mls.engine()?;

                let id = EventId::from_hex(&welcome_id_str)
                    .map_err(|e| MlsError::NostrMlsError(format!("Invalid welcome_event_id: {}", e)))?;
                let welcome = engine.get_welcome(&id)
                    .map_err(|e| MlsError::NostrMlsError(e.to_string()))?
                    .ok_or_else(|| MlsError::NostrMlsError("Welcome not found".to_string()))?;

                let nostr_group_id_bytes = welcome.nostr_group_id.clone();
                let group_name = welcome.group_name.clone();
                let group_description = if welcome.group_description.is_empty() {
                    None
                } else {
                    Some(welcome.group_description.clone())
                };
                let welcomer_hex = welcome.welcomer.to_hex();
                let invite_sent_at = welcome.event.created_at.as_secs();

                engine.accept_welcome(&welcome)
                    .map_err(|e| MlsError::NostrMlsError(format!("accept_welcome failed: {}", e)))?;

                let nostr_group_id = bytes_to_hex_string(&nostr_group_id_bytes);

                // Find our engine_group_id for the freshly joined group
                let engine_group_id = {
                    let groups = engine.get_groups()
                        .map_err(|e| MlsError::NostrMlsError(format!("get_groups failed: {}", e)))?;
                    groups.iter()
                        .find(|g| bytes_to_hex_string(&g.nostr_group_id) == nostr_group_id)
                        .map(|g| bytes_to_hex_string(g.mls_group_id.as_slice()))
                        .unwrap_or_else(|| nostr_group_id.clone())
                };

                (nostr_group_id, engine_group_id, group_name, group_description,
                 welcomer_hex, invite_sent_at)
            }; // engine dropped here

            // Phase 2: Persist group metadata (async-safe)
            let mut groups = mls.read_groups()?;
            let existing_index = groups.iter().position(|g| g.group_id == nostr_group_id);

            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| MlsError::NostrMlsError(e.to_string()))?
                .as_secs();

            let metadata = if let Some(idx) = existing_index {
                // Re-invited to existing/evicted group
                if groups[idx].evicted {
                    groups[idx].evicted = false;
                    groups[idx].profile.name = group_name.clone();
                    groups[idx].profile.description = group_description.clone();
                    groups[idx].engine_group_id = engine_group_id.clone();
                    groups[idx].created_at = invite_sent_at;
                    groups[idx].updated_at = now_secs;
                    crate::db::mls::save_mls_group(&groups[idx])
                        .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;
                    emit_group_metadata_event(&groups[idx]);
                }
                groups[idx].clone()
            } else {
                let meta = MlsGroupFull {
                    group: MlsGroup {
                        group_id: nostr_group_id.clone(),
                        engine_group_id: engine_group_id.clone(),
                        creator_pubkey: welcomer_hex,
                        created_at: invite_sent_at,
                        updated_at: now_secs,
                        evicted: false,
                    },
                    profile: MlsGroupProfile {
                        name: group_name,
                        description: group_description,
                        avatar_ref: None,
                        avatar_cached: None,
                    },
                };
                crate::db::mls::save_mls_group(&meta)
                    .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;
                emit_group_metadata_event(&meta);
                meta
            };

            // Ensure chat exists in STATE (idempotent — covers fresh join,
            // re-invite-after-leave, and cold-start-with-partial-state).
            {
                let mut state = crate::state::STATE.lock().await;
                let chat_id = state.create_or_get_mls_group_chat(&nostr_group_id, vec![]);
                if let Some(chat) = state.get_chat_mut(&chat_id) {
                    chat.metadata.set_name(metadata.profile.name.clone());
                }
            }

            // Phase 3: Sync participants + initial message sync
            if let Err(e) = mls.sync_group_participants(&nostr_group_id).await {
                log_warn!("[MLS] Post-accept participant sync failed: {}", e);
            }

            if let Err(e) = mls.sync_group_since_cursor(&nostr_group_id, None).await {
                log_warn!("[MLS] Post-accept initial sync failed: {}", e);
            }

            Ok::<String, MlsError>(nostr_group_id)
        })
    })
    .await
    .map_err(|e| MlsError::NostrMlsError(format!("Task join error: {}", e)))??;

    Ok(nostr_group_id)
}

/// Decline a pending invite (drops it from MDK without joining).
pub async fn decline_invite(welcome_event_id: &str) -> Result<(), MlsError> {
    let welcome_id_str = welcome_event_id.to_string();

    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static()?;
            let engine = mls.engine()?;

            let id = EventId::from_hex(&welcome_id_str)
                .map_err(|e| MlsError::NostrMlsError(format!("Invalid welcome_event_id: {}", e)))?;
            let welcome = engine.get_welcome(&id)
                .map_err(|e| MlsError::NostrMlsError(e.to_string()))?
                .ok_or_else(|| MlsError::NostrMlsError("Welcome not found".to_string()))?;

            engine.decline_welcome(&welcome)
                .map_err(|e| MlsError::NostrMlsError(format!("decline_welcome failed: {}", e)))?;

            Ok::<(), MlsError>(())
        })
    })
    .await
    .map_err(|e| MlsError::NostrMlsError(format!("Task join error: {}", e)))??;

    Ok(())
}
