//! Resumable attachment downloads. The ciphertext streams into the account's partial file behind
//! a durable checkpoint, so a failure, the next mirror or the next launch carries on from the last
//! byte that landed. The format is the desktop client's own, so either client resumes the other's:
//! `<account>/partial-downloads/<attachment id>.part`, beside `<id>.ckpt` holding the offset and
//! the total as two little-endian u64s (0 = not yet known).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;

use crate::error::DownloadError;
use crate::types::Attachment;
use crate::DownloadStep;

/// The account folder's directory of partial downloads.
pub const PARTIAL_DIR: &str = "partial-downloads";

/// How often a streamed download makes its progress durable: the bytes are synced and the
/// checkpoint moved. A resume starts from the last one, never from the file's length, which a
/// power cut can leave covering bytes that never landed.
const CHECKPOINT_EVERY: u64 = 4 * 1024 * 1024;

/// The cap for a blob whose message declared no size.
const UNDECLARED_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Partials untouched this long are abandoned, not paused.
const PARTIAL_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The caller stopped the transfer.
pub const CANCELLED: &str = "Download cancelled";

/// A partial download's durable progress, kept beside its bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Checkpoint {
    /// Bytes known to be on disk.
    pub offset: u64,
    /// The whole file's size, once a response has said it.
    pub total: Option<u64>,
}

fn checkpoint_path(part: &Path) -> PathBuf {
    part.with_extension("ckpt")
}

/// The checkpoint beside `part`, if both it and enough bytes to back it exist.
pub fn read_checkpoint(part: &Path) -> Option<Checkpoint> {
    let raw = std::fs::read(checkpoint_path(part)).ok()?;
    let raw: [u8; 16] = raw.try_into().ok()?;
    let offset = u64::from_le_bytes(raw[..8].try_into().ok()?);
    let total = u64::from_le_bytes(raw[8..].try_into().ok()?);
    let on_disk = std::fs::metadata(part).ok()?.len();
    (on_disk >= offset).then_some(Checkpoint { offset, total: (total > 0).then_some(total) })
}

fn write_checkpoint(part: &Path, at: Checkpoint) -> std::io::Result<()> {
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
pub fn discard_partial(part: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(checkpoint_path(part));
}

/// Where the open account keeps `attachment_id`'s partial download.
pub fn part_for(attachment_id: &str) -> Result<PathBuf, String> {
    Ok(crate::db::current_account_dir()?.join(PARTIAL_DIR).join(format!("{attachment_id}.part")))
}

/// The open account's paused downloads: attachment id → how far each got.
pub fn paused() -> HashMap<String, Checkpoint> {
    let mut out = HashMap::new();
    let Ok(dir) = crate::db::current_account_dir().map(|d| d.join(PARTIAL_DIR)) else { return out };
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if let Some(at) = read_checkpoint(&path).filter(|c| c.offset > 0) {
            out.insert(id.to_string(), at);
        }
    }
    out
}

/// Remove abandoned partials and their checkpoints.
pub fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > PARTIAL_MAX_AGE)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Stream `content_url` into `part`, resuming from its checkpoint when there is one. Holds a chunk
/// at a time, never the file. On any failure the bytes so far stay behind a fresh checkpoint for
/// the next attempt, from this source or another serving the same blob. `progress` hears the bytes
/// on disk and the total once known, and stops the transfer by returning false. Returns the file's
/// length once every byte is on disk.
pub async fn download_to_file(
    content_url: &str,
    part: &Path,
    declared: Option<u64>,
    progress: &mut (dyn FnMut(u64, Option<u64>) -> bool + Send),
) -> Result<u64, &'static str> {
    crate::net::validate_url_not_private(content_url)?;
    let crate::net::Egress { url, auth } = crate::net::egress(content_url).await;
    let client = crate::net::build_http_client_with_options(None, Some(crate::net::transfer_stall()), true)
        .map_err(|_| "Failed to create HTTP client")?;
    fetch_into(&client, &url, auth.as_ref(), part, declared, progress).await
}

