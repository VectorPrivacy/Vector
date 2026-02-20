//! Mini Apps Marketplace Module
//!
//! This module handles the decentralized marketplace for Mini Apps (WebXDC games/apps).
//! Games are stored on Blossom (decentralized storage) and metadata is published via Nostr
//! using Application-Specific Data events (NIP-78) with replaceable event semantics.
//!
//! ## Event Structure (Kind 30078 - Parameterized Replaceable)
//!
//! ```json
//! {
//!   "kind": 30078,
//!   "tags": [
//!     ["d", "<app-identifier>"],           // Unique app ID (for replaceability)
//!     ["name", "<app-name>"],              // Display name
//!     ["description", "<description>"],    // App description
//!     ["version", "<version>"],            // Version string
//!     ["x", "<blossom-hash>"],             // Blossom SHA-256 hash of .xdc file
//!     ["url", "<blossom-url>"],            // Full Blossom download URL
//!     ["size", "<file-size>"],             // File size in bytes
//!     ["icon", "<url>", "<mime-type>"],    // Optional: App icon URL + MIME type
//!     ["dev", "<developer-name>"],         // Optional: Developer/creator name
//!     ["src", "<source-url>"],             // Optional: Source code or website URL
//!     ["t", "miniapp"],                    // Tag for filtering
//!     ["t", "webxdc"],                     // Additional tag
//!     ["t", "<category>"],                 // Optional: Category tag (game, tool, etc.)
//!     ["permissions", "microphone,camera"] // Optional: Requested permissions (comma-separated)
//!   ],
//!   "content": "<optional-extended-description-or-changelog>"
//! }
//! ```
//!
//! The `icon` tag includes both the Blossom URL and the MIME type (e.g., "image/png",
//! "image/svg+xml") so clients can properly display the icon without downloading it first.
//!
//! The `dev` tag contains the name of the original developer/creator of the app.
//! The `src` tag contains a URL to the source code repository or website.
//!
//! Using Kind 30078 (Parameterized Replaceable Application-Specific Data) allows:
//! - Updates to overwrite previous versions (same pubkey + d-tag = replacement)
//! - Application-specific data that won't conflict with other clients
//! - Easy filtering by the "miniapp" tag

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use crate::net::ProgressReporter;
use crate::blossom;
use crate::NOSTR_CLIENT;
use crate::image_cache::{self, CacheResult, ImageType};
use crate::util::bytes_to_hex_string;

/// The event kind for Mini App marketplace listings
/// Kind 30078 = Parameterized Replaceable Application-Specific Data
pub const MINIAPP_MARKETPLACE_KIND: u16 = 30078;

/// The application identifier for Vector Mini Apps marketplace
#[allow(dead_code)]
pub const MINIAPP_APP_ID: &str = "vector/miniapp";

/// Trusted publisher npub (only apps from this pubkey are shown initially)
/// This is the Vector project's official npub for publishing verified apps
pub const TRUSTED_PUBLISHER: &str = "npub16ye7evyevwnl0fc9hujsxf9zym72e063awn0pvde0huvpyec5nyq4dg4wn";

/// Represents a Mini App listing in the marketplace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceApp {
    /// Unique identifier (from d-tag)
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    pub description: String,
    /// Version string
    pub version: String,
    /// Blossom SHA-256 hash of the .xdc file
    pub blossom_hash: String,
    /// Full Blossom download URL
    pub download_url: String,
    /// File size in bytes
    pub size: u64,
    /// Optional icon URL (Blossom)
    pub icon_url: Option<String>,
    /// Optional icon MIME type (e.g., "image/png", "image/svg+xml")
    pub icon_mime: Option<String>,
    /// Locally cached icon path (for offline support)
    pub icon_cached: Option<String>,
    /// Categories/tags
    pub categories: Vec<String>,
    /// Extended description or changelog (from content)
    pub changelog: Option<String>,
    /// Developer name (the actual creator of the game/app)
    pub developer: Option<String>,
    /// Source code or website URL
    pub source_url: Option<String>,
    /// Publisher's public key (npub)
    pub publisher: String,
    /// Event creation timestamp
    pub published_at: u64,
    /// Whether this app is installed locally
    pub installed: bool,
    /// Local file path if installed
    pub local_path: Option<String>,
    /// Installed version (if different from marketplace version, update is available)
    pub installed_version: Option<String>,
    /// Whether an update is available (marketplace version != installed version)
    pub update_available: bool,
    /// Requested permissions (comma-separated string for easy serialization)
    /// Format: "microphone,camera,fullscreen"
    pub requested_permissions: String,
}

