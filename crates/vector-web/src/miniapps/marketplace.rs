//! The mini app marketplace: listings from Nostr (kind 30078, `t=miniapp`),
//! installs verified against their Blossom hash and kept in OPFS.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use futures_util::StreamExt;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::db;
use vector_core::webxdc::{is_safe_app_id, parse_marketplace_event, MarketplaceApp, MINIAPP_MARKETPLACE_KIND, TRUSTED_PUBLISHER};

use crate::commands::Args;
use crate::emitter;

#[derive(Clone)]
enum Install {
    Downloading(u8),
    Installed(String),
    Failed(String),
}

#[derive(Default)]
struct Market {
    apps: HashMap<String, MarketplaceApp>,
    by_hash: HashMap<String, String>,
    installs: HashMap<String, Install>,
    trusted: Vec<String>,
    last_sync: u64,
}

impl Market {
    fn upsert(&mut self, app: MarketplaceApp) {
        if let Some(old) = self.apps.get(&app.id) {
            if old.blossom_hash != app.blossom_hash {
                self.by_hash.remove(&old.blossom_hash);
            }
        }
        self.by_hash.insert(app.blossom_hash.clone(), app.id.clone());
        self.apps.insert(app.id.clone(), app);
    }

    fn set_install(&mut self, id: &str, status: Option<Install>) {
        if let Some(app) = self.apps.get_mut(id) {
            match &status {
                Some(Install::Installed(path)) => {
                    app.installed = true;
                    app.local_path = Some(path.clone());
                }
                None => {
                    app.installed = false;
                    app.local_path = None;
                }
                _ => {}
            }
        }
        match status {
            Some(s) => self.installs.insert(id.to_string(), s),
            None => self.installs.remove(id),
        };
    }
}

static MARKET: Mutex<Option<Market>> = Mutex::new(None);
static FETCHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn market<R>(f: impl FnOnce(&mut Market) -> R) -> R {
    f(MARKET.lock().unwrap().get_or_insert_with(|| Market { trusted: vec![TRUSTED_PUBLISHER.to_string()], ..Default::default() }))
}

pub fn app_by_hash(hash: &str) -> Option<MarketplaceApp> {
    market(|m| m.by_hash.get(hash).and_then(|id| m.apps.get(id)).cloned())
}

fn apps() -> Vec<MarketplaceApp> {
    market(|m| m.apps.values().cloned().collect())
}

fn now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn install_path(id: &str) -> PathBuf {
    PathBuf::from(format!("/miniapps/marketplace/{id}.xdc"))
}

