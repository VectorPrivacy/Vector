//! A Nostr event's video as something the player can open: fetched once (through the account's
//! network, like any media), checked to really be a video and to match the event's hash, then
//! held in memory or kept sealed in the cache.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::json;

/// Larger than any clip a chat bubble should hold; a longer video belongs in a player.
const MAX_VIDEO_BYTES: u64 = 1024 * 1024 * 1024;
const CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 3;
const CANCELLED: &str = "Download cancelled";

/// Videos downloading, and those asked to stop at their next chunk.
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static STOP: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub enum Fetched {
    Held([u8; 32]),
    /// A sealed cache file, and the type its bytes are.
    Sealed(PathBuf, &'static str),
}

pub fn is_cancel(e: &str) -> bool {
    e == CANCELLED
}

/// Stop the download of `url` at its next chunk. False when none is running.
pub fn cancel(url: &str) -> bool {
    if !RUNNING.lock().unwrap().contains(url) {
        return false;
    }
    STOP.lock().unwrap().insert(url.to_string());
    true
}

fn stopped(url: &str) -> bool {
    STOP.lock().unwrap().contains(url)
}

fn progress(url: &str, pct: i64) {
    vector_core::emit_event("embed_video_progress", &json!({ "url": url, "progress": pct }));
}

fn dir() -> Result<PathBuf, String> {
    let dir = vector_core::db::get_app_data_dir()?.join("cache").join("embed_videos");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A file is reused only for the same link AND the same promised hash: another event naming
/// the link with a different hash must not be handed bytes that were never checked against it.
fn stem(url: &str, sha256: Option<&str>) -> String {
    let key = format!("{url}\n{}", sha256.unwrap_or("").to_ascii_lowercase());
    vector_core::crypto::sha256_hex(key.as_bytes())[..32].to_string()
}

fn kind(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        Some(if &head[8..12] == b"qt  " { "mov" } else { "mp4" })
    } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("webm")
    } else {
        None
    }
}

fn mime(ext: &str) -> &'static str {
    match ext {
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "video/mp4",
    }
}

fn cached(dir: &Path, stem: &str) -> Option<Fetched> {
    ["mp4", "webm", "mov"]
        .into_iter()
        .map(|ext| (dir.join(format!("{stem}.{ext}")), ext))
        .find(|(p, _)| vector_core::vcache::readable(p))
        .map(|(p, ext)| Fetched::Sealed(p, mime(ext)))
}

/// The video's opening, enough for its first frame, fetched as byte ranges rather than the
/// whole file. The whole file, when it is already here, does as well.
pub async fn preview(url: String, sha256: Option<String>) -> Result<Fetched, String> {
    if !url.starts_with("https://") {
        return Err("Not a web video".into());
    }
    let dir = dir()?;
    let stem = stem(&url, sha256.as_deref());
    let whole = vector_core::media::url_key("embed-video", &stem);
    if vector_core::media::resident(&whole) {
        return Ok(Fetched::Held(whole));
    }
    if let Some(done) = cached(&dir, &stem) {
        return Ok(done);
    }
    let named = format!("{stem}-poster");
    let held = vector_core::media::url_key("embed-poster", &stem);
    if vector_core::media::resident(&held) {
        return Ok(Fetched::Held(held));
    }
    if let Some(done) = cached(&dir, &named) {
        return Ok(done);
    }
    let poster = vector_core::video_poster::fetch(&url).await?;
    if !vector_core::media::keep_media() {
        vector_core::media::ram::insert(held, poster.bytes, mime(poster.ext)).map_err(|_| "Too much media is open to show this one".to_string())?;
        return Ok(Fetched::Held(held));
    }
    let path = dir.join(format!("{named}.{}", poster.ext));
    vector_core::vcache::write(&path, &poster.bytes)?;
    Ok(Fetched::Sealed(path, mime(poster.ext)))
}

