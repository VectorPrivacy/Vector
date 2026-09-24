use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use futures_util::StreamExt;
use reqwest::{self, Client};
use serde_json::json;
use tauri::{AppHandle, Emitter};

pub use vector_core::net::validate_url_not_private;
pub use vector_core::SiteMetadata;

use crate::simd::html_meta;

/// Transfers the user asked to stop, keyed by the id their progress events carry.
/// The reporter consults it once per chunk, so a stop lands inside the body read
/// instead of after a mirror walk that can run for minutes.
static CANCELLED_TRANSFERS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The error a cancelled transfer returns. Callers must not retry on it.
pub const TRANSFER_CANCELLED: &str = "Download cancelled";

/// Ask the transfer reporting under `id` to stop at its next chunk.
pub fn cancel_transfer(id: &str) {
    CANCELLED_TRANSFERS.lock().unwrap().insert(id.to_ascii_lowercase());
}

/// Whether a stop is outstanding for `id`.
pub fn transfer_cancelled(id: &str) -> bool {
    CANCELLED_TRANSFERS.lock().unwrap().contains(&id.to_ascii_lowercase())
}

/// Drop `id`'s stop. The download owning the id clears it on entry and on exit,
/// so a cancelled transfer never poisons the next attempt at the same file.
pub fn clear_transfer_cancel(id: &str) {
    CANCELLED_TRANSFERS.lock().unwrap().remove(&id.to_ascii_lowercase());
}

/// Trait for reporting download progress
pub trait ProgressReporter {
    /// Report progress of a download
    fn report_progress(&self, percentage: Option<u8>, bytes_downloaded: Option<u64>, bytes_per_sec: Option<f64>) -> Result<(), &'static str>;

    /// Report completion of a download
    fn report_complete(&self) -> Result<(), &'static str>;

    /// Whether the user has asked this transfer to stop. Checked at the points a
    /// download spends real time without reporting progress, so a stop during the
    /// size probe lands as fast as one mid-body.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A no-op progress reporter that does nothing when progress is reported
pub struct NoOpProgressReporter;

impl NoOpProgressReporter {
    /// Create a new NoOpProgressReporter
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {}
    }
}

impl ProgressReporter for NoOpProgressReporter {
    fn report_progress(&self, _percentage: Option<u8>, _bytes_downloaded: Option<u64>, _bytes_per_sec: Option<f64>) -> Result<(), &'static str> {
        // Do nothing
        Ok(())
    }
    
    fn report_complete(&self) -> Result<(), &'static str> {
        // Do nothing
        Ok(())
    }
}

/// Tauri implementation of ProgressReporter
pub struct TauriProgressReporter<'a, R: tauri::Runtime> {
    handle: &'a AppHandle<R>,
    attachment_id: &'a str,
}

impl<'a, R: tauri::Runtime> TauriProgressReporter<'a, R> {
    /// Create a new TauriProgressReporter
    pub fn new(handle: &'a AppHandle<R>, attachment_id: &'a str) -> Self {
        Self { handle, attachment_id }
    }
}

impl<'a, R: tauri::Runtime> ProgressReporter for TauriProgressReporter<'a, R> {
    fn report_progress(&self, percentage: Option<u8>, bytes_downloaded: Option<u64>, bytes_per_sec: Option<f64>) -> Result<(), &'static str> {
        // Every download loop propagates this error, so it is also the stop signal.
        if transfer_cancelled(self.attachment_id) {
            return Err(TRANSFER_CANCELLED);
        }
        let mut payload = json!({
            "id": self.attachment_id
        });

        if let Some(p) = percentage {
            payload["progress"] = json!(p);
        } else {
            payload["progress"] = json!(-1); // Use -1 to indicate unknown progress
        }

        if let Some(bytes) = bytes_downloaded {
            payload["bytesDownloaded"] = json!(bytes);
        }

        if let Some(bps) = bytes_per_sec {
            payload["bytesPerSec"] = json!(bps);
        }

        self.handle
            .emit("attachment_download_progress", payload)
            .map_err(|_| "Failed to emit event")
    }
    
    fn cancelled(&self) -> bool {
        transfer_cancelled(self.attachment_id)
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        if transfer_cancelled(self.attachment_id) {
            return Err(TRANSFER_CANCELLED);
        }
        self.handle
            .emit(
                "attachment_download_progress",
                json!({
                    "id": self.attachment_id,
                    "progress": 100
                }),
            )
            .map_err(|_| "Failed to emit event")
    }
}

