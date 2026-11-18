//! Boot hydration, history catch-up, the live subscription and profile persistence.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use vector_core::dm_catchup::{self, Opts, Report};
use vector_core::event_handler::{BatchingPersist, commit_prepared_event};
use vector_core::{CatchUp, STATE, SlimProfile, VectorCore, db, state};

use super::{Boot, UiEvent, events::Handler};

pub struct ProfileHandler;

impl vector_core::ProfileSyncHandler for ProfileHandler {
    fn on_profile_fetched(&self, slim: &SlimProfile, avatar_url: &str, banner_url: &str) {
        let slim = slim.clone();
        let (avatar, banner) = (avatar_url.to_string(), banner_url.to_string());
        db::spawn_bound(async move {
            let _ = db::profiles::set_profile(&slim);
            super::images::cache_profile_images(&slim.id, &avatar, &banner).await;
        });
    }

    fn forget_images(&self, images: Vec<vector_core::profile::CachedImage>) {
        for img in images {
            let _ = std::fs::remove_file(&img.path);
        }
    }
}

/// Load chats and profiles from the database into state, then hand them to the UI.
async fn hydrate() {
    let Ok(npub) = db::get_current_account() else {
        return;
    };
    let mut state = STATE.lock().await;
    if !state.db_loaded {
        if let Ok(profiles) = db::profiles::get_all_profiles() {
            state.merge_db_profiles(profiles, &npub);
        }
        // Before the first paint, so a blocked profile's pictures never show.
        let (slims, images) = state.forget_blocked_imagery();
        vector_core::profile::sync::persist_forgotten_imagery(&ProfileHandler, slims, images);
        let mut last_messages = db::events::get_all_chats_last_messages()
            .await
            .unwrap_or_default();
        if let Ok(slim_chats) = db::chats::get_all_chats() {
            let mut known: std::collections::HashSet<u16> =
                state.profiles.iter().map(|p| p.id).collect();
            for slim in slim_chats {
                let mut chat = slim.to_chat(&mut state.interner);
                let chat_id = chat.id().to_string();
                for &handle in chat.participants() {
                    if known.insert(handle) {
                        if let Some(p) = state.interner.resolve(handle).map(str::to_string) {
                            state.insert_or_replace_profile(&p, vector_core::Profile::new());
                        }
                    }
                }
                if let Some(existing) = state.chats.iter_mut().find(|c| c.id == chat_id) {
                    // Made before its row loaded: take the stored read markers, or its next save
                    // would overwrite them.
                    if existing.last_read == [0u8; 32] {
                        existing.last_read = chat.last_read;
                    }
                    if existing.unread_from == [0u8; 32] {
                        existing.unread_from = chat.unread_from;
                    }
                    continue;
                }
                for message in last_messages.remove(&chat_id).unwrap_or_default() {
                    chat.internal_add_message(message, &mut state.interner);
                }
                state.chats.push(chat);
            }
            state
                .chats
                .sort_by_key(|c| std::cmp::Reverse(c.last_message_time()));
        }
        state.shrink_after_load();
        state.db_loaded = true;
        let _ = db::id_cache::preload_id_caches();
    }
    let chats = state
        .chats
        .iter()
        .map(|c| c.to_serializable(&state.interner))
        .collect();
    let profiles = state
        .profiles
        .iter()
        .map(|p| SlimProfile::from_profile(p, &state.interner))
        .collect();
    drop(state);
    super::send(UiEvent::Booted(Boot {
        npub,
        chats,
        profiles,
    }));
    db::spawn_bound(super::images::cache_all_profile_images());
}

/// Paint from the database, then catch up behind it. `init` is a login.
pub async fn fetch_messages(init: bool) {
    if init {
        super::transport::set_account_booted(true);
        vector_core::transport::prelogin::commit();
        super::notify::logged_in();
        hydrate().await;
        db::spawn_bound(super::market::preload());
        db::spawn_bound(super::mediaport::after_boot());
    }
    db::spawn_bound(async move {
        super::emit("sync_progress", serde_json::json!({ "mode": "Syncing" })); // i18n-exempt: event payload
        let synced = db::current_session().scoped::<Synced, AtomicBool>();
        synced.store(false, Ordering::SeqCst);
        if dms(false).await.reached_none() {
            watch_unreachable();
        }
        synced.store(true, Ordering::SeqCst);
        super::emit("sync_finished", ());
        if init {
            db::spawn_bound(merge_media_servers());
            db::spawn_bound(vector_core::badges::refresh_own());
            db::spawn_bound(vector_core::profile::sync::stamp_vector_client_after_sync());
        }
    });
}