/// Progress reporter for marketplace downloads
/// Emits "marketplace_install_progress" events to the frontend
pub struct MarketplaceProgressReporter<'a, R: Runtime> {
    handle: &'a AppHandle<R>,
    app_id: &'a str,
}

impl<'a, R: Runtime> MarketplaceProgressReporter<'a, R> {
    pub fn new(handle: &'a AppHandle<R>, app_id: &'a str) -> Self {
        Self { handle, app_id }
    }
}

impl<'a, R: Runtime> ProgressReporter for MarketplaceProgressReporter<'a, R> {
    fn report_progress(&self, percentage: Option<u8>, _bytes_downloaded: Option<u64>) -> Result<(), &'static str> {
        let progress = percentage.unwrap_or(0);
        let _ = self.handle.emit("marketplace_install_progress", serde_json::json!({
            "app_id": self.app_id,
            "progress": progress
        }));
        Ok(())
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        let _ = self.handle.emit("marketplace_install_progress", serde_json::json!({
            "app_id": self.app_id,
            "progress": 100,
            "complete": true
        }));
        Ok(())
    }
}

/// Installation status of a marketplace app
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstallStatus {
    /// Not installed
    NotInstalled,
    /// Currently downloading
    Downloading { progress: u8 },
    /// Installed and ready
    Installed { path: String },
    /// Installation failed
    Failed { error: String },
}

/// Marketplace state
pub struct MarketplaceState {
    /// Cached marketplace apps
    apps: HashMap<String, MarketplaceApp>,
    /// Installation status for each app
    install_status: HashMap<String, InstallStatus>,
    /// Last sync timestamp
    last_sync: u64,
    /// Trusted publishers (npubs that are allowed to publish apps)
    trusted_publishers: Vec<String>,
}

impl MarketplaceState {
    pub fn new() -> Self {
        Self {
            apps: HashMap::new(),
            install_status: HashMap::new(),
            last_sync: 0,
            trusted_publishers: vec![TRUSTED_PUBLISHER.to_string()],
        }
    }

    /// Add or update an app in the cache
    pub fn upsert_app(&mut self, app: MarketplaceApp) {
        self.apps.insert(app.id.clone(), app);
    }

    /// Get all cached apps
    pub fn get_apps(&self) -> Vec<MarketplaceApp> {
        self.apps.values().cloned().collect()
    }

    /// Get a specific app by ID
    pub fn get_app(&self, id: &str) -> Option<&MarketplaceApp> {
        self.apps.get(id)
    }

    /// Get installation status for an app
    pub fn get_install_status(&self, id: &str) -> InstallStatus {
        self.install_status.get(id).cloned().unwrap_or(InstallStatus::NotInstalled)
    }

    /// Set installation status for an app
    pub fn set_install_status(&mut self, id: &str, status: InstallStatus) {
        // Update the app's installed flag based on status
        if let Some(app) = self.apps.get_mut(id) {
            match &status {
                InstallStatus::Installed { ref path } => {
                    app.installed = true;
                    app.local_path = Some(path.clone());
                }
                InstallStatus::NotInstalled => {
                    app.installed = false;
                    app.local_path = None;
                }
                _ => {}
            }
        }
        
        self.install_status.insert(id.to_string(), status);
    }

    /// Check if a publisher is trusted
    #[allow(dead_code)]
    pub fn is_trusted_publisher(&self, npub: &str) -> bool {
        self.trusted_publishers.iter().any(|p| p == npub)
    }

    /// Add a trusted publisher
    pub fn add_trusted_publisher(&mut self, npub: String) {
        if !self.trusted_publishers.contains(&npub) {
            self.trusted_publishers.push(npub);
        }
    }

