//! Mini apps shared as a link to an `.xdc` file, as desktop resolves them
//! (`miniapp_resolve_url_xdc`): fetched once on a tap, validated, kept by
//! content hash, and each message pinned to the version it first opened.
//!
//! The realtime topic is derived from the URL and the message, so everyone who
//! taps the same card lands in the same session whatever bytes they fetched.

use std::path::Path;

use futures_util::StreamExt;
use serde_json::{json, Value};
use vector_core::db;

use super::package;
use crate::emitter;

fn sha256_hex(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    vector_core::simd::hex::bytes_to_hex_string(&h.finalize())
}

fn stored_path(hash: &str) -> String {
    format!("/miniapps/url/{hash}.xdc")
}

fn info(url: &str, msg_id: &str, hash: &str, name: &str) -> Value {
    json!({
        "path": stored_path(hash),
        "hash": hash,
        "name": name,
        "topic": vector_core::webxdc::derive_url_topic_id(url, msg_id),
    })
}

/// `(hash, name)` from a stored latch. The hash becomes a path, so it must be one.
fn read_latch(raw: &str) -> Option<(String, String, String)> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let hash = v.get("hash")?.as_str()?.to_string();
    let name = v.get("name").and_then(Value::as_str).unwrap_or("Mini App").to_string();
    let etag = v.get("etag").and_then(Value::as_str).unwrap_or_default().to_string();
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some((hash, name, etag))
}

async fn held(hash: &str) -> bool {
    vector_core::webfiles::size(Path::new(&stored_path(hash))).await.is_some()
}

/// Resolve a message's `.xdc` link into a playable package. `download: false`
/// is the render-time probe and never touches the network: receiving a message
/// must not become a tracking beacon.
pub async fn resolve(url: String, msg_id: String, download: bool) -> Result<Value, String> {
    db::scoped_result(async move {
        let url_key = format!("xdcurl:{}", sha256_hex(&[url.as_bytes()]));
        let msg_key = format!("xdcmsg:{}", sha256_hex(&[url.as_bytes(), b"|", msg_id.as_bytes()]));

        if let Some((hash, name, _)) = db::get_sql_setting(msg_key.clone())?.as_deref().and_then(read_latch) {
            if held(&hash).await {
                return Ok(info(&url, &msg_id, &hash, &name));
            }
        }
        if !download {
            return Ok(Value::Null);
        }

        // Only http(s), only .xdc paths: anything else never leaves the app.
        let lower = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) || !lower.ends_with(".xdc") {
            return Err("Not a Mini App URL".into());
        }
        vector_core::net::validate_url_not_private(&url).map_err(str::to_string)?;

        // A re-post of a known URL reuses the copy only when the server says it
        // is unchanged: the same URL legitimately changes during development.
        let validator = validator(&url).await;
        if let (Some(saved), Some(current)) = (db::get_sql_setting(url_key.clone())?, validator.as_deref()) {
            if let Some((hash, name, etag)) = read_latch(&saved) {
                if !etag.is_empty() && etag == current && held(&hash).await {
                    db::set_sql_setting(msg_key, json!({ "hash": hash, "name": name }).to_string())?;
                    return Ok(info(&url, &msg_id, &hash, &name));
                }
            }
        }

        let bytes = fetch(&url).await?;
        // Validated before it is kept, by the same gates the opener applies.
        let fallback = lower.rsplit('/').next().unwrap_or("app.xdc").to_string();
        let pkg = package::parse(&bytes, fallback.trim_end_matches(".xdc"))?;
        let hash = pkg.file_hash.clone();
        vector_core::files::write(Path::new(&stored_path(&hash)), &bytes).await?;
        let name = if pkg.manifest.name.is_empty() { fallback } else { pkg.manifest.name.clone() };
        db::set_sql_setting(
            url_key,
            json!({ "hash": hash, "name": name, "etag": validator.unwrap_or_default() }).to_string(),
        )?;
        db::set_sql_setting(msg_key, json!({ "hash": hash, "name": name }).to_string())?;
        Ok(info(&url, &msg_id, &hash, &name))
    })
    .await
}

/// The URL's freshness validator: ETag, else Last-Modified. None means a re-post
/// can't prove the copy is current.
async fn validator(url: &str) -> Option<String> {
    let client = vector_core::net::shared_http_client();
    let res = vector_core::net::proxied_request(&client, reqwest::Method::HEAD, url).await.send().await.ok()?;
    let headers = res.headers();
    headers.get("etag").or_else(|| headers.get("last-modified"))?.to_str().ok().map(str::to_string)
}

/// Through the media proxy when one is offered: most hosts refuse a browser's
/// cross-origin read, which the proxy answers with its own permission.
async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let client = vector_core::net::shared_http_client();
    let resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, url)
        .await
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    if total as usize > package::MAX_PACKAGE_BYTES {
        return Err("App is too large".into());
    }
    let mut bytes = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    let mut last = 0u8;
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.map_err(|e| format!("Download failed: {e}"))?);
        if bytes.len() > package::MAX_PACKAGE_BYTES {
            return Err("App is too large".into());
        }
        if total > 0 {
            let pct = ((bytes.len() as u64 * 100) / total).min(99) as u8;
            if pct != last {
                last = pct;
                emitter::emit("webxdc_url_progress", &json!({ "url": url, "progress": pct }));
            }
        }
    }
    emitter::emit("webxdc_url_progress", &json!({ "url": url, "progress": 100 }));
    Ok(bytes)
}