pub async fn fetch(url: String, size: Option<u64>, sha256: Option<String>, fallbacks: Vec<String>) -> Result<Fetched, String> {
    if !url.starts_with("https://") {
        return Err("Not a web video".into());
    }
    let dir = dir()?;
    let stem = stem(&url, sha256.as_deref());
    let held = vector_core::media::url_key("embed-video", &stem);
    let ready = |dir: &Path| vector_core::media::resident(&held).then_some(Fetched::Held(held)).or_else(|| cached(dir, &stem));
    if let Some(done) = ready(&dir) {
        return Ok(done);
    }
    if !RUNNING.lock().unwrap().insert(url.clone()) {
        return Err("This video is already downloading".into());
    }
    STOP.lock().unwrap().remove(&url);
    let result = async {
        let declared = match size.filter(|n| *n > 0) {
            Some(n) => Some(n),
            None => vector_core::net::get_remote_file_size(&url).await,
        };
        if declared.is_some_and(|n| n > MAX_VIDEO_BYTES) {
            return Err("This video is too large to play in chat".to_string());
        }
        // Attachment rules: held in memory when it fits and the account doesn't keep media.
        let in_memory = !vector_core::media::keep_media() && declared.is_some_and(|n| vector_core::media::ram::cache().admits(n));
        let mirrors: Vec<&str> = fallbacks.iter().map(String::as_str).filter(|u| u.starts_with("https://")).take(3).collect();
        let expected = sha256.as_deref().map(str::to_ascii_lowercase);
        let limit = declared.unwrap_or(MAX_VIDEO_BYTES).saturating_add(64 * 1024);
        let mut last = "Failed to download".to_string();
        for source in std::iter::once(url.as_str()).chain(mirrors) {
            for attempt in 1..=MAX_ATTEMPTS {
                let got = if in_memory {
                    hold(&url, source, held, limit, expected.as_deref()).await
                } else {
                    keep(&url, source, &dir, &stem, limit, expected.as_deref()).await
                };
                match got {
                    Ok(done) => return Ok(done),
                    Err(Fail::Cancelled) => return Err(CANCELLED.to_string()),
                    // The bytes arrived and were wrong: another attempt at this host fetches the same.
                    Err(Fail::Refused(e)) => {
                        last = e;
                        break;
                    }
                    Err(Fail::Dropped(e)) => {
                        last = e;
                        if attempt < MAX_ATTEMPTS {
                            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                        }
                    }
                }
            }
        }
        Err(last)
    }
    .await;
    RUNNING.lock().unwrap().remove(&url);
    STOP.lock().unwrap().remove(&url);
    if result.is_ok() {
        progress(&url, 100);
        tokio::task::spawn_blocking(move || prune(&dir));
    }
    result
}

enum Fail {
    Cancelled,
    /// Not the video the event names, or the host refused it.
    Refused(String),
    /// The connection failed or dropped: worth another try.
    Dropped(String),
}

/// One streamed GET of `source`, each chunk handed to `sink`. Reports progress under `url`.
async fn stream(url: &str, source: &str, limit: u64, mut sink: impl FnMut(u64, &[u8]) -> Result<(), String>) -> Result<u64, Fail> {
    vector_core::net::validate_url_not_private(source).map_err(|e| Fail::Refused(e.to_string()))?;
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true).map_err(Fail::Dropped)?;
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, source)
        .await
        .send()
        .await
        .map_err(|e| Fail::Dropped(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Fail::Refused("Media server returned an error status".into()));
    }
    let total = resp.content_length().filter(|&n| n > 0);
    if total.is_some_and(|t| t > limit) {
        return Err(Fail::Refused("This video is too large to play in chat".into()));
    }
    progress(url, if total.is_some() { 0 } else { -1 });
    let mut at = 0u64;
    let mut last = -1;
    while let Some(chunk) = resp.chunk().await.map_err(|e| Fail::Dropped(e.to_string()))? {
        if stopped(url) {
            return Err(Fail::Cancelled);
        }
        if at + chunk.len() as u64 > limit {
            return Err(Fail::Refused("This video is too large to play in chat".into()));
        }
        sink(at, &chunk).map_err(Fail::Refused)?;
        at += chunk.len() as u64;
        if let Some(t) = total {
            let pct = (at * 100 / t).min(99) as i64;
            if pct > last {
                last = pct;
                progress(url, pct);
            }
        }
    }
    if total.is_some_and(|t| at < t) {
        return Err(Fail::Dropped("The download ended early".into()));
    }
    Ok(at)
}

