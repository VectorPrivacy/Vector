//! Relays: the pool this account connects to, their logs and health, and the DM relay list.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde_json::{Value, json};
use vector_core::inbox_relays::normalize_relay_url;
use vector_core::state::{self, TRUSTED_RELAYS as DEFAULT_RELAYS};
use vector_core::{ClientRelayExt, db};

// Relays
// ============================================================================

#[derive(serde::Serialize, Clone, Default)]
pub struct RelayMetrics {
    pub ping_ms: Option<u64>,
    bytes_up: u64,
    bytes_down: u64,
    pub last_check: Option<u64>,
    events_received: u64,
    events_sent: u64,
}

#[derive(serde::Serialize, Clone)]
pub struct RelayLog {
    pub timestamp: u64,
    pub level: String,
    pub message: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct CustomRelay {
    url: String,
    enabled: bool,
    #[serde(default = "default_relay_mode")]
    mode: String,
}

fn default_relay_mode() -> String {
    "both".to_string()
}

pub struct RelayMetricsKey;
pub struct RelayLogsKey;
pub struct MonitorStarted;

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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn add_relay_log(url: &str, level: &str, message: &str) {
    let log = RelayLog {
        timestamp: now_secs(),
        level: level.to_string(),
        message: message.to_string(),
    };
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

pub fn is_default_relay(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/');
    DEFAULT_RELAYS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(normalized))
}

/// A default relay or one the account added.
pub fn is_own_relay(url: &str) -> bool {
    use vector_core::transport::aliases::normalize_host;
    let Ok(host) = normalize_host(url) else { return false };
    let same = |u: &str| normalize_host(u).is_ok_and(|h| h == host);
    DEFAULT_RELAYS.iter().any(|d| same(d)) || load_custom_relays().unwrap_or_default().iter().any(|c| same(&c.url))
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
        Some(json) => {
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse {key}: {e}"))
        }
        None => Ok(T::default()),
    }
}

fn save_json_setting<T: serde::Serialize + ?Sized>(key: &str, value: &T) -> Result<(), String> {
    if db::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }
    let json =
        serde_json::to_string(value).map_err(|e| format!("Failed to serialize {key}: {e}"))?;
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
    out.extend(
        load_custom_relays()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.enabled)
            .map(|c| (c.url, c.mode)),
    );
    out
}

