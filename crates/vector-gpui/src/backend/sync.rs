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
        vector_core::transport::prelogin::commit();
        hydrate().await;
        db::spawn_bound(super::market::preload());
    }
    db::spawn_bound(async move {
        super::emit("sync_progress", serde_json::json!({ "mode": "Syncing" }));
        dms(false).await;
        super::emit("sync_finished", ());
        if init {
            db::spawn_bound(merge_media_servers());
            db::spawn_bound(vector_core::badges::refresh_own());
            db::spawn_bound(vector_core::profile::sync::stamp_vector_client_after_sync());
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

/// Quiet time after a sweep before a reconnect buys another.
const COOLDOWN_MS: u64 = 20_000;
static RECONNECT_ARMED: AtomicBool = AtomicBool::new(false);
static LAST_END_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A burst of relay reconnects becomes one catch-up once it settles.
pub fn after_reconnect() {
    if !vector_core::community::v2::realtime::subscription_ready()
        || RECONNECT_ARMED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    db::spawn_bound(async {
        let cooling = (LAST_END_MS.load(Ordering::SeqCst) + COOLDOWN_MS).saturating_sub(now_ms());
        tokio::time::sleep(Duration::from_millis(cooling.max(1500))).await;
        RECONNECT_ARMED.store(false, Ordering::SeqCst);
        super::community::request();
        dms(true).await;
        LAST_END_MS.store(now_ms(), Ordering::SeqCst);
    });
}

/// What a dropped live subscription or a stretch away missed.
pub fn catch_up(what: vector_core::live_subs::CatchUp) {
    if !vector_core::community::v2::realtime::subscription_ready() {
        return;
    }
    match what {
        vector_core::live_subs::CatchUp::Relay(_) => after_reconnect(),
        vector_core::live_subs::CatchUp::All { .. } => {
            super::community::request();
            db::spawn_bound(async {
                dms(true).await;
            });
        }
    }
}

struct Bootstrapping;

/// Catch up on DMs: each read relay's window since its cursor, retried behind it until held.
/// A relay with no cursor yet then reconciles its whole history once, in the background.
pub async fn dms(is_new: bool) -> Report {
    let report = dm_pass(Opts {
        is_new,
        only: None,
        bootstrap: false,
    })
    .await;
    if !report.complete() {
        let first = report.clone();
        db::spawn_bound(async move {
            dm_catchup::retry_incomplete(&first, |relays| {
                dm_pass(Opts {
                    is_new,
                    only: (!relays.is_empty()).then_some(relays),
                    bootstrap: false,
                })
            })
            .await;
        });
    }
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
