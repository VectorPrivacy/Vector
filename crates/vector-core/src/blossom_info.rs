//! The server information document (`GET /.well-known/blossom`).
//!
//! No BUD defines one. Magnitude publishes it (extension `magnitude-info`)
//! and personalises it for a signed caller: the caller's tier, per-file
//! limit, storage and daily allowances with what is used of each, and
//! whether the server would take an upload at all. A server that has it is
//! told what it can do before sending a byte; one that hasn't is learned the
//! old way, by bouncing uploads (`blossom_capabilities`).
//!
//! Documents are cached per account. Ranking reads the cache and never waits
//! on the network; login and the server dialog refresh it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nostr_sdk::prelude::{Event, FinalizeEventAsync, Timestamp, Url};
use nostr_blossom::prelude::*;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::Serialize;
use serde_json::Value;

use crate::signer::VectorSigner;

/// The extension name a server advertises when its document carries limits
/// and a `caller` block.
pub const INFO_EXTENSION: &str = "magnitude-info";

/// What this server will do for the account that asked.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct CallerInfo {
    pub tier: String,
    pub max_blob: u64,
    pub storage_used: u64,
    pub storage_limit: u64,
    pub blobs: u64,
    pub daily_used: u64,
    pub daily_limit: u64,
    /// Whether an upload would be admitted at all; `reasons` says why not.
    pub allowed: bool,
    pub reasons: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: Option<String>,
    pub software: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub buds: Vec<u8>,
    pub extensions: Vec<String>,
    /// The largest blob any account may upload.
    pub max_blob: Option<u64>,
    pub mime: Vec<String>,
    pub capacity_used: Option<u64>,
    pub capacity_limit: Option<u64>,
    pub caller: Option<CallerInfo>,
    /// Unix seconds.
    pub fetched_at: i64,
}

impl ServerInfo {
    /// Read a document. `None` when the JSON is not an information document
    /// at all (a server that answers the path with something else).
    pub fn parse(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let looks_like_one = obj.contains_key("buds")
            || obj.contains_key("software")
            || obj.contains_key("accepts");
        if !looks_like_one {
            return None;
        }
        let str_of = |key: &str| obj.get(key).and_then(|v| v.as_str()).map(|s| s.to_string());
        let u64_at = |path: &[&str]| {
            let mut cur = v;
            for p in path {
                cur = cur.get(p)?;
            }
            cur.as_u64()
        };
        let strings = |val: Option<&Value>| -> Vec<String> {
            val.and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default()
        };
        let caller = obj.get("caller").and_then(|c| {
            let allowed = c.get("allowed")?.as_bool()?;
            let n = |key: &str| c.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            Some(CallerInfo {
                tier: c.get("tier").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                max_blob: n("max_blob"),
                storage_used: n("storage_used"),
                storage_limit: n("storage_limit"),
                blobs: n("blobs"),
                daily_used: n("daily_used"),
                daily_limit: n("daily_limit"),
                allowed,
                reasons: strings(c.get("reasons")),
            })
        });
        Some(ServerInfo {
            name: str_of("name"),
            software: str_of("software"),
            version: str_of("version"),
            description: str_of("description"),
            buds: obj
                .get("buds")
                .and_then(|b| b.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect())
                .unwrap_or_default(),
            extensions: strings(obj.get("extensions")),
            max_blob: u64_at(&["accepts", "max_blob"]),
            mime: strings(obj.get("accepts").and_then(|a| a.get("mime"))),
            capacity_used: u64_at(&["capacity", "stored_bytes"]),
            capacity_limit: u64_at(&["capacity", "limit_bytes"]),
            caller,
            fetched_at: now_secs(),
        })
    }

    /// The document carries limits and a caller block, not just a BUD list.
    pub fn is_personalised(&self) -> bool {
        self.extensions.iter().any(|e| e == INFO_EXTENSION)
    }

    /// Whether this server would take a blob of `size` from us, as far as the
    /// document says. `None` when it says nothing about size.
    pub fn accepts_size(&self, size: u64) -> Option<bool> {
        if let Some(c) = &self.caller {
            return Some(c.allowed && size <= c.max_blob);
        }
        self.max_blob.map(|max| size <= max)
    }

