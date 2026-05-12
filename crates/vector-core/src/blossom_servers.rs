//! BUD-03 user blossom server list (kind 10063) — store, merge, publish, fetch.
//!
//! Two SQL settings rows back the user's preferences:
//!   * `custom_blossom_servers`             — `Vec<CustomBlossomServer>` JSON
//!   * `disabled_default_blossom_servers`   — `Vec<String>` JSON
//!
//! Resolution order (BUD-03 trust order): enabled defaults, then enabled customs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashSet;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::nostr_client;

/// Default servers in trust order (first = first try). All verified to
/// accept Vector's encrypted octet-stream uploads at common sizes.
pub const DEFAULT_BLOSSOM_SERVERS: &[&str] = &[
    "https://blossom.primal.net",
    "https://nostr.download",
    "https://blossom.ditto.pub",
    "https://blossom.data.haus",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CustomBlossomServer {
    pub url: String,
    pub enabled: bool,
}

/// Validate + canonicalize a server URL: trim, strip trailing slash,
/// auto-prefix `https://` on bare domains, enforce http(s) + non-empty host.
pub fn validate_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("URL cannot be empty".to_string());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };
    let parsed = ::url::Url::parse(&with_scheme)
        .map_err(|e| format!("Invalid URL: {}", e))?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        return Err("URL must use https:// or http://".to_string());
    }
    if parsed.host_str().map_or(true, str::is_empty) {
        return Err("URL must include a host".to_string());
    }
    Ok(with_scheme.trim_end_matches('/').to_string())
}

/// One row in the frontend's Media Servers list.
#[derive(Serialize, Clone, Debug)]
pub struct BlossomServerInfo {
    pub url: String,
    pub is_default: bool,
    pub is_custom: bool,
    pub enabled: bool,
}

// ============================================================================
// Storage
// ============================================================================

pub fn load_custom_blossom_servers() -> Result<Vec<CustomBlossomServer>, String> {
    match crate::db::get_sql_setting("custom_blossom_servers".to_string())
        .ok().flatten()
    {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse custom_blossom_servers: {}", e)),
        None => Ok(Vec::new()),
    }
}

pub fn save_custom_blossom_servers(servers: &[CustomBlossomServer]) -> Result<(), String> {
    let json = serde_json::to_string(servers)
        .map_err(|e| format!("Failed to serialize custom blossom servers: {}", e))?;
    crate::db::set_sql_setting("custom_blossom_servers".to_string(), json)
}

pub fn load_disabled_default_blossom_servers() -> Result<Vec<String>, String> {
    match crate::db::get_sql_setting("disabled_default_blossom_servers".to_string())
        .ok().flatten()
    {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse disabled_default_blossom_servers: {}", e)),
        None => Ok(Vec::new()),
    }
}

pub fn save_disabled_default_blossom_servers(urls: &[String]) -> Result<(), String> {
    let json = serde_json::to_string(urls)
        .map_err(|e| format!("Failed to serialize disabled default blossom servers: {}", e))?;
    crate::db::set_sql_setting("disabled_default_blossom_servers".to_string(), json)
}

// ============================================================================
// Resolver — defaults (minus disabled) + enabled customs, preserving order
// ============================================================================

pub fn is_default_server(url: &str) -> bool {
    let norm = url.trim().trim_end_matches('/').to_lowercase();
    DEFAULT_BLOSSOM_SERVERS.iter()
        .any(|d| d.trim_end_matches('/').to_lowercase() == norm)
}

/// Race-guard for in-flight probes: true if `url` is in the currently
/// enabled list at this moment.
pub fn is_enabled_server(url: &str) -> bool {
    let target = url.trim().trim_end_matches('/').to_lowercase();
    compute_enabled_servers().iter()
        .any(|s| s.trim_end_matches('/').to_lowercase() == target)
}

pub fn compute_enabled_servers() -> Vec<String> {
    let disabled = load_disabled_default_blossom_servers().unwrap_or_default();
    let disabled_lower: HashSet<String> = disabled.iter()
        .map(|s| s.trim().trim_end_matches('/').to_lowercase())
        .collect();

    let mut out: Vec<String> = Vec::new();
    for d in DEFAULT_BLOSSOM_SERVERS {
        let key = d.trim_end_matches('/').to_lowercase();
        if !disabled_lower.contains(&key) {
            out.push((*d).to_string());
        }
    }
    let customs = load_custom_blossom_servers().unwrap_or_default();
    for c in customs {
        if c.enabled {
            out.push(c.url);
        }
    }
    out
}

