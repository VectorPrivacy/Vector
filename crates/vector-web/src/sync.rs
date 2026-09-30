//! Boot hydration, history sync, the live subscription and profile persistence.

use std::sync::Arc;

use vector_core::db;
use vector_core::{CatchUp, SlimProfile, VectorCore, STATE};

use crate::emitter;

pub struct WebProfileSyncHandler;

impl vector_core::ProfileSyncHandler for WebProfileSyncHandler {
    fn on_profile_fetched(&self, slim: &SlimProfile, avatar_url: &str, banner_url: &str) {
        let slim = slim.clone();
        let (avatar, banner) = (avatar_url.to_string(), banner_url.to_string());
        db::spawn_bound(async move {
            let _ = db::profiles::set_profile(&slim);
            crate::images::cache_profile_images(&slim.id, &avatar, &banner).await;
        });
    }
}

/// Load chats and profiles from the database into state and hand them to the
/// page as `init_finished`: the event that reveals the chat list.
async fn hydrate_and_announce() {
    let Ok(npub) = db::get_current_account() else { return };
    let mut state = STATE.lock().await;
    if !state.db_loaded {
        if let Ok(profiles) = db::profiles::get_all_profiles() {
            state.merge_db_profiles(profiles, &npub);
        }
        let mut last_messages = db::events::get_all_chats_last_messages().await.unwrap_or_default();
        if let Ok(slim_chats) = db::chats::get_all_chats() {
            let mut known: std::collections::HashSet<u16> = state.profiles.iter().map(|p| p.id).collect();
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
                if state.chats.iter().any(|c| c.id == chat_id) {
                    continue;
                }
                for message in last_messages.remove(&chat_id).unwrap_or_default() {
                    chat.internal_add_message(message, &mut state.interner);
                }
                state.chats.push(chat);
            }
            state.chats.sort_by_key(|c| std::cmp::Reverse(c.last_message_time()));
        }
        state.db_loaded = true;
        let _ = db::id_cache::preload_id_caches();
    }

    let chats: Vec<_> = state.chats.iter().map(|c| c.to_serializable(&state.interner)).collect();
    let profiles: Vec<SlimProfile> = state.profiles.iter().map(|p| SlimProfile::from_profile(p, &state.interner)).collect();
    drop(state);
    emitter::emit("init_finished", &serde_json::json!({ "profiles": profiles, "chats": chats }));
    db::spawn_bound(crate::images::cache_all_profile_images());
}

pub async fn fetch_messages(init: bool) -> Result<(), String> {
    if init {
        hydrate_and_announce().await;
        db::spawn_bound(crate::miniapps::preload_marketplace());
    }
    // History arrives behind the painted list; `sync_finished` repaints what it added.
    db::spawn_bound(async {
        emitter::emit("sync_progress", &serde_json::json!({ "mode": "Syncing" }));
        if let Err(e) = VectorCore.sync_dms(None, &crate::events::WebEventHandler).await {
            vector_core::log_warn!("[Web] DM sync failed: {e}");
        }
        emitter::emit("sync_finished", &());
    });
    Ok(())
}

/// Start the live DM and community subscription. Runs for the session's life;
/// `catchup` fetches what it missed, behind it.
pub fn notifs() {
    db::spawn_bound(crate::selfsync::start());
    db::spawn_bound(async {
        if let Err(e) = VectorCore.listen_with(Arc::new(crate::events::WebEventHandler), CatchUp::External).await {
            vector_core::log_warn!("[Web] live subscription ended: {e}");
        }
    });
}
