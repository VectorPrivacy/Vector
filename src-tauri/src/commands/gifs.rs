//! The GIF picker's service, fetched from Rust so the privacy setting
//! applies to it. The WebView used to talk to gifverse.net itself, which
//! left every trending list, every search term and every preview as a
//! request from this device even with proxying on; through here the
//! request takes the same egress as everything else.

const GIF_SERVICE: &str = "https://gifverse.net";

/// One GIF API query (`trending?...` or `search?q=...`), answered as the
/// service's JSON text. The path is fixed here; the caller only picks the
/// query, so nothing else on that host can be asked for through this.
#[tauri::command]
pub async fn gif_api(query: String) -> Result<String, String> {
    let (path, _) = query.split_once('?').unwrap_or((query.as_str(), ""));
    if !matches!(path, "trending" | "search") {
        return Err("unknown GIF query".to_string());
    }
    if query.contains("/") || query.contains("..") || query.contains('#') {
        return Err("unknown GIF query".to_string());
    }
    let url = format!("{GIF_SERVICE}/api/v1/{query}");
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(15))?;
    let resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, &url)
        .await
        .send()
        .await
        .map_err(|e| format!("gif service: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("gif service: HTTP {}", resp.status()));
    }
    // A listing is a few KB; a megabyte is already a broken answer.
    const MAX: usize = 1024 * 1024;
    let mut body = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("gif service: {e}"))? {
        if body.len() + chunk.len() > MAX {
            return Err("gif service: answer too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "gif service: answer is not text".to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn only_the_two_listings_can_be_asked_for() {
        for bad in ["media/abc/original.gif", "../admin", "search/x?q=1", "trending#x"] {
            assert!(super::gif_api(bad.to_string()).await.is_err(), "{bad}");
        }
    }
}

/// Fetches of GIF previews in flight at once. They are tens of KB each and
/// nothing else shares this lane, so a scrolled grid fills fast without
/// crowding avatars or attachments.
static PREVIEW_LANE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(6));
const PREVIEW_MAX_BYTES: usize = 8 * 1024 * 1024;
const PREVIEW_CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// One GIF preview (the service's AV1/WebM/MP4 clip, or the GIF itself) as
/// a local file the picker can play, fetched through the privacy setting's
/// egress like everything else. Only the service's media path is accepted.
/// Idempotent: a file already on disk is answered without a request.
#[tauri::command]
pub async fn cache_gif_preview<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    url: String,
) -> Result<Option<String>, String> {
    use tauri::Manager;
    let parsed = url::Url::parse(&url).map_err(|_| "bad preview url".to_string())?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("gifverse.net") || !parsed.path().starts_with("/media/") {
        return Err("not a GIF preview".to_string());
    }
    let ext = match parsed.path().rsplit('.').next() {
        Some("av1") | Some("mp4") => "mp4",
        Some("webm") => "webm",
        Some("gif") => "gif",
        _ => return Err("not a GIF preview".to_string()),
    };
    let dir = handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("cache")
        .join("gif_previews");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = {
        use sha2::Digest;
        let h = sha2::Sha256::digest(url.as_bytes());
        format!("{}.{ext}", h.iter().map(|b| format!("{b:02x}")).collect::<String>())
    };
    let path = dir.join(&name);
    if path.is_file() {
        return Ok(Some(path.to_string_lossy().into_owned()));
    }

    let _permit = PREVIEW_LANE.acquire().await.map_err(|e| e.to_string())?;
    if path.is_file() {
        return Ok(Some(path.to_string_lossy().into_owned()));
    }
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(20))?;
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, &url)
        .await
        .send()
        .await
        .map_err(|e| format!("preview: {e}"))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let mut body: Vec<u8> = Vec::with_capacity(resp.content_length().unwrap_or(0).min(PREVIEW_MAX_BYTES as u64) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("preview: {e}"))? {
        if body.len() + chunk.len() > PREVIEW_MAX_BYTES {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    // Judged by its bytes, never by what the service said: an MP4 family
    // file, a WebM/Matroska one, or a GIF, and nothing else lands on disk.
    let looks_right = match ext {
        "mp4" => body.len() > 12 && &body[4..8] == b"ftyp",
        "webm" => body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => body.starts_with(b"GIF8"),
    };
    if !looks_right {
        return Ok(None);
    }
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, &body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let prune_dir = dir.clone();
    tokio::task::spawn_blocking(move || prune_previews(&prune_dir));
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Oldest previews go first once the folder is past its cap, down to 80%.
fn prune_previews(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (e.path(), m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, _, n)| n).sum();
    if total <= PREVIEW_CACHE_MAX_BYTES {
        return;
    }
    entries.sort_by_key(|(_, t, _)| *t);
    let target = PREVIEW_CACHE_MAX_BYTES * 8 / 10;
    for (p, _, n) in entries {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(n);
        }
    }
}
