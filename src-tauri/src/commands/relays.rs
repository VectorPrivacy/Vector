//! Relay management Tauri commands.
//!
//! This module handles all relay-related operations:
//! - Default relay configuration
//! - Custom relay management (add/remove/toggle)
//! - Relay metrics and logging
//! - Connection monitoring and health checks

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use std::sync::LazyLock;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Runtime};

use crate::{nostr_client, TAURI_APP, get_blossom_servers};

// ============================================================================
// Constants
// ============================================================================

/// Default relays that come pre-configured
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://jskitty.com/nostr",        // TRUSTED_RELAY
    "wss://asia.vectorapp.io/nostr",  // TRUSTED_RELAY
    "wss://nostr.computingcache.com", // TRUSTED_RELAY
    "wss://relay.damus.io",
];

// ============================================================================
// Types
// ============================================================================

/// Metrics tracked per relay
#[derive(serde::Serialize, Clone, Debug)]
pub struct RelayMetrics {
    pub ping_ms: Option<u64>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub last_check: Option<u64>,
    pub events_received: u64,
    pub events_sent: u64,
}

impl Default for RelayMetrics {
    fn default() -> Self {
        Self {
            ping_ms: None,
            bytes_up: 0,
            bytes_down: 0,
            last_check: None,
            events_received: 0,
            events_sent: 0,
        }
    }
}

/// A single log entry for a relay
#[derive(serde::Serialize, Clone, Debug)]
pub struct RelayLog {
    pub timestamp: u64,
    pub level: String,
    pub message: String,
}

/// Relay information for frontend display
#[derive(serde::Serialize)]
pub struct RelayInfo {
    pub url: String,
    pub status: String,
    pub is_default: bool,
    pub is_custom: bool,
    pub enabled: bool,
    pub mode: String,
}

/// Saved custom relay entry with optional metadata
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CustomRelay {
    pub url: String,
    pub enabled: bool,
    #[serde(default = "default_relay_mode")]
    pub mode: String,
}

fn default_relay_mode() -> String {
    "both".to_string()
}

// ============================================================================
// Global State
// ============================================================================

/// Global storage for relay metrics
pub(crate) static RELAY_METRICS: LazyLock<RwLock<HashMap<String, RelayMetrics>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Global storage for relay logs (max 10 per relay)
pub(crate) static RELAY_LOGS: LazyLock<RwLock<HashMap<String, VecDeque<RelayLog>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a URL is a default relay
pub fn is_default_relay(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/');
    DEFAULT_RELAYS.iter().any(|r| r.eq_ignore_ascii_case(normalized))
}

/// Validate a relay URL format (must be wss://)
pub fn validate_relay_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();

    if !trimmed.starts_with("wss://") {
        return Err("Relay URL must start with wss://".to_string());
    }

    let after_protocol = &trimmed[6..];
    if after_protocol.is_empty() {
        return Err("Relay URL must include a host".to_string());
    }

    let normalized = trimmed.trim_end_matches('/');
    Ok(normalized.to_string())
}

/// Add a relay to the pool with race-safe handling of Tor bootstrap completing
/// mid-call. The relay's stored `connection_mode` is captured at `add_relay`
/// time. If the user was bootstrapping when we built options but bootstrap
/// completes before we'd connect, the stored options point at the blackhole
/// while the live transport is now the actual proxy. Detect this transition
/// and refresh by cycling the relay; otherwise just add. The closure is
/// invoked twice only on the rare race path.
async fn add_relay_failsafe<F>(
    client: &nostr_sdk::Client,
    url: &str,
    mut make_opts: F,
) -> Result<(), nostr_sdk::client::Error>
where
    F: FnMut() -> nostr_sdk::RelayOptions,
{
    let was_deferring = defer_connect_for_bootstrap();
    let newly_added = client.pool().add_relay(url, make_opts()).await?;

    // Promotion: the relay was already pooled — possibly a GOSSIP|PING Community relay
    // (`community_relay_options`). The user is now adding it as their OWN relay, so grant READ+WRITE
    // on the existing handle. Additive + in-place: keeps the single live connection and any Community
    // subscription intact (no disconnect), and lets pool-wide DM/profile ops use it.
    if !newly_added {
        // all_relays(): a pre-existing GOSSIP-only community relay isn't in `relays()` (READ/WRITE only).
        if let Ok(parsed) = nostr_sdk::RelayUrl::parse(url) {
            if let Some(relay) = client.pool().all_relays().await.get(&parsed) {
                relay.flags().add(nostr_sdk::RelayServiceFlags::READ | nostr_sdk::RelayServiceFlags::WRITE);
            }
        }
    }

    if defer_connect_for_bootstrap() {
        // Still bootstrapping; switch_relay_transport will cycle this relay
        // when bootstrap completes.
        return Ok(());
    }

    if was_deferring {
        // Bootstrap completed between the options-capture and now. Stored
        // mode is stale (blackhole), refresh by cycling. Propagate the
        // re-add error so the caller can log it; otherwise a vanished
        // relay would be reported as success.
        let _ = client.remove_relay(url).await;
        client.pool().add_relay(url, make_opts()).await?;
    }
    if let Err(e) = client.pool().connect_relay(url).await {
        eprintln!("[Relay] connect_relay({}) failed: {}", url, e);
    }
    Ok(())
}

/// True when the user has Tor enabled but the service hasn't finished
/// bootstrapping yet. While in this state every TCP connection blackholes
/// (failsafe), so a `connect_relay` would always fail. Better to skip it —
/// `switch_relay_transport` cycles every relay onto the live proxy as soon
/// as bootstrap completes, picking up anything we deferred here.
fn defer_connect_for_bootstrap() -> bool {
    #[cfg(feature = "tor")]
    {
        matches!(
            vector_core::tor::transport_state(),
            vector_core::tor::TorTransportState::RequiredButInactive
        )
    }
    #[cfg(not(feature = "tor"))]
    {
        false
    }
}

