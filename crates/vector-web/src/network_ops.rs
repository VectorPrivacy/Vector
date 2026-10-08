//! Relays, Blossom servers and notification preferences.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::inbox_relays::normalize_relay_url;
use vector_core::notify::{self, NotifyLevel, MUTE_FOREVER, MUTE_OFF};
use vector_core::state::{self, TRUSTED_RELAYS as DEFAULT_RELAYS};
use vector_core::synced_prefs::{self, IdList, Pref};
use vector_core::{blossom_capabilities, blossom_info, blossom_servers, blossom_stats, db, ClientRelayExt, STATE};

use crate::commands::{to_value, Args};

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        Some(match cmd {
            // --- Relays ---
            "get_relays" => get_relays().await,
            "get_custom_relays" => load_custom_relays().and_then(to_value),
            "add_custom_relay" => match a.str("url") {
                Ok(url) => add_custom_relay(url, a.opt_str("mode")).await.and_then(to_value),
                Err(e) => Err(e),
            },
            "remove_custom_relay" => match a.str("url") {
                Ok(url) => remove_custom_relay(url).await.map(|v| json!(v)),
                Err(e) => Err(e),
            },
            "toggle_custom_relay" => match (a.str("url"), flag(a, "enabled")) {
                (Ok(url), Ok(on)) => toggle_custom_relay(url, on).await.map(|v| json!(v)),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "toggle_default_relay" => match (a.str("url"), flag(a, "enabled")) {
                (Ok(url), Ok(on)) => toggle_default_relay(url, on).await.map(|v| json!(v)),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "update_relay_mode" => match (a.str("url"), a.str("mode")) {
                (Ok(url), Ok(mode)) => update_relay_mode(url, mode).await.map(|v| json!(v)),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "validate_relay_url_cmd" => a.str("url").and_then(|u| validate_relay_url(&u)).map(|v| json!(v)),
            "get_relay_logs" => a.str("url").and_then(|url| {
                let logs: Vec<RelayLog> = relay_logs()
                    .read()
                    .ok()
                    .and_then(|l| l.get(&log_key(&url)).map(|l| l.iter().cloned().collect()))
                    .unwrap_or_default();
                to_value(logs)
            }),
            "get_relay_metrics" => a.str("url").and_then(|url| {
                to_value(relay_metrics().read().ok().and_then(|m| m.get(&log_key(&url)).cloned()).unwrap_or_default())
            }),

            // --- Blossom ---
            "add_custom_blossom_server" => match a.str("url") {
                Ok(url) => add_custom_blossom_server(url).await.map(|_| Value::Null),
                Err(e) => Err(e),
            },
            "remove_custom_blossom_server" => match a.str("url") {
                Ok(url) => remove_custom_blossom_server(url).await.map(|v| json!(v)),
                Err(e) => Err(e),
            },
            "toggle_custom_blossom_server" => match (a.str("url"), flag(a, "enabled")) {
                (Ok(url), Ok(on)) => toggle_custom_blossom_server(url, on).await.map(|v| json!(v)),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "toggle_default_blossom_server" => match (a.str("url"), flag(a, "enabled")) {
                (Ok(url), Ok(on)) => toggle_default_blossom_server(url, on).await.map(|v| json!(v)),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "get_blossom_server_capabilities" => a.str("url").and_then(|u| blossom_capabilities::list_for_server(&u)).and_then(to_value),
            "blossom_upload_verdict" => upload_verdict(a),
            "get_blossom_server_stats" => a.str("url").map(|url| {
                let enabled = a.bool("enabled").unwrap_or(false);
                json!({
                    "stats": blossom_stats::stats_for(&url),
                    "status": blossom_stats::status_for(&url, enabled),
                })
            }),
            "get_blossom_server_snapshot" => a.str("url").map(|url| {
                let enabled = a.bool("enabled").unwrap_or(false);
                json!({
                    "info": blossom_info::cached(&url),
                    "stats": blossom_stats::stats_for(&url),
                    "status": blossom_stats::status_for(&url, enabled),
                })
            }),
            "get_blossom_server_info" => match (a.str("url"), vector_core::signer::active_signer()) {
                (Ok(url), Ok(signer)) => blossom_info::refresh(&signer, &url, Duration::from_secs(30)).await.and_then(to_value),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },

            // --- Notifications ---
            "get_notification_settings" => Ok(notification_settings()),
            "set_notification_settings" => set_notification_settings(a).map(|_| Value::Null),
            "get_notify_prefs" => match a.de::<Option<Vec<String>>>("scopeIds") {
                Ok(scopes) => {
                    let community = a.opt_str("communityId");
                    let scopes = scopes.unwrap_or_default();
                    db::scoped(async move { get_notify_prefs(community.as_deref(), &scopes) }).await
                }
                Err(e) => Err(e),
            },
            "set_notify_level" => match (a.str("scopeId"), parse_level(a.opt_str("level"))) {
                (Ok(scope), Ok(level)) => {
                    db::scoped(async move {
                        notify::set_level(&scope, level)?;
                        reconcile().await;
                        Ok(Value::Null)
                    })
                    .await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "set_notify_mute" => match (a.str("scopeId"), a.de::<i64>("durationMs")) {
                (Ok(scope), Ok(duration)) => {
                    let until = match duration {
                        0 => MUTE_OFF,
                        d if d < 0 => MUTE_FOREVER,
                        d => notify::now_ms().saturating_add(d),
                    };
                    db::scoped(async move {
                        notify::set_mute(&scope, until)?;
                        reconcile().await;
                        Ok(Value::Null)
                    })
                    .await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "set_suppress_everyone" => match a.str("communityId") {
                Ok(community) => {
                    let suppress = a.bool("suppress");
                    db::scoped(async move {
                        notify::set_suppress_everyone(&community, suppress)?;
                        reconcile().await;
                        Ok(Value::Null)
                    })
                    .await
                }
                Err(e) => Err(e),
            },
            "toggle_chat_mute" => match a.str("chatId") {
                Ok(chat) => Ok(json!(toggle_chat_mute(chat).await)),
                Err(e) => Err(e),
            },

            _ => return None,
        })
    })
}

fn flag(a: &Args, key: &str) -> Result<bool, String> {
    a.bool(key).ok_or_else(|| format!("missing argument `{key}`"))
}

fn parse_level(level: Option<String>) -> Result<Option<NotifyLevel>, String> {
    match level {
        None => Ok(None),
        Some(s) => NotifyLevel::from_label(&s).map(Some).ok_or_else(|| format!("unknown level: {s}")),
    }
}

// ============================================================================
// Relays
// ============================================================================

#[derive(serde::Serialize, Clone, Default)]
struct RelayMetrics {
    ping_ms: Option<u64>,
    bytes_up: u64,
    bytes_down: u64,
    last_check: Option<u64>,
    events_received: u64,
    events_sent: u64,
}

#[derive(serde::Serialize, Clone)]
struct RelayLog {
    timestamp: u64,
    level: String,
    message: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct CustomRelay {
    url: String,
    enabled: bool,
    #[serde(default = "default_relay_mode")]
    mode: String,
}

fn default_relay_mode() -> String {
    "both".to_string()
}

struct RelayMetricsKey;
struct RelayLogsKey;
struct MonitorStarted;

fn relay_metrics() -> Arc<RwLock<HashMap<String, RelayMetrics>>> {
    db::current_session().scoped::<RelayMetricsKey, _>()
}

fn relay_logs() -> Arc<RwLock<HashMap<String, VecDeque<RelayLog>>>> {
    db::current_session().scoped::<RelayLogsKey, _>()
}

fn log_key(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn add_relay_log(url: &str, level: &str, message: &str) {
    let log = RelayLog { timestamp: now_secs(), level: level.to_string(), message: message.to_string() };
    if let Ok(mut logs) = relay_logs().write() {
        let entries = logs.entry(log_key(url)).or_default();
        entries.push_front(log);
        entries.truncate(10);
    }
}

fn update_relay_metrics(url: &str, f: impl FnOnce(&mut RelayMetrics)) {
    if let Ok(mut metrics) = relay_metrics().write() {
        f(metrics.entry(log_key(url)).or_default());
    }
}

fn is_default_relay(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/');
    DEFAULT_RELAYS.iter().any(|r| r.eq_ignore_ascii_case(normalized))
}

fn validate_relay_url(url: &str) -> Result<String, String> {
    vector_core::transport::validate_relay_url(url)
}

fn capabilities_for_mode(mode: &str) -> RelayCapabilities {
    match mode {
        "read" => RelayCapabilities::READ,
        "write" => RelayCapabilities::WRITE,
        _ => RelayCapabilities::READ | RelayCapabilities::WRITE,
    }
}

fn status_label(status: RelayStatus) -> &'static str {
    match status {
        RelayStatus::Initialized => "initialized",
        RelayStatus::Pending => "pending",
        RelayStatus::Connecting => "connecting",
        RelayStatus::Connected => "connected",
        RelayStatus::Disconnected => "disconnected",
        RelayStatus::Terminated => "terminated",
        RelayStatus::Shutdown => "shutdown",
        RelayStatus::Banned => "banned",
        RelayStatus::Sleeping => "sleeping",
    }
}

fn load_json_setting<T: serde::de::DeserializeOwned + Default>(key: &str) -> Result<T, String> {
    if db::get_current_account().is_err() {
        return Ok(T::default());
    }
    match db::get_sql_setting(key.to_string())? {
        Some(json) => serde_json::from_str(&json).map_err(|e| format!("Failed to parse {key}: {e}")),
        None => Ok(T::default()),
    }
}

fn save_json_setting<T: serde::Serialize + ?Sized>(key: &str, value: &T) -> Result<(), String> {
    if db::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }
    let json = serde_json::to_string(value).map_err(|e| format!("Failed to serialize {key}: {e}"))?;
    db::set_sql_setting(key.to_string(), json)
}

fn load_custom_relays() -> Result<Vec<CustomRelay>, String> {
    load_json_setting("custom_relays")
}

fn save_custom_relays(relays: &[CustomRelay]) -> Result<(), String> {
    save_json_setting("custom_relays", relays)
}

fn disabled_default_relays() -> Result<Vec<String>, String> {
    load_json_setting("disabled_default_relays")
}

fn save_disabled_default_relays(relays: &[String]) -> Result<(), String> {
    save_json_setting("disabled_default_relays", relays)
}

/// Enabled defaults plus enabled customs as (url, mode): what the pool should hold.
fn desired_enabled_relays() -> Vec<(String, String)> {
    let disabled = disabled_default_relays().unwrap_or_default();
    let mut out: Vec<(String, String)> = DEFAULT_RELAYS
        .iter()
        .filter(|d| !disabled.iter().any(|x| x.eq_ignore_ascii_case(d)))
        .map(|d| (d.to_string(), default_relay_mode()))
        .collect();
    out.extend(load_custom_relays().unwrap_or_default().into_iter().filter(|c| c.enabled).map(|c| (c.url, c.mode)));
    out
}

/// Add to the pool and connect. An already-pooled url (a community or
/// discovery relay) is promoted in place to READ+WRITE, keeping its socket.
async fn add_relay(client: &Client, url: &str, caps: RelayCapabilities) -> Result<(), String> {
    let newly_added = client.add_managed_relay(url).capabilities(caps).await.map_err(|e| e.to_string())?;
    if !newly_added {
        if let Ok(parsed) = RelayUrl::parse(url) {
            if let Some(relay) = client.relays().all().await.get(&parsed) {
                relay.capabilities().add(RelayCapabilities::READ | RelayCapabilities::WRITE);
            }
        }
    }
    if let Err(e) = client.connect_relay(url).await {
        vector_core::log_warn!("[Relay] connect_relay({url}) failed: {e}");
    }
    Ok(())
}

/// Removing a url that is also a Discovery Relay demotes it rather than
/// evicting it: nothing re-adds a lost Discovery Relay until the next connect.
async fn restore_discovery_role(client: &Client, url: &str) {
    let norm = normalize_relay_url(url);
    if !state::discovery_relay_iter().any(|d| normalize_relay_url(d) == norm) {
        return;
    }
    if client.add_managed_relay(url).capabilities(vector_core::discovery_relay_capabilities()).await.is_ok() {
        let _ = client.connect_relay(url).await;
    }
}

async fn get_relays() -> Result<Value, String> {
    let client = state::nostr_client().ok_or("Nostr client not initialized")?;
    let customs = load_custom_relays().unwrap_or_default();
    let disabled = disabled_default_relays().unwrap_or_default();
    let pool = client.relays().await;
    let status_of = |url: &str| {
        pool.iter()
            .find(|(u, _)| u.as_str().trim_end_matches('/').eq_ignore_ascii_case(url.trim_end_matches('/')))
            .map(|(_, r)| status_label(r.status()))
            .unwrap_or("disabled")
    };

    let mut infos: Vec<Value> = DEFAULT_RELAYS
        .iter()
        .map(|url| {
            json!({
                "url": url,
                "status": status_of(url),
                "is_default": true,
                "is_custom": false,
                "enabled": !disabled.iter().any(|d| d.eq_ignore_ascii_case(url)),
                "mode": "both",
            })
        })
        .collect();
    infos.extend(customs.iter().map(|c| {
        json!({
            "url": c.url,
            "status": status_of(&c.url),
            "is_default": false,
            "is_custom": true,
            "enabled": c.enabled,
            "mode": c.mode,
        })
    }));
    Ok(Value::Array(infos))
}

async fn toggle_default_relay(url: String, enabled: bool) -> Result<bool, String> {
    if !is_default_relay(&url) {
        return Err("Not a default relay".to_string());
    }
    let normalized = url.trim().trim_end_matches('/').to_string();
    let mut disabled = disabled_default_relays()?;
    if enabled {
        disabled.retain(|d| !d.eq_ignore_ascii_case(&normalized));
    } else if !disabled.iter().any(|d| d.eq_ignore_ascii_case(&normalized)) {
        disabled.push(normalized.clone());
    }
    save_disabled_default_relays(&disabled)?;

    if let Some(client) = state::nostr_client() {
        if enabled {
            if let Err(e) = add_relay(&client, &normalized, capabilities_for_mode("both")).await {
                vector_core::log_warn!("[Relay] Failed to enable default relay: {e}");
            }
        } else if client.remove_relay(&normalized).await.is_ok() {
            restore_discovery_role(&client, &normalized).await;
        }
        vector_core::inbox_relays::republish_inbox_relays_debounced();
    }
    Ok(true)
}

async fn add_custom_relay(url: String, mode: Option<String>) -> Result<CustomRelay, String> {
    let normalized = validate_relay_url(&url)?;
    let mode = mode.unwrap_or_else(default_relay_mode);
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }
    let mut relays = load_custom_relays()?;
    if relays.iter().any(|r| r.url.eq_ignore_ascii_case(&normalized)) {
        return Err("Relay already exists".to_string());
    }
    if is_default_relay(&normalized) {
        return Err("Cannot add default relay as custom relay".to_string());
    }
    let relay = CustomRelay { url: normalized, enabled: true, mode };
    relays.push(relay.clone());
    save_custom_relays(&relays)?;

    if let Some(client) = state::nostr_client() {
        if !client.relays().await.is_empty() {
            match add_relay(&client, &relay.url, capabilities_for_mode(&relay.mode)).await {
                Ok(()) => vector_core::inbox_relays::republish_inbox_relays_debounced(),
                Err(e) => vector_core::log_warn!("[Relay] Failed to add relay to pool: {e}"),
            }
        }
    }
    Ok(relay)
}

async fn remove_custom_relay(url: String) -> Result<bool, String> {
    let mut relays = load_custom_relays()?;
    let before = relays.len();
    relays.retain(|r| !r.url.eq_ignore_ascii_case(&url));
    if relays.len() == before {
        return Ok(false);
    }
    save_custom_relays(&relays)?;
    if let Some(client) = state::nostr_client() {
        if client.remove_relay(&url).await.is_ok() {
            restore_discovery_role(&client, &url).await;
        }
    }
    vector_core::inbox_relays::republish_inbox_relays_debounced();
    Ok(true)
}

async fn toggle_custom_relay(url: String, enabled: bool) -> Result<bool, String> {
    let mut relays = load_custom_relays()?;
    let relay = relays.iter_mut().find(|r| r.url.eq_ignore_ascii_case(&url)).ok_or("Relay not found")?;
    relay.enabled = enabled;
    let mode = relay.mode.clone();
    save_custom_relays(&relays)?;

    if let Some(client) = state::nostr_client() {
        if enabled {
            if let Err(e) = add_relay(&client, &url, capabilities_for_mode(&mode)).await {
                vector_core::log_warn!("[Relay] Failed to enable relay: {e}");
            }
        } else if client.remove_relay(&url).await.is_ok() {
            restore_discovery_role(&client, &url).await;
        }
        vector_core::inbox_relays::republish_inbox_relays_debounced();
    }
    Ok(true)
}

async fn update_relay_mode(url: String, mode: String) -> Result<bool, String> {
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }
    let mut relays = load_custom_relays()?;
    let relay = relays.iter_mut().find(|r| r.url.eq_ignore_ascii_case(&url)).ok_or("Relay not found")?;
    relay.mode = mode.clone();
    let enabled = relay.enabled;
    save_custom_relays(&relays)?;

    if enabled {
        if let Some(client) = state::nostr_client() {
            let _ = client.remove_relay(&url).await;
            if let Err(e) = add_relay(&client, &url, capabilities_for_mode(&mode)).await {
                vector_core::log_warn!("[Relay] Failed to update relay mode: {e}");
                restore_discovery_role(&client, &url).await;
            }
        }
        vector_core::inbox_relays::republish_inbox_relays_debounced();
    }
    Ok(true)
}

/// Pool the user's relays (defaults minus disabled, plus enabled customs) and
/// the Discovery Relays, then connect.
pub async fn add_configured_relays(client: &Client) {
    let disabled = disabled_default_relays().unwrap_or_default();
    for url in DEFAULT_RELAYS.iter().filter(|d| disabled.iter().any(|x| x.eq_ignore_ascii_case(d))) {
        add_relay_log(url, "info", "Skipped (disabled by user)");
    }
    let wanted = desired_enabled_relays();
    for (url, mode) in &wanted {
        match client.add_managed_relay(url.as_str()).capabilities(capabilities_for_mode(mode)).await {
            Ok(_) => add_relay_log(url, "info", &format!("Added to relay pool (mode: {mode})")),
            Err(e) => {
                vector_core::log_warn!("[Relay] add {url} failed: {e}");
                add_relay_log(url, "error", &format!("Failed to add: {e}"));
            }
        }
    }
    // A discovery add must not pin a user relay to GOSSIP|PING.
    for url in state::discovery_relay_iter() {
        let norm = normalize_relay_url(url);
        if wanted.iter().any(|(u, _)| normalize_relay_url(u) == norm) {
            continue;
        }
        let _ = client.add_managed_relay(url).capabilities(vector_core::discovery_relay_capabilities()).await;
    }
    client.connect().await;
}

/// The Relays tab IS the kind 10050 list: apply a newer published copy
/// locally, then merge-publish any outbound diff.
pub async fn reconcile_dm_relay_list() {
    let Some(client) = state::nostr_client() else { return };
    let fetched = match vector_core::inbox_relays::fetch_own_inbox_list(&client).await {
        Ok(f) => f,
        Err(e) => {
            vector_core::log_warn!("[Relay] DM relay list sync skipped: {e}");
            return;
        }
    };

    if let Some((remote, remote_ts)) = fetched.clone() {
        let (ours, declined) = local_relay_view();
        // Entries both published and enabled here are co-owned, else a disable
        // on this device could never propagate.
        let shared: Vec<String> = ours
            .iter()
            .filter(|u| {
                let n = normalize_relay_url(u);
                remote.iter().any(|r| normalize_relay_url(r) == n)
            })
            .cloned()
            .collect();
        vector_core::inbox_relays::note_contributed(&shared);

        let plan = vector_core::inbox_relays::plan_inbound_reconcile(&remote, remote_ts, &ours, &declined);
        if plan.adopt.is_empty() && plan.revive.is_empty() && plan.retire.is_empty() {
            vector_core::inbox_relays::note_list_seen(remote_ts);
        } else {
            apply_inbound_reconcile(&client, plan, remote_ts).await;
        }
    }

    // Publish the stored list, not pool state: the pool can hold a DM
    // recipient's relays and miss an adoptee whose connect failed.
    let (ours_now, _) = local_relay_view();
    if let Err(e) = vector_core::inbox_relays::publish_inbox_relays_synced(&client, fetched, Some(ours_now)).await {
        vector_core::log_warn!("[Relay] Failed to publish inbox relays: {e}");
    }
}

/// (enabled, disabled) relay urls from the stores the Relays tab edits.
fn local_relay_view() -> (Vec<String>, Vec<String>) {
    let customs = load_custom_relays().unwrap_or_default();
    let mut declined = disabled_default_relays().unwrap_or_default();
    let mut ours: Vec<String> = DEFAULT_RELAYS
        .iter()
        .filter(|d| !declined.iter().any(|x| x.eq_ignore_ascii_case(d)))
        .map(|s| s.to_string())
        .collect();
    for c in customs {
        if c.enabled {
            ours.push(c.url);
        } else {
            declined.push(c.url);
        }
    }
    (ours, declined)
}

/// Mutate the stores in one load-mutate-save pass, reconcile the pool, then
/// record only what was applied as our contribution: retire fires solely for
/// contributed entries, so over-claiming would resurrect a dropped relay.
async fn apply_inbound_reconcile(client: &Client, plan: vector_core::inbox_relays::InboundReconcile, remote_ts: u64) {
    let norm = normalize_relay_url;
    let mut customs = load_custom_relays().unwrap_or_default();
    let mut disabled = disabled_default_relays().unwrap_or_default();
    let (mut customs_dirty, mut defaults_dirty) = (false, false);
    let (mut adopted, mut revived, mut retired) = (Vec::new(), Vec::new(), Vec::new());

    for url in plan.adopt.iter().filter_map(|u| validate_relay_url(u).ok()) {
        if customs.iter().any(|c| norm(&c.url) == norm(&url)) {
            continue;
        }
        customs.push(CustomRelay { url: url.clone(), enabled: true, mode: default_relay_mode() });
        customs_dirty = true;
        adopted.push(url);
    }
    for url in &plan.revive {
        let n = norm(url);
        if let Some(pos) = disabled.iter().position(|d| norm(d) == n) {
            revived.push(disabled.remove(pos));
            defaults_dirty = true;
        } else if let Some(c) = customs.iter_mut().find(|c| norm(&c.url) == n && !c.enabled) {
            c.enabled = true;
            customs_dirty = true;
            revived.push(c.url.clone());
        }
    }
    for url in &plan.retire {
        let n = norm(url);
        if let Some(d) = DEFAULT_RELAYS.iter().find(|d| norm(d) == n) {
            if !disabled.iter().any(|x| norm(x) == n) {
                disabled.push(d.to_string());
                defaults_dirty = true;
                retired.push(url.clone());
            }
        } else if let Some(c) = customs.iter_mut().find(|c| norm(&c.url) == n && c.enabled) {
            c.enabled = false;
            customs_dirty = true;
            retired.push(url.clone());
        }
    }

    if customs_dirty {
        let _ = save_custom_relays(&customs);
    }
    if defaults_dirty {
        let _ = save_disabled_default_relays(&disabled);
    }

    for url in &adopted {
        if let Err(e) = add_relay(client, url, capabilities_for_mode("both")).await {
            vector_core::log_warn!("[Relay] Adopted relay add failed for {url}: {e}");
        }
        add_relay_log(url, "info", "Adopted from your DM relay list");
    }
    for url in &revived {
        let mode = customs.iter().find(|c| norm(&c.url) == norm(url)).map(|c| c.mode.clone()).unwrap_or_else(default_relay_mode);
        let _ = add_relay(client, url, capabilities_for_mode(&mode)).await;
    }
    for url in &retired {
        let _ = client.remove_relay(url.as_str()).await;
        restore_discovery_role(client, url).await;
        add_relay_log(url, "info", "Disabled (removed from your DM relay list elsewhere)");
    }

    adopted.extend(revived);
    vector_core::inbox_relays::note_contributed(&adopted);
    vector_core::inbox_relays::note_list_seen(remote_ts);
    vector_core::emit_event("relay_list_updated", &());
}

/// Status events, logs, ping metrics and the pool reconcile loop for this
/// session. Catch-up and zombie probing are `listen()`'s.
pub fn start_relay_monitor(client: &Client) {
    if db::current_session().scoped::<MonitorStarted, AtomicBool>().swap(true, Ordering::SeqCst) {
        return;
    }

    if let Some(monitor) = client.monitor() {
        let mut rx = monitor.subscribe();
        db::spawn_bound(async move {
            while let Ok(MonitorNotification::StatusChanged { relay_url, status }) = rx.recv().await {
                let url = relay_url.to_string();
                let level = match status {
                    RelayStatus::Disconnected | RelayStatus::Terminated => "warn",
                    RelayStatus::Banned => "error",
                    _ => "info",
                };
                add_relay_log(&url, level, &format!("Status changed to {}", status_label(status)));
                vector_core::emit_event("relay_status_change", &json!({ "url": url, "status": status_label(status) }));

                // A reconnected socket carries no live subscriptions: Vector owns reconnects.
                if status == RelayStatus::Connected && !STATE.lock().await.is_syncing {
                    if let Some(c) = state::nostr_client() {
                        vector_core::resubscribe_relay_after_reconnect(&c, &relay_url).await;
                    }
                    crate::catchup::after_reconnect();
                }
            }
        });
    }

    db::spawn_bound(async {
        vector_core::rt::time::sleep(Duration::from_secs(30)).await;
        while !db::session_stopped() {
            if let Some(client) = state::nostr_client() {
                for (url, relay) in client.relays().await {
                    if relay.status() != RelayStatus::Connected {
                        continue;
                    }
                    let start = web_time::Instant::now();
                    let probe = vector_core::rt::time::timeout(
                        Duration::from_secs(10),
                        client
                            .fetch_events(ReqTarget::single(url.to_string(), [Filter::new().kind(Kind::Metadata).limit(1)]))
                            .timeout(Duration::from_secs(8)),
                    )
                    .await;
                    if matches!(probe, Ok(Ok(_))) {
                        let ping = start.elapsed().as_millis() as u64;
                        update_relay_metrics(url.as_str(), |m| {
                            m.ping_ms = Some(ping);
                            m.last_check = Some(now_secs());
                        });
                    } else {
                        add_relay_log(url.as_str(), "warn", "Health check failed");
                    }
                }
            }
            vector_core::rt::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // nostr-sdk auto-reconnect is off, so a dropped relay can leave the pool
    // for good; re-add what's missing and revive what's dead or wedged.
    db::spawn_bound(async {
        let norm = |u: &str| u.trim_end_matches('/').to_ascii_lowercase();
        let mut pending_since: HashMap<String, web_time::Instant> = HashMap::new();
        vector_core::rt::time::sleep(Duration::from_secs(8)).await;
        while !db::session_stopped() {
            if let Some(client) = state::nostr_client() {
                let pooled: Vec<String> = client.relays().await.keys().map(|k| norm(k.as_str())).collect();
                for (url, mode) in desired_enabled_relays() {
                    if pooled.contains(&norm(&url))
                        || client.add_managed_relay(url.as_str()).capabilities(capabilities_for_mode(&mode)).await.is_err()
                    {
                        continue;
                    }
                    add_relay_log(&url, "info", "Reconcile: re-added missing relay; connecting...");
                    if let Ok(Some(relay)) = client.relay(url.as_str()).await {
                        let _ = relay.try_connect().timeout(Duration::from_secs(8)).await;
                    }
                }

                // A relay can sit in Pending with nothing driving it; forcing
                // Terminated makes it connectable again.
                let all = client.relays().all().await;
                pending_since.retain(|k, _| all.keys().any(|u| norm(u.as_str()) == *k));
                for (url, relay) in all {
                    let key = norm(url.as_str());
                    if relay.status() != RelayStatus::Pending {
                        pending_since.remove(&key);
                        continue;
                    }
                    let since = *pending_since.entry(key.clone()).or_insert_with(web_time::Instant::now);
                    if since.elapsed() >= Duration::from_secs(15) {
                        add_relay_log(url.as_str(), "warn", "Wedged in Pending; forcing reconnect");
                        relay.disconnect();
                        let _ = relay.try_connect().timeout(Duration::from_secs(5)).await;
                        pending_since.insert(key, web_time::Instant::now());
                    }
                }

                for (_, relay) in client.relays().await {
                    if matches!(relay.status(), RelayStatus::Terminated | RelayStatus::Sleeping) {
                        let _ = relay.try_connect().timeout(Duration::from_secs(5)).await;
                    }
                }
            }
            vector_core::rt::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

// ============================================================================
// Blossom
// ============================================================================

fn upload_verdict(a: &Args) -> Result<Value, String> {
    let mime = vector_core::crypto::mime_from_extension(&a.str("extension")?);
    let size: u64 = a.de("sizeBytes")?;
    let encrypted = a.bool("isEncrypted").unwrap_or(false);
    let servers = state::get_blossom_servers();
    let warm = servers.clone();
    db::spawn_bound(async move {
        vector_core::blossom::warm_upload_connection(warm, mime, encrypted, size).await;
    });
    to_value(blossom_capabilities::upload_verdict(&servers, mime, encrypted, size))
}

/// Refresh one server's info document and learn whether it takes encrypted
/// octet-stream blobs. A no-op when fresh rows already exist.
fn spawn_probe_for_server(server_url: String) {
    db::spawn_bound(async move {
        if state::nostr_client().is_none() {
            return;
        }
        let Ok(signer) = vector_core::signer::active_signer() else { return };
        if blossom_info::refresh_all(signer.clone(), vec![server_url.clone()], Duration::from_secs(60)).await > 0 {
            vector_core::emit_event("blossom_info_updated", &());
        }
        match vector_core::blossom::probe_servers_for_octet_stream(signer, vec![server_url]).await {
            Ok(0) => {}
            Ok(_) => vector_core::emit_event("blossom_capabilities_updated", &()),
            Err(e) => vector_core::log_warn!("[Blossom Probe] Single-server probe failed: {e}"),
        }
    });
}

fn blossom_key(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

fn after_blossom_change() {
    blossom_servers::refresh_cache();
    blossom_servers::republish_blossom_servers_debounced();
}

async fn add_custom_blossom_server(url: String) -> Result<(), String> {
    db::scoped(async move {
        let normalized = blossom_servers::validate_url(&url)?;
        if blossom_servers::is_default_server(&normalized) {
            return Err("Cannot add a default server as custom".to_string());
        }
        let key = blossom_key(&normalized);
        let mut customs = blossom_servers::load_custom_blossom_servers()?;
        if customs.iter().any(|c| blossom_key(&c.url) == key) {
            return Err("Server already exists".to_string());
        }
        customs.push(blossom_servers::CustomBlossomServer { url: normalized.clone(), enabled: true });
        blossom_servers::save_custom_blossom_servers(&customs)?;
        after_blossom_change();
        spawn_probe_for_server(normalized);
        Ok(())
    })
    .await
}

async fn remove_custom_blossom_server(url: String) -> Result<bool, String> {
    db::scoped(async move {
        let key = blossom_key(&url);
        let mut customs = blossom_servers::load_custom_blossom_servers()?;
        let before = customs.len();
        customs.retain(|c| blossom_key(&c.url) != key);
        if customs.len() == before {
            return Ok(false);
        }
        blossom_servers::save_custom_blossom_servers(&customs)?;
        let _ = blossom_capabilities::purge_server(&url);
        after_blossom_change();
        Ok(true)
    })
    .await
}

async fn toggle_custom_blossom_server(url: String, enabled: bool) -> Result<bool, String> {
    db::scoped(async move {
        let key = blossom_key(&url);
        let mut customs = blossom_servers::load_custom_blossom_servers()?;
        let server = customs.iter_mut().find(|c| blossom_key(&c.url) == key).ok_or("Server not found")?;
        server.enabled = enabled;
        let stored = server.url.clone();
        blossom_servers::save_custom_blossom_servers(&customs)?;
        after_blossom_change();
        if enabled {
            spawn_probe_for_server(stored);
        } else {
            let _ = blossom_capabilities::purge_server(&stored);
        }
        Ok(true)
    })
    .await
}

async fn toggle_default_blossom_server(url: String, enabled: bool) -> Result<bool, String> {
    db::scoped(async move {
        if !blossom_servers::is_default_server(&url) {
            return Err("Not a default server".to_string());
        }
        let key = blossom_key(&url);
        let mut disabled = blossom_servers::load_disabled_default_blossom_servers()?;
        if enabled {
            disabled.retain(|d| blossom_key(d) != key);
        } else if !disabled.iter().any(|d| blossom_key(d) == key) {
            disabled.push(key);
        }
        blossom_servers::save_disabled_default_blossom_servers(&disabled)?;
        after_blossom_change();
        if enabled {
            spawn_probe_for_server(url);
        } else {
            let _ = blossom_capabilities::purge_server(&url);
        }
        Ok(true)
    })
    .await
}

// ============================================================================
// Notifications
// ============================================================================

// Desktop's keys: core's `everyone_allowed` reads `notif_mute_everyone`.
const GLOBAL_MUTE_KEY: &str = "notif_global_mute";
const MUTE_EVERYONE_KEY: &str = "notif_mute_everyone";

fn bool_setting(key: &str) -> bool {
    db::get_current_account().is_ok() && matches!(db::get_sql_setting(key.to_string()), Ok(Some(v)) if v == "true")
}

/// The desktop shape; the web plays no notification sounds.
fn notification_settings() -> Value {
    json!({
        "global_mute": bool_setting(GLOBAL_MUTE_KEY),
        "sound": { "type": "None" },
        "mute_everyone": bool_setting(MUTE_EVERYONE_KEY),
    })
}

fn set_notification_settings(a: &Args) -> Result<(), String> {
    let settings = a.get("settings").ok_or("missing argument `settings`")?;
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    db::set_sql_setting(GLOBAL_MUTE_KEY.to_string(), flag("global_mute").to_string())?;
    db::set_sql_setting(MUTE_EVERYONE_KEY.to_string(), flag("mute_everyone").to_string())?;
    Ok(())
}

fn notify_view(scope_id: &str, community_id: Option<&str>) -> Value {
    let stored = notify::prefs(scope_id);
    let resolved = notify::resolve(scope_id, community_id, notify::now_ms());
    json!({
        "scope_id": scope_id,
        "level": stored.level.map(|l| l.as_str()),
        "mute_until": stored.mute_until,
        "suppress_everyone": stored.suppress_everyone,
        "effective_level": resolved.level.as_str(),
        "muted": resolved.muted,
        "everyone_allowed": notify::everyone_allowed(community_id.or(Some(scope_id))),
        "everyone_muted_globally": !notify::everyone_allowed(None),
    })
}

/// The community itself, then each channel resolved against it.
fn get_notify_prefs(community_id: Option<&str>, scope_ids: &[String]) -> Result<Value, String> {
    notify::warm();
    let mut out = Vec::with_capacity(scope_ids.len() + 1);
    if let Some(id) = community_id {
        out.push(notify_view(id, None));
    }
    for scope in scope_ids.iter().filter(|s| Some(s.as_str()) != community_id) {
        out.push(notify_view(scope, community_id));
    }
    Ok(Value::Array(out))
}

async fn toggle_chat_mute(chat_id: String) -> bool {
    db::scoped(async move {
        {
            let mut state = STATE.lock().await;
            // A community-only contact gets a DM row so the mute has somewhere to live.
            if state.get_chat(&chat_id).is_none() {
                if !chat_id.starts_with("npub1") {
                    return false;
                }
                state.create_dm_chat(&chat_id);
            }
        }
        let muted = !notify::prefs(&chat_id).muted_at(notify::now_ms());
        if notify::set_mute(&chat_id, if muted { MUTE_FOREVER } else { MUTE_OFF }).is_err() {
            return false;
        }
        reconcile().await;
        muted
    })
    .await
}

/// Bring `chats.muted` in line with the resolved chain, persisting and
/// surfacing only the rows that moved.
pub(crate) async fn reconcile_mirror() {
    // An unloaded cache answers "default" everywhere; acting on it would clear every mute.
    if !notify::is_loaded() {
        notify::warm();
        if !notify::is_loaded() {
            return;
        }
    }
    let (moved, slims, rows) = {
        let mut state = STATE.lock().await;
        let now = notify::now_ms();
        let mut moved: Vec<(String, bool)> = Vec::new();
        let mut rows = Vec::with_capacity(state.chats.len());
        for i in 0..state.chats.len() {
            let community = state.chats[i].metadata.custom_fields.get("community_id").cloned();
            let want = notify::resolve(&state.chats[i].id, community.as_deref(), now);
            if state.chats[i].muted != want.muted {
                state.chats[i].muted = want.muted;
                moved.push((state.chats[i].id.clone(), want.muted));
            }
            rows.push(json!({
                "id": state.chats[i].id,
                "muted": want.muted,
                "notify": want.ring.as_u8(),
                "everyone": notify::everyone_pings_for_chat(&state.chats[i]),
                "community_muted": notify::community_muted_for_chat(&state.chats[i]),
            }));
        }
        let slims: Vec<_> = state
            .chats
            .iter()
            .filter(|c| moved.iter().any(|(id, _)| id == &c.id))
            .map(|c| db::chats::SlimChatDB::from_chat(c, &state.interner))
            .collect();
        (moved, slims, rows)
    };

    for slim in &slims {
        let _ = db::chats::save_slim_chat(slim);
    }
    for (chat_id, muted) in &moved {
        vector_core::traits::emit_event_json("chat_muted", json!({ "chat_id": chat_id, "value": muted }));
    }
    vector_core::traits::emit_event_json("notify_prefs_changed", json!({ "chats": rows }));
}

/// Mirror, reseed unread counts, re-arm the expiry timer, then publish both
/// synced projections.
async fn reconcile() {
    reconcile_mirror().await;
    let counts = db::events::unread_counts().await.unwrap_or_default();
    STATE.lock().await.unread_seed(counts);
    arm_expiry_timer();
    publish_projection(Pref::Notify);
    publish_projection(Pref::Mutes);
}

pub(crate) fn publish_projection(pref: Pref) {
    db::spawn_bound(async move {
        let Some(client) = state::nostr_client() else { return };
        // Never publish over a relay copy this device hasn't reconciled.
        if !synced_prefs::is_hydrated(pref) {
            return;
        }
        if pref == Pref::Settings {
            let before = synced_prefs::load_settings().streamer;
            match synced_prefs::publish_settings(&client, true).await {
                Ok(Some(json)) => {
                    crate::selfsync::settings_changed(&before, &synced_prefs::SyncedSettings::from_json(&json))
                }
                Ok(None) => {}
                Err(e) => vector_core::log_warn!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag()),
            }
            return;
        }
        let json = match pref {
            Pref::Notify => notify::to_wire().to_json(),
            Pref::Banners => synced_prefs::load_hidden_banners().to_json(),
            Pref::Mutes => {
                let mut list = IdList::default();
                let state = STATE.lock().await;
                for c in state.chats.iter().filter(|c| notify::legacy_muted_for_chat(c)) {
                    let _ = list.add(&c.id);
                }
                list.to_json()
            }
            _ => return,
        };
        if let Err(e) = synced_prefs::publish_raw(&client, pref, &json).await {
            vector_core::log_warn!("[SyncedPrefs] publishing {} failed: {e}", pref.d_tag());
        }
    });
}

struct ExpiryGeneration;

/// One wake-up for the nearest mute expiry. Re-arming bumps the generation,
/// so a superseded timer wakes and exits instead of being aborted mid-sweep.
pub(crate) fn arm_expiry_timer() {
    let generation = db::current_session().scoped::<ExpiryGeneration, AtomicU64>();
    let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(at) = notify::next_expiry(notify::now_ms()) else { return };
    let delay = (at - notify::now_ms()).max(0) as u64;
    db::spawn_bound(async move {
        vector_core::rt::time::sleep(Duration::from_millis(delay)).await;
        if generation.load(Ordering::SeqCst) != mine {
            return;
        }
        if notify::sweep_expired(notify::now_ms()).is_empty() {
            arm_expiry_timer();
        } else {
            reconcile().await;
        }
    });
}
