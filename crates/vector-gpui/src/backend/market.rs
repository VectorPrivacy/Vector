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

/// What a launch asks the permission prompt about.
pub enum Ask {
    App(String),
    File(String),
}

/// A first launch's prompt: the bundle hash it's keyed by, and `(id, label, description)` rows.
pub struct Prompt {
    pub hash: String,
    pub name: String,
    pub items: Vec<(String, String, String)>,
}

/// `checkMiniAppPermissions` / `checkChatMiniAppPermissions`: a prompt only on an app's first
/// launch of this bundle, and only when it asks for anything.
pub async fn prompt_for(ask: Ask) -> Option<Prompt> {
    let app = match ask {
        Ask::App(id) => MARKETPLACE_STATE.read().await.get_app(&id).cloned()?,
        Ask::File(path) => {
            let hash = crate::miniapp::info(path).await.ok()?.hash;
            if vector_core::db::miniapps::has_miniapp_permission_prompt(&hash).unwrap_or(true) {
                return None;
            }
            MARKETPLACE_STATE.read().await.get_app_by_hash(&hash).cloned()?
        }
    };
    prompt_of(&app, |h| vector_core::db::miniapps::has_miniapp_permission_prompt(h).unwrap_or(true))
}

pub fn prompt_of(app: &MarketplaceApp, prompted: impl Fn(&str) -> bool) -> Option<Prompt> {
    if app.requested_permissions.trim().is_empty() || app.blossom_hash.is_empty() || prompted(&app.blossom_hash) {
        return None;
    }
    let items: Vec<_> = permission_rows(&app.requested_permissions, "")
        .into_iter()
        .map(|(id, label, sub, _)| (id, label, sub))
        .collect();
    Some(Prompt { hash: app.blossom_hash.clone(), name: app.name.clone(), items })
}

/// The requested permissions this build knows, in request order: `(id, label, description, granted)`.
pub fn permission_rows(requested: &str, granted: &str) -> Vec<(String, String, String, bool)> {
    let known = vector_core::webxdc_permissions::get_all_permission_info();
    let granted: Vec<&str> = granted.split(',').filter(|s| !s.is_empty()).collect();
    requested
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|id| {
            let info = known.iter().find(|p| p.id == id)?;
            Some((id.to_string(), info.label.clone(), info.description.clone(), granted.contains(&id)))
        })
        .collect()
}

pub fn granted(hash: &str) -> Result<String, String> {
    vector_core::db::miniapps::get_miniapp_granted_permissions(hash)
}

pub fn set_permissions(hash: &str, perms: &[(String, bool)]) -> Result<(), String> {
    let refs: Vec<(&str, bool)> = perms.iter().map(|(p, g)| (p.as_str(), *g)).collect();
    vector_core::db::miniapps::set_miniapp_permissions(hash, &refs)
}

pub fn set_permission(hash: &str, perm: &str, granted: bool) -> Result<(), String> {
    vector_core::db::miniapps::set_miniapp_permission(hash, perm, granted)
}

pub fn revoke_permissions(hash: &str) -> Result<(), String> {
    vector_core::db::miniapps::revoke_all_miniapp_permissions(hash)
}

/// The publish form's fields, owned so they cross into the backend.
pub struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub categories: Vec<String>,
    pub changelog: Option<String>,
    pub developer: Option<String>,
    pub source: Option<String>,
    pub permissions: Option<String>,
}

impl Listing {
    fn borrow<'a>(&'a self, version: &'a str) -> vector_core::webxdc::MarketplaceListing<'a> {
        vector_core::webxdc::MarketplaceListing {
            app_id: &self.id,
            name: &self.name,
            description: &self.description,
            version: if version.is_empty() { &self.version } else { version },
            categories: self.categories.iter().map(String::as_str).collect(),
            changelog: self.changelog.as_deref(),
            developer: self.developer.as_deref(),
            source_url: self.source.as_deref(),
            permissions: self.permissions.as_deref(),
            released_at: None,
        }
    }
}

pub fn trusted_publisher() -> &'static str {
    mk::TRUSTED_PUBLISHER
}

/// `marketplace_publish_app`: upload the package, then sign its listing.
pub async fn publish(file: String, listing: Listing) -> Result<String, String> {
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Failed to get signer: {e}"))?;
    let servers = vector_core::state::get_blossom_servers();
    mk::publish_to_marketplace(signer, &file, &listing.borrow(""), servers).await
}

/// `marketplace_edit_listing`: a new listing over the package it already names.
pub async fn edit_listing(listing: Listing) -> Result<Vec<MarketplaceApp>, String> {
    let session = vector_core::db::current_session();
    let current = MARKETPLACE_STATE.read().await.get_app(&listing.id).cloned().ok_or("Listing not found")?;
    let edited = vector_core::webxdc::edit_marketplace_listing(&current, &listing.borrow(&current.version)).await?;
    if !session.is_live() {
        return Err("Account changed during the edit".into());
    }
    let apps = {
        let mut state = MARKETPLACE_STATE.write().await;
        state.upsert_app(edited);
        state.get_apps()
    };
    if let Err(e) = vector_core::db::miniapps::save_marketplace_cache(&apps) {
        log::warn!("[Marketplace] Failed to cache the edited listing: {e}");
    }
    Ok(tidy(apps))
}
