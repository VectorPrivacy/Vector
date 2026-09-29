//! The account's own synced lists: hydrated at session start, then followed live
//! so an edit on another device lands here without a reload.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde_json::json;
use vector_core::community::transport::LiveTransport;
use vector_core::stored_event::event_kind::APPLICATION_SPECIFIC;
use vector_core::synced_prefs::{self, IdList, NicknameMap, NotifyMap, Pref};
use vector_core::{db, notify, STATE};

use crate::sync::WebProfileSyncHandler;

thread_local! {
    static LAST_EVENT: RefCell<HashMap<String, EventId>> = RefCell::new(HashMap::new());
}

pub async fn start() {
    hydrate().await;
    subscribe().await;
}

async fn hydrate() {
    let Some(client) = vector_core::state::nostr_client() else { return };
    for (pref, json) in synced_prefs::hydrate_all(&client).await {
        apply(pref, &json).await;
    }
}

async fn apply(pref: Pref, json: &str) {
    match pref {
        Pref::Blocks => apply_blocks(IdList::from_json(json)).await,
        Pref::Mutes => apply_mutes(IdList::from_json(json)).await,
        Pref::Nicknames => apply_nicknames(NicknameMap::from_json(json)).await,
        Pref::Notify => {
            if matches!(notify::apply_wire(&NotifyMap::from_json(json)), Ok(moved) if !moved.is_empty()) {
                reconcile_local().await;
            }
        }
        Pref::Rail => vector_core::traits::emit_event_json(
            "rail_layout_updated",
            serde_json::to_value(vector_core::rail_layout::RailLayout::from_json(json)).unwrap_or_default(),
        ),
    }
}

/// Mirror resolved mutes into the chat rows and reseed unread; a remote copy is never republished.
async fn reconcile_local() {
    crate::network_ops::reconcile_mirror().await;
    let counts = db::events::unread_counts().await.unwrap_or_default();
    STATE.lock().await.unread_seed(counts);
    crate::network_ops::arm_expiry_timer();
}

async fn apply_blocks(list: IdList) {
    let current: Vec<String> = vector_core::profile::sync::get_blocked_users().await.into_iter().map(|p| p.id).collect();
    for npub in list.ids.iter().filter(|n| !current.contains(n)) {
        vector_core::profile::sync::block_user(npub.clone(), &WebProfileSyncHandler).await;
    }
    for npub in current.iter().filter(|n| !list.contains(n)) {
        vector_core::profile::sync::unblock_user(npub.clone(), &WebProfileSyncHandler).await;
    }
    let counts = db::events::unread_counts().await.unwrap_or_default();
    STATE.lock().await.unread_seed(counts);
}

async fn apply_mutes(list: IdList) {
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
            .map(|c| (c.id.clone(), notify::legacy_muted_for_chat(c), notify::prefs(&c.id).mute_until != notify::MUTE_OFF))
            .collect()
    };
    let mut changed = false;
    for (chat_id, already, owns_its_mute) in chats {
        let want = list.contains(&chat_id);
        if want == already {
            continue;
        }
        let mute = if want { Some(notify::MUTE_FOREVER) } else { owns_its_mute.then_some(notify::MUTE_OFF) };
        if let Some(until) = mute {
            changed |= notify::set_mute(&chat_id, until).is_ok();
        }
    }
    if changed {
        reconcile_local().await;
    }
}

async fn apply_nicknames(map: NicknameMap) {
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
            vector_core::profile::sync::set_nickname(npub.clone(), nick.clone(), &WebProfileSyncHandler).await;
        }
    }
    for (npub, _) in existing.iter().filter(|(n, _)| !map.names.contains_key(n)) {
        vector_core::profile::sync::set_nickname(npub.clone(), String::new(), &WebProfileSyncHandler).await;
    }
}

