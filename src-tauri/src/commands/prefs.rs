//! Cross-device sync for blocks, mutes and nicknames.
//!
//! Local state stays the single source of truth on this device; each synced
//! list is a PROJECTION of it, republished whenever it changes. That is what
//! makes newest-wins coherent — there is no second copy to drift from — and it
//! means a list arriving from another device is applied by mirroring it onto
//! the local flags, not merged into a parallel structure.

use nostr_sdk::prelude::Event;
use vector_core::synced_prefs::{self, IdList, NicknameMap, Pref};

/// Publish a projection of the current local state for `pref`. Runs behind the
/// caller's return: these are triggered by user actions whose UI has already
/// updated, and a slow relay must not hold the action open.
pub fn publish_projection(pref: Pref) {
    vector_core::db::spawn_bound(async move {
        let Some(client) = vector_core::state::nostr_client() else { return };
        // Never publish a list we have not reconciled: this device's local state
        // is only the whole truth once the relay copy has been applied.
        if !synced_prefs::is_hydrated(pref) {
            eprintln!("[SyncedPrefs] {} not reconciled yet — not publishing over it", pref.d_tag());
            return;
        }
        let json = match pref {
            Pref::Blocks => {
                let mut l = IdList::default();
                for p in vector_core::profile::sync::get_blocked_users().await {
                    let _ = l.add(&p.id);
                }
                l.to_json()
            }
            Pref::Mutes => {
                let mut l = IdList::default();
                let state = vector_core::state::STATE.lock().await;
                for c in state.chats.iter().filter(|c| c.muted) {
                    let _ = l.add(&c.id);
                }
                drop(state);
                l.to_json()
            }
            Pref::Nicknames => {
                let mut m = NicknameMap::default();
                let state = vector_core::state::STATE.lock().await;
                for p in state.profiles.iter().filter(|p| !p.nickname().is_empty()) {
                    if let Some(npub) = state.interner.resolve(p.id) {
                        let _ = m.set(npub, p.nickname());
                    }
                }
                drop(state);
                m.to_json()
            }
        };
        if let Err(e) = synced_prefs::publish_raw(&client, pref, &json).await {
            eprintln!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag());
        }
    });
}

/// Reconcile all three lists at login and apply them, BEFORE the user can
/// change anything. The live subscription delivers these too, but it races the
/// user; this does not.
pub async fn hydrate_prefs() {
    let Some(client) = vector_core::state::nostr_client() else { return };
    for (pref, json) in synced_prefs::hydrate_all(&client).await {
        match pref {
            Pref::Blocks => apply_blocks(IdList::from_json(&json)).await,
            Pref::Mutes => apply_mutes(IdList::from_json(&json)).await,
            Pref::Nicknames => apply_nicknames(NicknameMap::from_json(&json)).await,
        }
    }
}

/// A sibling device changed one of the lists: mirror it onto local state.
pub async fn ingest_prefs_update(event: Event) {
    let Some(my_pk) = vector_core::my_public_key() else { return };
    let Some((pref, json)) = synced_prefs::ingest_remote(&my_pk, &event).await else { return };
    match pref {
        Pref::Blocks => apply_blocks(IdList::from_json(&json)).await,
        Pref::Mutes => apply_mutes(IdList::from_json(&json)).await,
        Pref::Nicknames => apply_nicknames(NicknameMap::from_json(&json)).await,
    }
}

/// Mirror the block list: block what it names, unblock what it does not. Goes
/// through the core mutators so flags, DB rows and unread counts stay in step.
async fn apply_blocks(list: IdList) {
    let handler = &crate::profile_sync::TauriProfileSyncHandler;
    let currently: Vec<String> = vector_core::profile::sync::get_blocked_users()
        .await
        .into_iter()
        .map(|p| p.id)
        .collect();
    for npub in list.ids.iter() {
        if !currently.contains(npub) {
            vector_core::profile::sync::block_user(npub.clone(), handler).await;
        }
    }
    for npub in currently.iter().filter(|n| !list.contains(n)) {
        vector_core::profile::sync::unblock_user(npub.clone(), handler).await;
    }
    // Blocks change other chats' counts (SQL sender exclusion) — reseed, then re-badge.
    let counts = crate::db::unread_counts().await.unwrap_or_default();
    vector_core::state::STATE.lock().await.unread_seed(counts);
    if let Some(handle) = crate::TAURI_APP.get() {
        crate::commands::messaging::update_unread_counter(handle.clone()).await;
    }
}

/// Mirror the mute list onto chat rows, persisting and surfacing only the ones
/// that actually flipped.
async fn apply_mutes(list: IdList) {
    let (changed, slims) = {
        let mut state = vector_core::state::STATE.lock().await;
        // A sibling device can mute someone this device has never DM'd: create
        // the DM row so the mute has somewhere to live (and so this device's own
        // projection republishes them instead of erasing the mute fleet-wide).
        for id in list.ids.iter().filter(|id| id.starts_with("npub1")) {
            if state.get_chat(id).is_none() {
                state.create_dm_chat(id);
            }
        }
        let mut out = Vec::new();
        for chat in state.chats.iter_mut() {
            let want = list.contains(&chat.id);
            if chat.muted != want {
                chat.muted = want;
                out.push((chat.id.clone(), want));
            }
        }
        let slims: Vec<_> = state
            .chats
            .iter()
            .filter(|c| out.iter().any(|(id, _)| id == &c.id))
            .map(|c| crate::db::chats::SlimChatDB::from_chat(c, &state.interner))
            .collect();
        (out, slims)
    };
    let any_changed = !changed.is_empty();
    for slim in slims {
        let _ = crate::db::chats::save_slim_chat(slim).await;
    }
    for (chat_id, muted) in changed {
        vector_core::traits::emit_event_json(
            "chat_muted",
            serde_json::json!({ "chat_id": chat_id, "value": muted }),
        );
    }
    // Sender-level mutes change other chats' counts — reseed, then re-badge.
    if any_changed {
        let counts = crate::db::unread_counts().await.unwrap_or_default();
        vector_core::state::STATE.lock().await.unread_seed(counts);
        if let Some(handle) = crate::TAURI_APP.get() {
            crate::commands::messaging::update_unread_counter(handle.clone()).await;
        }
    }
}

/// Mirror nicknames: set what the map names, clear what it omits.
async fn apply_nicknames(map: NicknameMap) {
    let handler = &crate::profile_sync::TauriProfileSyncHandler;
    let existing: Vec<(String, String)> = {
        let state = vector_core::state::STATE.lock().await;
        state
            .profiles
            .iter()
            .filter(|p| !p.nickname().is_empty())
            .filter_map(|p| state.interner.resolve(p.id).map(|n| (n.to_string(), p.nickname().to_string())))
            .collect()
    };
    for (npub, nick) in map.names.iter() {
        if existing.iter().any(|(n, v)| n == npub && v == nick) {
            continue;
        }
        vector_core::profile::sync::set_nickname(npub.clone(), nick.clone(), handler).await;
    }
    for (npub, _) in existing.iter().filter(|(n, _)| !map.names.contains_key(n)) {
        vector_core::profile::sync::set_nickname(npub.clone(), String::new(), handler).await;
    }
}