/// Attach a proxied request's signed authorization, when there is one.
fn with_auth(req: reqwest::RequestBuilder, auth: Option<&reqwest::header::HeaderValue>) -> reqwest::RequestBuilder {
    match auth {
        Some(v) => req.header(reqwest::header::AUTHORIZATION, v.clone()),
        None => req,
    }
}

/// How often a streamed download makes its progress durable: the bytes are synced
/// and the checkpoint moved. A resume starts from the last one, never from the
/// file's length, which a power cut can leave covering bytes that never landed.
const CHECKPOINT_EVERY: u64 = 4 * 1024 * 1024;

/// A partial download's durable progress, kept beside its bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Checkpoint {
    /// Bytes known to be on disk.
    pub offset: u64,
    /// The whole file's size, once a response has said it.
    pub total: Option<u64>,
}

fn checkpoint_path(part: &std::path::Path) -> std::path::PathBuf {
    part.with_extension("ckpt")
}

/// The checkpoint beside `part`, if both it and enough bytes to back it exist.
pub fn read_checkpoint(part: &std::path::Path) -> Option<Checkpoint> {
    let raw = std::fs::read(checkpoint_path(part)).ok()?;
    let raw: [u8; 16] = raw.try_into().ok()?;
    let offset = u64::from_le_bytes(raw[..8].try_into().ok()?);
    let total = u64::from_le_bytes(raw[8..].try_into().ok()?);
    let on_disk = std::fs::metadata(part).ok()?.len();
    (on_disk >= offset).then_some(Checkpoint { offset, total: (total > 0).then_some(total) })
}

fn write_checkpoint(part: &std::path::Path, at: Checkpoint) -> std::io::Result<()> {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&at.offset.to_le_bytes());
    raw[8..].copy_from_slice(&at.total.unwrap_or(0).to_le_bytes());
    let tmp = part.with_extension("ckpt.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut f, &raw)?;
    f.sync_all()?;
    std::fs::rename(tmp, checkpoint_path(part))
}

/// Forget a partial download: its bytes and its checkpoint.
pub fn discard_partial(part: &std::path::Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(checkpoint_path(part));
}

