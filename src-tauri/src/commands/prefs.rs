//! Cross-device sync for the account's own lists: blocks, mutes, nicknames, notification
//! levels, banners, the archive and the general settings.
//!
//! Local state stays the single source of truth on this device; each synced
//! list is a PROJECTION of it, republished whenever it changes. That is what
//! makes newest-wins coherent — there is no second copy to drift from — and it
//! means a list arriving from another device is applied by mirroring it onto
//! the local flags, not merged into a parallel structure.

use nostr_sdk::prelude::Event;
use vector_core::notify;
use vector_core::synced_prefs::{self, ArchiveMap, IdList, NicknameMap, NotifyMap, Pref, SyncedSettings};

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
        if pref == Pref::Settings {
            let was_live = synced_prefs::load_settings().streamer.on;
            match synced_prefs::publish_settings(&client, true).await {
                Ok(Some(json)) => settings_changed(was_live, &SyncedSettings::from_json(&json)),
                Ok(None) => {}
                Err(e) => eprintln!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag()),
            }
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
                // Written with the SAME predicate `apply_mutes` reads it with, or an
                // inherited mute round-trips into a permanent per-channel one.
                let mut l = IdList::default();
                let state = vector_core::state::STATE.lock().await;
                for c in state.chats.iter().filter(|c| notify::legacy_muted_for_chat(c)) {
                    let _ = l.add(&c.id);
                }
                drop(state);
                l.to_json()
            }
            Pref::Notify => notify::to_wire().to_json(),
            // Its own state, not a projection of another: it publishes itself.
            Pref::Rail => {
                synced_prefs::flush_rail().await;
                return;
            }
            Pref::Banners => synced_prefs::load_hidden_banners().to_json(),
            Pref::Archive => synced_prefs::load_archive().to_json(),
            Pref::Settings => return,
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
    let was_live = synced_prefs::load_settings().streamer.on;
    for (pref, json) in synced_prefs::hydrate_all(&client).await {
        match pref {
            Pref::Blocks => apply_blocks(IdList::from_json(&json)).await,
            Pref::Mutes => apply_mutes(IdList::from_json(&json)).await,
            Pref::Nicknames => apply_nicknames(NicknameMap::from_json(&json)).await,
            Pref::Notify => apply_notify(NotifyMap::from_json(&json)).await,
            Pref::Rail => crate::commands::rail::emit(&vector_core::rail_layout::RailLayout::from_json(&json)),
            Pref::Banners => emit_hidden_banners(&IdList::from_json(&json)),
            Pref::Archive => emit_archive(&ArchiveMap::from_json(&json)),
            Pref::Settings => settings_changed(was_live, &SyncedSettings::from_json(&json)),
        }
    }
}

/// A sibling device changed one of the lists: mirror it onto local state.
pub async fn ingest_prefs_update(event: Event) {
    let Some(my_pk) = vector_core::my_public_key() else { return };
    let was_live = synced_prefs::load_settings().streamer.on;
    let Some((pref, json)) = synced_prefs::ingest_remote(&my_pk, &event).await else { return };
    match pref {
        Pref::Blocks => apply_blocks(IdList::from_json(&json)).await,
        Pref::Mutes => apply_mutes(IdList::from_json(&json)).await,
        Pref::Nicknames => apply_nicknames(NicknameMap::from_json(&json)).await,
        Pref::Notify => apply_notify(NotifyMap::from_json(&json)).await,
        Pref::Rail => crate::commands::rail::emit(&vector_core::rail_layout::RailLayout::from_json(&json)),
        Pref::Banners => emit_hidden_banners(&IdList::from_json(&json)),
        Pref::Archive => emit_archive(&ArchiveMap::from_json(&json)),
        Pref::Settings => settings_changed(was_live, &SyncedSettings::from_json(&json)),
    }
}