/// Add to the pool and connect. An already-pooled url (a community or
/// discovery relay) is promoted in place to READ+WRITE, keeping its socket.
async fn add_relay(client: &Client, url: &str, caps: RelayCapabilities) -> Result<(), String> {
    let newly_added = client
        .add_managed_relay(url)
        .capabilities(caps)
        .await
        .map_err(|e| e.to_string())?;
    if !newly_added {
        if let Ok(parsed) = RelayUrl::parse(url) {
            if let Some(relay) = client.relays().all().await.get(&parsed) {
                relay
                    .capabilities()
                    .add(RelayCapabilities::READ | RelayCapabilities::WRITE);
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
    if client
        .add_managed_relay(url)
        .capabilities(vector_core::discovery_relay_capabilities())
        .await
        .is_ok()
    {
        let _ = client.connect_relay(url).await;
    }
}

pub async fn get_relays() -> Result<Value, String> {
    let client = state::nostr_client().ok_or("Nostr client not initialized")?;
    let customs = load_custom_relays().unwrap_or_default();
    let disabled = disabled_default_relays().unwrap_or_default();
    let pool = client.relays().await;
    let status_of = |url: &str| {
        pool.iter()
            .find(|(u, _)| {
                u.as_str()
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(url.trim_end_matches('/'))
            })
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

pub async fn toggle_default_relay(url: String, enabled: bool) -> Result<bool, String> {
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

pub async fn add_custom_relay(url: String, mode: Option<String>) -> Result<CustomRelay, String> {
    let normalized = validate_relay_url(&url)?;
    let mode = mode.unwrap_or_else(default_relay_mode);
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }
    let mut relays = load_custom_relays()?;
    if relays
        .iter()
        .any(|r| r.url.eq_ignore_ascii_case(&normalized))
    {
        return Err("Relay already exists".to_string());
    }
    if is_default_relay(&normalized) {
        return Err("Cannot add default relay as custom relay".to_string());
    }
    let relay = CustomRelay {
        url: normalized,
        enabled: true,
        mode,
    };
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

pub async fn remove_custom_relay(url: String) -> Result<bool, String> {
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

pub async fn toggle_custom_relay(url: String, enabled: bool) -> Result<bool, String> {
    let mut relays = load_custom_relays()?;
    let relay = relays
        .iter_mut()
        .find(|r| r.url.eq_ignore_ascii_case(&url))
        .ok_or("Relay not found")?;
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

pub async fn update_relay_mode(url: String, mode: String) -> Result<bool, String> {
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }
    let mut relays = load_custom_relays()?;
    let relay = relays
        .iter_mut()
        .find(|r| r.url.eq_ignore_ascii_case(&url))
        .ok_or("Relay not found")?;
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
    for url in DEFAULT_RELAYS
        .iter()
        .filter(|d| disabled.iter().any(|x| x.eq_ignore_ascii_case(d)))
    {
        add_relay_log(url, "info", "Skipped (disabled by user)");
    }
    let wanted = desired_enabled_relays();
    for (url, mode) in &wanted {
        match client
            .add_managed_relay(url.as_str())
            .capabilities(capabilities_for_mode(mode))
            .await
        {
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
        let _ = client
            .add_managed_relay(url)
            .capabilities(vector_core::discovery_relay_capabilities())
            .await;
    }
    client.connect().await;
}

/// The Relays tab IS the kind 10050 list: apply a newer published copy
/// locally, then merge-publish any outbound diff.
pub async fn reconcile_dm_relay_list() {
    let Some(client) = state::nostr_client() else {
        return;
    };
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

        let plan =
            vector_core::inbox_relays::plan_inbound_reconcile(&remote, remote_ts, &ours, &declined);
        if plan.adopt.is_empty() && plan.revive.is_empty() && plan.retire.is_empty() {
            vector_core::inbox_relays::note_list_seen(remote_ts);
        } else {
            apply_inbound_reconcile(&client, plan, remote_ts).await;
        }
    }

    // Publish the stored list, not pool state: the pool can hold a DM
    // recipient's relays and miss an adoptee whose connect failed.
    let (ours_now, _) = local_relay_view();
    if let Err(e) =
        vector_core::inbox_relays::publish_inbox_relays_synced(&client, fetched, Some(ours_now))
            .await
    {
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
async fn apply_inbound_reconcile(
    client: &Client,
    plan: vector_core::inbox_relays::InboundReconcile,
    remote_ts: u64,
) {
    let norm = normalize_relay_url;
    let mut customs = load_custom_relays().unwrap_or_default();
    let mut disabled = disabled_default_relays().unwrap_or_default();
    let (mut customs_dirty, mut defaults_dirty) = (false, false);
    let (mut adopted, mut revived, mut retired) = (Vec::new(), Vec::new(), Vec::new());

    for url in plan.adopt.iter().filter_map(|u| validate_relay_url(u).ok()) {
        if customs.iter().any(|c| norm(&c.url) == norm(&url)) {
            continue;
        }
        customs.push(CustomRelay {
            url: url.clone(),
            enabled: true,
            mode: default_relay_mode(),
        });
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
        let mode = customs
            .iter()
            .find(|c| norm(&c.url) == norm(url))
            .map(|c| c.mode.clone())
            .unwrap_or_else(default_relay_mode);
        let _ = add_relay(client, url, capabilities_for_mode(&mode)).await;
    }
    for url in &retired {
        let _ = client.remove_relay(url.as_str()).await;
        restore_discovery_role(client, url).await;
        add_relay_log(
            url,
            "info",
            "Disabled (removed from your DM relay list elsewhere)",
        );
    }

    adopted.extend(revived);
    vector_core::inbox_relays::note_contributed(&adopted);
    vector_core::inbox_relays::note_list_seen(remote_ts);
    vector_core::emit_event("relay_list_updated", &());
}

/// Status events, logs, ping metrics and the pool reconcile loop for this
/// session. Catch-up and zombie probing are `listen()`'s.
pub fn start_relay_monitor(client: &Client) {
    if db::current_session()
        .scoped::<MonitorStarted, AtomicBool>()
        .swap(true, Ordering::SeqCst)
    {
        return;
    }
    vector_core::live_subs::set_catch_up(super::sync::catch_up);

    if let Some(monitor) = client.monitor() {
        let mut rx = monitor.subscribe();
        db::spawn_bound(async move {
            while let Ok(MonitorNotification::StatusChanged { relay_url, status }) = rx.recv().await
            {
                let url = relay_url.to_string();
                let level = match status {
                    RelayStatus::Disconnected | RelayStatus::Terminated => "warn",
                    RelayStatus::Banned => "error",
                    _ => "info",
                };
                add_relay_log(
                    &url,
                    level,
                    &format!("Status changed to {}", status_label(status)),
                );
                vector_core::emit_event(
                    "relay_status_change",
                    &json!({ "url": url, "status": status_label(status) }),
                );

                // A reconnected socket carries no live subscriptions: Vector owns reconnects.
                if status == RelayStatus::Connected {
                    if let Some(c) = state::nostr_client() {
                        vector_core::resubscribe_relay_after_reconnect(&c, &relay_url).await;
                    }
                    super::sync::after_reconnect();
                }
            }
        });
    }

    // A relay that sent nothing since the last look must answer a probe, or its socket
    // is redialled: a phone's sockets can die without closing.
    db::spawn_bound(async {
        vector_core::rt::time::sleep(Duration::from_secs(30)).await;
        let mut heard: HashMap<RelayUrl, usize> = HashMap::new();
        while !db::session_stopped() {
            if let Some(client) = state::nostr_client() {
                for (url, relay) in client.relays().await {
                    if relay.status() != RelayStatus::Connected {
                        continue;
                    }
                    let received = relay.stats().bytes_received();
                    if heard
                        .insert(url.clone(), received)
                        .is_some_and(|before| received > before)
                    {
                        update_relay_metrics(url.as_str(), |m| m.last_check = Some(now_secs()));
                        continue;
                    }
                    let start = std::time::Instant::now();
                    // An id no event can have: the relay answers with a bare EOSE.
                    let probe_filter = Filter::new().id(EventId::from_byte_array([0; 32]));
                    let budget = vector_core::relay_request_timeout(Duration::from_secs(8));
                    let probe = vector_core::rt::time::timeout(
                        budget + Duration::from_secs(2),
                        client
                            .fetch_events(ReqTarget::single(url.to_string(), [probe_filter]))
                            .timeout(budget),
                    )
                    .await;
                    heard.insert(url.clone(), relay.stats().bytes_received());
                    if matches!(probe, Ok(Ok(_))) {
                        let ping = start.elapsed().as_millis() as u64;
                        update_relay_metrics(url.as_str(), |m| {
                            m.ping_ms = Some(ping);
                            m.last_check = Some(now_secs());
                        });
                    } else {
                        add_relay_log(url.as_str(), "warn", "Health check failed; reconnecting");
                        relay.disconnect();
                        let _ = relay
                            .try_connect()
                            .timeout(vector_core::relay_connect_timeout(Duration::from_secs(8)))
                            .await;
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
        let mut pending_since: HashMap<String, std::time::Instant> = HashMap::new();
        vector_core::rt::time::sleep(Duration::from_secs(8)).await;
        while !db::session_stopped() {
            if let Some(client) = state::nostr_client() {
                let pooled: Vec<String> = client
                    .relays()
                    .await
                    .keys()
                    .map(|k| norm(k.as_str()))
                    .collect();
                for (url, mode) in desired_enabled_relays() {
                    if pooled.contains(&norm(&url))
                        || client
                            .add_managed_relay(url.as_str())
                            .capabilities(capabilities_for_mode(&mode))
                            .await
                            .is_err()
                    {
                        continue;
                    }
                    add_relay_log(
                        &url,
                        "info",
                        "Reconcile: re-added missing relay; connecting...",
                    );
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
                    let since = *pending_since
                        .entry(key.clone())
                        .or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= Duration::from_secs(15) {
                        add_relay_log(url.as_str(), "warn", "Wedged in Pending; forcing reconnect");
                        relay.disconnect();
                        let _ = relay.try_connect().timeout(Duration::from_secs(5)).await;
                        pending_since.insert(key, std::time::Instant::now());
                    }
                }

                for (_, relay) in client.relays().await {
                    if matches!(
                        relay.status(),
                        RelayStatus::Terminated | RelayStatus::Sleeping
                    ) {
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

/// A relay as the Network list shows it.
#[derive(Clone)]
pub struct RelayRow {
    pub url: String,
    pub status: &'static str,
    pub is_default: bool,
    pub enabled: bool,
    pub mode: String,
}

pub async fn relay_rows() -> Vec<RelayRow> {
    let customs = load_custom_relays().unwrap_or_default();
    let disabled = disabled_default_relays().unwrap_or_default();
    let pool = match state::nostr_client() {
        Some(c) => c.relays().await,
        None => Default::default(),
    };
    let status_of = |url: &str| {
        pool.iter()
            .find(|(u, _)| {
                u.as_str()
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(url.trim_end_matches('/'))
            })
            .map(|(_, r)| status_label(r.status()))
            .unwrap_or("disconnected")
    };
    let mut out: Vec<RelayRow> = DEFAULT_RELAYS
        .iter()
        .map(|url| {
            let enabled = !disabled.iter().any(|d| d.eq_ignore_ascii_case(url));
            RelayRow {
                url: url.to_string(),
                status: if enabled { status_of(url) } else { "disabled" },
                is_default: true,
                enabled,
                mode: "both".into(),
            }
        })
        .collect();
    out.extend(customs.into_iter().map(|c| RelayRow {
        status: if c.enabled {
            status_of(&c.url)
        } else {
            "disabled"
        },
        url: c.url,
        is_default: false,
        enabled: c.enabled,
        mode: c.mode,
    }));
    out
}

pub fn relay_metrics_of(url: &str) -> RelayMetrics {
    relay_metrics()
        .read()
        .ok()
        .and_then(|m| m.get(&log_key(url)).cloned())
        .unwrap_or_default()
}

pub fn relay_logs_of(url: &str) -> Vec<RelayLog> {
    relay_logs()
        .read()
        .ok()
        .and_then(|l| l.get(&log_key(url)).map(|l| l.iter().cloned().collect()))
        .unwrap_or_default()
}

// Media servers
// ============================================================================

use vector_core::blossom_servers as bs;

fn probe_server(url: String) {
    db::spawn_bound(async move {
        if state::nostr_client().is_none() {
            return;
        }
        let Ok(signer) = vector_core::signer::active_signer() else {
            return;
        };
        if vector_core::blossom_info::refresh_all(
            signer.clone(),
            vec![url.clone()],
            Duration::from_secs(60),
        )
        .await
            > 0
        {
            vector_core::traits::emit_event("blossom_info_updated", &());
        }
        if let Ok(n) = vector_core::blossom::probe_servers_for_octet_stream(signer, vec![url]).await
            && n > 0
        {
            vector_core::traits::emit_event("blossom_capabilities_updated", &());
        }
    });
}

pub fn server_rows() -> Vec<bs::BlossomServerInfo> {
    bs::list_all_servers()
}

pub async fn add_server(url: String) -> Result<(), String> {
    db::scoped(async move {
        let normalized = bs::validate_url(&url)?;
        if bs::is_default_server(&normalized) {
            return Err("Cannot add a default server as custom".to_string());
        }
        let key = normalized.to_lowercase();
        let mut customs = bs::load_custom_blossom_servers()?;
        if customs
            .iter()
            .any(|c| c.url.trim_end_matches('/').to_lowercase() == key)
        {
            return Err("Server already exists".to_string());
        }
        customs.push(bs::CustomBlossomServer {
            url: normalized.clone(),
            enabled: true,
        });
        bs::save_custom_blossom_servers(&customs)?;
        bs::refresh_cache();
        bs::republish_blossom_servers_debounced();
        probe_server(normalized);
        Ok(())
    })
    .await
}

pub async fn remove_server(url: String) -> Result<bool, String> {
    db::scoped(async move {
        let target = url.trim().trim_end_matches('/').to_lowercase();
        let mut customs = bs::load_custom_blossom_servers()?;
        let before = customs.len();
        customs.retain(|c| c.url.trim_end_matches('/').to_lowercase() != target);
        if customs.len() == before {
            return Ok(false);
        }
        bs::save_custom_blossom_servers(&customs)?;
        let _ = vector_core::blossom_capabilities::purge_server(&url);
        bs::refresh_cache();
        bs::republish_blossom_servers_debounced();
        Ok(true)
    })
    .await
}

/// Enable or disable a server, default or custom; re-enabling probes it afresh.
pub async fn toggle_server(url: String, enabled: bool) -> Result<(), String> {
    db::scoped(async move {
        let key = url.trim().trim_end_matches('/').to_lowercase();
        if bs::is_default_server(&url) {
            let mut disabled = bs::load_disabled_default_blossom_servers()?;
            if enabled {
                disabled.retain(|d| d.trim_end_matches('/').to_lowercase() != key);
            } else if !disabled
                .iter()
                .any(|d| d.trim_end_matches('/').to_lowercase() == key)
            {
                disabled.push(key);
            }
            bs::save_disabled_default_blossom_servers(&disabled)?;
        } else {
            let mut customs = bs::load_custom_blossom_servers()?;
            customs
                .iter_mut()
                .find(|c| c.url.trim_end_matches('/').to_lowercase() == key)
                .ok_or("Server not found")?
                .enabled = enabled;
            bs::save_custom_blossom_servers(&customs)?;
        }
        bs::refresh_cache();
        bs::republish_blossom_servers_debounced();
        if enabled {
            probe_server(url);
        } else {
            let _ = vector_core::blossom_capabilities::purge_server(&url);
        }
        Ok(())
    })
    .await
}

/// What a media server's dialog paints before asking the network: the held document, this
/// device's history with it, and the status they imply.
pub async fn server_snapshot(url: String, enabled: bool) -> (Option<Value>, Option<Value>, vector_core::blossom_stats::ServerStatus) {
    db::scoped(async move {
        let info = vector_core::blossom_info::cached(&url).and_then(|i| serde_json::to_value(i).ok());
        let stats = vector_core::blossom_stats::stats_for(&url).and_then(|s| serde_json::to_value(s).ok());
        (info, stats, vector_core::blossom_stats::status_for(&url, enabled))
    })
    .await
}

/// The server's information document for this account, refetched past half a minute.
pub async fn server_info(url: String) -> Result<Option<Value>, String> {
    db::scoped(async move {
        let signer = vector_core::signer::active_signer()?;
        let info = vector_core::blossom_info::refresh(&signer, &url, Duration::from_secs(30)).await?;
        Ok(info.and_then(|i| serde_json::to_value(i).ok()))
    })
    .await
}

pub async fn server_stats(url: String, enabled: bool) -> (Option<Value>, vector_core::blossom_stats::ServerStatus) {
    db::scoped(async move {
        let stats = vector_core::blossom_stats::stats_for(&url).and_then(|s| serde_json::to_value(s).ok());
        (stats, vector_core::blossom_stats::status_for(&url, enabled))
    })
    .await
}

/// What uploads have taught us this server takes.
pub async fn server_caps(url: String) -> Result<Vec<Value>, String> {
    db::scoped(async move {
        let list = vector_core::blossom_capabilities::list_for_server(&url)?;
        Ok(list.into_iter().filter_map(|c| serde_json::to_value(c).ok()).collect())
    })
    .await
}