    /// Update the installed version and update_available flag for an app
    pub fn set_app_version_info(&mut self, id: &str, installed_version: Option<String>, update_available: bool) {
        if let Some(app) = self.apps.get_mut(id) {
            app.installed_version = installed_version;
            app.update_available = update_available;
        }
    }
}

// Global marketplace state
pub static MARKETPLACE_STATE: std::sync::LazyLock<Arc<RwLock<MarketplaceState>>> = std::sync::LazyLock::new(|| Arc::new(RwLock::new(MarketplaceState::new())));

/// Parse a Nostr event into a MarketplaceApp
pub fn parse_marketplace_event(event: &Event) -> Option<MarketplaceApp> {
    // Verify it's the correct kind
    if event.kind.as_u16() != MINIAPP_MARKETPLACE_KIND {
        return None;
    }

    // Extract required tags
    let mut id = None;
    let mut name = None;
    let mut description = None;
    let mut version = None;
    let mut blossom_hash = None;
    let mut download_url = None;
    let mut size: Option<u64> = None;
    let mut icon_url = None;
    let mut icon_mime = None;
    let mut categories = Vec::new();
    let mut developer = None;
    let mut source_url = None;
    let mut requested_permissions = String::new();

    for tag in event.tags.iter() {
        let tag_vec: Vec<String> = tag.clone().to_vec();
        if tag_vec.len() < 2 {
            continue;
        }

        match tag_vec[0].as_str() {
            "d" => id = Some(tag_vec[1].clone()),
            "name" => name = Some(tag_vec[1].clone()),
            "description" => description = Some(tag_vec[1].clone()),
            "version" => version = Some(tag_vec[1].clone()),
            "x" => blossom_hash = Some(tag_vec[1].clone()),
            "url" => download_url = Some(tag_vec[1].clone()),
            "size" => size = tag_vec[1].parse().ok(),
            "icon" => {
                // Icon tag format: ["icon", "<url>", "<mime-type>"]
                icon_url = Some(tag_vec[1].clone());
                if tag_vec.len() >= 3 {
                    icon_mime = Some(tag_vec[2].clone());
                }
            }
            "dev" => developer = Some(tag_vec[1].clone()),
            "src" => source_url = Some(tag_vec[1].clone()),
            "permissions" => {
                // Permissions tag format: ["permissions", "microphone,camera,fullscreen"]
                requested_permissions = tag_vec[1].clone();
            }
            "t" => {
                // Skip the generic "miniapp" and "webxdc" tags for categories
                let tag_value = &tag_vec[1];
                if tag_value != "miniapp" && tag_value != "webxdc" {
                    categories.push(tag_value.clone());
                }
            }
            _ => {}
        }
    }

    // Validate required fields
    let id = id?;
    let name = name?;
    let blossom_hash = blossom_hash?;
    let download_url = download_url?;

    let publisher = event.pubkey.to_bech32().unwrap_or_else(|_| event.pubkey.to_hex());

    Some(MarketplaceApp {
        id,
        name,
        description: description.unwrap_or_default(),
        version: version.unwrap_or_else(|| "1.0.0".to_string()),
        blossom_hash,
        download_url,
        size: size.unwrap_or(0),
        icon_url,
        icon_mime,
        icon_cached: None,
        categories,
        changelog: if event.content.is_empty() { None } else { Some(event.content.clone()) },
        developer,
        source_url,
        publisher,
        published_at: event.created_at.as_secs(),
        installed: false,
        local_path: None,
        installed_version: None,
        update_available: false,
        requested_permissions,
    })
}

