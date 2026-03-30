//! Profile sync — thin Tauri wrapper around vector-core's profile sync.
//!
//! Re-exports all queue/sync logic from vector-core. Provides
//! `TauriProfileSyncHandler` for DB persistence + image caching.

pub use vector_core::profile::sync::*;

use std::sync::Arc;
use vector_core::SlimProfile;
use crate::{db, profile};

/// Tauri-specific handler: persists profiles to SQLite and caches images.
pub struct TauriProfileSyncHandler;

impl ProfileSyncHandler for TauriProfileSyncHandler {
    fn on_profile_fetched(&self, slim: &SlimProfile, avatar_url: &str, banner_url: &str) {
        let slim = slim.clone();
        let avatar = avatar_url.to_string();
        let banner = banner_url.to_string();
        let npub = slim.id.clone();
        tokio::spawn(async move {
            db::set_profile(slim).await.ok();
            profile::cache_profile_images(&npub, &avatar, &banner).await;
        });
    }
}

/// Start the background profile sync processor with Tauri handler.
pub async fn start_tauri_profile_sync_processor() {
    let handler: Arc<dyn ProfileSyncHandler> = Arc::new(TauriProfileSyncHandler);
    start_profile_sync_processor(handler).await;
}