/// Add a log entry for a relay
pub fn add_relay_log(url: &str, level: &str, message: &str) {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let log = RelayLog {
        timestamp,
        level: level.to_string(),
        message: message.to_string(),
    };

    if let Ok(mut logs) = RELAY_LOGS.write() {
        let relay_logs = logs.entry(normalized).or_insert_with(VecDeque::new);
        relay_logs.push_front(log);
        while relay_logs.len() > 10 {
            relay_logs.pop_back();
        }
    }
}

/// Update metrics for a relay
pub fn update_relay_metrics(url: &str, update_fn: impl FnOnce(&mut RelayMetrics)) {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    if let Ok(mut metrics) = RELAY_METRICS.write() {
        let relay_metrics = metrics.entry(normalized).or_insert_with(RelayMetrics::default);
        update_fn(relay_metrics);
    }
}

/// Helper to build RelayOptions based on mode. Tor-aware: when the embedded
/// Tor service is active, the returned options carry `ConnectionMode::proxy`
/// so the new relay socket comes up through Tor immediately.
pub fn relay_options_for_mode(mode: &str) -> RelayOptions {
    let opts = RelayOptions::new().reconnect(false);
    let opts = match mode {
        "read" => opts.write(false),
        "write" => opts.read(false),
        _ => opts,
    };
    vector_core::tor_aware_relay_options(opts)
}

/// Resolve the desired *enabled* relay set — the "north star" the reconcile
/// loop drives the live pool toward. Precedence is already baked into the DB:
/// the user's Nostr-synced relay list and manual edits persist into the
/// custom-relay and disabled-default tables, so this returns the hardcoded
/// defaults (minus any the user disabled) plus every enabled custom relay.
/// Never empty — the defaults are the floor, so there is always somewhere to
/// connect. Returns (url, mode) pairs.
async fn desired_enabled_relays<R: Runtime>(handle: &AppHandle<R>) -> Vec<(String, String)> {
    let disabled = get_disabled_default_relays(handle).await.unwrap_or_default();
    let customs = load_custom_relays(handle).await.unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    for d in DEFAULT_RELAYS {
        if disabled.iter().any(|x| x.eq_ignore_ascii_case(d)) { continue; }
        out.push(((*d).to_string(), "both".to_string()));
    }
    for c in customs {
        if c.enabled {
            out.push((c.url, c.mode));
        }
    }
    out
}

// ============================================================================
// Database Helpers
// ============================================================================

/// Get the list of custom relays from settings
async fn load_custom_relays<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<CustomRelay>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["custom_relays"],
        |row| row.get(0)
    ).ok();



    match result {
        Some(json_str) => {
            serde_json::from_str(&json_str)
                .map_err(|e| format!("Failed to parse custom relays: {}", e))
        }
        None => Ok(vec![])
    }
}

/// Save the list of custom relays to settings
async fn save_custom_relays<R: Runtime>(handle: &AppHandle<R>, relays: &[CustomRelay]) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }

    let json_str = serde_json::to_string(relays)
        .map_err(|e| format!("Failed to serialize relays: {}", e))?;

    let conn = crate::account_manager::get_write_connection_guard(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["custom_relays", json_str],
    ).map_err(|e| format!("Failed to save custom relays: {}", e))?;


    Ok(())
}

/// Get the list of disabled default relays from settings
pub async fn get_disabled_default_relays<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["disabled_default_relays"],
        |row| row.get(0)
    ).ok();



    match result {
        Some(json_str) => {
            serde_json::from_str(&json_str)
                .map_err(|e| format!("Failed to parse disabled default relays: {}", e))
        }
        None => Ok(vec![])
    }
}

/// Save the list of disabled default relays to settings
async fn save_disabled_default_relays<R: Runtime>(handle: &AppHandle<R>, relays: &[String]) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }

    let json_str = serde_json::to_string(relays)
        .map_err(|e| format!("Failed to serialize disabled relays: {}", e))?;

    let conn = crate::account_manager::get_write_connection_guard(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["disabled_default_relays", json_str],
    ).map_err(|e| format!("Failed to save disabled default relays: {}", e))?;


    Ok(())
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get metrics for a relay
#[tauri::command]
pub async fn get_relay_metrics(url: String) -> Result<RelayMetrics, String> {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let metrics = RELAY_METRICS.read()
        .map_err(|_| "Failed to read metrics")?
        .get(&normalized)
        .cloned()
        .unwrap_or_default();
    Ok(metrics)
}

/// Get logs for a relay
#[tauri::command]
pub async fn get_relay_logs(url: String) -> Result<Vec<RelayLog>, String> {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let logs = RELAY_LOGS.read()
        .map_err(|_| "Failed to read logs")?
        .get(&normalized)
        .map(|l| l.iter().cloned().collect())
        .unwrap_or_default();
    Ok(logs)
}