/// Build a Nostr event for publishing a Mini App to the marketplace
#[allow(dead_code)]
pub async fn build_marketplace_event<T: NostrSigner>(
    signer: &T,
    app_id: &str,
    name: &str,
    description: &str,
    version: &str,
    blossom_hash: &str,
    download_url: &str,
    size: u64,
    icon_info: Option<(&str, &str)>, // (url, mime_type)
    categories: Vec<&str>,
    changelog: Option<&str>,
    developer: Option<&str>,
    source_url: Option<&str>,
    permissions: Option<&str>, // comma-separated permissions string
) -> Result<Event, String> {
    let mut tags = vec![
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("d")), vec![app_id.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("name")), vec![name.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("description")), vec![description.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("version")), vec![version.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("x")), vec![blossom_hash.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("url")), vec![download_url.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("size")), vec![size.to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("t")), vec!["miniapp".to_string()]),
        Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("t")), vec!["webxdc".to_string()]),
    ];

    // Add optional icon with MIME type: ["icon", "<url>", "<mime-type>"]
    if let Some((icon_url, mime_type)) = icon_info {
        tags.push(Tag::custom(
            TagKind::Custom(std::borrow::Cow::Borrowed("icon")),
            vec![icon_url.to_string(), mime_type.to_string()]
        ));
    }

    // Add optional developer name
    if let Some(dev) = developer {
        if !dev.is_empty() {
            tags.push(Tag::custom(
                TagKind::Custom(std::borrow::Cow::Borrowed("dev")),
                vec![dev.to_string()]
            ));
        }
    }

    // Add optional source URL
    if let Some(src) = source_url {
        if !src.is_empty() {
            tags.push(Tag::custom(
                TagKind::Custom(std::borrow::Cow::Borrowed("src")),
                vec![src.to_string()]
            ));
        }
    }

    // Add category tags
    for category in categories {
        tags.push(Tag::custom(TagKind::Custom(std::borrow::Cow::Borrowed("t")), vec![category.to_string()]));
    }

    // Add optional permissions tag
    if let Some(perms) = permissions {
        if !perms.is_empty() {
            tags.push(Tag::custom(
                TagKind::Custom(std::borrow::Cow::Borrowed("permissions")),
                vec![perms.to_string()]
            ));
        }
    }

    let content = changelog.unwrap_or("");

    let event_builder = EventBuilder::new(Kind::from(MINIAPP_MARKETPLACE_KIND), content)
        .tags(tags);

    event_builder
        .sign(signer)
        .await
        .map_err(|e| format!("Failed to sign marketplace event: {}", e))
}

/// Fetch marketplace apps from Nostr relays
pub async fn fetch_marketplace_apps(trusted_only: bool) -> Result<Vec<MarketplaceApp>, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Build filter for marketplace events
    let mut filter = Filter::new()
        .kind(Kind::from(MINIAPP_MARKETPLACE_KIND))
        .custom_tag(SingleLetterTag::lowercase(Alphabet::T), "miniapp");

    // If trusted_only, filter by trusted publishers
    if trusted_only {
        let state = MARKETPLACE_STATE.read().await;
        let trusted_pubkeys: Vec<PublicKey> = state.trusted_publishers
            .iter()
            .filter_map(|npub| PublicKey::from_bech32(npub).ok())
            .collect();
        
        if !trusted_pubkeys.is_empty() {
            filter = filter.authors(trusted_pubkeys);
        }
    }

    // Fetch events from relays
    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Failed to fetch marketplace events: {}", e))?;

    let mut apps = Vec::new();
    let mut state = MARKETPLACE_STATE.write().await;

    for event in events.iter() {
        if let Some(app) = parse_marketplace_event(event) {
            // Check if already installed by looking for local file
            let mut app = app;
            if let Some(existing) = state.get_app(&app.id) {
                app.installed = existing.installed;
                app.local_path = existing.local_path.clone();
                // Preserve cached icon path if it exists
                if app.icon_cached.is_none() {
                    app.icon_cached = existing.icon_cached.clone();
                }
            }

            state.upsert_app(app.clone());
            apps.push(app);
        }
    }

    state.last_sync = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    log_info!("Fetched {} marketplace apps", apps.len());

    // Spawn background tasks to cache icons for apps that have icon URLs but no cached path
    // Cache is stored globally (not per-account) for deduplication across accounts
    if let Some(handle) = crate::TAURI_APP.get() {
        for app in &apps {
            if app.icon_url.is_some() && app.icon_cached.is_none() {
                let handle = handle.clone();
                let app_id = app.id.clone();
                let icon_url = app.icon_url.clone().unwrap();

                tokio::spawn(async move {
                    cache_miniapp_icon(&handle, &app_id, &icon_url).await;
                });
            }
        }
    }

    Ok(apps)
}