/// All rows for the frontend (defaults then customs, including disabled).
pub fn list_all_servers() -> Vec<BlossomServerInfo> {
    let disabled = load_disabled_default_blossom_servers().unwrap_or_default();
    let disabled_lower: HashSet<String> = disabled.iter()
        .map(|s| s.trim().trim_end_matches('/').to_lowercase())
        .collect();

    let mut out: Vec<BlossomServerInfo> = Vec::new();
    for d in DEFAULT_BLOSSOM_SERVERS {
        let key = d.trim_end_matches('/').to_lowercase();
        out.push(BlossomServerInfo {
            url: (*d).to_string(),
            is_default: true,
            is_custom: false,
            enabled: !disabled_lower.contains(&key),
        });
    }
    for c in load_custom_blossom_servers().unwrap_or_default() {
        out.push(BlossomServerInfo {
            url: c.url,
            is_default: false,
            is_custom: true,
            enabled: c.enabled,
        });
    }
    out
}

/// Refresh the in-memory `BLOSSOM_SERVERS` cache. Call after edits + on login.
pub fn refresh_cache() {
    let merged = compute_enabled_servers();
    let mutex = crate::state::BLOSSOM_SERVERS
        .get_or_init(|| std::sync::Mutex::new(merged.clone()));
    if let Ok(mut guard) = mutex.lock() {
        *guard = merged;
    }
}

// ============================================================================
// BUD-03 publish (kind 10063)
// ============================================================================

/// Publish the resolved enabled list (defaults + customs, in trust order)
/// as a BUD-03 kind 10063 replaceable event. Peers using our list as a
/// fallback need to see every server we use, not just the customs.
pub async fn publish_blossom_servers(client: &Client) -> Result<(), String> {
    let servers = compute_enabled_servers();
    let mut builder = EventBuilder::new(Kind::Custom(10063), "");
    for url in &servers {
        builder = builder.tag(Tag::custom(TagKind::custom("server"), vec![url.clone()]));
    }
    client.send_event_builder(builder).await
        .map_err(|e| format!("Failed to publish blossom servers: {}", e))?;
    crate::log_info!("[BlossomServers] Published kind 10063 with {} server(s)", servers.len());
    Ok(())
}

static REPUBLISH_GEN: AtomicU64 = AtomicU64::new(0);

/// Debounced republish: rapid edits coalesce; mid-window session swap aborts.
/// Retries once on failure (5s backoff): a stale event on the network would
/// otherwise let `fetch_and_merge_own_list` overwrite the local prefs on
/// the next boot.
pub fn republish_blossom_servers_debounced() {
    let gen = REPUBLISH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
        if !session.is_valid() { return; }
        let client = match nostr_client() {
            Some(c) => c,
            None => return,
        };
        if let Err(e) = publish_blossom_servers(&client).await {
            crate::log_warn!("[BlossomServers] Republish failed: {} (retrying in 5s)", e);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
            if !session.is_valid() { return; }
            if let Err(e2) = publish_blossom_servers(&client).await {
                crate::log_warn!("[BlossomServers] Republish retry failed: {}", e2);
            }
        }
    });
}

// ============================================================================
// BUD-03 fetch — pull our own kind 10063, merge into customs
// ============================================================================

/// Pure merge: append novel URLs from `incoming` (validated, normalized)
/// into `customs` as enabled. Existing rows are never removed or reordered.
pub fn merge_urls_into_customs(
    incoming: &[String],
    mut customs: Vec<CustomBlossomServer>,
) -> (Vec<CustomBlossomServer>, usize) {
    let mut known_lower: HashSet<String> = customs.iter()
        .map(|c| c.url.trim_end_matches('/').to_lowercase())
        .collect();
    for d in DEFAULT_BLOSSOM_SERVERS {
        known_lower.insert(d.trim_end_matches('/').to_lowercase());
    }

    let mut added = 0usize;
    for raw in incoming {
        let normalized = match validate_url(raw) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let key = normalized.to_lowercase();
        if known_lower.contains(&key) { continue; }
        customs.push(CustomBlossomServer { url: normalized, enabled: true });
        known_lower.insert(key);
        added += 1;
    }
    (customs, added)
}