/// Warn that the relays are unreachable once none has connected in a long grace: a slow
/// network is not an offline one.
fn watch_unreachable() {
    use nostr_sdk::prelude::RelayStatus;
    let Some(pool) = state::nostr_client() else { return };
    db::spawn_bound(async move {
        let grace = vector_core::relay_connect_timeout(Duration::from_secs(30));
        let deadline = std::time::Instant::now() + grace;
        loop {
            let relays = pool.relays().await;
            if relays.values().any(|r| r.status() == RelayStatus::Connected) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                vector_core::emit_event(
                    "show_toast",
                    &crate::t::network_relays_unreachable(relays.len()),
                );
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// The account's published media servers (kind 10063), merged in after the first sync.
async fn merge_media_servers() {
    use vector_core::{blossom, blossom_info, blossom_servers};
    let (Some(client), Some(me)) = (state::nostr_client(), vector_core::my_public_key()) else {
        return;
    };
    if let Err(e) = blossom_servers::fetch_and_merge_own_list(&client, me).await {
        vector_core::log_warn!("[BlossomServers] list fetch failed: {e}");
    }
    let Ok(signer) = vector_core::signer::active_signer() else {
        return;
    };
    let servers = state::get_blossom_servers();
    if blossom_info::refresh_all(signer.clone(), servers.clone(), Duration::from_secs(60)).await > 0
    {
        vector_core::emit_event("blossom_info_updated", &());
    }
    if let Ok(n) = blossom::probe_servers_for_octet_stream(signer, servers).await {
        if n > 0 {
            vector_core::emit_event("blossom_capabilities_updated", &());
        }
    }
}

/// Start the live DM subscription. Runs for the session's life.
pub fn notifs() {
    db::spawn_bound(async {
        if let Err(e) = VectorCore
            .listen_with(Arc::new(Handler), CatchUp::External)
            .await
        {
            vector_core::log_warn!("[GPUI] live subscription ended: {e}");
        }
    });
}

// ── Catch-up ────────────────────────────────────────────────────────────────

/// The login's DM sync has finished: until then it covers every relay itself.
struct Synced;
/// Relays that have connected at least once this session: only a reconnect can drop events.
struct SeenConnected;
/// Relays that came back mid-sync, caught up once it ends.
struct AfterSync;
/// When Connected last refolded the held Communities.
struct LastRefold;

fn syncing() -> bool {
    !db::current_session().scoped::<Synced, AtomicBool>().load(Ordering::SeqCst)
}

/// A relay's socket came up. What it can have missed depends on what it is to us: DMs only
/// where we read, Community planes only on a Community relay. Someone else's inbox, dialled for
/// one send, is neither.
pub fn relay_connected(url: String, has_read: bool, has_gossip: bool) {
    refold_held();
    let session = db::current_session();
    let reconnect = !session
        .scoped::<SeenConnected, std::sync::Mutex<std::collections::HashSet<String>>>()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.clone());
    let discovery_only = !has_read && {
        let norm = vector_core::inbox_relays::normalize_relay_url(&url);
        let same = |d: &str| vector_core::inbox_relays::normalize_relay_url(d) == norm;
        state::discovery_relay_iter().any(same) && !state::TRUSTED_RELAYS.iter().any(|d| same(d))
    };
    let todo = Owed::of(Seen { has_read, has_gossip, reconnect, syncing: syncing(), discovery_only });
    let after_sync = session.scoped::<AfterSync, std::sync::Mutex<std::collections::HashSet<String>>>();
    if todo.dms_after_sync && after_sync.lock().unwrap_or_else(|e| e.into_inner()).insert(url.clone()) {
        let url = url.clone();
        db::spawn_bound(async move {
            let deadline = std::time::Instant::now() + Duration::from_secs(15 * 60);
            while syncing() {
                if db::session_stopped() || std::time::Instant::now() > deadline {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            after_sync.lock().unwrap_or_else(|e| e.into_inner()).remove(&url);
            if !db::session_stopped() {
                dms_from(vec![url]).await;
            }
        });
    }
    if todo.dms_now {
        let url = url.clone();
        db::spawn_bound(async move {
            dms_from(vec![url]).await;
        });
    }
    if todo.communities {
        super::community::reconnected(url, has_read);
    }
}

/// What a relay that just came up is to us.
#[derive(Clone, Copy)]
struct Seen {
    has_read: bool,
    has_gossip: bool,
    reconnect: bool,
    syncing: bool,
    /// A discovery relay that isn't also one we read or a default: it holds no planes.
    discovery_only: bool,
}

/// What a relay that came up can have missed, and so is asked for.
#[derive(Debug, PartialEq)]
struct Owed {
    /// Its DMs once the login's sync ends: the sync may already have passed it.
    dms_after_sync: bool,
    dms_now: bool,
    /// Plane fetches dial their relays themselves, so only a reconnect drops Community events.
    communities: bool,
}

impl Owed {
    fn of(s: Seen) -> Self {
        Self {
            dms_after_sync: s.has_read && s.syncing && s.reconnect,
            dms_now: s.has_read && !s.syncing,
            communities: !s.syncing && s.reconnect && (s.has_read || s.has_gossip) && !s.discovery_only,
        }
    }
}

/// A `limit(0)` subscription never replays what a relay missed while down: each Connected
/// refolds the held Communities and re-tracks the subscription at their current epochs, once
/// per burst.
fn refold_held() {
    let last = db::current_session().scoped::<LastRefold, AtomicU64>();
    let now = vector_core::clock::now_ms();
    let before = last.load(Ordering::SeqCst);
    if now.saturating_sub(before) < 3000 || last.compare_exchange(before, now, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    db::spawn_bound(async {
        for c in vector_core::community::v2::realtime::load_held_v2() {
            vector_core::community::v2::realtime::enqueue_follow(c.id());
        }
        if let Some(c) = state::nostr_client() {
            vector_core::community::v2::realtime::refresh_subscription(&c).await;
        }
    });
}

/// What a dropped live subscription or a stretch away missed.
pub fn catch_up(what: vector_core::live_subs::CatchUp) {
    use vector_core::live_subs::CatchUp;
    db::spawn_bound(async move {
        let Some(client) = state::nostr_client() else { return };
        match what {
            CatchUp::Relay(url) => {
                let has_read = matches!(client.relay(&url).await, Ok(Some(r)) if r.capabilities().has_read());
                if has_read {
                    dms_from(vec![url.clone()]).await;
                }
                super::community::reconnected(url, has_read);
            }
            CatchUp::All { .. } => {
                if syncing() {
                    return;
                }
                for (url, relay) in client.relays().await {
                    super::community::reconnected(url.to_string(), relay.capabilities().has_read());
                }
                retrying(Opts { is_new: true, only: None, bootstrap: false }).await;
            }
        }
    });
}

/// What `relays` hold that we missed: news, so it notifies.
async fn dms_from(relays: Vec<String>) {
    retrying(Opts { is_new: true, only: Some(relays), bootstrap: false }).await;
}

async fn retrying(opts: Opts) -> Report {
    let (is_new, bootstrap) = (opts.is_new, opts.bootstrap);
    let report = dm_pass(opts).await;
    if !report.complete() {
        let first = report.clone();
        db::spawn_bound(async move {
            dm_catchup::retry_incomplete(&first, |relays| {
                dm_pass(Opts { is_new, only: (!relays.is_empty()).then_some(relays), bootstrap })
            })
            .await;
        });
    }
    report
}

struct Bootstrapping;

/// Catch up on DMs: each read relay's window since its cursor, retried behind it until held.
/// A relay with no cursor yet then reconciles its whole history once, in the background.
pub async fn dms(is_new: bool) -> Report {
    let report = retrying(Opts { is_new, only: None, bootstrap: false }).await;
    bootstrap_cursorless().await;
    dm_catchup::start_safety_net(|| {
        dm_pass(Opts {
            is_new: true,
            only: None,
            bootstrap: false,
        })
    });
    report
}

async fn dm_pass(opts: Opts) -> Report {
    let batcher = BatchingPersist::new(&Handler);
    dm_catchup::catch_up(&opts, &batcher, |prepared, is_new| {
        commit_prepared_event(prepared, is_new, &batcher)
    })
    .await
}

async fn bootstrap_cursorless() {
    let Some(client) = state::nostr_client() else {
        return;
    };
    let fresh: Vec<String> = client
        .relays()
        .await
        .into_iter()
        .filter(|(url, relay)| {
            relay.capabilities().has_read()
                && vector_core::negentropy::reconcile_cursor(url.as_str()).is_none()
        })
        .map(|(url, _)| url.to_string())
        .collect();
    let busy = db::current_session().scoped::<Bootstrapping, AtomicBool>();
    if fresh.is_empty() || busy.swap(true, Ordering::SeqCst) {
        return;
    }
    db::spawn_bound(async move {
        dm_pass(Opts {
            is_new: false,
            only: Some(fresh),
            bootstrap: true,
        })
        .await;
        busy.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::{Owed, Seen};

    fn seen(has_read: bool, has_gossip: bool, reconnect: bool, syncing: bool) -> Seen {
        Seen { has_read, has_gossip, reconnect, syncing, discovery_only: false }
    }

    #[test]
    fn a_relay_is_asked_only_for_what_it_can_have_missed() {
        // A read relay's first connect after the sync: its DMs, not its Communities.
        assert_eq!(Owed::of(seen(true, false, false, false)), Owed { dms_after_sync: false, dms_now: true, communities: false });
        // Its reconnect: both.
        assert_eq!(Owed::of(seen(true, false, true, false)), Owed { dms_after_sync: false, dms_now: true, communities: true });
        // Back mid-sync: its DMs once the sync ends, nothing now.
        assert_eq!(Owed::of(seen(true, false, true, true)), Owed { dms_after_sync: true, dms_now: false, communities: false });
        // A Community relay we don't read: its planes on a reconnect only.
        assert_eq!(Owed::of(seen(false, true, true, false)), Owed { dms_after_sync: false, dms_now: false, communities: true });
        assert_eq!(Owed::of(seen(false, true, false, false)), Owed { dms_after_sync: false, dms_now: false, communities: false });
        // Someone else's inbox, dialled for one send.
        assert_eq!(Owed::of(seen(false, false, true, false)), Owed { dms_after_sync: false, dms_now: false, communities: false });
        // A discovery relay holds no planes.
        let indexer = Seen { discovery_only: true, ..seen(false, true, true, false) };
        assert_eq!(Owed::of(indexer), Owed { dms_after_sync: false, dms_now: false, communities: false });
    }
}