/// Get all relays with their current status
#[tauri::command]
pub async fn get_relays<R: Runtime>(handle: AppHandle<R>) -> Result<Vec<RelayInfo>, String> {
    let client = nostr_client().ok_or("Nostr client not initialized")?;

    let custom_relays = get_custom_relays(handle.clone()).await.unwrap_or_default();
    let disabled_defaults = get_disabled_default_relays(&handle).await.unwrap_or_default();

    let pool_relays = client.relays().await;

    let mut relay_infos: Vec<RelayInfo> = Vec::new();

    // Add all default relays
    for default_url in DEFAULT_RELAYS {
        let url_str = *default_url;
        let is_disabled = disabled_defaults.iter().any(|d| d.eq_ignore_ascii_case(url_str));

        let (status, mode) = if let Some((_, relay)) = pool_relays.iter().find(|(u, _)| u.as_str().eq_ignore_ascii_case(url_str)) {
            let status = match relay.status() {
                RelayStatus::Initialized => "initialized",
                RelayStatus::Pending => "pending",
                RelayStatus::Connecting => "connecting",
                RelayStatus::Connected => "connected",
                RelayStatus::Disconnected => "disconnected",
                RelayStatus::Terminated => "terminated",
                RelayStatus::Banned => "banned",
                RelayStatus::Sleeping => "sleeping",
            };
            (status.to_string(), "both".to_string())
        } else {
            ("disabled".to_string(), "both".to_string())
        };

        relay_infos.push(RelayInfo {
            url: url_str.to_string(),
            status,
            is_default: true,
            is_custom: false,
            enabled: !is_disabled,
            mode,
        });
    }

    // Add custom relays
    for custom in &custom_relays {
        let status = if let Some((_, relay)) = pool_relays.iter().find(|(u, _)| u.as_str().eq_ignore_ascii_case(&custom.url)) {
            match relay.status() {
                RelayStatus::Initialized => "initialized",
                RelayStatus::Pending => "pending",
                RelayStatus::Connecting => "connecting",
                RelayStatus::Connected => "connected",
                RelayStatus::Disconnected => "disconnected",
                RelayStatus::Terminated => "terminated",
                RelayStatus::Banned => "banned",
                RelayStatus::Sleeping => "sleeping",
            }.to_string()
        } else {
            "disabled".to_string()
        };

        relay_infos.push(RelayInfo {
            url: custom.url.clone(),
            status,
            is_default: false,
            is_custom: true,
            enabled: custom.enabled,
            mode: custom.mode.clone(),
        });
    }

    Ok(relay_infos)
}

/// Get the list of Blossom media servers
#[tauri::command]
pub async fn get_media_servers() -> Vec<String> {
    get_blossom_servers()
}

// ============================================================================
// Blossom (BUD-03) server CRUD
// ============================================================================

#[tauri::command]
pub async fn get_blossom_servers_config() -> Vec<vector_core::blossom_servers::BlossomServerInfo> {
    vector_core::blossom_servers::list_all_servers()
}

/// Guard CRUD against account swap: requires an active account and pins
/// the SessionGuard to the one in effect when the call started.
fn require_active_blossom_session() -> Result<vector_core::state::SessionGuard, String> {
    crate::account_manager::get_current_account()
        .map_err(|_| "No active account".to_string())?;
    Ok(vector_core::state::SessionGuard::capture())
}

/// Fire-and-forget probe of a single server. No-op when a fresh
/// capability row already exists.
fn spawn_probe_for_server(server_url: String) {
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        if !session.is_valid() { return; }
        // Route through the active client signer — local accounts get the
        // local GuardedSigner, bunker accounts get NostrConnect (so the
        // probe auth event is signed by the user identity, not the client
        // device key).
        let client = match crate::nostr_client() { Some(c) => c, None => return };
        let signer = match client.signer().await { Ok(s) => s, Err(_) => return };
        match vector_core::blossom::probe_servers_for_octet_stream(
            signer, vec![server_url], session,
        ).await {
            Ok(0) => {}
            Ok(_) => {
                vector_core::traits::emit_event("blossom_capabilities_updated", &());
            }
            Err(e) => vector_core::log_warn!("[Blossom Probe] Single-server probe failed: {}", e),
        }
    });
}

#[tauri::command]
pub async fn add_custom_blossom_server(url: String) -> Result<(), String> {
    let session = require_active_blossom_session()?;
    let normalized = vector_core::blossom_servers::validate_url(&url)?;
    if vector_core::blossom_servers::is_default_server(&normalized) {
        return Err("Cannot add a default server as custom".to_string());
    }
    let key = normalized.to_lowercase();
    let mut customs = vector_core::blossom_servers::load_custom_blossom_servers()?;
    if customs.iter().any(|c| c.url.trim_end_matches('/').to_lowercase() == key) {
        return Err("Server already exists".to_string());
    }
    let probe_url = normalized.clone();
    customs.push(vector_core::blossom_servers::CustomBlossomServer {
        url: normalized,
        enabled: true,
    });
    if !session.is_valid() { return Err("Session changed".to_string()); }
    vector_core::blossom_servers::save_custom_blossom_servers(&customs)?;
    vector_core::blossom_servers::refresh_cache();
    vector_core::blossom_servers::republish_blossom_servers_debounced();
    spawn_probe_for_server(probe_url);
    Ok(())
}

#[tauri::command]
pub async fn remove_custom_blossom_server(url: String) -> Result<bool, String> {
    let session = require_active_blossom_session()?;
    let target = url.trim().trim_end_matches('/').to_lowercase();
    let mut customs = vector_core::blossom_servers::load_custom_blossom_servers()?;
    let before = customs.len();
    customs.retain(|c| c.url.trim_end_matches('/').to_lowercase() != target);
    if customs.len() == before {
        return Ok(false);
    }
    if !session.is_valid() { return Err("Session changed".to_string()); }
    vector_core::blossom_servers::save_custom_blossom_servers(&customs)?;
    // Clean slate on re-add.
    let _ = vector_core::blossom_capabilities::purge_server(&url);
    vector_core::blossom_servers::refresh_cache();
    vector_core::blossom_servers::republish_blossom_servers_debounced();
    Ok(true)
}

#[tauri::command]
pub async fn toggle_custom_blossom_server(url: String, enabled: bool) -> Result<bool, String> {
    let session = require_active_blossom_session()?;
    let target = url.trim().trim_end_matches('/').to_lowercase();
    let mut customs = vector_core::blossom_servers::load_custom_blossom_servers()?;
    let mut found = false;
    let mut stored_url: Option<String> = None;
    for c in customs.iter_mut() {
        if c.url.trim_end_matches('/').to_lowercase() == target {
            c.enabled = enabled;
            found = true;
            stored_url = Some(c.url.clone());
            break;
        }
    }
    if !found { return Err("Server not found".to_string()); }
    if !session.is_valid() { return Err("Session changed".to_string()); }
    vector_core::blossom_servers::save_custom_blossom_servers(&customs)?;
    vector_core::blossom_servers::refresh_cache();
    vector_core::blossom_servers::republish_blossom_servers_debounced();
    if enabled {
        if let Some(u) = stored_url { spawn_probe_for_server(u); }
    } else {
        // Disable wipes cached capabilities so re-enable starts fresh.
        if let Some(u) = stored_url { let _ = vector_core::blossom_capabilities::purge_server(&u); }
    }
    Ok(true)
}