/// Adopt a sibling's per-scope settings, then let the reconcile bring the
/// `chats.muted` mirror, the badge and the row repaints along with it.
async fn apply_notify(map: NotifyMap) {
    match notify::apply_wire(&map) {
        Ok(moved) if moved.is_empty() => {}
        Ok(_) => crate::commands::notify::reconcile_local().await,
        Err(e) => eprintln!("[SyncedPrefs] applying {} failed: {e}", Pref::Notify.d_tag()),
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
        if !currently.contains(npub) && vector_core::profile::sync::block_user(npub.clone(), handler).await {
            #[cfg(not(target_os = "android"))]
            crate::services::native_notify::remove_sender(npub);
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
    let chat_ids: Vec<(String, bool, bool)> = {
        let mut state = vector_core::state::STATE.lock().await;
        // A sibling device can mute someone this device has never DM'd: create
        // the DM row so the mute has somewhere to live (and so this device's own
        // projection republishes them instead of erasing the mute fleet-wide).
        for id in list.ids.iter().filter(|id| id.starts_with("npub1")) {
            if state.get_chat(id).is_none() {
                state.create_dm_chat(id);
            }
        }
        state
            .chats
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    notify::legacy_muted_for_chat(c),
                    notify::prefs(&c.id).mute_until != notify::MUTE_OFF,
                )
            })
            .collect()
    };

    // This list only ever names chats, so it speaks for chat scopes alone. A
    // community-scope mute has no id here and must survive a sibling that is
    // too old to know about one.
    let mut changed = false;
    for (chat_id, already, owns_its_mute) in chat_ids {
        let want = list.contains(&chat_id);
        if want == already {
            continue;
        }
        if want {
            // The list carries no deadline, so anything it adds is indefinite.
            if notify::set_mute(&chat_id, notify::MUTE_FOREVER).is_ok() {
                changed = true;
            }
        } else if owns_its_mute {
            // Inherited from its community, which this list cannot address: leave it
            // alone rather than writing a clear that changes nothing.
            if notify::set_mute(&chat_id, notify::MUTE_OFF).is_ok() {
                changed = true;
            }
        }
    }
    if changed {
        // Adopting a sibling's list must not publish back at it.
        crate::commands::notify::reconcile_local().await;
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

/// Communities whose banner this account hides, for the first paint.
#[tauri::command]
pub async fn get_hidden_banners() -> Result<Vec<String>, String> {
    vector_core::db::scoped(async move { Ok(synced_prefs::load_hidden_banners().ids) }).await
}

#[tauri::command]
pub async fn set_banner_hidden(community_id: String, hidden: bool) -> Result<Vec<String>, String> {
    vector_core::db::scoped(async move {
        let list = synced_prefs::set_banner_hidden(&community_id, hidden)?;
        emit_hidden_banners(&list);
        publish_projection(Pref::Banners);
        Ok(list.ids)
    })
    .await
}

/// Through `emit_event` so a list belonging to an account since swapped away paints nothing.
fn emit_hidden_banners(list: &IdList) {
    vector_core::traits::emit_event_json("hidden_banners_updated", serde_json::json!({ "ids": list.ids }));
}

/// Archived chats (chat id → archived at, unix ms), for the first paint.
#[tauri::command]
pub async fn get_archived_chats() -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move { Ok(serde_json::json!(synced_prefs::load_archive().times())) }).await
}

/// Archive a DM as of `at` (unix ms), or bring it back when `at` is absent.
#[tauri::command]
pub async fn set_chat_archived(chat_id: String, at: Option<u64>) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let map = synced_prefs::set_archived(&chat_id, at)?;
        emit_archive(&map);
        publish_projection(Pref::Archive);
        Ok(serde_json::json!(map.times()))
    })
    .await
}

/// A message at `active_at` (unix ms) outlived the chat's archive: forget it here. Returns the
/// archive as it now stands, so a page holding a stale entry stops asking.
#[tauri::command]
pub async fn revoke_chat_archive(chat_id: String, active_at: u64) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let (map, changed) = synced_prefs::revoke_archive(&chat_id, active_at)?;
        if changed {
            emit_archive(&map);
        }
        Ok(serde_json::json!(map.times()))
    })
    .await
}

fn emit_archive(map: &ArchiveMap) {
    vector_core::traits::emit_event_json("archived_chats_updated", serde_json::json!(map.times()));
}

/// The synced settings, for the first paint.
#[tauri::command]
pub async fn get_synced_settings() -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move { Ok(synced_prefs::load_settings().view()) }).await
}

#[tauri::command]
pub async fn set_advanced_mode(on: bool) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let settings = synced_prefs::set_advanced(on)?;
        emit_settings(&settings);
        publish_projection(Pref::Settings);
        Ok(settings.view())
    })
    .await
}

/// Takes effect at once, synced or not: switching players off must not wait for the relay copy.
#[tauri::command]
pub async fn set_embedded_players(on: bool) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let settings = synced_prefs::set_players(on)?;
        emit_settings(&settings);
        publish_projection(Pref::Settings);
        Ok(settings.view())
    })
    .await
}

/// Takes effect at once, synced or not: before hydration the relay copy adopts it later.
#[tauri::command]
pub async fn set_streamer_mode(on: bool) -> Result<serde_json::Value, String> {
    vector_core::db::scoped_result(async move {
        let was_live = synced_prefs::load_settings().streamer.on;
        let settings = synced_prefs::set_streamer_on(on)?;
        settings_changed(was_live, &settings);
        publish_projection(Pref::Settings);
        Ok(settings.view())
    })
    .await
}

#[tauri::command]
pub async fn set_streamer_notif(level: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped_result(async move {
        let settings = synced_prefs::set_streamer_notif(&level)?;
        emit_settings(&settings);
        publish_projection(Pref::Settings);
        Ok(settings.view())
    })
    .await
}

#[tauri::command]
pub async fn set_streamer_wallpapers(hide: bool) -> Result<serde_json::Value, String> {
    vector_core::db::scoped_result(async move {
        let settings = synced_prefs::set_streamer_wallpapers(hide)?;
        emit_settings(&settings);
        publish_projection(Pref::Settings);
        Ok(settings.view())
    })
    .await
}

fn settings_changed(was_live: bool, settings: &SyncedSettings) {
    emit_settings(settings);
    if settings.streamer.on && !was_live {
        clear_shown_notifications();
    }
}

/// Only Android can withdraw a notification it has already shown.
fn clear_shown_notifications() {
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_all_message_notifications_jni();
}

fn emit_settings(settings: &SyncedSettings) {
    vector_core::traits::emit_event_json("synced_settings_updated", settings.view());
}

// Handlers: get_hidden_banners, set_banner_hidden, get_archived_chats, set_chat_archived, revoke_chat_archive, get_synced_settings, set_advanced_mode,
// set_embedded_players, set_streamer_mode, set_streamer_notif, set_streamer_wallpapers