    /// Why an upload of `size` would be refused, for a person. `None` when
    /// the document does not say it would be.
    pub fn refusal_reason(&self, host: &str, size: u64) -> Option<String> {
        let bytes = crate::crypto::format_bytes;
        match &self.caller {
            Some(c) if !c.allowed => Some(match c.reasons.first() {
                Some(why) => format!("{}: {}", host, why),
                None => format!("{} won't take uploads from this account.", host),
            }),
            Some(c) if size > c.max_blob => Some(format!(
                "{} allows files up to {} for your account.", host, bytes(c.max_blob),
            )),
            Some(_) => None,
            None => match self.max_blob {
                Some(max) if size > max => Some(format!("{} allows files up to {}.", host, bytes(max))),
                _ => None,
            },
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn norm_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

// ============================================================================
// Cache — one document per server, per account
// ============================================================================

struct Cached {
    /// `None`: the server was asked and has no document.
    info: Option<ServerInfo>,
    at: Instant,
}

struct InfoCache;
fn cache() -> Arc<Mutex<HashMap<String, Cached>>> {
    crate::db::current_session().scoped::<InfoCache, _>()
}

/// The last document fetched for `server_url`, at any age. `None` when the
/// server has none or was never asked.
pub fn cached(server_url: &str) -> Option<ServerInfo> {
    cache().lock().ok()?.get(&norm_url(server_url))?.info.clone()
}

/// Whether `server_url` has been asked at all this session.
pub fn is_known(server_url: &str) -> bool {
    cache().lock().map(|c| c.contains_key(&norm_url(server_url))).unwrap_or(false)
}

fn age_of(server_url: &str) -> Option<Duration> {
    cache().lock().ok()?.get(&norm_url(server_url)).map(|c| c.at.elapsed())
}

fn store(server_url: &str, info: Option<ServerInfo>) {
    if let Ok(mut c) = cache().lock() {
        c.insert(norm_url(server_url), Cached { info, at: Instant::now() });
    }
}

pub fn invalidate(server_url: &str) {
    if let Ok(mut c) = cache().lock() {
        c.remove(&norm_url(server_url));
    }
}

/// An upload of `bytes` just landed on `server_url`: move the caller's usage
/// so the dialog is right before the next fetch.
pub fn note_upload(server_url: &str, bytes: u64) {
    if let Ok(mut c) = cache().lock() {
        if let Some(caller) = c
            .get_mut(&norm_url(server_url))
            .and_then(|e| e.info.as_mut())
            .and_then(|i| i.caller.as_mut())
        {
            caller.storage_used = caller.storage_used.saturating_add(bytes);
            caller.daily_used = caller.daily_used.saturating_add(bytes);
            caller.blobs = caller.blobs.saturating_add(1);
        }
    }
}

// ============================================================================
// Fetch
// ============================================================================

/// A signed `get` authorization scoped to the server, so the document comes
/// back personalised.
async fn build_info_auth_header<T>(signer: &T, server: &Url) -> Result<reqwest::header::HeaderValue, String>
where
    T: VectorSigner,
{
    let expiration = Timestamp::now() + Duration::from_secs(120);
    let auth = BlossomAuthorization::new(
        "Blossom server information".to_string(),
        expiration,
        BlossomAuthorizationVerb::Get,
        BlossomAuthorizationScope::ServerUrl(server.clone()),
    );
    let auth_event: Event = auth
        .finalize_async(signer)
        .await
        .map_err(|e| format!("Failed to sign auth event: {}", e))?;
    let encoded = base64_simd::STANDARD.encode_to_string(auth_event.as_json());
    reqwest::header::HeaderValue::try_from(format!("Nostr {}", encoded))
        .map_err(|e| format!("Failed to create header value: {}", e))
}

/// Ask `server_url` for its document. `Ok(None)` is a server without one.
pub async fn fetch<T>(signer: &T, server_url: &str) -> Result<Option<ServerInfo>, String>
where
    T: VectorSigner,
{
    let base = Url::parse(&format!("{}/", server_url.trim_end_matches('/')))
        .map_err(|e| format!("Invalid server URL: {}", e))?;
    let doc_url = base
        .join(".well-known/blossom")
        .map_err(|e| format!("Invalid server URL: {}", e))?;
    let auth = build_info_auth_header(signer, &base).await?;
    let client = crate::net::build_http_client(Duration::from_secs(8))?;
    let resp = client
        .get(doc_url)
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, auth)
        .send()
        .await
        .map_err(|e| format!("Info request failed: {}", e))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body = resp.text().await.map_err(|e| format!("Info body unreadable: {}", e))?;
    Ok(serde_json::from_str::<Value>(&body).ok().and_then(|v| ServerInfo::parse(&v)))
}

/// The document for `server_url`, fetched unless a copy younger than
/// `max_age` is held.
pub async fn refresh<T>(signer: &T, server_url: &str, max_age: Duration) -> Result<Option<ServerInfo>, String>
where
    T: VectorSigner,
{
    if age_of(server_url).is_some_and(|age| age < max_age) {
        return Ok(cached(server_url));
    }
    let info = fetch(signer, server_url).await?;
    match &info {
        Some(i) => crate::log_info!(
            "[Blossom Info] {} → {} {} (personalised: {}, tier: {})",
            server_url,
            i.software.as_deref().unwrap_or("?"),
            i.version.as_deref().unwrap_or(""),
            i.is_personalised(),
            i.caller.as_ref().map(|c| c.tier.as_str()).unwrap_or("-"),
        ),
        None => crate::log_debug!("[Blossom Info] {} has no information document", server_url),
    }
    store(server_url, info.clone());
    Ok(info)
}

/// Refresh every server, concurrently. Returns how many have a document.
pub async fn refresh_all<T>(signer: T, server_urls: Vec<String>, max_age: Duration) -> usize
where
    T: VectorSigner + Clone,
{
    let futures = server_urls.into_iter().map(|server| {
        let signer = signer.clone();
        async move {
            match refresh(&signer, &server, max_age).await {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(e) => {
                    crate::log_debug!("[Blossom Info] {} not fetched: {}", server, e);
                    false
                }
            }
        }
    });
    futures_util::future::join_all(futures)
        .await
        .into_iter()
        .filter(|has| *has)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn magnitude_doc() -> Value {
        json!({
            "name": "Magnitude", "software": "magnitude", "version": "0.1.0",
            "buds": [1, 2, 4, 6, 8, 11, 12],
            "extensions": ["magnitude-info", "magnitude-errors"],
            "accepts": { "max_blob": 1073741824, "mime": ["*/*"] },
            "capacity": { "stored_bytes": 552631601, "limit_bytes": 107374182400u64 },
            "description": "Media server for Vector",
            "caller": {
                "pubkey": "ab", "tier": "badged", "max_blob": 1073741824,
                "storage_used": 1000, "storage_limit": 26843545600u64, "blobs": 3,
                "daily_used": 500, "daily_limit": 10737418240u64, "rps": 10,
                "allowed": true, "reasons": []
            }
        })
    }

    #[test]
    fn a_magnitude_document_is_read_in_full() {
        let info = ServerInfo::parse(&magnitude_doc()).unwrap();
        assert_eq!(info.name.as_deref(), Some("Magnitude"));
        assert_eq!(info.buds, vec![1, 2, 4, 6, 8, 11, 12]);
        assert!(info.is_personalised());
        assert_eq!(info.max_blob, Some(1073741824));
        assert_eq!(info.capacity_limit, Some(107374182400));
        let c = info.caller.unwrap();
        assert_eq!(c.tier, "badged");
        assert_eq!(c.storage_limit, 26843545600);
        assert!(c.allowed);
    }

    #[test]
    fn the_caller_block_decides_what_is_accepted() {
        let info = ServerInfo::parse(&magnitude_doc()).unwrap();
        assert_eq!(info.accepts_size(1 << 20), Some(true));
        assert_eq!(info.accepts_size(2 << 30), Some(false));
        assert_eq!(info.refusal_reason("m.example", 1 << 20), None);
        assert_eq!(
            info.refusal_reason("m.example", 2 << 30),
            Some("m.example allows files up to 1.0 GB for your account.".to_string()),
        );
    }

    #[test]
    fn a_closed_gate_refuses_every_size_and_says_why() {
        let mut doc = magnitude_doc();
        doc["caller"] = json!({
            "tier": "default", "max_blob": 0, "allowed": false,
            "reasons": ["Uploads require a profile published from Vector"]
        });
        let info = ServerInfo::parse(&doc).unwrap();
        assert_eq!(info.accepts_size(1), Some(false));
        assert_eq!(
            info.refusal_reason("m.example", 1),
            Some("m.example: Uploads require a profile published from Vector".to_string()),
        );
    }

    #[test]
    fn a_bad_token_leaves_a_caller_that_is_not_allowed() {
        // The shape Magnitude sends when the Authorization header did not verify.
        let mut doc = magnitude_doc();
        doc["caller"] = json!({ "allowed": false, "reasons": ["authorization expired"] });
        let info = ServerInfo::parse(&doc).unwrap();
        let c = info.caller.unwrap();
        assert!(!c.allowed);
        assert_eq!(c.tier, "");
        assert_eq!(c.reasons, vec!["authorization expired"]);
    }

    #[test]
    fn a_document_without_a_caller_falls_back_to_the_server_wide_limit() {
        let mut doc = magnitude_doc();
        doc.as_object_mut().unwrap().remove("caller");
        let info = ServerInfo::parse(&doc).unwrap();
        assert_eq!(info.accepts_size(1 << 20), Some(true));
        assert_eq!(info.accepts_size(2 << 30), Some(false));
        assert_eq!(info.refusal_reason("m.example", 2 << 30), Some("m.example allows files up to 1.0 GB.".to_string()));
    }

    #[test]
    fn a_bare_bud_list_says_nothing_about_size() {
        let info = ServerInfo::parse(&json!({ "buds": [1, 2] })).unwrap();
        assert!(!info.is_personalised());
        assert_eq!(info.accepts_size(1), None);
        assert_eq!(info.refusal_reason("x", 1), None);
    }

    #[test]
    fn something_that_is_not_a_document_is_not_one() {
        assert!(ServerInfo::parse(&json!({ "status": "ok" })).is_none());
        assert!(ServerInfo::parse(&json!("<html>")).is_none());
        assert!(ServerInfo::parse(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn a_noted_upload_moves_the_usage_counters() {
        let info = ServerInfo::parse(&magnitude_doc()).unwrap();
        store("https://m.example/", Some(info));
        note_upload("https://M.example", 250);
        let c = cached("https://m.example").unwrap().caller.unwrap();
        assert_eq!(c.storage_used, 1250);
        assert_eq!(c.daily_used, 750);
        assert_eq!(c.blobs, 4);
        invalidate("https://m.example");
        assert!(cached("https://m.example").is_none());
    }
}
