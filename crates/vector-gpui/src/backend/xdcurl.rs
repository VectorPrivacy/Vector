//! A Mini App shared as an `.xdc` link: fetched once on a tap, checked to open, kept by its
//! content hash. Each message stays pinned to the version it first resolved to, so its card,
//! bytes and realtime topic never drift; a newer message revalidates against the server.

use std::path::PathBuf;

use serde_json::json;
use vector_core::db::settings::{get_sql_setting, set_sql_setting};
use vector_media::miniapps::state::MiniAppPackage;
use vector_media::net::{self, ProgressReporter};

#[derive(Clone)]
pub struct Resolved {
    pub path: PathBuf,
    pub topic: String,
    pub name: String,
}

/// The first `.xdc` link in `text`: from an `http` to the next space, quote or angle bracket.
/// One pass, whatever the text repeats.
pub fn find(text: &str) -> Option<&str> {
    let ci = |s: &[u8], at: usize, word: &[u8]| s.get(at..at + word.len()).is_some_and(|w| w.eq_ignore_ascii_case(word));
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>')).find_map(|token| {
        let b = token.as_bytes();
        // The query or fragment the path at `start` ends at: it only moves on.
        let mut query = None;
        (0..b.len()).filter(|&i| ci(b, i, b"http")).find_map(|start| {
            let q = match query {
                Some(q) if q >= start => q,
                _ => *query.insert(b[start..].iter().position(|&c| c == b'?' || c == b'#').map_or(b.len(), |i| start + i)),
            };
            let path = &b[start..q];
            ((ci(path, 0, b"https://") || ci(path, 0, b"http://"))
                && path.len() > "https://.xdc".len()
                && ci(path, path.len() - 4, b".xdc"))
            .then(|| &token[start..])
        })
    })
}

/// The link's file name, for the card before it is fetched.
pub fn file_name(url: &str) -> String {
    let raw = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("app.xdc");
    url::form_urlencoded::parse(raw.replace('+', "%2B").as_bytes())
        .next()
        .map(|(k, _)| k.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| raw.to_string())
}

pub fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

struct Progress<'a> {
    url: &'a str,
}

impl ProgressReporter for Progress<'_> {
    fn report_progress(&self, pct: Option<u8>, _: Option<u64>, _: Option<f64>) -> Result<(), &'static str> {
        vector_core::emit_event("webxdc_url_progress", &json!({ "url": self.url, "progress": pct.unwrap_or(0) }));
        Ok(())
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        vector_core::emit_event("webxdc_url_progress", &json!({ "url": self.url, "progress": 100 }));
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    vector_core::simd::hex::bytes_to_hex_string(bytes)
}

/// A latch's hash becomes a path component, so it is checked to be one.
fn read_latch(raw: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let hash = v.get("hash")?.as_str()?.to_string();
    let name = v.get("name").and_then(|n| n.as_str()).map(str::to_string).unwrap_or_else(|| crate::t::miniapp_unnamed().to_string());
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some((hash, name))
}

/// ETag, else Last-Modified. None means a re-post cannot prove the cache is current.
async fn validator(url: &str) -> Option<String> {
    let client = vector_core::net::build_http_client_with_options(Some(std::time::Duration::from_secs(20)), Some(std::time::Duration::from_secs(10)), true).ok()?;
    let res = vector_core::net::proxied_request(&client, reqwest::Method::HEAD, url).await.send().await.ok()?;
    let headers = res.headers();
    headers.get("etag").or_else(|| headers.get("last-modified"))?.to_str().ok().map(str::to_string)
}

