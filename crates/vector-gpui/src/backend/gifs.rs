//! The GIF picker's service, asked through the privacy setting's egress (`commands/gifs.rs`).

use std::path::PathBuf;
use std::sync::LazyLock;

const SERVICE: &str = "https://gifverse.net";
const LISTING_MAX: usize = 1024 * 1024;
const PREVIEW_MAX: usize = 8 * 1024 * 1024;
const PREVIEW_CACHE_MAX: u64 = 64 * 1024 * 1024;

/// Previews in flight at once; nothing else shares this lane.
static PREVIEW_LANE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(6));

#[derive(Clone)]
pub struct Gif {
    pub id: String,
    pub title: String,
    pub thumbhash: Option<String>,
}

async fn fetch(url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(20))?;
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, url)
        .await
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// One page of `trending` or `search`; the path is fixed here, the caller picks only the query.
pub async fn listing(query: String) -> Result<Vec<Gif>, String> {
    let body = fetch(&format!("{SERVICE}/api/v1/{query}"), LISTING_MAX)
        .await
        .map_err(|e| format!("gif service: {e}"))?
        .ok_or("gif service: no usable answer")?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| "gif service: answer is not JSON")?;
    let results = json
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(results
        .iter()
        .filter_map(|g| {
            let id = g.get("i")?.as_str()?.to_string();
            let s = |k: &str| {
                g.get(k)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some(Gif {
                id,
                title: s("ti").unwrap_or_default(),
                thumbhash: s("th"),
            })
        })
        .collect())
}

/// `encodeURIComponent`.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn trending(offset: usize, limit: usize) -> String {
    format!("trending?limit={limit}&offset={offset}&sort=popular")
}

pub fn search(q: &str, offset: usize, limit: usize) -> String {
    let q = encode(q);
    format!("search?q={q}&limit={limit}&offset={offset}&sort=relevant")
}

/// What a picked GIF sends: the service's original.
pub fn original_url(id: &str) -> String {
    format!("{SERVICE}/media/{id}/original.gif")
}

fn preview_dir() -> Option<PathBuf> {
    let dir = vector_core::db::get_app_data_dir()
        .ok()?
        .join("cache")
        .join("gif_previews");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A GIF's animated preview, kept sealed in the cache.
pub async fn preview(id: String) -> Option<Vec<u8>> {
    let url = original_url(&encode(&id));
    let dir = preview_dir()?;
    let path = dir.join(format!(
        "{}.gif",
        vector_core::crypto::sha256_hex(url.as_bytes())
    ));
    let read = |path: PathBuf| async move {
        tokio::task::spawn_blocking(move || {
            vector_core::vcache::read(&path).ok().map(|b| b.to_vec())
        })
        .await
        .ok()
        .flatten()
    };
    if vector_core::vcache::readable(&path) {
        return read(path).await;
    }
    let _permit = PREVIEW_LANE.acquire().await.ok()?;
    if vector_core::vcache::readable(&path) {
        return read(path).await;
    }
    let body = fetch(&url, PREVIEW_MAX).await.ok()??;
    if !body.starts_with(b"GIF8") {
        return None;
    }
    let (write_path, bytes) = (path.clone(), body);
    tokio::task::spawn_blocking(move || {
        let _ = vector_core::vcache::write(&write_path, &bytes);
        prune(&dir);
        bytes
    })
    .await
    .ok()
}

/// Oldest previews go first once the folder is past its cap, down to 80%.
fn prune(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| {
                (
                    e.path(),
                    m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    m.len(),
                )
            })
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, _, n)| n).sum();
    if total <= PREVIEW_CACHE_MAX {
        return;
    }
    entries.sort_by_key(|(_, t, _)| *t);
    for (p, _, n) in entries {
        if total <= PREVIEW_CACHE_MAX * 8 / 10 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(n);
        }
    }
}
