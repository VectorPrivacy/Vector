//! Relay management Tauri commands.
//!
//! This module handles all relay-related operations:
//! - Default relay configuration
//! - Custom relay management (add/remove/toggle)
//! - Relay metrics and logging
//! - Connection monitoring and health checks

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use once_cell::sync::Lazy;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Runtime};

use crate::{db, NOSTR_CLIENT, TAURI_APP, get_blossom_servers};

// ============================================================================
// Constants
// ============================================================================

/// Default relays that come pre-configured
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://jskitty.cat/nostr",        // TRUSTED_RELAY
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
static RELAY_METRICS: Lazy<RwLock<HashMap<String, RelayMetrics>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Global storage for relay logs (max 10 per relay)
static RELAY_LOGS: Lazy<RwLock<HashMap<String, VecDeque<RelayLog>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

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

/// Helper to build RelayOptions based on mode
pub fn relay_options_for_mode(mode: &str) -> RelayOptions {
    let opts = RelayOptions::new().reconnect(false);
    match mode {
        "read" => opts.write(false),
        "write" => opts.read(false),
        _ => opts,
    }
}

// ============================================================================
// Database Helpers
// ============================================================================

/// Get the list of custom relays from settings
async fn load_custom_relays<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<CustomRelay>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection(handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["custom_relays"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);

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

    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["custom_relays", json_str],
    ).map_err(|e| format!("Failed to save custom relays: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Get the list of disabled default relays from settings
pub async fn get_disabled_default_relays<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection(handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["disabled_default_relays"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);

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

    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["disabled_default_relays", json_str],
    ).map_err(|e| format!("Failed to save disabled default relays: {}", e))?;

    crate::account_manager::return_db_connection(conn);
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
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

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

    if let Some(client) = NOSTR_CLIENT.get() {
        if enabled {
            match client.pool().add_relay(&normalized_url, RelayOptions::new().reconnect(false)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&normalized_url).await;
                    println!("[Relay] Enabled default relay: {}", normalized_url);
                }
                Err(e) => eprintln!("[Relay] Failed to enable default relay: {}", e),
            }
        } else {
            if let Err(e) = client.pool().remove_relay(&normalized_url).await {
                eprintln!("[Relay] Note: Could not disable default relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled default relay: {}", normalized_url);
            }
        }
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

    if let Some(client) = NOSTR_CLIENT.get() {
        if client.relays().await.len() > 0 {
            match client.pool().add_relay(&new_relay.url, relay_options_for_mode(&relay_mode)).await {
                Ok(_) => {
                    println!("[Relay] Added custom relay to pool: {} (mode: {})", new_relay.url, relay_mode);
                    if let Err(e) = client.pool().connect_relay(&new_relay.url).await {
                        eprintln!("[Relay] Failed to connect to new relay: {}", e);
                    }
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

    if let Some(client) = NOSTR_CLIENT.get() {
        if let Err(e) = client.pool().remove_relay(&url).await {
            eprintln!("[Relay] Note: Could not remove relay from pool: {}", e);
        } else {
            println!("[Relay] Removed custom relay from pool: {}", url);
        }
    }

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

    if let Some(client) = NOSTR_CLIENT.get() {
        if enabled {
            match client.pool().add_relay(&url, relay_options_for_mode(&relay_mode)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&url).await;
                    println!("[Relay] Enabled custom relay: {} (mode: {})", url, relay_mode);
                }
                Err(e) => eprintln!("[Relay] Failed to enable relay: {}", e),
            }
        } else {
            if let Err(e) = client.pool().remove_relay(&url).await {
                eprintln!("[Relay] Note: Could not disable relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled custom relay: {}", url);
            }
        }
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
        if let Some(client) = NOSTR_CLIENT.get() {
            let _ = client.pool().remove_relay(&url).await;
            match client.pool().add_relay(&url, relay_options_for_mode(&mode)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&url).await;
                    println!("[Relay] Updated relay mode: {} -> {}", url, mode);
                }
                Err(e) => eprintln!("[Relay] Failed to update relay mode: {}", e),
            }
        }
    }

    Ok(true)
}

/// Validate a relay URL without saving it
#[tauri::command]
pub async fn validate_relay_url_cmd(url: String) -> Result<String, String> {
    validate_relay_url(&url)
}

