//! The account's own lists that sync across devices, and the profile actions that change them.
//! Each synced list is a projection of local state, republished whenever that state changes.

use serde_json::json;
use vector_core::db;
use vector_core::notify;
use vector_core::state::STATE;
use vector_core::synced_prefs::{self, IdList, NicknameMap, Pref};

/// Publish `pref`'s projection behind the caller: the UI has already moved.
pub fn publish_projection(pref: Pref) {
    db::spawn_bound(async move {
        let Some(client) = vector_core::state::nostr_client() else { return };
        // Never publish a list not yet reconciled with the relay copy.
        if !synced_prefs::is_hydrated(pref) {
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
                // The same predicate `apply_mutes` reads with, so an inherited mute stays inherited.
                let mut l = IdList::default();
                let state = STATE.lock().await;
                for c in state.chats.iter().filter(|c| notify::legacy_muted_for_chat(c)) {
                    let _ = l.add(&c.id);
                }
                drop(state);
                for id in db::v1_purge::purged_muted_chats() {
                    let _ = l.add(&id);
                }
                l.to_json()
            }
            Pref::Notify => notify::to_wire().to_json(),
            Pref::Nicknames => {
                let mut m = NicknameMap::default();
                let state = STATE.lock().await;
                for p in state.profiles.iter().filter(|p| !p.nickname().is_empty()) {
                    if let Some(npub) = state.interner.resolve(p.id) {
                        let _ = m.set(npub, p.nickname());
                    }
                }
                drop(state);
                m.to_json()
            }
            _ => return,
        };
        if let Err(e) = synced_prefs::publish_raw(&client, pref, &json).await {
            eprintln!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag());
        }
    });
}

/// Bring `chats.muted` in line with what the notify chain resolves to, persisting and
/// surfacing only the rows that moved.
async fn reconcile_mirror() {
    // An unloaded cache answers "default" for every scope: acting on it would clear every mute.
    if !notify::is_loaded() {
        notify::warm();
        if !notify::is_loaded() {
            return;
        }
    }
    let (changed, slims, resolved) = {
        let mut state = STATE.lock().await;
        let now = notify::now_ms();
        let mut moved: Vec<(String, bool)> = Vec::new();
        let mut rows = Vec::with_capacity(state.chats.len());
        for i in 0..state.chats.len() {
            let want = notify::resolve(&state.chats[i].id, state.chats[i].metadata.community_id(), now);
            if state.chats[i].muted != want.muted {
                state.chats[i].muted = want.muted;
                moved.push((state.chats[i].id.clone(), want.muted));
            }
            rows.push(json!({
                "id": state.chats[i].id,
                "muted": want.muted,
                "notify": want.ring.as_u8(),
                "everyone": notify::everyone_pings_for_chat(&state.chats[i]),
                "community_muted": notify::community_muted_for_chat(&state.chats[i]),
            }));
        }
        let slims: Vec<_> = state
            .chats
            .iter()
            .filter(|c| moved.iter().any(|(id, _)| id == &c.id))
            .map(|c| db::chats::SlimChatDB::from_chat(c, &state.interner))
            .collect();
        (moved, slims, rows)
    };
    for slim in slims {
        let _ = db::chats::save_slim_chat(&slim);
    }
    for (chat_id, muted) in &changed {
        vector_core::traits::emit_event_json("chat_muted", json!({ "chat_id": chat_id, "value": muted }));
    }
    vector_core::traits::emit_event_json("notify_prefs_changed", json!({ "chats": resolved }));
}

async fn reseed_unread() {
    let counts = db::events::unread_counts().await.unwrap_or_default();
    STATE.lock().await.unread_seed(counts);
}

/// Mute or unmute a chat for good; the new state.
pub async fn toggle_mute(chat_id: String) -> Option<bool> {
    db::scoped(async move {
        {
            let mut state = STATE.lock().await;
            // Muting someone known only from a community: the DM row holds the mute.
            if state.get_chat(&chat_id).is_none() {
                if !chat_id.starts_with("npub1") {
                    return None;
                }
                state.create_dm_chat(&chat_id);
            }
        }
        let now = notify::now_ms();
        let muted = !notify::prefs(&chat_id).muted_at(now);
        let until = if muted { notify::MUTE_FOREVER } else { notify::MUTE_OFF };
        notify::set_mute(&chat_id, until).ok()?;
        reconcile_mirror().await;
        reseed_unread().await;
        publish_projection(Pref::Notify);
        publish_projection(Pref::Mutes);
        Some(muted)
    })
    .await
}

pub async fn block(npub: String) -> bool {
    let ok = vector_core::profile::sync::block_user(npub, &super::sync::ProfileHandler).await;
    if ok {
        reseed_unread().await;
        publish_projection(Pref::Blocks);
    }
    ok
}

pub async fn unblock(npub: String) -> bool {
    let ok = vector_core::profile::sync::unblock_user(npub, &super::sync::ProfileHandler).await;
    if ok {
        reseed_unread().await;
        publish_projection(Pref::Blocks);
    }
    ok
}

pub async fn set_nickname(npub: String, nickname: String) -> bool {
    let ok = vector_core::profile::sync::set_nickname(npub, nickname, &super::sync::ProfileHandler).await;
    if ok {
        publish_projection(Pref::Nicknames);
    }
    ok
}

/// Fetch a profile now, ahead of the background queue.
pub fn refresh(npub: String) {
    vector_core::profile::sync::refresh_profile_now(npub);
}

/// Let a DM request in without answering it.
pub async fn accept(npub: String) -> Result<(), String> {
    vector_core::profile::sync::accept_chat(npub).await
}

/// The request senders whose pictures show anyway: a Community we hold counts them.
pub async fn request_mutuals() -> Vec<String> {
    vector_core::requests::request_mutuals().await
}