#[tauri::command]
pub async fn toggle_default_blossom_server(url: String, enabled: bool) -> Result<bool, String> {
    let session = require_active_blossom_session()?;
    if !vector_core::blossom_servers::is_default_server(&url) {
        return Err("Not a default server".to_string());
    }
    let key = url.trim().trim_end_matches('/').to_lowercase();
    let mut disabled = vector_core::blossom_servers::load_disabled_default_blossom_servers()?;
    if enabled {
        disabled.retain(|d| d.trim_end_matches('/').to_lowercase() != key);
    } else if !disabled.iter().any(|d| d.trim_end_matches('/').to_lowercase() == key) {
        disabled.push(key);
    }
    if !session.is_valid() { return Err("Session changed".to_string()); }
    vector_core::blossom_servers::save_disabled_default_blossom_servers(&disabled)?;
    vector_core::blossom_servers::refresh_cache();
    vector_core::blossom_servers::republish_blossom_servers_debounced();
    if enabled {
        spawn_probe_for_server(url);
    } else {
        let _ = vector_core::blossom_capabilities::purge_server(&url);
    }
    Ok(true)
}

#[tauri::command]
pub async fn get_blossom_server_capabilities(url: String) -> Result<Vec<vector_core::blossom_capabilities::CapabilityEntry>, String> {
    vector_core::blossom_capabilities::list_for_server(&url)
}

/// Pre-flight: returns false only when every enabled server has already
/// rejected this MIME or has a known size cap at-or-below `size_bytes`.
/// MIME is resolved server-side via the same `mime_from_extension` table
/// the upload uses, so the pre-flight key matches the upload's cache row.
#[tauri::command]
pub async fn blossom_can_likely_upload(
    extension: String,
    size_bytes: u64,
    is_encrypted: bool,
) -> bool {
    let mime = vector_core::crypto::mime_from_extension(&extension);
    let servers = vector_core::state::get_blossom_servers();
    vector_core::blossom_capabilities::any_server_likely_accepts(&servers, mime, is_encrypted, size_bytes)
}

/// Get the list of custom relays from settings (Tauri command)
#[tauri::command]
pub async fn get_custom_relays<R: Runtime>(handle: AppHandle<R>) -> Result<Vec<CustomRelay>, String> {
    load_custom_relays(&handle).await
}

/// Toggle a default relay's enabled state
#[tauri::command]
pub async fn toggle_default_relay<R: Runtime>(handle: AppHandle<R>, url: String, enabled: bool) -> Result<bool, String> {
    if !is_default_relay(&url) {
        return Err("Not a default relay".to_string());
    }

    let normalized_url = url.trim().trim_end_matches('/').to_string();
    let mut disabled = get_disabled_default_relays(&handle).await?;

    if enabled {
        disabled.retain(|d| !d.eq_ignore_ascii_case(&normalized_url));
    } else {
        if !disabled.iter().any(|d| d.eq_ignore_ascii_case(&normalized_url)) {
            disabled.push(normalized_url.clone());
        }
    }

    save_disabled_default_relays(&handle, &disabled).await?;

    if let Some(client) = nostr_client() {
        if enabled {
            // Wrap with tor_aware_relay_options so a re-enabled default relay
            // doesn't come up Direct when Tor is on (or pre-bootstrap). The
            // failsafe helper handles the boot-completes-mid-call race.
            match add_relay_failsafe(&client, &normalized_url, || {
                vector_core::tor_aware_relay_options(RelayOptions::new().reconnect(false))
            }).await {
                Ok(_) => {
                    if defer_connect_for_bootstrap() {
                        println!("[Relay] Enabled default relay (deferred connect, Tor bootstrapping): {}", normalized_url);
                    } else {
                        println!("[Relay] Enabled default relay: {}", normalized_url);
                    }
                }
                Err(e) => eprintln!("[Relay] Failed to enable default relay: {}", e),
            }
        } else {
            if let Err(e) = client.pool().remove_relay(&normalized_url).await {
                eprintln!("[Relay] Note: Could not disable default relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled default relay: {}", normalized_url);
                restore_discovery_role(&client, &normalized_url).await;
            }
        }
        crate::inbox_relays::republish_inbox_relays_debounced();
    }

    Ok(true)
}

/// Add a custom relay URL
#[tauri::command]
pub async fn add_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String, mode: Option<String>) -> Result<CustomRelay, String> {
    let normalized_url = validate_relay_url(&url)?;

    let relay_mode = mode.unwrap_or_else(|| "both".to_string());
    if !["read", "write", "both"].contains(&relay_mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }

    let mut relays = load_custom_relays(&handle).await?;

    if relays.iter().any(|r| r.url.eq_ignore_ascii_case(&normalized_url)) {
        return Err("Relay already exists".to_string());
    }

    if is_default_relay(&normalized_url) {
        return Err("Cannot add default relay as custom relay".to_string());
    }

    let new_relay = CustomRelay {
        url: normalized_url,
        enabled: true,
        mode: relay_mode.clone(),
    };

    relays.push(new_relay.clone());
    save_custom_relays(&handle, &relays).await?;

    if let Some(client) = nostr_client() {
        if client.relays().await.len() > 0 {
            match add_relay_failsafe(&client, &new_relay.url, || {
                relay_options_for_mode(&relay_mode)
            }).await {
                Ok(_) => {
                    println!("[Relay] Added custom relay to pool: {} (mode: {})", new_relay.url, relay_mode);
                    if defer_connect_for_bootstrap() {
                        println!("[Relay] Connect deferred, Tor still bootstrapping: {}", new_relay.url);
                    }
                    crate::inbox_relays::republish_inbox_relays_debounced();
                }
                Err(e) => eprintln!("[Relay] Failed to add relay to pool: {}", e),
            }
        }
    }

    Ok(new_relay)
}

