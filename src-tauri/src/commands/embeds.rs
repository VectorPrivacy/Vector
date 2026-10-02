//! Nostr posts, articles and videos referenced in chat: the event from relays,
//! and an embedded video as a local file the webview can play.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use tauri::{AppHandle, Manager, Runtime};

use crate::net::{self, ProgressReporter};

/// Larger than any clip a chat bubble should hold; a longer video belongs in a player.
const MAX_VIDEO_BYTES: u64 = 1024 * 1024 * 1024;
const VIDEO_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 4;

/// One download per video: a second card asking for the same file waits for the first.
static VIDEO_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
pub async fn fetch_nostr_embed(reference: String) -> Result<vector_core::nostr_embed::Embed, String> {
    vector_core::nostr_embed::fetch(&reference).await
}

/// The video at `url` as a local file, downloaded once (through Tor and the privacy
/// proxy like any other media) and checked to really be a video before it is kept.
/// `fallbacks` are mirrors of the same file; `sha256`, when the event names one,
/// must match or nothing is kept.
#[tauri::command]
pub async fn cache_embed_video<R: Runtime>(
    handle: AppHandle<R>,
    url: String,
    size: Option<u64>,
    sha256: Option<String>,
    fallbacks: Option<Vec<String>>,
) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("Not a web video".into());
    }
    let dir = video_dir(&handle)?;
    let stem = video_stem(&url, sha256.as_deref());
    if let Some(done) = cached(&dir, &stem) {
        return Ok(done);
    }
    let lock = VIDEO_LOCKS.lock().unwrap().entry(url.clone()).or_default().clone();
    let _held = lock.lock().await;
    if let Some(done) = cached(&dir, &stem) {
        return Ok(done);
    }
    net::clear_transfer_cancel(&url);
    let result = download(&dir, &stem, &url, size, sha256.as_deref(), fallbacks.unwrap_or_default()).await;
    net::clear_transfer_cancel(&url);
    VIDEO_LOCKS.lock().unwrap().remove(&url);
    let path = result?;
    let prune_dir = dir.clone();
    let keep = path.clone();
    tokio::task::spawn_blocking(move || prune(&prune_dir, &keep));
    Ok(path)
}

/// A file is reused only for the same link AND the same promised hash: another event naming
/// the link with a different hash must not be handed bytes that were never checked against it.
fn video_stem(url: &str, sha256: Option<&str>) -> String {
    let key = format!("{url}\n{}", sha256.unwrap_or("").to_ascii_lowercase());
    vector_core::crypto::sha256_hex(key.as_bytes())[..32].to_string()
}

/// Stop the download of `url` at its next chunk. False when none is running.
#[tauri::command]
pub async fn cancel_embed_video(url: String) -> bool {
    if !VIDEO_LOCKS.lock().unwrap().contains_key(&url) {
        return false;
    }
    net::cancel_transfer(&url);
    true
}