/// Stream `content_url` into `part`, resuming from its checkpoint when there is
/// one. Holds a chunk at a time, never the file. On any failure the bytes so far
/// stay behind a fresh checkpoint for the next attempt, from this source or another
/// serving the same blob. Returns the file's length once every byte is on disk.
pub async fn download_to_file(
    content_url: &str,
    part: &std::path::Path,
    declared: Option<u64>,
    reporter: &impl ProgressReporter,
) -> Result<u64, &'static str> {
    // The message said how big the blob is, so a body running past that is not it.
    // The slack covers senders that state the plaintext's size, 16 bytes short.
    let limit = declared.filter(|&d| d > 0).map(|d| d + 64 * 1024).unwrap_or(UNDECLARED_MAX_BYTES);
    use tokio::io::AsyncWriteExt;

    validate_url_not_private(content_url)?;
    let vector_core::net::Egress { url: fetch_url, auth } = vector_core::net::egress(content_url).await;
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::TRANSFER_STALL), true)
        .map_err(|_| "Failed to create HTTP client")?;
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "Failed to prepare the download")?;
    }

    let mut at = read_checkpoint(part).unwrap_or_default();
    // A checkpoint that already covers the whole file needs no request at all.
    if at.total.is_some_and(|t| t > 0 && at.offset == t) {
        return Ok(at.offset);
    }
    // Two passes at most: a server that answers the range with the wrong bytes is
    // asked once more for the whole file.
    for _ in 0..2 {
        if reporter.cancelled() {
            return Err(TRANSFER_CANCELLED);
        }
        let mut request = with_auth(client.get(&fetch_url), auth.as_ref());
        if at.offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={}-", at.offset));
        }
        let res = request.send().await.map_err(|e| {
            vector_core::log_warn!("[AttachmentDownload] request failed for {}: {}", fetch_url, e);
            "Failed to download"
        })?;

        let status = res.status().as_u16();
        let (start, total) = match status {
            206 => {
                // `bytes START-END/TOTAL`; anything else can't be trusted to line up.
                let range = res.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).unwrap_or("");
                let parsed = range.strip_prefix("bytes ").and_then(|r| {
                    let (span, total) = r.split_once('/')?;
                    let start = span.split_once('-')?.0.parse::<u64>().ok()?;
                    Some((start, total.parse::<u64>().ok()))
                });
                match parsed {
                    Some((start, total)) if start == at.offset => (start, total),
                    _ => {
                        at = Checkpoint::default();
                        continue;
                    }
                }
            }
            // The whole body: the server ignored the range, or none was asked.
            200 => (0, res.content_length().filter(|&n| n > 0)),
            416 if at.offset > 0 => {
                // Nothing past the offset: finished if the offset is the whole file.
                if at.total == Some(at.offset) {
                    return Ok(at.offset);
                }
                at = Checkpoint::default();
                continue;
            }
            _ => {
                vector_core::log_debug!("[AttachmentDownload] HTTP {} for {}", status, fetch_url);
                return Err("Media server returned an error status");
            }
        };
        let total = total.or_else(|| res.content_length().map(|n| n + start));
        if total.is_some_and(|t| t > limit) {
            return Err("File exceeds the maximum download size");
        }

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(part)
            .await
            .map_err(|_| "Failed to prepare the download")?;
        file.set_len(start).await.map_err(|_| "Failed to prepare the download")?;
        let mut file = tokio::io::BufWriter::with_capacity(256 * 1024, file);
        tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(start))
            .await
            .map_err(|_| "Failed to prepare the download")?;

        let mut written = start;
        let mut durable = Checkpoint { offset: start, total };
        let _ = write_checkpoint(part, durable);
        let started = std::time::Instant::now();
        let mut last_percentage: Option<u8> = None;
        let report = |written: u64, secs: f64, last: &mut Option<u8>| -> Result<(), &'static str> {
            let speed = (secs > 0.0).then(|| (written - start) as f64 / secs);
            match total {
                Some(t) if t > 0 => {
                    let pct = ((written as f64 / t as f64) * 100.0).min(100.0) as u8;
                    if *last != Some(pct) {
                        *last = Some(pct);
                        reporter.report_progress(Some(pct), Some(written), speed)?;
                    }
                    Ok(())
                }
                _ => reporter.report_progress(None, Some(written), speed),
            }
        };
        // A resume paints where it stands before the first new byte.
        report(written, 0.0, &mut last_percentage)?;

        // Make everything so far durable; the failure path runs it too.
        async fn settle(
            file: &mut tokio::io::BufWriter<tokio::fs::File>,
            part: &std::path::Path,
            at: Checkpoint,
        ) -> Result<(), &'static str> {
            file.flush().await.map_err(|_| "Failed to save the download")?;
            file.get_ref().sync_data().await.map_err(|_| "Failed to save the download")?;
            write_checkpoint(part, at).map_err(|_| "Failed to save the download")
        }

        let mut stream = res.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    vector_core::log_warn!("[AttachmentDownload] stream interrupted for {}: {}", fetch_url, e);
                    let _ = settle(&mut file, part, Checkpoint { offset: written, total }).await;
                    return Err("Error downloading chunk");
                }
            };
            if written + chunk.len() as u64 > limit {
                return Err("File exceeds the maximum download size");
            }
            file.write_all(&chunk).await.map_err(|_| "Failed to save the download")?;
            written += chunk.len() as u64;
            if written - durable.offset >= CHECKPOINT_EVERY {
                durable = Checkpoint { offset: written, total };
                settle(&mut file, part, durable).await?;
            }
            if let Err(e) = report(written, started.elapsed().as_secs_f64(), &mut last_percentage) {
                let _ = settle(&mut file, part, Checkpoint { offset: written, total }).await;
                return Err(e);
            }
        }
        settle(&mut file, part, Checkpoint { offset: written, total: total.or(Some(written)) }).await?;
        if total.is_some_and(|t| written < t) {
            return Err("Error downloading chunk");
        }
        reporter.report_complete()?;
        return Ok(written);
    }
    Err("Server did not honor range request")
}