/// Monitor relay pool connection status changes
#[tauri::command]
pub async fn monitor_relay_connections() -> Result<bool, String> {
    static MONITOR_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if MONITOR_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Ok(false);
    }

    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let handle = TAURI_APP.get().unwrap().clone();

    let monitor = client.monitor().ok_or("Failed to get monitor")?;
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

                    handle_clone.emit("relay_status_change", serde_json::json!({
                        "url": url_str,
                        "status": status_str
                    })).unwrap();

                    match status {
                        RelayStatus::Connected => {
                            let handle_inner = handle_clone.clone();
                            let url_string = url_str.clone();
                            tokio::spawn(async move {
                                crate::commands::sync::fetch_messages(handle_inner, false, Some(url_string)).await;
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Spawn aggressive health check task
    let client_health = client.clone();
    let handle_health = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

        loop {
            let relays = client_health.relays().await;
            let mut unhealthy_relays = Vec::new();

            for (url, relay) in &relays {
                let status = relay.status();

                if status == RelayStatus::Connected {
                    let test_filter = Filter::new()
                        .kinds(vec![Kind::Metadata])
                        .limit(1);

                    let start = std::time::Instant::now();
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        client_health.fetch_events_from(
                            vec![url.to_string()],
                            test_filter,
                            std::time::Duration::from_secs(2)
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
                        Ok(Ok(events)) => {
                            if events.is_empty() && elapsed.as_secs() >= 2 {
                                unhealthy_relays.push((url.clone(), relay.clone()));
                                add_relay_log(&url_str, "warn", "Health check failed: slow/empty response");
                            } else {
                                update_relay_metrics(&url_str, |m| {
                                    m.ping_ms = Some(ping_ms);
                                    m.last_check = Some(now_secs);
                                });
                            }
                        }
                        Ok(Err(e)) => {
                            unhealthy_relays.push((url.clone(), relay.clone()));
                            add_relay_log(&url_str, "warn", &format!("Health check failed: {}", e));
                        }
                        Err(_) => {
                            unhealthy_relays.push((url.clone(), relay.clone()));
                            add_relay_log(&url_str, "warn", "Health check failed: timeout");
                        }
                    }
                } else if status == RelayStatus::Terminated || status == RelayStatus::Disconnected {
                    unhealthy_relays.push((url.clone(), relay.clone()));
                }
            }

            for (url, relay) in unhealthy_relays {
                let url_str = url.to_string();
                if relay.status() == RelayStatus::Connected {
                    let _ = relay.disconnect();
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }

                add_relay_log(&url_str, "info", "Attempting reconnection...");
                let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;

                handle_health.emit("relay_health_check", serde_json::json!({
                    "url": url_str,
                    "healthy": false,
                    "action": "force_reconnect"
                })).unwrap();
            }

            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });

    // Spawn periodic terminated relay check
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        loop {
            let relays = client.relays().await;

            for (_url, relay) in relays {
                let status = relay.status();
                if status == RelayStatus::Terminated {
                    let _ = relay.try_connect(std::time::Duration::from_secs(5)).await;
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // If we're already connected to some relays - skip and tell the frontend our client is already online
    if client.relays().await.len() > 0 {
        return false;
    }

    // Get disabled default relays
    let disabled_defaults = get_disabled_default_relays(&handle).await.unwrap_or_default();

    // Add default relays (unless disabled)
    for default_url in DEFAULT_RELAYS {
        let is_disabled = disabled_defaults.iter().any(|d| d.eq_ignore_ascii_case(default_url));
        if !is_disabled {
            match client.pool().add_relay(*default_url, RelayOptions::new().reconnect(false)).await {
                Ok(_) => {
                    println!("[Relay] Added default relay: {}", default_url);
                    add_relay_log(default_url, "info", "Added to relay pool");
                }
                Err(e) => {
                    eprintln!("[Relay] Failed to add default relay {}: {}", default_url, e);
                    add_relay_log(default_url, "error", &format!("Failed to add: {}", e));
                }
            }
        } else {
            println!("[Relay] Skipping disabled default relay: {}", default_url);
            add_relay_log(default_url, "info", "Skipped (disabled by user)");
        }
    }

    // Add user's custom relays (if any)
    match get_custom_relays(handle.clone()).await {
        Ok(custom_relays) => {
            for relay in custom_relays {
                if relay.enabled {
                    match client.pool().add_relay(&relay.url, relay_options_for_mode(&relay.mode)).await {
                        Ok(_) => {
                            println!("[Relay] Added custom relay: {} (mode: {})", relay.url, relay.mode);
                            add_relay_log(&relay.url, "info", &format!("Added to relay pool (mode: {})", relay.mode));
                        }
                        Err(e) => {
                            eprintln!("[Relay] Failed to add custom relay {}: {}", relay.url, e);
                            add_relay_log(&relay.url, "error", &format!("Failed to add: {}", e));
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("[Relay] Failed to load custom relays: {}", e),
    }

    // Connect!
    client.connect().await;

    // Post-connect: force-regenerate device KeyPackage if flagged by migration 13
    // (v0.3.0 upgrade: old keypackages used incompatible MLS engine format)
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if crate::account_manager::get_current_account().is_err() {
            return;
        }

        let handle = match TAURI_APP.get() {
            Some(h) => h.clone(),
            None => return,
        };

        // Check if migration flagged a forced keypackage regeneration
        let force_regen = db::get_sql_setting(handle.clone(), "mls_force_keypackage_regen".into())
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false);

        if force_regen {
            println!("[MLS] v0.3.0 upgrade: forcing fresh KeyPackage regeneration...");
            match crate::commands::mls::regenerate_device_keypackage(false).await {
                Ok(info) => {
                    let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                    println!("[MLS] Device KeyPackage regenerated (new format): device_id={}", device_id);
                    // Clear the flag so we don't regenerate again
                    let _ = db::remove_setting(handle, "mls_force_keypackage_regen".into());
                }
                Err(e) => println!("[MLS] Device KeyPackage regeneration FAILED: {}", e),
            }
        }
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