/// Remove a custom relay URL
#[tauri::command]
pub async fn remove_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String) -> Result<bool, String> {
    let mut relays = load_custom_relays(&handle).await?;

    let original_len = relays.len();
    relays.retain(|r| !r.url.eq_ignore_ascii_case(&url));

    if relays.len() == original_len {
        return Ok(false);
    }

    save_custom_relays(&handle, &relays).await?;

    if let Some(client) = nostr_client() {
        if let Err(e) = client.pool().remove_relay(&url).await {
            eprintln!("[Relay] Note: Could not remove relay from pool: {}", e);
        } else {
            println!("[Relay] Removed custom relay from pool: {}", url);
            restore_discovery_role(&client, &url).await;
        }
    }

    // Republish regardless of pool removal result — config changed either way
    crate::inbox_relays::republish_inbox_relays_debounced();

    Ok(true)
}

/// Toggle a custom relay's enabled state
#[tauri::command]
pub async fn toggle_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String, enabled: bool) -> Result<bool, String> {
    let mut relays = load_custom_relays(&handle).await?;

    let mut found = false;
    let mut relay_mode = "both".to_string();

    for relay in relays.iter_mut() {
        if relay.url.eq_ignore_ascii_case(&url) {
            relay.enabled = enabled;
            relay_mode = relay.mode.clone();
            found = true;
            break;
        }
    }

    if !found {
        return Err("Relay not found".to_string());
    }

    save_custom_relays(&handle, &relays).await?;

    if let Some(client) = nostr_client() {
        if enabled {
            match add_relay_failsafe(&client, &url, || relay_options_for_mode(&relay_mode)).await {
                Ok(_) => println!("[Relay] Enabled custom relay: {} (mode: {})", url, relay_mode),
                Err(e) => eprintln!("[Relay] Failed to enable relay: {}", e),
            }
        } else {
            if let Err(e) = client.pool().remove_relay(&url).await {
                eprintln!("[Relay] Note: Could not disable relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled custom relay: {}", url);
                restore_discovery_role(&client, &url).await;
            }
        }
        crate::inbox_relays::republish_inbox_relays_debounced();
    }

    Ok(true)
}

/// Update a custom relay's mode (read/write/both)
#[tauri::command]
pub async fn update_relay_mode<R: Runtime>(handle: AppHandle<R>, url: String, mode: String) -> Result<bool, String> {
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }

    let mut relays = load_custom_relays(&handle).await?;

    let mut found = false;
    let mut is_enabled = false;

    for relay in relays.iter_mut() {
        if relay.url.eq_ignore_ascii_case(&url) {
            relay.mode = mode.clone();
            is_enabled = relay.enabled;
            found = true;
            break;
        }
    }

    if !found {
        return Err("Relay not found".to_string());
    }

    save_custom_relays(&handle, &relays).await?;

    if is_enabled {
        if let Some(client) = nostr_client() {
            let _ = client.pool().remove_relay(&url).await;
            match add_relay_failsafe(&client, &url, || relay_options_for_mode(&mode)).await {
                Ok(_) => println!("[Relay] Updated relay mode: {} -> {}", url, mode),
                Err(e) => {
                    eprintln!("[Relay] Failed to update relay mode: {}", e);
                    restore_discovery_role(&client, &url).await;
                }
            }
        }
        // Republish regardless — relay was removed and mode config changed either way
        crate::inbox_relays::republish_inbox_relays_debounced();
    }

    Ok(true)
}

/// Boot-time DM Relay List sync. The Relays tab IS the kind 10050 list:
/// fetch the newest published copy, apply inbound changes locally (adopt new
/// entries, revive re-listed ones, retire ones a newer list dropped), then
/// run the merge-publish for any outbound diff.
pub async fn reconcile_dm_relay_list<R: Runtime>(handle: AppHandle<R>) {
    let session = vector_core::state::SessionGuard::capture();
    let Some(client) = nostr_client() else { return };

    let fetched = match vector_core::inbox_relays::fetch_own_inbox_list(&client).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[Relay] DM relay list sync skipped: {}", e);
            return;
        }
    };

    // The fetch can straddle an account swap; nothing below may touch the
    // per-account stores or KV on a stale session.
    if !session.is_valid() {
        return;
    }

    if let Some((remote, remote_ts)) = fetched.clone() {
        let (ours, declined) = local_relay_view(&handle).await;

        // The tab IS the list: entries both published and enabled locally are
        // co-owned, whoever first listed them — else a disable on this device
        // could never propagate (and would be reverted by other devices).
        let shared: Vec<String> = ours
            .iter()
            .filter(|u| {
                let n = vector_core::inbox_relays::normalize_relay_url(u);
                remote
                    .iter()
                    .any(|r| vector_core::inbox_relays::normalize_relay_url(r) == n)
            })
            .cloned()
            .collect();
        vector_core::inbox_relays::note_contributed(&shared);

        let plan =
            vector_core::inbox_relays::plan_inbound_reconcile(&remote, remote_ts, &ours, &declined);
        if plan.adopt.is_empty() && plan.revive.is_empty() && plan.retire.is_empty() {
            vector_core::inbox_relays::note_list_seen(remote_ts);
        } else {
            apply_inbound_reconcile(&handle, &client, &session, plan, remote_ts).await;
        }
    }

    if !session.is_valid() {
        return;
    }
    // Publish against the store-derived list, not pool state (the pool can
    // transiently hold a DM recipient's relays and miss a failed-connect
    // adoptee).
    let (ours_now, _) = local_relay_view(&handle).await;
    if let Err(e) =
        vector_core::inbox_relays::publish_inbox_relays_synced(&client, fetched, Some(ours_now))
            .await
    {
        eprintln!("[Relay] Failed to publish inbox relays: {}", e);
    }
}