async fn fetch_apps(trusted_only: bool) -> Result<Vec<MarketplaceApp>, String> {
    let _serial = FETCHING.lock().await;
    let fresh = market(|m| (m.last_sync > 0 && now().saturating_sub(m.last_sync) < 30).then(|| m.apps.values().cloned().collect::<Vec<_>>()));
    if let Some(apps) = fresh {
        return Ok(apps);
    }
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let mut filter = Filter::new().kind(Kind::from(MINIAPP_MARKETPLACE_KIND)).custom_tag(SingleLetterTag::LOWERCASE_T, "miniapp");
    if trusted_only {
        let authors: Vec<PublicKey> = market(|m| m.trusted.iter().filter_map(|n| PublicKey::from_bech32(n).ok()).collect());
        if !authors.is_empty() {
            filter = filter.authors(authors);
        }
    }
    let events = client
        .fetch_events(filter)
        .timeout(std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Failed to fetch marketplace events: {e}"))?;

    let fetched: Vec<MarketplaceApp> = market(|m| {
        let mut out = Vec::new();
        for event in events.iter() {
            let Some(mut app) = parse_marketplace_event(event) else { continue };
            if let Some(old) = m.apps.get(&app.id) {
                app.installed = old.installed;
                app.local_path = old.local_path.clone();
                app.icon_cached = app.icon_cached.or_else(|| old.icon_cached.clone());
                app.installed_version = old.installed_version.clone();
                app.update_available = old.update_available;
            }
            m.upsert(app.clone());
            out.push(app);
        }
        m.last_sync = now();
        out
    });
    if !fetched.is_empty() {
        let _ = db::miniapps::save_marketplace_cache(&fetched);
    }
    cache_icons(&fetched);
    sync_installs().await;
    Ok(apps())
}

fn cache_icons(list: &[MarketplaceApp]) {
    for app in list.iter().filter(|a| a.icon_cached.is_none()) {
        let Some(url) = app.icon_url.clone() else { continue };
        let id = app.id.clone();
        db::spawn_bound(async move {
            if let Ok(path) = crate::images::cache(&url, crate::images::Kind::MiniAppIcon).await {
                if path.starts_with('/') {
                    market(|m| {
                        if let Some(a) = m.apps.get_mut(&id) {
                            a.icon_cached = Some(path);
                        }
                    });
                }
            }
        });
    }
}

/// Install state follows what OPFS holds; the files are the truth.
async fn sync_installs() {
    for (id, version) in market(|m| m.apps.values().map(|a| (a.id.clone(), a.version.clone())).collect::<Vec<_>>()) {
        let path = install_path(&id);
        if vector_core::webfiles::exists(&path).await {
            let installed = db::miniapps::get_miniapp_installed_version(&id).unwrap_or(None);
            let update = installed.as_ref().is_some_and(|v| v != &version);
            market(|m| {
                m.set_install(&id, Some(Install::Installed(path.to_string_lossy().into())));
                if let Some(a) = m.apps.get_mut(&id) {
                    a.installed_version = installed;
                    a.update_available = update;
                }
            });
        } else if !matches!(market(|m| m.installs.get(&id).cloned()), Some(Install::Downloading(_))) {
            market(|m| {
                m.set_install(&id, None);
                if let Some(a) = m.apps.get_mut(&id) {
                    a.installed_version = None;
                    a.update_available = false;
                }
            });
        }
    }
}

/// Load the cached listings, then refresh them from relays. Runs after login.
pub async fn preload() {
    if let Ok(cached) = db::miniapps::load_marketplace_cache() {
        if !cached.is_empty() {
            market(|m| cached.into_iter().for_each(|a| m.upsert(a)));
            let _ = db::miniapps::backfill_marketplace_ids(&apps());
            sync_installs().await;
            emitter::emit("marketplace_apps_updated", &apps());
        }
    }
    match fetch_apps(true).await {
        Ok(list) => emitter::emit("marketplace_apps_updated", &list),
        Err(e) => vector_core::log_warn!("[Marketplace] refresh failed: {e}"),
    }
}

/// Download `url`, reporting progress, and require it to hash to `expected`.
async fn download_verified(app_id: &str, url: &str, expected: &str) -> Result<Vec<u8>, String> {
    vector_core::net::validate_url_not_private(url).map_err(str::to_string)?;
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(600))?;
    let resp = client.get(url).send().await.map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    if total as usize > super::package::MAX_PACKAGE_BYTES {
        return Err("App is too large".into());
    }
    let mut bytes = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    let mut last = 0u8;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download failed: {e}"))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() > super::package::MAX_PACKAGE_BYTES {
            return Err("App is too large".into());
        }
        if total > 0 {
            let pct = ((bytes.len() as u64 * 100) / total).min(99) as u8;
            if pct != last {
                last = pct;
                market(|m| m.set_install(app_id, Some(Install::Downloading(pct))));
                emitter::emit("marketplace_install_progress", &json!({ "app_id": app_id, "progress": pct }));
            }
        }
    }
    if vector_core::crypto::sha256_hex(&bytes) != expected {
        return Err("Hash mismatch - file may be corrupted".into());
    }
    Ok(bytes)
}

fn listing(id: &str) -> Result<MarketplaceApp, String> {
    if !is_safe_app_id(id) {
        return Err("Invalid app id".into());
    }
    market(|m| m.apps.get(id).cloned()).ok_or_else(|| format!("App not found: {id}"))
}