/// Cache a Mini App icon in the background and update the marketplace state
/// Cache is stored globally (not per-account) for deduplication across accounts.
async fn cache_miniapp_icon<R: Runtime>(
    handle: &AppHandle<R>,
    app_id: &str,
    icon_url: &str,
) {
    match image_cache::cache_image(handle, icon_url, ImageType::MiniAppIcon).await {
        CacheResult::Cached(path) | CacheResult::AlreadyCached(path) => {
            // Update the marketplace state with the cached path
            let mut state = MARKETPLACE_STATE.write().await;
            if let Some(app) = state.apps.get_mut(app_id) {
                app.icon_cached = Some(path.clone());
                log_info!("[Marketplace] Cached icon for app {}: {}", app_id, path);
            }
        }
        CacheResult::Failed(e) => {
            log_warn!("[Marketplace] Failed to cache icon for {}: {}", app_id, e);
        }
    }
}

/// Force re-cache a Mini App icon (removes old cache first, then downloads fresh)
/// Used during install/update when the icon may have changed
async fn recache_miniapp_icon<R: Runtime>(
    handle: &AppHandle<R>,
    app_id: &str,
    icon_url: &str,
) {
    // Remove old cached icon first to force re-download
    if let Err(e) = image_cache::remove_cached_image(handle, icon_url, ImageType::MiniAppIcon) {
        // Log but don't fail - the old cache might not exist
        log_info!("[Marketplace] Could not remove old icon cache for {}: {}", app_id, e);
    }

    // Now cache the (potentially new) icon
    cache_miniapp_icon(handle, app_id, icon_url).await;
}

/// Download and install a marketplace app
pub async fn install_marketplace_app<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    app_id: &str,
) -> Result<String, String> {
    // Get app info from cache
    let app = {
        let state = MARKETPLACE_STATE.read().await;
        state.get_app(app_id).cloned()
    };

    let app = app.ok_or_else(|| format!("App not found: {}", app_id))?;

    // Update status to downloading
    {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Downloading { progress: 0 });
    }

    // Get the miniapps directory
    let app_data_dir = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let miniapps_dir = app_data_dir.join("miniapps").join("marketplace");
    
    // Create directory if it doesn't exist
    std::fs::create_dir_all(&miniapps_dir)
        .map_err(|e| format!("Failed to create miniapps directory: {}", e))?;

    // Download the .xdc file from Blossom
    let file_name = format!("{}.xdc", app.id);
    let file_path = miniapps_dir.join(&file_name);

    log_info!("Downloading marketplace app {} from {}", app_id, app.download_url);

    // Use the same download system as attachments for consistent progress reporting
    let reporter = MarketplaceProgressReporter::new(handle, app_id);
    let bytes = match crate::net::download_with_reporter(&app.download_url, &reporter, None).await {
        Ok(data) => data,
        Err(e) => {
            let mut state = MARKETPLACE_STATE.write().await;
            state.set_install_status(app_id, InstallStatus::Failed {
                error: e.to_string()
            });
            return Err(e.to_string());
        }
    };

    // Verify the hash matches
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = bytes_to_hex_string(hasher.finalize().as_slice());

    if hash != app.blossom_hash {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Failed { 
            error: "Hash mismatch - file may be corrupted".to_string() 
        });
        return Err("Hash mismatch - file may be corrupted".to_string());
    }

    // Write the file
    std::fs::write(&file_path, &bytes)
        .map_err(|e| format!("Failed to write app file: {}", e))?;

    let path_str = file_path.to_string_lossy().to_string();

    // Update status to installed and emit completion event
    {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Installed { path: path_str.clone() });
    }

    // Emit 100% completion event
    let _ = handle.emit("marketplace_install_progress", serde_json::json!({
        "app_id": app_id,
        "progress": 100,
        "complete": true
    }));

    // Record to Mini Apps history so it appears in the Mini Apps panel
    // Include categories, marketplace_id, and version for proper linking
    let categories_str = app.categories.join(",");
    if let Err(e) = crate::db::record_miniapp_opened_with_metadata(
        app.name.clone(),
        path_str.clone(),
        path_str.clone(), // attachment_ref
        categories_str,
        Some(app.id.clone()),
        Some(app.version.clone()),
    ) {
        log_warn!("Failed to record installed app to history: {}", e);
        // Don't fail the install if history recording fails
    }

    // Re-cache the app icon (it may have changed or this is a fresh install)
    if let Some(ref icon_url) = app.icon_url {
        let handle_clone = handle.clone();
        let app_id_clone = app_id.to_string();
        let icon_url_clone = icon_url.clone();
        tokio::spawn(async move {
            recache_miniapp_icon(&handle_clone, &app_id_clone, &icon_url_clone).await;
        });
    }

    log_info!("Successfully installed marketplace app {} to {}", app_id, path_str);
    Ok(path_str)
}