/// The package `msg_id`'s link resolved to. Without `download` this only reads what is here.
pub async fn resolve(url: String, msg_id: String, download: bool) -> Result<Option<Resolved>, String> {
    vector_core::db::scoped(async move {
        let dir = vector_core::db::get_app_data_dir()?.join("miniapps").join("url");
        let url_key = format!("xdcurl:{}", hex(&vector_core::crypto::sha256::digest(url.as_bytes())));
        let msg_key = format!("xdcmsg:{}", hex(&vector_core::crypto::sha256::digest(format!("{url}|{msg_id}").as_bytes())));
        let topic = vector_core::webxdc::derive_url_topic_id(&url, &msg_id);
        let found = |hash: &str, name: String| {
            let path = dir.join(format!("{hash}.xdc"));
            path.exists().then(|| Resolved { path, topic: topic.clone(), name })
        };
        if let Some((hash, name)) = get_sql_setting(msg_key.clone()).ok().flatten().as_deref().and_then(read_latch)
            && let Some(done) = found(&hash, name)
        {
            return Ok(Some(done));
        }
        if !download {
            return Ok(None);
        }
        let lower = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) || !lower.ends_with(".xdc") {
            return Err(crate::t::miniapp_not_a_link().into());
        }
        vector_core::net::validate_url_not_private(&url).map_err(|e| e.to_string())?;
        let current = validator(&url).await;
        // The same link changes between posts while an app is being built: reuse only what the
        // server vouches is unchanged.
        if let (Some(saved), Some(cur)) = (get_sql_setting(url_key.clone()).ok().flatten(), current.as_deref()) {
            let stored = serde_json::from_str::<serde_json::Value>(&saved).ok().and_then(|v| v.get("etag")?.as_str().map(str::to_string));
            if stored.as_deref().is_some_and(|s| !s.is_empty() && s == cur)
                && let Some((hash, name)) = read_latch(&saved)
                && let Some(done) = found(&hash, name.clone())
            {
                let _ = set_sql_setting(msg_key, json!({ "hash": hash, "name": name }).to_string());
                return Ok(Some(done));
            }
        }
        let bytes = net::download_with_reporter(&url, &Progress { url: &url }, None).await.map_err(str::to_string)?;
        let fallback = lower.rsplit('/').next().unwrap_or("app.xdc").to_string();
        // The opener's own gates, before anything is kept: a latch must never pin a package that
        // can't open.
        let (bytes, manifest) = tokio::task::spawn_blocking(move || {
            MiniAppPackage::validate_bytes_openable(&bytes).map_err(|e| e.to_string())?;
            let (manifest, _) = MiniAppPackage::load_info_from_bytes(&bytes, &fallback).map_err(|e| e.to_string())?;
            Ok::<_, String>((bytes, manifest))
        })
        .await
        .map_err(|e| e.to_string())??;
        let hash = hex(&vector_core::crypto::sha256::digest(&bytes));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{hash}.xdc"));
        let tmp = dir.join(format!("{hash}.xdc.tmp-{}", std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        let name = if manifest.name.is_empty() { lower.rsplit('/').next().unwrap_or("app.xdc").to_string() } else { manifest.name.clone() };
        let _ = set_sql_setting(url_key, json!({ "hash": hash, "name": name, "etag": current.unwrap_or_default() }).to_string());
        let _ = set_sql_setting(msg_key, json!({ "hash": hash, "name": name }).to_string());
        Ok(Some(Resolved { path, topic, name }))
    })
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_the_first_mini_app_link() {
        assert_eq!(super::find("try https://x.org/games/chess.xdc now"), Some("https://x.org/games/chess.xdc"));
        assert_eq!(super::find("https://x.org/a.xdc?v=2"), Some("https://x.org/a.xdc?v=2"));
        assert_eq!(super::find("https://x.org/a.xdcz"), None);
        assert_eq!(super::find("no link here.xdc"), None);
        assert_eq!(super::file_name("https://x.org/My%20App.xdc#top"), "My App.xdc");
    }

    /// The scan before it was one pass: it rescanned to the next space from every `http`.
    fn find_by_rescanning(text: &str) -> Option<&str> {
        let lower = text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lower[from..].find("http") {
            let start = from + i;
            let rest = &text[start..];
            let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>')).unwrap_or(rest.len());
            let candidate = &rest[..end];
            let path = candidate.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
            if (path.starts_with("https://") || path.starts_with("http://")) && path.ends_with(".xdc") && path.len() > "https://.xdc".len() {
                return Some(candidate);
            }
            from = start + 4;
        }
        None
    }

    #[test]
    fn one_pass_finds_what_rescanning_found() {
        let parts = ["http", "HTTPS://", "https://", "http://", "x.org/", "a", ".xdc", ".XDC", "?", "#", " ", "\u{a0}", "\"", "<", ">", "'", "東", "é", "xdc", "."];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..40_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut r = seed;
            let s: String = (0..(r % 12) as usize)
                .map(|_| {
                    r = r.rotate_left(5) ^ 0x2545_f491_4f6c_dd1d;
                    parts[(r % parts.len() as u64) as usize]
                })
                .collect();
            assert_eq!(super::find(&s), find_by_rescanning(&s), "{s:?}");
        }
        let long = "https://".repeat(20_000);
        let t = std::time::Instant::now();
        assert_eq!(super::find(&long), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }
}