fn video_dir<R: Runtime>(handle: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = handle.path().app_data_dir().map_err(|e| e.to_string())?.join("cache").join("embed_videos");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn cached(dir: &Path, stem: &str) -> Option<String> {
    ["mp4", "webm", "mov"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

async fn download(
    dir: &Path,
    stem: &str,
    url: &str,
    size: Option<u64>,
    sha256: Option<&str>,
    fallbacks: Vec<String>,
) -> Result<String, String> {
    let declared = match size.filter(|n| *n > 0) {
        Some(n) => Some(n),
        None => vector_core::net::get_remote_file_size(url).await,
    };
    if declared.is_some_and(|n| n > MAX_VIDEO_BYTES) {
        return Err("This video is too large to play in chat".into());
    }
    // The download's size limit comes from here: a host that won't say gets the cap.
    let declared = Some(declared.unwrap_or(MAX_VIDEO_BYTES));
    let part = dir.join(format!("{stem}.part"));
    let reporter = VideoProgress { url };
    let mut last_err = "Failed to download".to_string();
    let mirrors = fallbacks.into_iter().filter(|u| u.starts_with("https://")).take(3);
    for (i, source) in std::iter::once(url.to_string()).chain(mirrors).enumerate() {
        // A partial from another host is only the same file when a hash vouches for it.
        if i > 0 && sha256.is_none() {
            net::discard_partial(&part);
        }
        // Media hosts drop long streams: a dropped one resumes from its checkpoint, with backoff.
        // Only a refusal moves on to the next mirror at once.
        for attempt in 1..=MAX_ATTEMPTS {
            match net::download_to_file(&source, &part, declared, &reporter).await {
                Ok(_) => {
                    let finished = finish(dir, stem, &part, sha256).await;
                    if finished.is_ok() {
                        let _ = reporter.report_complete();
                    }
                    return finished;
                }
                Err(e) if e == net::TRANSFER_CANCELLED => {
                    net::discard_partial(&part);
                    return Err(e.to_string());
                }
                Err(e) => {
                    last_err = e.to_string();
                    let permanent = matches!(e, "Media server returned an error status" | "File exceeds the maximum download size");
                    if permanent || attempt == MAX_ATTEMPTS {
                        break;
                    }
                    vector_core::log_warn!("[EmbedVideo] attempt {attempt} for {source} failed, resuming: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
    Err(last_err)
}

/// Keep the download only if its bytes are a video (and the event's hash, when it gave one).
async fn finish(dir: &Path, stem: &str, part: &Path, sha256: Option<&str>) -> Result<String, String> {
    let part_owned = part.to_path_buf();
    let expected = sha256.map(str::to_ascii_lowercase);
    let verdict = tokio::task::spawn_blocking(move || -> Result<&'static str, String> {
        use std::io::Read;
        let mut file = std::fs::File::open(&part_owned).map_err(|e| e.to_string())?;
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| "Not a video".to_string())?;
        let ext = if &head[4..8] == b"ftyp" {
            if &head[8..12] == b"qt  " { "mov" } else { "mp4" }
        } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
            "webm"
        } else {
            return Err("Not a video".into());
        };
        if let Some(expected) = expected {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            let mut file = std::fs::File::open(&part_owned).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
            let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if got != expected {
                return Err("The video doesn't match the post's fingerprint".into());
            }
        }
        Ok(ext)
    })
    .await
    .map_err(|e| e.to_string())?;
    let ext = match verdict {
        Ok(ext) => ext,
        Err(e) => {
            net::discard_partial(part);
            return Err(e);
        }
    };
    let path = dir.join(format!("{stem}.{ext}"));
    std::fs::rename(part, &path).map_err(|e| e.to_string())?;
    net::discard_partial(part);
    Ok(path.to_string_lossy().into_owned())
}

struct VideoProgress<'a> {
    url: &'a str,
}

impl ProgressReporter for VideoProgress<'_> {
    fn report_progress(&self, percentage: Option<u8>, bytes: Option<u64>, _bps: Option<f64>) -> Result<(), &'static str> {
        if net::transfer_cancelled(self.url) {
            return Err(net::TRANSFER_CANCELLED);
        }
        vector_core::traits::emit_event_json(
            "embed_video_progress",
            serde_json::json!({ "url": self.url, "progress": percentage.map_or(-1, i32::from), "bytesDownloaded": bytes }),
        );
        Ok(())
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        vector_core::traits::emit_event_json("embed_video_progress", serde_json::json!({ "url": self.url, "progress": 100 }));
        Ok(())
    }

    fn cancelled(&self) -> bool {
        net::transfer_cancelled(self.url)
    }
}

/// Oldest videos go first once the folder is past its cap, down to 80%. The one just
/// fetched always stays: it is about to play.
fn prune(dir: &Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let p = e.path();
            let video = matches!(p.extension().and_then(|x| x.to_str()), Some("mp4" | "webm" | "mov"));
            (m.is_file() && video && p.to_string_lossy() != keep).then(|| (p, m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, _, n)| n).sum();
    if total <= VIDEO_CACHE_MAX_BYTES {
        return;
    }
    entries.sort_by_key(|(_, t, _)| *t);
    for (p, _, n) in entries {
        if total <= VIDEO_CACHE_MAX_BYTES * 8 / 10 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(n);
        }
    }
}

/// Empty the embedded-video cache, for the Clear Cache action. Returns the bytes freed.
pub fn clear_embed_videos<R: Runtime>(handle: &AppHandle<R>) -> u64 {
    let Ok(dir) = video_dir(handle) else { return 0 };
    let Ok(rd) = std::fs::read_dir(&dir) else { return 0 };
    // A download in flight keeps its partial; everything else goes.
    let busy = !VIDEO_LOCKS.lock().unwrap().is_empty();
    rd.flatten()
        .filter(|e| !(busy && e.path().extension().is_some_and(|x| x == "part" || x == "ckpt")))
        .filter_map(|e| {
            let len = e.metadata().ok()?.len();
            std::fs::remove_file(e.path()).ok().map(|_| len)
        })
        .sum()
}
