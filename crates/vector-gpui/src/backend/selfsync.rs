//! Cross-device sync for the account's own lists: one open subscription to them (the relay
//! replays the stored copy on connect and reconnect, then streams edits), and a reconcile of
//! the synced prefs at login, before the user can change them.

use nostr_sdk::prelude::*;
use vector_core::synced_prefs::{self, ArchiveMap, IdList, NicknameMap, NotifyMap, Pref, SyncedSettings};
use vector_core::{STATE, db, notify, pinned_chats, rail_layout::RailLayout};

/// Subscribe, reconcile, then route what arrives for the session's life.
pub fn start() {
    db::spawn_bound(async {
        let Some(client) = vector_core::state::nostr_client() else { return };
        let Some(me) = vector_core::my_public_key() else { return };
        let mut notifications = client.notifications();
        let ids = subscribe(&client, me).await;
        hydrate().await;
        let mut last: std::collections::HashMap<String, EventId> = std::collections::HashMap::new();
        loop {
            let Some(n) = futures::StreamExt::next(&mut notifications).await else { break };
            let ClientNotification::Event { subscription_id, event, .. } = n else { continue };
            if !ids.contains(&subscription_id) || event.pubkey != me {
                continue;
            }
            // Several relays deliver one edit: one update, one sweep.
            let key = match event.tags.identifier() {
                Some(d) => format!("{}:{d}", event.kind.as_u16()),
                None => event.kind.as_u16().to_string(),
            };
            if last.get(&key) == Some(&event.id) {
                continue;
            }
            last.insert(key, event.id);
            route(*event);
            if db::session_stopped() {
                break;
            }
        }
    });
}

async fn subscribe(client: &Client, me: PublicKey) -> Vec<SubscriptionId> {
    let filters = [
        Filter::new()
            .author(me)
            .kind(Kind::Custom(vector_core::stored_event::event_kind::APPLICATION_SPECIFIC))
            .identifiers(synced_prefs::live_d_tags()),
        Filter::new().author(me).kind(Kind::Custom(vector_core::community::v2::kind::COMMUNITY_LIST_FRAG)),
        Filter::new().author(me).kind(Kind::Custom(10030)),
    ];
    let mut ids = Vec::new();
    for f in filters {
        match client.subscribe(f.clone()).await {
            Ok(out) => {
                vector_core::live_subs::keep_output(&out, vec![f]);
                ids.push(out.value);
            }
            Err(e) => vector_core::log_warn!("[self-sync] subscribe failed: {e:?}"),
        }
    }
    ids
}

fn route(event: Event) {
    let kind = event.kind.as_u16();
    if kind == vector_core::stored_event::event_kind::APPLICATION_SPECIFIC {
        let d = event.tags.identifier().unwrap_or_default().to_string();
        db::spawn_bound(async move {
            if d == pinned_chats::PINNED_D_TAG {
                let Some(me) = vector_core::my_public_key() else { return };
                match pinned_chats::ingest_remote_event(&me, &event).await {
                    Ok(list) => emit_pinned(&list.chats),
                    Err(e) => vector_core::log_warn!("[PinnedChats] ingest failed: {e}"),
                }
            } else if Pref::from_d_tag(&d).is_some() {
                ingest_prefs(event).await;
            }
        });
    } else if kind == vector_core::community::v2::kind::COMMUNITY_LIST_FRAG {
        super::community::request();
    } else if kind == 10030 {
        db::spawn_bound(async {
            if vector_core::emoji_packs::refresh_subscribed_packs().await.is_ok() {
                vector_core::emit_event("emoji_packs_updated", &());
            }
        });
    }
}

pub fn emit_pinned(chats: &[String]) {
    vector_core::traits::emit_event_json("pinned_chats_updated", serde_json::json!(chats));
}

pub fn emit_archive(map: &ArchiveMap) {
    vector_core::traits::emit_event_json("archived_chats_updated", serde_json::json!(map.times()));
}

/// Reconcile the synced lists at login. The subscription delivers them too, but it races the
/// user: acting first would publish this device's emptier view over a sibling's.
async fn hydrate() {
    let Some(client) = vector_core::state::nostr_client() else { return };
    for (pref, json) in synced_prefs::hydrate_all(&client).await {
        apply(pref, &json).await;
    }
}