/// A streamed download's ceiling when its message declared no size: only disk is at
/// stake, so it guards against an endless body rather than bounding the file.
const UNDECLARED_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Hard ceiling for any download held in memory (inline images, mini-apps;
/// attachments stream to disk instead). The advertised size is attacker/server
/// controlled — without a cap, a lying Content-Length means a giant
/// `Vec::with_capacity` (alloc abort = process kill) and an endless body
/// streams until OOM.
pub const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

/// Cap for pre-allocation hints: never trust the advertised size for more
/// than this up front; the Vec grows naturally past it for honest servers.
const MAX_PREALLOC_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Generic download function that works with any progress reporter
pub async fn download_with_reporter(
    content_url: &str,
    reporter: &impl ProgressReporter,
    timeout: Option<std::time::Duration>,
) -> Result<Vec<u8>, &'static str> {
    validate_url_not_private(content_url)?;
    // Through the user's Magnitude when the privacy setting is on and the
    // host is not ours: the host sees the edge, not this device. Every
    // attachment and picture comes through here, so this is the one place.
    let vector_core::net::Egress { url: fetch_url, auth: authorization } = vector_core::net::egress(content_url).await;

    // Route through vector-core so the Tor failsafe applies — blackhole when
    // Tor is enabled-but-inactive, proxy when Tor is up. No deadline unless the
    // caller sets one: a download is bounded by progress, so a slow link
    // finishes and only a body that stops moving is abandoned.
    let client = vector_core::net::build_http_client_with_options(
        timeout,
        Some(vector_core::net::TRANSFER_STALL),
        true,
    )
    .map_err(|_| "Failed to create HTTP client")?;

    // One GET: its Content-Length sizes the progress, so no probe round trips go first.
    download_with_streaming(&client, &fetch_url, reporter, authorization.as_ref()).await
}

/// Downloads using a streaming approach with progress reporting
async fn download_with_streaming(
    client: &Client,
    url: &str,
    reporter: &impl ProgressReporter,
    auth: Option<&reqwest::header::HeaderValue>,
) -> Result<Vec<u8>, &'static str> {
    if reporter.cancelled() {
        return Err(TRANSFER_CANCELLED);
    }
    let res = with_auth(client.get(url), auth)
        .send()
        .await
        .map_err(|e| {
            vector_core::log_warn!("[AttachmentDownload] request failed for {}: {}", url, e);
            "Failed to download"
        })?;

    // A non-2xx (commonly 404 for a blob deleted/expired on the media server)
    // still returns Ok from send() and would otherwise stream the error page,
    // surfacing later as a misleading "file too small" / "decryption failed".
    if !res.status().is_success() {
        // A 404/expired blob is a normal outcome (favicons, missing previews, GC'd
        // media) — debug, not a warning, so the default log isn't flooded.
        vector_core::log_debug!("[AttachmentDownload] HTTP {} for {}", res.status().as_u16(), url);
        return Err("Media server returned an error status");
    }

    let total_size = res.content_length().filter(|&n| n > 0);
    if matches!(total_size, Some(size) if size > MAX_DOWNLOAD_BYTES) {
        return Err("File exceeds the maximum download size");
    }
    let started = std::time::Instant::now();

    // Create a buffer to store all data
    let capacity = total_size.unwrap_or(1024 * 1024).min(MAX_PREALLOC_BYTES) as usize;
    let mut result = Vec::with_capacity(capacity);
    let mut downloaded: u64 = 0;
    let mut last_emitted_percentage: u8 = 0;
    let mut last_bytes_update: u64 = 0;

    // Get the stream and process it
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        if reporter.cancelled() {
            return Err(TRANSFER_CANCELLED);
        }
        let chunk = item.map_err(|e| {
            vector_core::log_warn!("[AttachmentDownload] stream interrupted for {}: {}", url, e);
            "Error downloading chunk"
        })?;

        result.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err("File exceeds the maximum download size");
        }

        // Report progress
        if let Some(size) = total_size {
            // We know the total size
            let progress = (downloaded as f64 / size as f64) * 100.0;
            let current_percentage = progress as u8;

            // Only emit events when percentage changes (to reduce events)
            if current_percentage > last_emitted_percentage {
                let secs = started.elapsed().as_secs_f64();
                let speed = (secs > 0.0).then(|| downloaded as f64 / secs);
                reporter.report_progress(Some(current_percentage), Some(downloaded), speed)?;
                last_emitted_percentage = current_percentage;
            }
        } else {
            // Unknown size, emit progress updates at reasonable intervals
            // For example, every 256KB
            if downloaded - last_bytes_update >= 256 * 1024 {
            // We can't calculate percentage, but we can still show activity
            // Report with bytes downloaded instead of percentage
            reporter.report_progress(None, Some(downloaded), None)?;

                last_bytes_update = downloaded;
            }
        }
    }

    // Final event with complete status
    reporter.report_complete()?;

    Ok(result)
}