/// (enabled, disabled) relay urls from the same per-account stores the
/// Relays tab edits.
async fn local_relay_view<R: Runtime>(handle: &AppHandle<R>) -> (Vec<String>, Vec<String>) {
    let customs = load_custom_relays(handle).await.unwrap_or_default();
    let disabled_defaults = get_disabled_default_relays(handle).await.unwrap_or_default();
    let mut ours: Vec<String> = DEFAULT_RELAYS
        .iter()
        .filter(|d| !disabled_defaults.iter().any(|x| x.eq_ignore_ascii_case(d)))
        .map(|s| s.to_string())
        .collect();
    let mut declined: Vec<String> = disabled_defaults;
    for c in customs {
        if c.enabled {
            ours.push(c.url);
        } else {
            declined.push(c.url);
        }
    }
    (ours, declined)
}

/// Apply an inbound reconcile plan: mutate the persisted relay stores in one
/// tight load-mutate-save pass (bounding the race window against concurrent
/// relay commands), then reconcile the live pool, then record ONLY the
/// entries actually applied as our contribution (retire fires solely for
/// contributed entries; without this a merge would resurrect a relay a newer
/// remote list deliberately dropped).
async fn apply_inbound_reconcile<R: Runtime>(
    handle: &AppHandle<R>,
    client: &nostr_sdk::Client,
    session: &vector_core::state::SessionGuard,
    plan: vector_core::inbox_relays::InboundReconcile,
    remote_ts: u64,
) {
    use vector_core::inbox_relays::normalize_relay_url as norm;

    let adopts: Vec<String> = plan
        .adopt
        .iter()
        .filter_map(|u| validate_relay_url(u).ok())
        .collect();

    // Store mutation: no awaits between load and save beyond the loads
    // themselves, so a user relay edit landing mid-apply isn't clobbered.
    let mut customs = load_custom_relays(handle).await.unwrap_or_default();
    let mut disabled_defaults = get_disabled_default_relays(handle).await.unwrap_or_default();
    let mut customs_dirty = false;
    let mut defaults_dirty = false;
    let mut applied_adopts: Vec<String> = Vec::new();
    let mut applied_revives: Vec<String> = Vec::new();
    let mut applied_retires: Vec<String> = Vec::new();

    for url in &adopts {
        if customs.iter().any(|c| norm(&c.url) == norm(url)) {
            continue;
        }
        customs.push(CustomRelay { url: url.clone(), enabled: true, mode: "both".to_string() });
        customs_dirty = true;
        applied_adopts.push(url.clone());
    }
    for url in &plan.revive {
        let n = norm(url);
        if let Some(pos) = disabled_defaults.iter().position(|d| norm(d) == n) {
            applied_revives.push(disabled_defaults.remove(pos));
            defaults_dirty = true;
        } else if let Some(c) = customs.iter_mut().find(|c| norm(&c.url) == n) {
            if !c.enabled {
                c.enabled = true;
                customs_dirty = true;
                applied_revives.push(c.url.clone());
            }
        }
    }
    for url in &plan.retire {
        let n = norm(url);
        if let Some(d) = DEFAULT_RELAYS.iter().find(|d| norm(d) == n) {
            if !disabled_defaults.iter().any(|x| norm(x) == n) {
                disabled_defaults.push(d.to_string());
                defaults_dirty = true;
                applied_retires.push(url.clone());
            }
        } else if let Some(c) = customs.iter_mut().find(|c| norm(&c.url) == n) {
            if c.enabled {
                c.enabled = false;
                customs_dirty = true;
                applied_retires.push(url.clone());
            }
        }
    }

    // The loads above await; a swap could have landed since the caller's
    // check. Nothing before this line has side effects.
    if !session.is_valid() {
        return;
    }
    if customs_dirty {
        let _ = save_custom_relays(handle, &customs).await;
    }
    if defaults_dirty {
        let _ = save_disabled_default_relays(handle, &disabled_defaults).await;
    }

    // Pool reconciliation for what was actually applied.
    for url in &applied_adopts {
        if let Err(e) = add_relay_failsafe(client, url, || relay_options_for_mode("both")).await {
            eprintln!("[Relay] Adopted relay add failed for {}: {}", url, e);
        }
        println!("[Relay] Adopted from DM relay list: {}", url);
        add_relay_log(url, "info", "Adopted from your DM relay list");
    }
    for url in &applied_revives {
        let mode = customs
            .iter()
            .find(|c| norm(&c.url) == norm(url))
            .map(|c| c.mode.clone())
            .unwrap_or_else(|| "both".to_string());
        let _ = add_relay_failsafe(client, url, || relay_options_for_mode(&mode)).await;
        println!("[Relay] Re-enabled from DM relay list: {}", url);
    }
    for url in &applied_retires {
        let _ = client.pool().remove_relay(url.as_str()).await;
        restore_discovery_role(client, url).await;
        println!("[Relay] Retired (removed on another device): {}", url);
        add_relay_log(url, "info", "Disabled (removed from your DM relay list elsewhere)");
    }

    if !session.is_valid() {
        return;
    }
    let mut contributed = applied_adopts;
    contributed.extend(applied_revives);
    vector_core::inbox_relays::note_contributed(&contributed);
    vector_core::inbox_relays::note_list_seen(remote_ts);

    // The Relays panel may be open; let it re-pull.
    let _ = handle.emit("relay_list_updated", ());
}

/// A user remove/disable of a url that doubles as a Discovery Relay must
/// demote it back to its discovery role, not evict it: nothing re-adds a
/// lost Discovery Relay until the next full connect().
async fn restore_discovery_role(client: &Client, url: &str) {
    let norm = vector_core::inbox_relays::normalize_relay_url(url);
    let is_discovery = vector_core::state::discovery_relay_iter()
        .any(|d| vector_core::inbox_relays::normalize_relay_url(d) == norm);
    if !is_discovery {
        return;
    }
    let pool = client.pool();
    if pool.add_relay(url, vector_core::discovery_relay_options()).await.is_ok() {
        let _ = pool.connect_relay(url).await;
        println!("[Relay] Restored Discovery Relay role: {}", url);
    }
}