/// Fetch our latest kind 10063 and reconcile: append unknown customs,
/// follow the originating device's default enable/disable choices.
/// `session` gates writes against a mid-fetch account swap.
pub async fn fetch_and_merge_own_list(
    client: &Client,
    my_pubkey: PublicKey,
    session: crate::state::SessionGuard,
) -> Result<usize, String> {
    let filter = Filter::new()
        .author(my_pubkey)
        .kind(Kind::Custom(10063))
        .limit(1);
    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(8))
        .await
        .map_err(|e| format!("Failed to fetch kind 10063: {}", e))?;

    if !session.is_valid() { return Ok(0); }

    let event = match events.into_iter().max_by_key(|e| e.created_at) {
        Some(e) => e,
        None => {
            crate::log_debug!("[BlossomServers] No kind 10063 found for own pubkey");
            return Ok(0);
        }
    };

    let urls_from_event: Vec<String> = event.tags.iter()
        .filter_map(|t| {
            if t.kind() == TagKind::custom("server") {
                t.content().map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();

    // Event presence means the user has published preferences somewhere.
    // A default's absence from the list = explicit disable on that
    // device. (No event at all takes the early return above.)
    let urls_lower: HashSet<String> = urls_from_event.iter()
        .map(|u| u.trim().trim_end_matches('/').to_lowercase())
        .collect();
    let mut disabled = load_disabled_default_blossom_servers().unwrap_or_default();
    let mut defaults_changed = false;
    for d in DEFAULT_BLOSSOM_SERVERS {
        let key = d.trim_end_matches('/').to_lowercase();
        let in_event = urls_lower.contains(&key);
        let currently_disabled = disabled.iter()
            .any(|s| s.trim_end_matches('/').to_lowercase() == key);
        if in_event && currently_disabled {
            disabled.retain(|s| s.trim_end_matches('/').to_lowercase() != key);
            defaults_changed = true;
        } else if !in_event && !currently_disabled {
            disabled.push(d.to_string());
            defaults_changed = true;
        }
    }

    let customs = load_custom_blossom_servers().unwrap_or_default();
    let (new_customs, customs_added) = merge_urls_into_customs(&urls_from_event, customs);

    let any_changes = customs_added > 0 || defaults_changed;
    if any_changes {
        if !session.is_valid() { return Ok(0); }
        if defaults_changed {
            save_disabled_default_blossom_servers(&disabled)?;
        }
        if customs_added > 0 {
            save_custom_blossom_servers(&new_customs)?;
        }
        if !session.is_valid() { return Ok(customs_added); }
        refresh_cache();
        crate::traits::emit_event("blossom_servers_updated", &());
        crate::log_info!(
            "[BlossomServers] Merged kind 10063: {} custom server(s) added, defaults reconciled (disabled now {})",
            customs_added, disabled.len(),
        );
    }
    Ok(customs_added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(url: &str, enabled: bool) -> CustomBlossomServer {
        CustomBlossomServer { url: url.to_string(), enabled }
    }

    #[test]
    fn validate_url_strips_trailing_slash_and_whitespace() {
        assert_eq!(validate_url("  https://example.com/  ").unwrap(), "https://example.com");
        assert_eq!(validate_url("https://example.com").unwrap(), "https://example.com");
    }

    #[test]
    fn validate_url_auto_prefixes_bare_domain_with_https() {
        assert_eq!(validate_url("blossom.band").unwrap(), "https://blossom.band");
        assert_eq!(validate_url("  blossom.primal.net/ ").unwrap(), "https://blossom.primal.net");
    }

    #[test]
    fn validate_url_keeps_explicit_http_scheme() {
        assert_eq!(validate_url("http://localhost:8080").unwrap(), "http://localhost:8080");
    }

    #[test]
    fn validate_url_rejects_non_http_schemes() {
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("wss://example.com").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn validate_url_rejects_empty_or_hostless() {
        assert!(validate_url("").is_err());
        assert!(validate_url("https://").is_err());
        assert!(validate_url("not a url").is_err());
    }

    #[test]
    fn is_default_server_normalizes() {
        assert!(is_default_server("https://blossom.primal.net"));
        assert!(is_default_server("https://blossom.primal.net/"));
        assert!(is_default_server("HTTPS://BLOSSOM.PRIMAL.NET"));
        assert!(!is_default_server("https://other.example.com"));
    }

    #[test]
    fn merge_appends_new_urls_normalized() {
        let incoming = vec![
            "https://new.example.com/".to_string(),
            "https://another.example.com".to_string(),
        ];
        let (out, added) = merge_urls_into_customs(&incoming, vec![]);
        assert_eq!(added, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://new.example.com");
        assert!(out[0].enabled);
    }

    #[test]
    fn merge_skips_urls_already_in_customs_regardless_of_slash_or_case() {
        let existing = vec![custom("https://Existing.example.com", true)];
        let incoming = vec![
            "https://existing.example.com/".to_string(),
            "HTTPS://EXISTING.EXAMPLE.COM".to_string(),
        ];
        let (out, added) = merge_urls_into_customs(&incoming, existing);
        assert_eq!(added, 0);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn merge_skips_default_servers() {
        let incoming = vec!["https://blossom.primal.net/".to_string()];
        let (out, added) = merge_urls_into_customs(&incoming, vec![]);
        assert_eq!(added, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn merge_drops_malformed_urls() {
        let incoming = vec![
            "".to_string(),
            "not a url".to_string(),
            "ftp://example.com".to_string(),
            "https://".to_string(),
            "https://valid.example.com".to_string(),
        ];
        let (out, added) = merge_urls_into_customs(&incoming, vec![]);
        assert_eq!(added, 1);
        assert_eq!(out[0].url, "https://valid.example.com");
    }

    #[test]
    fn merge_preserves_existing_order_and_appends() {
        let existing = vec![
            custom("https://a.example.com", true),
            custom("https://b.example.com", false),
        ];
        let incoming = vec!["https://c.example.com".to_string()];
        let (out, _) = merge_urls_into_customs(&incoming, existing);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].url, "https://a.example.com");
        assert_eq!(out[1].url, "https://b.example.com");
        assert!(!out[1].enabled, "merge must not flip enabled state of existing rows");
        assert_eq!(out[2].url, "https://c.example.com");
    }
}