/// Fetch metadata specifically for Twitter/X posts using their oEmbed API
async fn fetch_twitter_metadata(url: &str) -> Result<SiteMetadata, String> {
    validate_url_not_private(url).map_err(|e| e.to_string())?;
    // Use Twitter's oEmbed API for reliable metadata extraction
    let encoded_url = url.replace("&", "%26").replace("?", "%3F").replace("=", "%3D");
    let oembed_url = format!("https://publish.twitter.com/oembed?url={}", encoded_url);
    
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(10))?;

    let response = client
        .get(&oembed_url)
        .send()
        .await
        .map_err(|e| format!("Twitter oEmbed request failed: {}", e))?;
    
    if !response.status().is_success() {
        return Err(format!("Twitter oEmbed returned status: {}", response.status()));
    }
    
    let oembed_data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse Twitter oEmbed response: {}", e))?;
    
    // Extract metadata from oEmbed response
    let author_name = oembed_data["author_name"].as_str().unwrap_or("Twitter");
    let html = oembed_data["html"].as_str().unwrap_or("");
    
    // Parse the HTML to extract the tweet text
    let tweet_text = html_meta::extract_first_p_inner_html(html)
        .map(|h| h.split("<a ").next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    
    // Note: Twitter's oEmbed API does not provide images for regular tweets
    // Images are only available for video tweets via thumbnail_url
    let thumbnail_url = oembed_data["thumbnail_url"]
        .as_str()
        .map(|s| s.to_string());
    
    let metadata = SiteMetadata {
        domain: "https://x.com/".to_string(),
        og_title: Some(format!("{} on X", author_name)),
        og_description: Some(tweet_text),
        og_image: thumbnail_url,
        og_url: Some(url.to_string()),
        og_type: Some("article".to_string()),
        title: Some(format!("{} on X", author_name)),
        description: Some(format!("Post by {}", author_name)),
        favicon: Some("https://abs.twimg.com/favicons/twitter.3.ico".to_string()),
    };
    
    Ok(metadata)
}

