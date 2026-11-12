//! The Nexus over the desktop's marketplace core: the catalogue, installs, updates and plays.

use vector_media::miniapps::marketplace::{self as mk, MARKETPLACE_STATE};

pub use mk::MarketplaceApp;

fn handle() -> Result<tauri::AppHandle, String> {
    vector_media::TAURI_APP.get().copied().ok_or_else(|| "The App Store isn't ready yet".to_string())
}

/// Tags are relay data: one spelling of each, so every page and count agrees.
fn tidy(mut apps: Vec<MarketplaceApp>) -> Vec<MarketplaceApp> {
    for app in &mut apps {
        let mut seen = Vec::with_capacity(app.categories.len());
        for c in &app.categories {
            let c = c.trim().to_lowercase();
            if !c.is_empty() && !seen.contains(&c) {
                seen.push(c);
            }
        }
        app.categories = seen;
    }
    apps
}

pub async fn cached() -> Vec<MarketplaceApp> {
    tidy(MARKETPLACE_STATE.read().await.get_apps())
}

/// `initMarketplace`'s network leg: fetch, read install status off disk, then the merged list.
pub async fn refresh() -> Result<Vec<MarketplaceApp>, String> {
    mk::fetch_marketplace_apps(true).await?;
    mk::sync_install_status_from_disk(&handle()?).await;
    Ok(cached().await)
}

pub async fn install(id: String) -> Result<String, String> {
    mk::install_marketplace_app(&handle()?, &id).await
}

pub async fn update(id: String) -> Result<String, String> {
    mk::update_marketplace_app(&handle()?, &id).await
}

pub async fn uninstall(id: String, name: String) -> Result<(), String> {
    mk::uninstall_marketplace_app(&handle()?, &id, &name).await
}

/// `marketplace_open_app`: install when missing, then play it alone.
pub async fn open(id: String) -> Result<(), String> {
    let h = handle()?;
    let path = match mk::check_app_installed(&h, &id).await {
        Some(p) => p,
        None => mk::install_marketplace_app(&h, &id).await?,
    };
    crate::miniapp::open(path, String::new(), String::new(), None, None).await
}

pub async fn installed_path(id: String) -> Option<String> {
    mk::check_app_installed(&handle().ok()?, &id).await
}

/// One play-history entry per Nexus app: `(id, last played ms, opens)`, newest first.
pub fn plays() -> Vec<(String, i64, i64)> {
    let history = vector_core::db::miniapps::get_miniapps_history(None).unwrap_or_default();
    let mut seen: Vec<(String, i64, i64)> = Vec::new();
    for e in history {
        let Some(id) = e.marketplace_id else { continue };
        let at = e.last_opened_at * 1000;
        let count = e.open_count as i64;
        match seen.iter_mut().find(|s| s.0 == id) {
            Some(had) => {
                had.2 += count;
                had.1 = had.1.max(at);
            }
            None => seen.push((id, at, count)),
        }
    }
    seen.sort_by(|a, b| b.1.cmp(&a.1));
    seen
}

pub async fn preload() {
    mk::preload_marketplace_cache().await;
}

pub async fn reset() {
    MARKETPLACE_STATE.write().await.clear_for_session_reset();
}