fn verify(head: &[u8], hash: Option<String>, expected: Option<&str>) -> Result<&'static str, Fail> {
    let ext = kind(head).ok_or_else(|| Fail::Refused("Not a video".into()))?;
    if let (Some(want), Some(got)) = (expected, hash)
        && got != want
    {
        return Err(Fail::Refused("The video doesn't match the post's fingerprint".into()));
    }
    Ok(ext)
}

async fn hold(url: &str, source: &str, key: [u8; 32], limit: u64, expected: Option<&str>) -> Result<Fetched, Fail> {
    let mut body = Vec::new();
    stream(url, source, limit, |_, chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })
    .await?;
    let check = expected.is_some();
    let (body, hash) = tokio::task::spawn_blocking(move || {
        let hash = check.then(|| vector_core::crypto::sha256_hex(&body));
        (body, hash)
    })
    .await
    .map_err(|e| Fail::Refused(e.to_string()))?;
    let ext = verify(&body[..body.len().min(12)], hash, expected)?;
    vector_core::media::ram::insert(key, body, mime(ext)).map_err(|_| Fail::Refused("Too much media is open to play this one".into()))?;
    Ok(Fetched::Held(key))
}

/// Streamed into a sealed part file, so no byte of it is ever plain on disk; kept by a rename.
async fn keep(url: &str, source: &str, dir: &Path, stem: &str, limit: u64, expected: Option<&str>) -> Result<Fetched, Fail> {
    use std::io::Write;
    let part = dir.join(format!("{stem}.part"));
    let fail = |e: std::io::Error| Fail::Refused(e.to_string());
    let sealer = vector_core::vcache::Sealer::for_part(&part).map_err(Fail::Refused)?;
    let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&part).map_err(fail)?;
    file.set_len(vector_core::vcache::HEADER_LEN).map_err(fail)?;
    let mut out = std::io::BufWriter::with_capacity(256 * 1024, file);
    std::io::Seek::seek(&mut out, std::io::SeekFrom::Start(vector_core::vcache::HEADER_LEN)).map_err(fail)?;
    let mut head = Vec::with_capacity(12);
    let mut hasher = expected.map(|_| vector_core::crypto::sha256::Sha256::new());
    let streamed = stream(url, source, limit, |at, chunk| {
        if head.len() < 12 {
            head.extend_from_slice(&chunk[..chunk.len().min(12 - head.len())]);
        }
        if let Some(h) = hasher.as_mut() {
            h.update(chunk);
        }
        let mut sealed = chunk.to_vec();
        sealer.seal_at(at, &mut sealed)?;
        out.write_all(&sealed).map_err(|e| e.to_string())
    })
    .await;
    let finished = streamed.and_then(|_| {
        out.flush().map_err(fail)?;
        let hash = hasher.map(|h| h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>());
        let ext = verify(&head, hash, expected)?;
        let path = dir.join(format!("{stem}.{ext}"));
        std::fs::rename(&part, &path).map_err(fail)?;
        Ok(Fetched::Sealed(path, mime(ext)))
    });
    if finished.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    finished
}

/// Oldest videos go first once the folder is past its cap, down to 80%.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let p = e.path();
            let video = matches!(p.extension().and_then(|x| x.to_str()), Some("mp4" | "webm" | "mov"));
            (m.is_file() && video).then(|| (p, m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, _, n)| n).sum();
    if total <= CACHE_MAX_BYTES {
        return;
    }
    entries.sort_by_key(|(_, t, _)| *t);
    // The newest stays: it is about to play.
    entries.pop();
    for (p, _, n) in entries {
        if total <= CACHE_MAX_BYTES * 8 / 10 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(n);
        }
    }
}