pub async fn fetch_site_metadata(url: &str) -> Result<SiteMetadata, String> {
    validate_url_not_private(url).map_err(|e| e.to_string())?;
    // Check if this is a Twitter/X URL and use specialized handler
    if url.contains("twitter.com") || url.contains("x.com") {
        return fetch_twitter_metadata(url).await;
    }
    
    // Extract and normalize domain (zero-alloc scan, no Vec<&str>)
    let domain = {
        // URL format: "scheme://host/..." — find the third '/'
        let bytes = url.as_bytes();
        let scheme_end = match bytes.iter().position(|&b| b == b':') {
            Some(i) => i,
            None => 0,
        };
        let host_start = if scheme_end + 2 < bytes.len() && bytes[scheme_end + 1] == b'/' && bytes[scheme_end + 2] == b'/' {
            scheme_end + 3
        } else {
            0
        };
        let host_end = bytes[host_start..].iter().position(|&b| b == b'/').map(|i| host_start + i).unwrap_or(bytes.len());
        if host_start > 0 {
            let mut d = String::with_capacity(host_end + 1);
            d.push_str(&url[..host_end]);
            d.push('/');
            d
        } else {
            let mut d = String::with_capacity(url.len() + 1);
            d.push_str(url);
            if !d.ends_with('/') { d.push('/'); }
            d
        }
    };

    let mut html_chunk = Vec::new();

    // Tor-aware: when Tor is on, this routes through the SOCKS proxy.
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(15))?;
    let mut response = client
        .get(url)
        .header("Range", "bytes=0-32768")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // Hard ceiling: the `Range` header above is only a REQUEST — a hostile server can ignore it and
    // stream forever. OG/meta tags live near the top of <head>, so stop well before any real head ends.
    const MAX_HEAD_SCAN_BYTES: usize = 512 * 1024;
    // Read the response in chunks — SIMD scan for </head> on raw bytes (no clone, no UTF-8 re-check)
    loop {
        let chunk = response.chunk().await.map_err(|e| e.to_string())?;
        match chunk {
            Some(data) => {
                let prev_len = html_chunk.len();
                html_chunk.extend_from_slice(&data);

                // SIMD-scan only the new overlap region for </head>
                let search_start = prev_len.saturating_sub(6);
                if let Some(end) = html_meta::find_closing_head(&html_chunk, search_start) {
                    html_chunk.truncate(end);
                    break;
                }
                if html_chunk.len() >= MAX_HEAD_SCAN_BYTES {
                    html_chunk.truncate(MAX_HEAD_SCAN_BYTES);
                    break;
                }
            }
            None => break,
        }
    }

    // Lossy: the 512 KB cap (or a server cutting off) can truncate mid-UTF-8; a link preview should
    // degrade gracefully on a partial byte sequence, not fail the whole fetch.
    let html_string = String::from_utf8_lossy(&html_chunk).into_owned();
    let parsed = html_meta::extract_html_meta(&html_string);

    let mut metadata = SiteMetadata {
        domain: domain.clone(),
        og_title: parsed.og_title.map(|c| c.into_owned()),
        og_description: parsed.og_description.map(|c| c.into_owned()),
        og_image: parsed.og_image.map(|u| normalize_url(&u, &domain)),
        // Sites can (against the OG spec) serve a RELATIVE og:url — GitHub does —
        // and the preview card opens this value, so it must come out absolute.
        og_url: parsed.og_url.map(|u| normalize_url(&u, &domain)).or(Some(url.to_string())),
        og_type: parsed.og_type.map(|c| c.into_owned()),
        title: parsed.title.map(|c| c.into_owned()),
        description: parsed.description.map(|c| c.into_owned()),
        favicon: None,
    };

    // Favicon selection with priority order
    let base = domain.trim_end_matches('/');
    if parsed.favicons.is_empty() {
        let mut s = String::with_capacity(base.len() + 12);
        s.push_str(base);
        s.push_str("/favicon.ico");
        metadata.favicon = Some(s);
    } else {
        let favicon_candidates: Vec<(String, &str)> = parsed.favicons.iter()
            .map(|f| (normalize_url(&f.href, &domain), f.rel.as_ref()))
            .collect();

        let favicon = favicon_candidates.iter()
            .find(|(_, rel)| rel.eq_ignore_ascii_case("apple-touch-icon"))
            .or_else(|| favicon_candidates.iter().find(|(url, _)| url.ends_with(".png")))
            .or_else(|| favicon_candidates.iter().find(|(_, rel)|
                rel.eq_ignore_ascii_case("icon") || rel.eq_ignore_ascii_case("shortcut icon")))
            .map(|(url, _)| url.clone())
            .unwrap_or_else(|| {
                let mut s = String::with_capacity(base.len() + 12);
                s.push_str(base);
                s.push_str("/favicon.ico");
                s
            });

        metadata.favicon = Some(favicon);
    }

    Ok(metadata)
}

/// Normalize a URL: upgrade http to https, resolve protocol-relative and path-relative URLs.
/// Uses pre-calculated capacity + push_str (single alloc, no format! overhead).
fn normalize_url(url: &str, domain: &str) -> String {
    if url.starts_with("https://") {
        url.to_string()
    } else if url.starts_with("http://") {
        let rest = &url[7..];
        let mut s = String::with_capacity(8 + rest.len());
        s.push_str("https://");
        s.push_str(rest);
        s
    } else if url.starts_with("//") {
        let mut s = String::with_capacity(6 + url.len());
        s.push_str("https:");
        s.push_str(url);
        s
    } else {
        let base = domain.trim_end_matches('/');
        if url.starts_with('/') {
            let mut s = String::with_capacity(base.len() + url.len());
            s.push_str(base);
            s.push_str(url);
            s
        } else {
            let mut s = String::with_capacity(base.len() + 1 + url.len());
            s.push_str(base);
            s.push('/');
            s.push_str(url);
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    // GitHub serves a spec-violating ROOT-RELATIVE og:url; stored raw, the preview
    // opener turns its first path segment into a hostname.
    #[test]
    fn relative_og_url_resolves_against_the_page_domain() {
        assert_eq!(
            normalize_url("/VectorPrivacy/Vector/releases/tag/v0.4.2-4", "https://github.com/"),
            "https://github.com/VectorPrivacy/Vector/releases/tag/v0.4.2-4"
        );
        assert_eq!(normalize_url("//cdn.example.com/x.png", "https://a.com/"), "https://cdn.example.com/x.png");
        assert_eq!(normalize_url("http://a.com/x", "https://a.com/"), "https://a.com/x");
        assert_eq!(normalize_url("https://b.com/x", "https://a.com/"), "https://b.com/x");
    }
}