/// [`download_to_file`] past its guard and routing: `url` is already where the bytes come from.
pub(crate) async fn fetch_into(
    client: &crate::net::HttpClient,
    url: &str,
    auth: Option<&reqwest::header::HeaderValue>,
    part: &Path,
    declared: Option<u64>,
    progress: &mut (dyn FnMut(u64, Option<u64>) -> bool + Send),
) -> Result<u64, &'static str> {
    use tokio::io::AsyncWriteExt;
    // The message said how big the blob is, so a body running past that is not it. The slack
    // covers senders that state the plaintext's size, 16 bytes short.
    let limit = declared.filter(|&d| d > 0).map(|d| d + 64 * 1024).unwrap_or(UNDECLARED_MAX_BYTES);
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "Failed to prepare the download")?;
    }
    let mut at = read_checkpoint(part).unwrap_or_default();
    // A checkpoint that already covers the whole file needs no request at all.
    if at.total.is_some_and(|t| t > 0 && at.offset == t) {
        return Ok(at.offset);
    }
    // Two passes at most: a server that answers the range with the wrong bytes is asked once more
    // for the whole file. A network switch resumes from the checkpoint once the new network is
    // ready, spending no pass.
    let mut passes = 0;
    let mut network_waits = 0u8;
    while passes < 2 {
        passes += 1;
        if !progress(at.offset, at.total) {
            return Err(CANCELLED);
        }
        // A signed proxied request identifies the user, so it rides the Account lane.
        let mut request = match auth {
            Some(v) => client.with_lane(crate::net::Lane::Account).get(url).header(reqwest::header::AUTHORIZATION, v.clone()),
            None => client.get(url),
        };
        if at.offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={}-", at.offset));
        }
        let res = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                crate::log_warn!("[AttachmentDownload] request failed for {}: {}", url, e);
                if e.is_transient() && network_waits < 8 && crate::net::wait_after(&e).await.is_ok() {
                    network_waits += 1;
                    passes -= 1;
                    continue;
                }
                return Err("Failed to download");
            }
        };
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
                crate::log_debug!("[AttachmentDownload] HTTP {} for {}", status, url);
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

        // Make everything so far durable; the failure path runs it too.
        async fn settle(file: &mut tokio::io::BufWriter<tokio::fs::File>, part: &Path, at: Checkpoint) -> Result<(), &'static str> {
            file.flush().await.map_err(|_| "Failed to save the download")?;
            file.get_ref().sync_data().await.map_err(|_| "Failed to save the download")?;
            write_checkpoint(part, at).map_err(|_| "Failed to save the download")
        }

        let mut stream = res.bytes_stream();
        let mut resume = false;
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    crate::log_warn!("[AttachmentDownload] stream interrupted for {}: {}", url, e);
                    let _ = settle(&mut file, part, Checkpoint { offset: written, total }).await;
                    if e.is_transient() && network_waits < 8 && crate::net::wait_after(&e).await.is_ok() {
                        network_waits += 1;
                        resume = true;
                        break;
                    }
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
            if !progress(written, total) {
                let _ = settle(&mut file, part, Checkpoint { offset: written, total }).await;
                return Err(CANCELLED);
            }
        }
        if resume {
            at = Checkpoint { offset: written, total };
            passes -= 1;
            continue;
        }
        settle(&mut file, part, Checkpoint { offset: written, total: total.or(Some(written)) }).await?;
        if total.is_some_and(|t| written < t) {
            return Err("Error downloading chunk");
        }
        return Ok(written);
    }
    Err("Server did not honor range request")
}

/// What a whole part file holds for `attachment`: its plaintext, or why this source's bytes are
/// wrong. Plaintext blobs answer to their content address and the sender's claim; encrypted ones
/// to their key.
fn open_part(part: &Path, url: &str, attachment: &Attachment) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(part).map_err(|e| e.to_string())?;
    if attachment.key.is_empty() || attachment.nonce.is_empty() {
        let hash = crate::crypto::sha256_hex(&bytes);
        return if crate::blossom::plaintext_matches(url, attachment.original_hash.as_deref(), &hash) {
            Ok(bytes)
        } else {
            Err("plaintext blob fails its hash claim".into())
        };
    }
    crate::crypto::decrypt_data_owned(bytes, &attachment.key, &attachment.nonce).map_err(|e| format!("decrypt: {e}"))
}