async fn subscribe() {
    let (Some(client), Some(me)) = (vector_core::state::nostr_client(), vector_core::my_public_key()) else { return };
    let lists = Filter::new().author(me).kind(Kind::Custom(APPLICATION_SPECIFIC)).identifiers([
        vector_core::community::list::COMMUNITY_LIST_D_TAG.to_string(),
        vector_core::community::invite_list::INVITE_LIST_D_TAG.to_string(),
        vector_core::pinned_chats::PINNED_D_TAG.to_string(),
        synced_prefs::BLOCKS_D_TAG.to_string(),
        synced_prefs::MUTES_D_TAG.to_string(),
        synced_prefs::NICKNAMES_D_TAG.to_string(),
        synced_prefs::NOTIFY_D_TAG.to_string(),
        synced_prefs::RAIL_D_TAG.to_string(),
    ]);
    let v2_list = Filter::new().author(me).kind(Kind::Custom(vector_core::community::v2::kind::COMMUNITY_LIST_FRAG));
    let emoji = Filter::new().author(me).kind(Kind::Custom(10030));
    let mut subs = Vec::new();
    for filter in [lists, v2_list, emoji] {
        match client.subscribe(filter).await {
            Ok(out) => subs.push(out.value),
            Err(e) => vector_core::log_warn!("[self-sync] subscribe failed: {e:?}"),
        }
    }

    let mut notifications = client.notifications();
    db::spawn_bound(async move {
        while let Some(n) = futures_util::StreamExt::next(&mut notifications).await {
            if let ClientNotification::Event { event, subscription_id, .. } = n {
                if subs.contains(&subscription_id) {
                    handle(*event).await;
                }
            }
        }
    });
}

async fn handle(event: Event) {
    let key = match event.tags.identifier() {
        Some(d) => format!("{}:{d}", event.kind.as_u16()),
        None => event.kind.as_u16().to_string(),
    };
    if LAST_EVENT.with(|l| l.borrow_mut().insert(key, event.id)) == Some(event.id) {
        return;
    }
    let Some(me) = vector_core::my_public_key() else { return };
    match event.kind.as_u16() {
        k if k == APPLICATION_SPECIFIC => {
            let d = event.tags.identifier().unwrap_or_default().to_string();
            if d == vector_core::pinned_chats::PINNED_D_TAG {
                if let Ok(list) = vector_core::pinned_chats::ingest_remote_event(&me, &event).await {
                    vector_core::traits::emit_event_json("pinned_chats_updated", json!(list.chats));
                }
            } else if Pref::from_d_tag(&d).is_some() {
                if let Some((pref, json)) = synced_prefs::ingest_remote(&me, &event).await {
                    apply(pref, &json).await;
                }
            }
        }
        k if k == vector_core::community::v2::kind::COMMUNITY_LIST_FRAG => ingest_v2_community_list().await,
        10030 => {
            let _ = vector_core::emoji_packs::refresh_subscribed_packs().await;
        }
        _ => {}
    }
}

/// Another device joined or left a community: follow it here.
async fn ingest_v2_community_list() {
    use vector_core::community::v2::service as v2;
    let Some(client) = vector_core::state::nostr_client() else { return };
    let bootstrap: Vec<String> = client.relays().await.keys().map(|r| r.to_string()).collect();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let Ok(outcome) = v2::sync_community_list(&transport, &bootstrap).await else { return };
    for (community_id, channel_ids) in &outcome.removed {
        crate::community_ops::teardown_local(community_id, channel_ids, &[]).await;
        vector_core::emit_event("community_kicked", &json!({ "community_id": community_id }));
    }
    if matches!(db::community::purge_pending_invites_for_held_communities(), Ok(n) if n > 0) {
        vector_core::emit_event("community_invites_purged", &json!({}));
    }
    for c in &outcome.joined {
        vector_core::community::v2::realtime::enqueue_follow(c.id());
        vector_core::emit_event("community_surfaced", &crate::community::summarize_v2(c));
    }
    vector_core::community::v2::realtime::refresh_subscription(&client).await;
}