/// Validate a relay URL without saving it
#[tauri::command]
pub async fn validate_relay_url_cmd(url: String) -> Result<String, String> {
    validate_relay_url(&url)
}

/// Tracks whether the relay-monitor task is live for the current session.
/// Reset by `reset_session()`: the monitor task exits with its channel when
/// the old client drops, so without a reset the relay-status UI would freeze
/// on the prior account's state with no new monitor ever spawning.
pub(crate) static MONITOR_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
pub async fn monitor_relay_connections() -> Result<bool, String> {
    if MONITOR_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Ok(false);
    }

    // Wait for client/app to be available — monitor is started fire-and-forget from
    // frontend, so returning Err would silently kill it forever (MONITOR_STARTED stays true).
    let (client, handle) = loop {
        if let (Some(c), Some(h)) = (nostr_client(), TAURI_APP.get()) {
            break (c, h.clone());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };

    let monitor = match client.monitor() {
        Some(m) => m,
        None => {
            MONITOR_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
            return Err("Failed to get monitor".to_string());
        }
    };
    let mut receiver = monitor.subscribe();

    // Spawn task for real-time relay status notifications
    let handle_clone = handle.clone();
    tokio::spawn(async move {
        while let Ok(notification) = receiver.recv().await {
            match notification {
                MonitorNotification::StatusChanged { relay_url, status } => {
                    let url_str = relay_url.to_string();
                    let status_str = match status {
                        RelayStatus::Initialized => "initialized",
                        RelayStatus::Pending => "pending",
                        RelayStatus::Connecting => "connecting",
                        RelayStatus::Connected => "connected",
                        RelayStatus::Disconnected => "disconnected",
                        RelayStatus::Terminated => "terminated",
                        RelayStatus::Banned => "banned",
                        RelayStatus::Sleeping => "sleeping",
                    };

                    let log_level = match status {
                        RelayStatus::Connected => "info",
                        RelayStatus::Disconnected | RelayStatus::Terminated => "warn",
                        RelayStatus::Banned => "error",
                        _ => "info",
                    };
                    add_relay_log(&url_str, log_level, &format!("Status changed to {}", status_str));

                    let _ = handle_clone.emit("relay_status_change", serde_json::json!({
                        "url": url_str,
                        "status": status_str
                    }));

                    match status {
                        RelayStatus::Connected => {
                            // Only trigger single-relay sync for REconnections (mid-session).
                            // During initial sync, the main sync already covers all relays.
                            let is_syncing = {
                                let state = crate::STATE.lock().await;
                                state.is_syncing
                            };
                            if !is_syncing {
                                let handle_inner = handle_clone.clone();
                                let url_string = url_str.clone();
                                tokio::spawn(async move {
                                    crate::commands::sync::fetch_messages(handle_inner, false, Some(url_string)).await;
                                });
                                // Communities re-sync on reconnect too (NIP-17 parity). Debounced full
                                // sweep — coalesces a multi-relay reconnect burst into one sweep.
                                crate::commands::community::trigger_community_reconnect_resync();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Spawn health check task — checks relay responsiveness and reconnects dead relays.
    // Uses a 10s timeout to avoid false positives on busy relays, and runs every 60s
    // to prevent a disconnect→reconnect→sync death loop.
    let client_health = client.clone();
    let handle_health = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        loop {
            let relays = client_health.relays().await;

            for (url, relay) in &relays {
                let status = relay.status();

                if status == RelayStatus::Connected {
                    let test_filter = Filter::new()
                        .kinds(vec![Kind::Metadata])
                        .limit(1);

                    let start = std::time::Instant::now();
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        client_health.fetch_events_from(
                            vec![url.to_string()],
                            test_filter,
                            std::time::Duration::from_secs(8)
                        )
                    ).await;

                    let elapsed = start.elapsed();
                    let url_str = url.to_string();
                    let ping_ms = elapsed.as_millis() as u64;
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    match result {
                        Ok(Ok(_events)) => {
                            update_relay_metrics(&url_str, |m| {
                                m.ping_ms = Some(ping_ms);
                                m.last_check = Some(now_secs);
                            });
                        }
                        Ok(Err(e)) => {
                            add_relay_log(&url_str, "warn", &format!("Health check failed: {}", e));
                            let _ = relay.disconnect();
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            add_relay_log(&url_str, "info", "Attempting reconnection...");
                            let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                            let _ = handle_health.emit("relay_health_check", serde_json::json!({
                                "url": url_str,
                                "healthy": false,
                                "action": "force_reconnect"
                            }));
                        }
                        Err(_) => {
                            add_relay_log(&url_str, "warn", "Health check failed: timeout");
                            let _ = relay.disconnect();
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            add_relay_log(&url_str, "info", "Attempting reconnection...");
                            let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                            let _ = handle_health.emit("relay_health_check", serde_json::json!({
                                "url": url_str,
                                "healthy": false,
                                "action": "force_reconnect"
                            }));
                        }
                    }
                } else if status == RelayStatus::Terminated || status == RelayStatus::Disconnected {
                    let url_str = url.to_string();
                    add_relay_log(&url_str, "info", "Attempting reconnection...");
                    let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                    let _ = handle_health.emit("relay_health_check", serde_json::json!({
                        "url": url_str,
                        "healthy": false,
                        "action": "force_reconnect"
                    }));
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });

    // Pool reconcile: nostr-sdk auto-reconnect is off by design, so a relay
    // that drops can leave the pool entirely and never return on its own.
    // Every 10s, re-add any enabled relay missing from the pool and reconnect
    // dead ones; the short warmup covers the startup window. Re-resolves the
    // active client each pass so an account swap can't reconcile a stale pool.
    let handle_recon = handle.clone();
    tokio::spawn(async move {
        let norm = |u: &str| u.trim_end_matches('/').to_ascii_lowercase();
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;

        loop {
            if let Some(client) = nostr_client() {
                let desired = desired_enabled_relays(&handle_recon).await;
                let pool = client.pool();
                let pool_keys: Vec<String> = client.relays().await.keys()
                    .map(|k| norm(&k.to_string()))
                    .collect();

                // Re-add anything in the desired set that's missing entirely.
                for (url, mode) in &desired {
                    if !pool_keys.iter().any(|k| k == &norm(url)) {
                        if pool.add_relay(url.as_str(), relay_options_for_mode(mode)).await.is_ok() {
                            println!("[Reconcile] re-added missing relay {}; connecting...", url);
                            add_relay_log(url.as_str(), "info", "Reconcile: re-added missing relay; connecting...");
                            if let Ok(relay) = pool.relay(url.as_str()).await {
                                let _ = relay.try_connect(std::time::Duration::from_secs(8)).await;
                            }
                        }
                    }
                }

                // Reconnect present-but-dead relays — the manual replacement
                // for nostr-sdk's disabled auto-reconnect.
                for (_url, relay) in client.relays().await {
                    match relay.status() {
                        RelayStatus::Terminated
                        | RelayStatus::Disconnected
                        | RelayStatus::Sleeping => {
                            let _ = relay.try_connect(std::time::Duration::from_secs(5)).await;
                        }
                        _ => {}
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    Ok(true)
}

// ============================================================================
// Connection Commands
// ============================================================================

/// Connect to all configured relays (default + custom)
/// Returns `true` if the client connected, `false` if already connected
#[tauri::command]
pub async fn connect<R: Runtime>(handle: AppHandle<R>) -> bool {
    // Frontend invokes `connect` speculatively on every reload; if
    // login hasn't installed the client yet, return `false` so the
    // frontend's retry path handles the no-op.
    let Some(client) = nostr_client() else { return false; };

    // If we're already connected to some relays - skip and tell the frontend our client is already online
    if !client.relays().await.is_empty() {
        return false;
    }

    // Get disabled default relays and custom relays concurrently
    let (disabled_defaults, custom_relays_result) = tokio::join!(
        get_disabled_default_relays(&handle),
        get_custom_relays(handle.clone())
    );
    let disabled_defaults = disabled_defaults.unwrap_or_default();

    // Collect all relays to add (URL, options, is_default, mode_info)
    let mut relays_to_add: Vec<(String, RelayOptions, bool, String)> = Vec::new();

    // Add default relays (unless disabled)
    for default_url in DEFAULT_RELAYS {
        let is_disabled = disabled_defaults.iter().any(|d| d.eq_ignore_ascii_case(default_url));
        if !is_disabled {
            relays_to_add.push((
                default_url.to_string(),
                vector_core::tor_aware_relay_options(RelayOptions::new().reconnect(false)),
                true,
                "both".to_string(),
            ));
        } else {
            println!("[Relay] Skipping disabled default relay: {}", default_url);
            add_relay_log(default_url, "info", "Skipped (disabled by user)");
        }
    }

    // Add custom relays
    if let Ok(custom_relays) = custom_relays_result {
        for relay in custom_relays {
            if relay.enabled {
                relays_to_add.push((
                    relay.url,
                    relay_options_for_mode(&relay.mode),
                    false,
                    relay.mode,
                ));
            }
        }
    }

    // Add Discovery Relays (kind 10050 sync/publish only). GOSSIP|PING keeps
    // them out of every pool-wide DM/profile op. Skipped when the url is
    // already a user relay: the adds below race in parallel, and a discovery
    // add landing first would pin the url to GOSSIP|PING, silencing a relay
    // the user relies on for DMs.
    for url in vector_core::state::discovery_relay_iter() {
        let norm = vector_core::inbox_relays::normalize_relay_url(url);
        let is_user_relay = relays_to_add
            .iter()
            .any(|(u, ..)| vector_core::inbox_relays::normalize_relay_url(u) == norm);
        if is_user_relay {
            continue;
        }
        relays_to_add.push((
            url.to_string(),
            vector_core::discovery_relay_options(),
            false,
            "discovery".to_string(),
        ));
    }

    // Add all relays in parallel
    let pool = client.pool();
    let add_futures: Vec<_> = relays_to_add.into_iter().map(|(url, opts, is_default, mode)| {
        let pool = pool.clone();
        async move {
            match pool.add_relay(&url, opts).await {
                Ok(_) => {
                    if is_default {
                        println!("[Relay] Added default relay: {}", url);
                        add_relay_log(&url, "info", "Added to relay pool");
                    } else {
                        println!("[Relay] Added custom relay: {} (mode: {})", url, mode);
                        add_relay_log(&url, "info", &format!("Added to relay pool (mode: {})", mode));
                    }
                }
                Err(e) => {
                    if is_default {
                        eprintln!("[Relay] Failed to add default relay {}: {}", url, e);
                    } else {
                        eprintln!("[Relay] Failed to add custom relay {}: {}", url, e);
                    }
                    add_relay_log(&url, "error", &format!("Failed to add: {}", e));
                }
            }
        }
    }).collect();

    futures_util::future::join_all(add_futures).await;

    // Connect to all added relays
    client.connect().await;

    // Post-connect: sync the DM Relay List (kind 10050). The Relays tab IS
    // that list — inbound changes from other devices/apps apply locally,
    // then any outbound diff merge-publishes.
    let reconcile_handle = handle.clone();
    tokio::spawn(async move {
        // Small delay to let relay connections stabilise
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        reconcile_dm_relay_list(reconcile_handle).await;
    });

    true
}

// Handler list for this module (for reference):
// - get_relays
// - get_media_servers
// - get_custom_relays
// - add_custom_relay
// - remove_custom_relay
// - toggle_custom_relay
// - toggle_default_relay
// - update_relay_mode
// - validate_relay_url_cmd
// - get_relay_metrics
// - get_relay_logs
// - monitor_relay_connections
// - connect