async fn ingest_prefs(event: Event) {
    let Some(me) = vector_core::my_public_key() else { return };
    let Some((pref, json)) = synced_prefs::ingest_remote(&me, &event).await else { return };
    apply(pref, &json).await;
}

async fn apply(pref: Pref, json: &str) {
    match pref {
        Pref::Blocks => apply_blocks(IdList::from_json(json)).await,
        Pref::Mutes => apply_mutes(IdList::from_json(json)).await,
        Pref::Nicknames => apply_nicknames(NicknameMap::from_json(json)).await,
        Pref::Notify => match notify::apply_wire(&NotifyMap::from_json(json)) {
            Ok(moved) if moved.is_empty() => {}
            Ok(_) => super::prefs::reconcile_local().await,
            Err(e) => vector_core::log_warn!("[SyncedPrefs] applying notify failed: {e}"),
        },
        Pref::Rail => emit_layout("rail_layout_updated", json),
        Pref::Start => emit_layout("start_layout_updated", json),
        Pref::Banners => vector_core::traits::emit_event_json(
            "hidden_banners_updated",
            serde_json::json!({ "ids": IdList::from_json(json).ids }),
        ),
        Pref::Archive => emit_archive(&ArchiveMap::from_json(json)),
        Pref::Settings => {
            vector_core::traits::emit_event_json("synced_settings_updated", SyncedSettings::from_json(json).view())
        }
    }
}

fn emit_layout(event: &str, json: &str) {
    vector_core::traits::emit_event_json(event, serde_json::to_value(RailLayout::from_json(json)).unwrap_or_default());
}

/// Block what the list names, unblock what it omits, through the core mutators.
async fn apply_blocks(list: IdList) {
    let handler = &super::sync::ProfileHandler;
    let currently: Vec<String> =
        vector_core::profile::sync::get_blocked_users().await.into_iter().map(|p| p.id).collect();
    for npub in list.ids.iter().filter(|n| !currently.contains(n)) {
        if vector_core::profile::sync::block_user(npub.clone(), handler).await {
            super::notify::blocked(npub);
        }
    }
    for npub in currently.iter().filter(|n| !list.contains(n)) {
        vector_core::profile::sync::unblock_user(npub.clone(), handler).await;
    }
    super::prefs::reconcile_local().await;
}

/// Mirror the mute list onto chat rows. It names chats only, so a community-scope mute survives.
async fn apply_mutes(list: IdList) {
    vector_core::db::v1_purge::retain_purged_mutes(|id| list.contains(id));
    let chats: Vec<(String, bool, bool)> = {
        let mut state = STATE.lock().await;
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
    let mut changed = false;
    for (id, already, owns) in chats {
        let want = list.contains(&id);
        if want == already {
            continue;
        }
        if want {
            changed |= notify::set_mute(&id, notify::MUTE_FOREVER).is_ok();
        } else if owns {
            changed |= notify::set_mute(&id, notify::MUTE_OFF).is_ok();
        }
    }
    if changed {
        super::prefs::reconcile_local().await;
    }
}

/// Set what the map names, clear what it omits.
async fn apply_nicknames(map: NicknameMap) {
    let handler = &super::sync::ProfileHandler;
    let existing: Vec<(String, String)> = {
        let state = STATE.lock().await;
        state
            .profiles
            .iter()
            .filter(|p| !p.nickname().is_empty())
            .filter_map(|p| state.interner.resolve(p.id).map(|n| (n.to_string(), p.nickname().to_string())))
            .collect()
    };
    for (npub, nick) in map.names.iter() {
        if !existing.iter().any(|(n, v)| n == npub && v == nick) {
            vector_core::profile::sync::set_nickname(npub.clone(), nick.clone(), handler).await;
        }
    }
    for (npub, _) in existing.iter().filter(|(n, _)| !map.names.contains_key(n)) {
        vector_core::profile::sync::set_nickname(npub.clone(), String::new(), handler).await;
    }
}