/// Check if an app is already installed locally
pub async fn check_app_installed<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    app_id: &str,
) -> Option<String> {
    let app_data_dir = handle.path().app_data_dir().ok()?;
    let file_path = app_data_dir
        .join("miniapps")
        .join("marketplace")
        .join(format!("{}.xdc", app_id));

    if file_path.exists() {
        Some(file_path.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Uninstall a marketplace app by deleting the file and removing from history
pub async fn uninstall_marketplace_app<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    app_id: &str,
    app_name: &str,
) -> Result<(), String> {
    // Get the file path
    let app_data_dir = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let file_path = app_data_dir
        .join("miniapps")
        .join("marketplace")
        .join(format!("{}.xdc", app_id));

    // Delete the file if it exists
    if file_path.exists() {
        std::fs::remove_file(&file_path)
            .map_err(|e| format!("Failed to delete app file: {}", e))?;
        log_info!("Deleted marketplace app file: {}", file_path.display());
    }

    // Remove from miniapps_history in the database
    // We need to find the entry by name (which is unique)
    if let Err(e) = crate::db::remove_miniapp_from_history(app_name) {
        log_warn!("Failed to remove app from history: {}", e);
        // Don't fail the uninstall if history removal fails
    }

    // Update the install status
    {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::NotInstalled);
    }

    log_info!("Successfully uninstalled marketplace app: {}", app_id);
    Ok(())
}

/// Update a marketplace app to a new version
/// Downloads to a temp file first, verifies hash, then replaces the old file
/// This ensures the old version is only deleted after the new version is successfully downloaded
/// Also migrates permissions from the old file hash to the new file hash
pub async fn update_marketplace_app<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    app_id: &str,
) -> Result<String, String> {
    // Get app info from cache
    let app = {
        let state = MARKETPLACE_STATE.read().await;
        state.get_app(app_id).cloned()
    };

    let app = app.ok_or_else(|| format!("App not found: {}", app_id))?;

    // Update status to downloading
    {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Downloading { progress: 0 });
    }

    // Get the miniapps directory
    let app_data_dir = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let miniapps_dir = app_data_dir.join("miniapps").join("marketplace");

    // Ensure directory exists
    std::fs::create_dir_all(&miniapps_dir)
        .map_err(|e| format!("Failed to create miniapps directory: {}", e))?;

    let file_name = format!("{}.xdc", app.id);
    let file_path = miniapps_dir.join(&file_name);

    // Compute the old file's hash before we replace it (for permission migration)
    let old_file_hash = if file_path.exists() {
        match std::fs::read(&file_path) {
            Ok(old_data) => {
                use sha2::{Sha256, Digest};
                let mut hasher = Sha256::new();
                hasher.update(&old_data);
                Some(bytes_to_hex_string(hasher.finalize().as_slice()))
            }
            Err(e) => {
                log_warn!("Failed to read old app file for hash: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Create a temp file path for the new download
    let temp_file_name = format!("{}.xdc.tmp", app.id);
    let temp_file_path = miniapps_dir.join(&temp_file_name);

    // Use the same download system as attachments for consistent progress reporting
    let reporter = MarketplaceProgressReporter::new(handle, app_id);
    let bytes_vec = match crate::net::download_with_reporter(&app.download_url, &reporter, None).await {
        Ok(data) => data,
        Err(e) => {
            let mut state = MARKETPLACE_STATE.write().await;
            state.set_install_status(app_id, InstallStatus::Failed {
                error: e.to_string()
            });
            return Err(e.to_string());
        }
    };

    // Verify the hash matches (this is also the new file's permission hash)
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes_vec);
    let new_file_hash = bytes_to_hex_string(hasher.finalize().as_slice());

    if new_file_hash != app.blossom_hash {
        // Clean up temp file if it exists
        let _ = std::fs::remove_file(&temp_file_path);

        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Failed {
            error: "Hash mismatch - file may be corrupted".to_string()
        });
        return Err("Hash mismatch - file may be corrupted".to_string());
    }

    // Write to temp file first
    std::fs::write(&temp_file_path, &bytes_vec)
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp_file_path);
            format!("Failed to write temp file: {}", e)
        })?;

    // Now that we have verified the new file, we can safely replace the old one
    // Remove the old file (if it exists)
    if file_path.exists() {
        if let Err(e) = std::fs::remove_file(&file_path) {
            log_warn!("Failed to remove old app file: {}. Attempting to overwrite.", e);
        }
    }

    // Rename temp file to final location
    std::fs::rename(&temp_file_path, &file_path)
        .map_err(|e| {
            // If rename fails, try copy + delete as fallback (for cross-filesystem moves)
            if let Err(copy_err) = std::fs::copy(&temp_file_path, &file_path) {
                let _ = std::fs::remove_file(&temp_file_path);
                return format!("Failed to move temp file to final location: {} (copy also failed: {})", e, copy_err);
            }
            let _ = std::fs::remove_file(&temp_file_path);
            String::new() // Success via copy+delete
        })
        .or_else(|e| if e.is_empty() { Ok(()) } else { Err(e) })?;

    let path_str = file_path.to_string_lossy().to_string();

    // Migrate permissions from old hash to new hash
    if let Some(ref old_hash) = old_file_hash {
        if old_hash != &new_file_hash {
            log_info!("Migrating permissions from {} to {}", old_hash, new_file_hash);
            if let Err(e) = crate::db::copy_miniapp_permissions(old_hash, &new_file_hash) {
                log_warn!("Failed to migrate permissions: {}", e);
                // Don't fail the update if permission migration fails
            }
        }
    }

    // Update status to installed
    {
        let mut state = MARKETPLACE_STATE.write().await;
        state.set_install_status(app_id, InstallStatus::Installed { path: path_str.clone() });

        // Update the app's installed_version in the cache
        state.set_app_version_info(app_id, Some(app.version.clone()), false);
    }

    // Emit 100% completion event
    let _ = handle.emit("marketplace_install_progress", serde_json::json!({
        "app_id": app_id,
        "progress": 100,
        "complete": true
    }));

    // Update the version in the database
    if let Err(e) = crate::db::update_miniapp_version(app_id, &app.version) {
        log_warn!("Failed to update app version in history: {}", e);
        // Don't fail the update if version recording fails
    }

    // Re-cache the app icon (icons can change with updates)
    if let Some(ref icon_url) = app.icon_url {
        let handle_clone = handle.clone();
        let app_id_clone = app_id.to_string();
        let icon_url_clone = icon_url.clone();
        tokio::spawn(async move {
            recache_miniapp_icon(&handle_clone, &app_id_clone, &icon_url_clone).await;
        });
    }

    log_info!("Successfully updated marketplace app {} to version {} at {}", app_id, app.version, path_str);
    Ok(path_str)
}