impl crate::VectorCore {
    /// [`download_attachment_reporting`](crate::VectorCore::download_attachment_reporting) through
    /// the account's partial file: a failure, a stop or a restart leaves the bytes so far for the
    /// next try, from any source serving the same blob. Walks the primary URL, its mirrors, then
    /// the author's servers by content address; bytes that don't open condemn only their source.
    pub async fn download_attachment_resumable<F: FnMut(DownloadStep) -> bool + Send>(
        &self,
        attachment: &Attachment,
        author_npub: Option<&str>,
        mut step: F,
    ) -> Result<Vec<u8>, DownloadError> {
        if attachment.url.is_empty() {
            return Err(DownloadError::Unusable("attachment has no URL".into()));
        }
        let part = part_for(&attachment.id).map_err(DownloadError::Unreachable)?;
        if let Some(dir) = part.parent() {
            sweep_stale(dir);
        }
        let declared = (attachment.size > 0).then_some(attachment.size);
        let mut candidates: Vec<String> = vec![attachment.url.clone()];
        candidates.extend(attachment.fallback_urls.iter().cloned());
        let mut last_err = String::from("download failed");
        let mut all_answered = true;
        let mut hash_swap_tried = false;
        // The source already given a fresh start: bytes it didn't serve can't condemn it twice.
        let mut clean_retry: Option<usize> = None;
        let mut i = 0;
        while i < candidates.len() {
            let url = candidates[i].clone();
            // Bytes already on disk came from an earlier source or launch, not this one.
            let inherited = read_checkpoint(&part).is_some_and(|c| c.offset > 0);
            let mut stopped = false;
            let got = {
                let mut progress = |bytes: u64, total: Option<u64>| {
                    let go = step(DownloadStep::Received { bytes, total });
                    stopped |= !go;
                    go
                };
                download_to_file(&url, &part, declared, &mut progress).await
            };
            match got {
                Ok(_) => {}
                Err(_) if stopped => return Err(DownloadError::Cancelled),
                Err(e) => {
                    last_err = e.to_string();
                    if e != "File exceeds the maximum download size" {
                        all_answered &= e == "Media server returned an error status";
                    } else {
                        // Every mirror serves the same blob: none would fit either.
                        discard_partial(&part);
                        return Err(DownloadError::TooLarge(declared.unwrap_or(0) as usize));
                    }
                    i += 1;
                    if i == candidates.len() && !hash_swap_tried {
                        hash_swap_tried = true;
                        let servers = crate::blossom_servers::author_swap_servers(author_npub, false).await;
                        for c in crate::blossom::hash_swap_candidates(&attachment.url, &servers) {
                            if !candidates.contains(&c) {
                                candidates.push(c);
                            }
                        }
                    }
                    continue;
                }
            }
            if !step(DownloadStep::Opening) {
                return Err(DownloadError::Cancelled);
            }
            let (src, check, att) = (part.clone(), url.clone(), attachment.clone());
            let opened = tokio::task::spawn_blocking(move || open_part(&src, &check, &att))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            match opened {
                Ok(plain) => {
                    discard_partial(&part);
                    return Ok(plain);
                }
                Err(reason) => {
                    discard_partial(&part);
                    last_err = reason;
                    // Part of these bytes came from elsewhere: this source gets one clean run
                    // of its own before it is judged.
                    if inherited && clean_retry != Some(i) {
                        clean_retry = Some(i);
                        continue;
                    }
                    i += 1;
                }
            }
        }
        Err(if all_answered { DownloadError::Unusable(last_err) } else { DownloadError::Unreachable(last_err) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn blob(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    /// A one-file server on loopback. The first `cut` responses stop after `cut_at` bytes of the
    /// body; every response honours `Range: bytes=N-`. Returns its URL and the Range starts seen.
    async fn serve(body: Vec<u8>, cut: usize, cut_at: usize) -> (String, Arc<std::sync::Mutex<Vec<u64>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = ranges.clone();
        let served = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (body, seen, served) = (body.clone(), seen.clone(), served.clone());
                tokio::spawn(async move {
                    let mut req = vec![0u8; 4096];
                    let n = sock.read(&mut req).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&req[..n]).to_ascii_lowercase();
                    let start = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').trim().trim_end_matches('-').parse::<u64>().ok());
                    seen.lock().unwrap().push(start.unwrap_or(0));
                    let from = start.unwrap_or(0) as usize;
                    let head = match start {
                        Some(s) => format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                            body.len() - from,
                            s,
                            body.len() - 1,
                            body.len()
                        ),
                        None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    let nth = served.fetch_add(1, Ordering::SeqCst);
                    let end = if nth < cut { (from + cut_at).min(body.len()) } else { body.len() };
                    let _ = sock.write_all(&body[from..end]).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}/blob"), ranges)
    }

    fn client() -> crate::net::HttpClient {
        crate::net::build_http_client_with_options(None, Some(std::time::Duration::from_secs(5)), true).unwrap()
    }

    #[test]
    fn a_checkpoint_round_trips_and_needs_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("abc.part");
        std::fs::write(&part, vec![7u8; 100]).unwrap();
        write_checkpoint(&part, Checkpoint { offset: 100, total: Some(400) }).unwrap();
        assert_eq!(read_checkpoint(&part), Some(Checkpoint { offset: 100, total: Some(400) }));
        // A checkpoint claiming more than the file holds is not trusted.
        write_checkpoint(&part, Checkpoint { offset: 200, total: None }).unwrap();
        assert_eq!(read_checkpoint(&part), None);
        discard_partial(&part);
        assert!(!part.exists() && !part.with_extension("ckpt").exists());
    }

    #[test]
    fn the_checkpoint_file_is_the_desktop_clients_format() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("abc.part");
        std::fs::write(&part, vec![0u8; 5]).unwrap();
        write_checkpoint(&part, Checkpoint { offset: 5, total: Some(9) }).unwrap();
        let raw = std::fs::read(part.with_extension("ckpt")).unwrap();
        assert_eq!(raw, [5u64.to_le_bytes(), 9u64.to_le_bytes()].concat());
    }

    #[tokio::test]
    async fn a_cut_download_resumes_from_its_checkpoint() {
        let body = blob(300_000);
        let (url, ranges) = serve(body.clone(), 1, 120_000).await;
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        let mut progress = |_: u64, _: Option<u64>| true;
        // The first answer stops short: the bytes so far stay behind a checkpoint.
        assert!(fetch_into(&client(), &url, None, &part, Some(body.len() as u64), &mut progress).await.is_err());
        let at = read_checkpoint(&part).expect("a checkpoint after the cut");
        assert_eq!(at.offset, 120_000);
        // The next attempt asks for the rest, and the file comes out whole.
        let n = fetch_into(&client(), &url, None, &part, Some(body.len() as u64), &mut progress).await.unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(std::fs::read(&part).unwrap(), body);
        assert_eq!(*ranges.lock().unwrap(), vec![0, 120_000]);
    }

    #[tokio::test]
    async fn a_stop_keeps_the_bytes_for_later() {
        let body = blob(200_000);
        let (url, _) = serve(body.clone(), 0, 0).await;
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("y.part");
        let mut stop_early = |bytes: u64, _: Option<u64>| bytes < 50_000;
        assert_eq!(fetch_into(&client(), &url, None, &part, None, &mut stop_early).await, Err(CANCELLED));
        assert!(read_checkpoint(&part).is_some_and(|c| c.offset >= 50_000));
        let mut go = |_: u64, _: Option<u64>| true;
        fetch_into(&client(), &url, None, &part, None, &mut go).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), body);
    }

    #[tokio::test]
    async fn a_finished_checkpoint_needs_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("z.part");
        std::fs::write(&part, vec![1u8; 10]).unwrap();
        write_checkpoint(&part, Checkpoint { offset: 10, total: Some(10) }).unwrap();
        let mut go = |_: u64, _: Option<u64>| true;
        // Nothing listens there: any request would fail.
        assert_eq!(fetch_into(&client(), "http://127.0.0.1:9/none", None, &part, None, &mut go).await, Ok(10));
    }
}