async fn install(id: &str, updating: bool) -> Result<String, String> {
    let app = listing(id)?;
    market(|m| m.set_install(id, Some(Install::Downloading(0))));
    let path = install_path(id);
    let old_hash = if updating {
        vector_core::webfiles::read(&path).await.ok().map(|b| vector_core::crypto::sha256_hex(&b))
    } else {
        None
    };
    let bytes = match download_verified(id, &app.download_url, &app.blossom_hash).await {
        Ok(b) => b,
        Err(e) => {
            market(|m| m.set_install(id, Some(Install::Failed(e.clone()))));
            return Err(e);
        }
    };
    vector_core::webfiles::write(&path, &bytes).await?;
    let path_str = path.to_string_lossy().to_string();

    if let Some(old) = old_hash.filter(|h| h != &app.blossom_hash) {
        let _ = db::miniapps::copy_miniapp_permissions(&old, &app.blossom_hash);
    }
    market(|m| {
        m.set_install(id, Some(Install::Installed(path_str.clone())));
        if let Some(a) = m.apps.get_mut(id) {
            a.installed_version = Some(app.version.clone());
            a.update_available = false;
        }
    });
    emitter::emit("marketplace_install_progress", &json!({ "app_id": id, "progress": 100, "complete": true }));
    if updating {
        let _ = db::miniapps::update_miniapp_version(id, &app.version);
    } else {
        let _ = db::miniapps::record_miniapp_opened_with_metadata(
            app.name.clone(),
            path_str.clone(),
            path_str.clone(),
            app.categories.join(","),
            Some(app.id.clone()),
            Some(app.version.clone()),
        );
    }
    Ok(path_str)
}

async fn installed_path(id: &str) -> Option<String> {
    if !is_safe_app_id(id) {
        return None;
    }
    let path = install_path(id);
    vector_core::webfiles::exists(&path).await.then(|| path.to_string_lossy().to_string())
}

async fn uninstall(id: &str, name: &str) -> Result<(), String> {
    if !is_safe_app_id(id) {
        return Err("Invalid app id".into());
    }
    let _ = vector_core::webfiles::remove(&install_path(id)).await;
    let _ = db::miniapps::remove_miniapp_from_history(name);
    market(|m| m.set_install(id, None));
    Ok(())
}

fn install_status(id: &str) -> Value {
    match market(|m| m.installs.get(id).cloned()) {
        None => json!("NotInstalled"),
        Some(Install::Downloading(p)) => json!({ "Downloading": { "progress": p } }),
        Some(Install::Installed(path)) => json!({ "Installed": { "path": path } }),
        Some(Install::Failed(error)) => json!({ "Failed": { "error": error } }),
    }
}

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let id = || a.str("appId");
        Some(match cmd {
            "marketplace_fetch_apps" => fetch_apps(a.bool("trustedOnly").unwrap_or(true)).await.and_then(crate::commands::to_value),
            "marketplace_get_cached_apps" => crate::commands::to_value(apps()),
            "marketplace_get_app" => id().and_then(|id| crate::commands::to_value(market(|m| m.apps.get(&id).cloned()))),
            "marketplace_get_app_by_hash" => a.str("fileHash").and_then(|h| crate::commands::to_value(app_by_hash(&h))),
            "marketplace_get_install_status" => id().map(|id| install_status(&id)),
            "marketplace_install_app" => match id() {
                Ok(id) => install(&id, false).await.map(|p| json!(p)),
                Err(e) => Err(e),
            },
            "marketplace_update_app" => match id() {
                Ok(id) => install(&id, true).await.map(|p| json!(p)),
                Err(e) => Err(e),
            },
            "marketplace_check_installed" => match id() {
                Ok(id) => Ok(json!(installed_path(&id).await)),
                Err(e) => Err(e),
            },
            // The page opens the window; this installs first when needed.
            "marketplace_resolve_app" => match id() {
                Ok(id) => match installed_path(&id).await {
                    Some(p) => Ok(json!(p)),
                    None => install(&id, false).await.map(|p| json!(p)),
                },
                Err(e) => Err(e),
            },
            "marketplace_sync_install_status" => {
                sync_installs().await;
                Ok(Value::Null)
            }
            "marketplace_uninstall_app" => match (id(), a.str("appName")) {
                (Ok(id), Ok(name)) => uninstall(&id, &name).await.map(|_| Value::Null),
                _ => Err("missing arguments".into()),
            },
            "marketplace_add_trusted_publisher" => a.str("npub").map(|npub| {
                market(|m| {
                    if !m.trusted.contains(&npub) {
                        m.trusted.push(npub);
                    }
                });
                Value::Null
            }),
            "marketplace_get_trusted_publisher" => Ok(json!(TRUSTED_PUBLISHER)),
            _ => return None,
        })
    })
}