/// Publish a Mini App to the marketplace (upload to Blossom + publish Nostr event)
#[allow(dead_code)]
pub async fn publish_to_marketplace<T: NostrSigner + Clone>(
    signer: T,
    xdc_path: &str,
    app_id: &str,
    name: &str,
    description: &str,
    version: &str,
    categories: Vec<&str>,
    changelog: Option<&str>,
    developer: Option<&str>,
    source_url: Option<&str>,
    permissions: Option<&str>,
    blossom_servers: Vec<String>,
) -> Result<String, String> {
    // Read the .xdc file
    let file_data = std::fs::read(xdc_path)
        .map_err(|e| format!("Failed to read .xdc file: {}", e))?;

    let file_size = file_data.len() as u64;

    // Calculate hash
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&file_data);
    let blossom_hash = bytes_to_hex_string(hasher.finalize().as_slice());

    // Extract icon from the .xdc file
    let icon_bytes = extract_icon_from_xdc(&file_data);
    
    // Upload icon to Blossom if available, tracking both URL and MIME type
    let icon_info: Option<(String, &'static str)> = if let Some(icon_data) = icon_bytes {
        log_info!("Uploading app icon to Blossom...");
        
        // Detect MIME type from icon bytes
        let mime_type = detect_image_mime_type(&icon_data);
        
        match blossom::upload_blob_with_failover(
            signer.clone(),
            blossom_servers.clone(),
            Arc::new(icon_data),
            Some(mime_type),
        )
        .await
        {
            Ok(url) => {
                log_info!("Uploaded icon to Blossom: {} ({})", url, mime_type);
                Some((url, mime_type))
            }
            Err(e) => {
                // Log warning but continue without icon
                log_info!("Warning: Failed to upload icon: {}. Continuing without icon.", e);
                None
            }
        }
    } else {
        log_info!("No icon found in .xdc file");
        None
    };

    // Upload .xdc file to Blossom
    log_info!("Uploading {} to Blossom...", xdc_path);
    
    let download_url = blossom::upload_blob_with_failover(
        signer.clone(),
        blossom_servers,
        Arc::new(file_data),
        Some("application/octet-stream"),
    )
    .await?;

    log_info!("Uploaded to Blossom: {}", download_url);

    // Build and publish the marketplace event
    let event = build_marketplace_event(
        &signer,
        app_id,
        name,
        description,
        version,
        &blossom_hash,
        &download_url,
        file_size,
        icon_info.as_ref().map(|(url, mime)| (url.as_str(), *mime)),
        categories,
        changelog,
        developer,
        source_url,
        permissions,
    )
    .await?;

    // Publish to relays
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    crate::inbox_relays::send_event_pool_first_ok(client, &event)
        .await
        .map_err(|e| format!("Failed to publish marketplace event: {}", e))?;

    log_info!("Published marketplace event for app: {}", app_id);
    Ok(event.id.to_hex())
}

/// Extract icon from .xdc file bytes
fn extract_icon_from_xdc(xdc_bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;
    
    let cursor = Cursor::new(xdc_bytes);
    let mut archive = ZipArchive::new(cursor).ok()?;
    
    // First, try to read manifest.toml to get the icon path
    let icon_path = if let Ok(mut manifest_file) = archive.by_name("manifest.toml") {
        let mut manifest_content = String::new();
        if manifest_file.read_to_string(&mut manifest_content).is_ok() {
            // Parse manifest to get icon path
            if let Ok(manifest) = toml::from_str::<super::state::MiniAppManifest>(&manifest_content) {
                if !manifest.icon.is_empty() {
                    Some(manifest.icon)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    
    // Re-open archive since we consumed it
    let cursor = Cursor::new(xdc_bytes);
    let mut archive = ZipArchive::new(cursor).ok()?;
    
    // Try to get icon from manifest path or common icon names
    let icon_names = if let Some(path) = icon_path {
        vec![path]
    } else {
        vec![
            "icon.png".to_string(),
            "icon.jpg".to_string(),
            "icon.jpeg".to_string(),
            "icon.svg".to_string(),
            "icon.webp".to_string(),
        ]
    };
    
    for icon_name in icon_names {
        if let Ok(mut file) = archive.by_name(&icon_name) {
            let mut data = Vec::new();
            if file.read_to_end(&mut data).is_ok() && !data.is_empty() {
                return Some(data);
            }
        }
    }
    
    None
}

/// Detect MIME type from image bytes
fn detect_image_mime_type(bytes: &[u8]) -> &'static str {
    if bytes.len() < 4 {
        return "application/octet-stream";
    }
    
    // Check magic bytes
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_marketplace_event() {
        // Test parsing would go here
    }
}
