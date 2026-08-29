use crate::signer::VectorSigner;
use nostr_sdk::prelude::{Event, FinalizeEventAsync, Timestamp, Url};
use bitcoin_hashes::sha256::Hash as Sha256Hash;
use nostr_blossom::prelude::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Body, StatusCode};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use futures_util::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Progress callback function type
pub type ProgressCallback = std::sync::Arc<dyn Fn(Option<u8>, Option<u64>) -> Result<(), String> + Send + Sync>;

/// Custom upload stream that tracks progress
struct ProgressTrackingStream {
    bytes_sent: Arc<Mutex<u64>>,
    inner: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
}

impl ProgressTrackingStream {
    fn new(data: Arc<Vec<u8>>, bytes_sent: Arc<Mutex<u64>>) -> Self {
        let (tx, rx) = mpsc::channel(8); // Buffer size of 8 chunks

        // Spawn a background task to feed the stream. NOT bound: this pumps
        // bytes already in hand into a channel and never touches account state.
        // spawn-detached: byte-pump into a channel; the bytes are already in hand.
        tokio::spawn(async move {
            let chunk_size = 64 * 1024; // 64 KB chunks - only unavoidable copy
            let mut position = 0;

            while position < data.len() {
                let end = std::cmp::min(position + chunk_size, data.len());
                let chunk = data[position..end].to_vec();

                // Send chunk through channel
                if tx.send(Ok(chunk)).await.is_err() {
                    break; // Receiver was dropped
                }

                position = end;
            }
        });

        Self {
            bytes_sent,
            inner: rx,
        }
    }
}

impl Stream for ProgressTrackingStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.inner.poll_recv(cx) {
            Poll::Ready(Some(result)) => {
                // Update the bytes sent counter
                if let Ok(chunk) = &result {
                    let mut bytes_sent = self.bytes_sent.lock().unwrap();
                    *bytes_sent += chunk.len() as u64;
                }
                Poll::Ready(Some(result))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Builds the Blossom authorization header
async fn build_auth_header<T>(
    signer: &T,
    hash: Sha256Hash,
) -> Result<HeaderValue, String>
where
    T: VectorSigner,
{
    // Create Blossom authorization
    let expiration = Timestamp::now() + std::time::Duration::from_secs(300);
    let auth = BlossomAuthorization::new(
        "Blossom upload authorization".to_string(),
        expiration,
        BlossomAuthorizationVerb::Upload,
        BlossomAuthorizationScope::BlobSha256Hashes(vec![hash]),
    );

    // Sign the authorization event
    let auth_event: Event = auth
        .finalize_async(signer)
        .await
        .map_err(|e| format!("Failed to sign auth event: {}", e))?;

    // Encode as base64
    let encoded_auth = base64_simd::STANDARD.encode_to_string(auth_event.as_json());
    let value = format!("Nostr {}", encoded_auth);

    HeaderValue::try_from(value)
        .map_err(|e| format!("Failed to create header value: {}", e))
}

/// Upload to a single Blossom server with progress callbacks.
/// `retry_count` defaults to 0; `retry_spacing` defaults to 1s.
pub async fn upload_blob_with_progress<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<String, String>
where
    T: VectorSigner + Clone,
{
    let retry_count = retry_count.unwrap_or(0);
    let retry_spacing = retry_spacing.unwrap_or(std::time::Duration::from_secs(1));

    let mut last_error = None;

    for attempt in 0..=retry_count {
        if attempt > 0 {
            tokio::time::sleep(retry_spacing).await;
        }

        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::Relaxed) {
                return Err("Upload cancelled".to_string());
            }
        }

        match upload_attempt(
            signer.clone(),
            server_url,
            file_data.clone(),
            mime_type,
            &progress_callback,
            cancel_flag.clone(),
        ).await {
            Ok(url) => return Ok(url),
            Err(e) => {
                if e == "Upload cancelled" {
                    return Err(e);
                }
                crate::log_warn!(
                    "[Blossom] Attempt {}/{} to {} failed: {}",
                    attempt + 1, retry_count + 1, server_url, e,
                );
                // Deterministic rejections (413/415 etc.) — outer failover handles them.
                let status = parse_status_from_error(&e);
                let permanent = crate::blossom_capabilities::is_mime_rejection(status, &e)
                    || crate::blossom_capabilities::is_size_rejection(status);
                if permanent {
                    return Err(e);
                }
                // Cloudflare 52x (520 unknown / 521 down / 522 timeout / 523 unreachable /
                // 524 timeout / 525 TLS-handshake-failed / 526 bad-cert) + 504: the origin
                // can't ingest the upload, and retrying the same server just repeats the
                // failure, so route around to the next server immediately.
                if matches!(status, Some(504 | 520 | 521 | 522 | 523 | 524 | 525 | 526)) {
                    crate::log_warn!(
                        "[Blossom] {} origin unreachable (status {}) on {} bytes; routing to the next server",
                        server_url, status.unwrap_or(0), file_data.len(),
                    );
                    return Err(e);
                }
                // On large uploads, mid-stream drops are almost always a
                // size policy; don't burn retries. Below 8MB, treat as a
                // genuine transient blip and retry.
                let looks_like_mid_stream_drop = (
                    e.contains("Upload request failed")
                    || e.contains("error sending request")
                    || e.contains("connection reset")
                    || e.contains("connection closed")
                    || e.contains("connection refused")
                    || e.contains("body write")
                    || e.contains("IncompleteMessage")
                    || e.contains("broken pipe")
                ) && file_data.len() > 8 * 1024 * 1024;
                if looks_like_mid_stream_drop {
                    crate::log_warn!(
                        "[Blossom] {} dropped the connection mid-upload of {} bytes, treating as permanent",
                        server_url, file_data.len(),
                    );
                    return Err(e);
                }
                last_error = Some(e);
            }
        }
    }

    // All attempts failed, return the last error
    Err(last_error.unwrap_or_else(|| "No upload attempts were made".to_string()))
}

/// Internal function that performs a single upload attempt with progress tracking
async fn upload_attempt<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    progress_callback: &ProgressCallback,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<String, String>
where
    T: VectorSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| format!("Invalid server URL: {}", e))?;

    let total_size = file_data.len() as u64;
    let hash = Sha256Hash::hash(&*file_data);

    progress_callback(Some(0), Some(0)).map_err(|e| e)?;

    // One auth event covers both HEAD preflight and PUT.
    let auth_header = build_auth_header(&signer, hash).await?;

    // Redirects disabled: a 3xx mid-PUT would re-issue as GET and drop the body.
    let client = crate::net::build_http_client_with_options(
        std::time::Duration::from_secs(300),
        None,
        false,
    )?;

    // BUD-06 preflight (best-effort; non-supporting servers 404/405).
    {
        let mut head_headers = HeaderMap::new();
        head_headers.insert(AUTHORIZATION, auth_header.clone());
        head_headers.insert(
            "X-Content-Length",
            HeaderValue::from_str(&total_size.to_string())
                .map_err(|e| format!("Invalid X-Content-Length: {}", e))?,
        );
        // BUD-06 requires lowercase hex. SIMD encode of the 32-byte digest (sha256::Hash displays
        // in forward byte order, matching to_byte_array — see the parity test).
        head_headers.insert(
            "X-SHA-256",
            HeaderValue::from_str(&crate::simd::hex::bytes_to_hex_32(&hash.to_byte_array()))
                .map_err(|e| format!("Invalid X-SHA-256: {}", e))?,
        );
        if let Some(ct) = mime_type {
            head_headers.insert(
                "X-Content-Type",
                HeaderValue::from_str(ct).map_err(|e| format!("Invalid X-Content-Type: {}", e))?,
            );
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.head(upload_url.clone()).headers(head_headers).send(),
        ).await {
            Ok(Ok(resp)) => {
                let status = resp.status();
                // BUD-02: X-Reason is display-only. Body IS fed to the classifier
                // to catch non-compliant servers that 400 instead of 415.
                let x_reason = resp.headers().get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let body = resp.text().await.unwrap_or_default();
                let diag = if !body.is_empty() {
                    if let Some(r) = &x_reason {
                        format!("{} (X-Reason: {})", body, r)
                    } else {
                        body
                    }
                } else if let Some(r) = x_reason {
                    r
                } else {
                    format!("rejected at preflight ({})", status)
                };
                let is_413 = status == StatusCode::PAYLOAD_TOO_LARGE;
                let is_415 = status == StatusCode::UNSUPPORTED_MEDIA_TYPE;
                let mime_hinted = status.is_client_error() && !is_413 && {
                    crate::blossom_capabilities::is_mime_rejection(Some(status.as_u16()), &diag)
                };
                if is_413 || is_415 || mime_hinted {
                    crate::log_warn!(
                        "[Blossom Preflight] {} REJECTED {} ({} bytes, {}): {}",
                        server_url, status, total_size,
                        mime_type.unwrap_or("(no mime)"), diag,
                    );
                    return Err(format!(
                        "Upload failed with status {}: {}",
                        status, diag,
                    ));
                }
                crate::log_debug!(
                    "[Blossom Preflight] {} → {} ({} bytes); proceeding to PUT",
                    server_url, status, total_size,
                );
            }
            Ok(Err(e)) => {
                crate::log_debug!("[Blossom Preflight] {} HEAD failed: {}, falling through to PUT", server_url, e);
            }
            Err(_) => {
                crate::log_debug!("[Blossom Preflight] {} HEAD timed out (5s), falling through to PUT", server_url);
            }
        }
    }

    let bytes_sent = Arc::new(Mutex::new(0u64));
    let tracking_stream = ProgressTrackingStream::new(file_data, Arc::clone(&bytes_sent));
    let body = Body::wrap_stream(tracking_stream);

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| format!("Invalid content type: {}", e))?
        );
    }
    // `Body::wrap_stream` is unknown-length so reqwest would default to
    // chunked encoding and omit Content-Length — some servers (e.g.
    // blossom.data.haus) then 411.
    headers.insert(CONTENT_LENGTH, HeaderValue::from(total_size));

    let mut request_future = Box::pin(client
        .put(upload_url.clone())
        .headers(headers)
        .body(body)
        .send());

    let mut last_percentage = 0;
    let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));

    let response = loop {
        tokio::select! {
            response = &mut request_future => {
                break response.map_err(|e| format!("Upload request failed: {}", e))?;
            },
            _ = poll_interval.tick() => {
                if let Some(ref flag) = cancel_flag {
                    if flag.load(Ordering::Relaxed) {
                        return Err("Upload cancelled".to_string());
                    }
                }

                let current_bytes = *bytes_sent.lock().unwrap();
                let percentage = if total_size > 0 {
                    ((current_bytes as f64 / total_size as f64) * 100.0) as u8
                } else {
                    0
                };

                if percentage != last_percentage {
                    if let Err(e) = progress_callback(Some(percentage), Some(current_bytes)) {
                        return Err(e);
                    }
                    last_percentage = percentage;
                }
            }
        }
    };

    let final_bytes = *bytes_sent.lock().unwrap();
    if final_bytes == total_size && last_percentage < 100 {
        progress_callback(Some(100), Some(total_size)).map_err(|e| e)?;
    }

    // BUD-02: accept any 2xx (200 OK or 201 Created).
    let status = response.status();
    if status.is_success() {
        let descriptor: BlobDescriptor = response.json().await
            .map_err(|e| format!("Failed to parse response: {}", e))?;
        // Integrity gate: a compliant server stores our bytes verbatim, so the
        // returned descriptor hash MUST equal what we uploaded. A mismatch means
        // the server transformed/re-encoded the blob, which is fatal for an
        // encrypted upload (corrupts the ciphertext). `[INTEGRITY]` marks it so
        // the failover loop routes around the server like a hard rejection.
        if descriptor.sha256 != hash {
            return Err(format!(
                "[INTEGRITY] {} transformed the upload (returned {}, expected {})",
                server_url, descriptor.sha256, hash,
            ));
        }
        Ok(descriptor.url.to_string())
    } else {
        // BUD-02: X-Reason is display-only; body feeds the classifier.
        let x_reason = response.headers().get("X-Reason")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        let display = match (error_text.is_empty(), x_reason) {
            (false, Some(r)) => format!("{} (X-Reason: {})", error_text, r),
            (false, None)    => error_text,
            (true, Some(r))  => r,
            (true, None)     => "Unknown error".to_string(),
        };
        crate::log_net_fail!("[Blossom] upload rejected: HTTP {} — {}", status, display);
        Err(format!("Upload failed with status {}: {}", status, display))
    }
}

/// Simple upload without progress tracking. `read_timeout` fast-fails a dead server
/// (see `build_http_client_with_options`); pass it for small uploads (emoji, avatars)
/// so failover is quick, `None` for large blobs whose server may go quiet mid-store.
pub async fn upload_blob<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    read_timeout: Option<std::time::Duration>,
) -> Result<String, String>
where
    T: VectorSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| format!("Invalid server URL: {}", e))?;

    let hash = Sha256Hash::hash(&*file_data);
    let total_size = file_data.len() as u64;

    let auth_header = build_auth_header(&signer, hash).await?;

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| format!("Invalid content type: {}", e))?
        );
    }
    headers.insert(CONTENT_LENGTH, HeaderValue::from(total_size));

    // Redirects disabled so a 3xx mid-PUT doesn't re-issue as GET.
    let client = crate::net::build_http_client_with_options(
        std::time::Duration::from_secs(300),
        read_timeout,
        false,
    )?;

    let body_data: Vec<u8> = Arc::try_unwrap(file_data)
        .unwrap_or_else(|arc| (*arc).clone());
    let response = client
        .put(upload_url)
        .headers(headers)
        .body(body_data)
        .send()
        .await
        .map_err(|e| format!("Upload request failed: {}", e))?;

    // BUD-02: accept any 2xx (200 OK or 201 Created).
    let status = response.status();
    if status.is_success() {
        let descriptor: BlobDescriptor = response.json().await
            .map_err(|e| format!("Failed to parse response: {}", e))?;
        // Integrity gate (see upload_attempt): reject a server that returns a
        // different hash than we uploaded — it re-encoded the blob.
        if descriptor.sha256 != hash {
            return Err(format!(
                "[INTEGRITY] {} transformed the upload (returned {}, expected {})",
                server_url, descriptor.sha256, hash,
            ));
        }
        Ok(descriptor.url.to_string())
    } else {
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        Err(format!("Upload failed with status {}: {}", status, error_text))
    }
}

/// Post-upload liveness gate: confirm the server actually SERVES the blob it
/// just ACKed. Some servers 2xx an upload they dedupe against a stale index
/// or quietly drop — the ACK is worthless, only a cache-cold fetch tells the
/// truth. A definitive 404/410 (after a short grace retry) fails the server;
/// STRICT liveness for smart-forward reuse: only a positive 2xx HEAD counts.
/// The post-upload verifier above fails OPEN (an unreachable server shouldn't
/// sink an upload that ACKed); a REUSE decision is the opposite — on anything
/// but proof the blob serves, we fall through to a fresh upload, which always
/// works. Never assume here.
pub async fn blob_is_served(url: &str, timeout: std::time::Duration) -> bool {
    let Ok(client) = crate::net::build_http_client(timeout) else {
        return false;
    };
    match client.head(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// anything else passes, so a flaky HEAD can't sink a good upload.
async fn uploaded_blob_serves(url: &str) -> bool {
    let client = match crate::net::build_http_client(std::time::Duration::from_secs(10)) {
        Ok(c) => c,
        Err(_) => return true,
    };
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        match client.head(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                    continue;
                }
                return true;
            }
            Err(_) => {
                crate::log_net_info!("[Blossom] {} unreachable for post-upload verify — assuming stored", url);
                return true;
            }
        }
    }
    crate::log_net_fail!("[Blossom] {} ACKed the upload but serves 404/410 — treating as dropped", url);
    false
}

/// Upload to multiple Blossom servers with failover, in input order.
///
/// **Does NOT participate in the capability cache.** Used by the
/// marketplace (plaintext mini-app uploads); for high-volume callers
/// prefer `upload_blob_with_progress_and_failover` so they benefit
/// from cache-aware routing.
pub async fn upload_blob_with_failover<T>(
    signer: T,
    server_urls: Vec<String>,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    read_timeout: Option<std::time::Duration>,
) -> Result<String, String>
where
    T: VectorSigner + Clone,
{
    let mut last_error = String::from("No servers available");

    for (index, server_url_str) in server_urls.iter().enumerate() {
        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                crate::log_net_fail!("[Blossom] invalid server URL '{}': {}", server_url_str, e);
                last_error = format!("Invalid server URL: {}", e);
                continue;
            }
        };

        crate::log_info!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, server_urls.len(), server_url_str);

        match upload_blob(signer.clone(), &server_url, file_data.clone(), mime_type, read_timeout).await {
            Ok(url) => {
                if !uploaded_blob_serves(&url).await {
                    crate::log_warn!(
                        "[Blossom Error] {} ACKed the upload but does not serve {} — failing over",
                        server_url_str, url,
                    );
                    last_error = format!("{} accepted the upload but the blob is not retrievable", server_url_str);
                    continue;
                }
                crate::log_net_info!("[Blossom] upload OK via {}", server_url_str);
                return Ok(url);
            }
            Err(e) => {
                crate::log_net_fail!("[Blossom] upload failed to {}: {}", server_url_str, e);
                last_error = e;
            }
        }
    }

    crate::log_net_fail!("[Blossom] ALL servers failed; last error: {}", last_error);
    Err(format!("All Blossom servers failed. Last error: {}", last_error))
}

/// Upload with progress + failover, cache-aware routing, and capability learning.
pub async fn upload_blob_with_progress_and_failover<T>(
    signer: T,
    server_urls: Vec<String>,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    is_encrypted: bool,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<String, String>
where
    T: VectorSigner + Clone,
{
    let mut last_error = String::from("No servers available");

    // Known-good first, unknown second, MIME-rejected last. Stable within
    // tier so the user's BUD-03 trust order wins ties.
    let size_bytes = file_data.len() as u64;
    let mime_for_routing = mime_type.unwrap_or("application/octet-stream");
    let ranked = crate::blossom_capabilities::rank_servers(server_urls, mime_for_routing, is_encrypted, size_bytes);
    // Pin capability writes to the account that started the upload.

    for (index, server_url_str) in ranked.iter().enumerate() {
        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::Relaxed) {
                return Err("Upload cancelled".to_string());
            }
        }

        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                crate::log_net_fail!("[Blossom] invalid server URL '{}': {}", server_url_str, e);
                last_error = format!("Invalid server URL: {}", e);
                continue;
            }
        };

        crate::log_info!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, ranked.len(), server_url_str);

        match upload_blob_with_progress(
            signer.clone(),
            &server_url,
            file_data.clone(),
            mime_type,
            progress_callback.clone(),
            retry_count,
            retry_spacing,
            cancel_flag.clone(),
        ).await {
            Ok(url) => {
                if !uploaded_blob_serves(&url).await {
                    crate::log_warn!(
                        "[Blossom Error] {} ACKed the upload but does not serve {} — failing over",
                        server_url_str, url,
                    );
                    last_error = format!("{} accepted the upload but the blob is not retrievable", server_url_str);
                    let _ = progress_callback(Some(0), Some(0));
                    continue;
                }
                crate::log_net_info!("[Blossom] upload OK via {}", server_url_str);
                if let Err(err) = crate::blossom_capabilities::record_accepted(
                    server_url_str, mime_for_routing, is_encrypted, size_bytes,
                ) {
                    crate::log_warn!("[Blossom Cap] record_accepted failed: {}", err);
                }
                return Ok(url);
            }
            Err(e) => {
                if e == "Upload cancelled" {
                    return Err(e);
                }
                crate::log_net_fail!("[Blossom] upload failed to {}: {}", server_url_str, e);
                let status = parse_status_from_error(&e);
                // `[INTEGRITY]` = server stored a different hash (transformed the
                // blob); route around it exactly like a hard MIME rejection.
                if e.contains("[INTEGRITY]") || crate::blossom_capabilities::is_mime_rejection(status, &e) {
                    if let Err(err) = crate::blossom_capabilities::record_rejected_mime(
                        server_url_str, mime_for_routing, is_encrypted,
                    ) {
                        crate::log_warn!("[Blossom Cap] record_rejected_mime failed: {}", err);
                    }
                } else if crate::blossom_capabilities::is_size_rejection(status) {
                    if let Err(err) = crate::blossom_capabilities::record_rejected_size(
                        server_url_str, mime_for_routing, is_encrypted, size_bytes,
                    ) {
                        crate::log_warn!("[Blossom Cap] record_rejected_size failed: {}", err);
                    }
                }
                // Mid-stream drops aren't cached (too ambiguous); only
                // an explicit 413 sets min_rejected_size.
                last_error = e;
                let _ = progress_callback(Some(0), Some(0));
            }
        }
    }

    crate::log_net_fail!("[Blossom] ALL servers failed; last error: {}", last_error);
    Err(format!("All Blossom servers failed. Last error: {}", last_error))
}

// ============================================================================
// Blossom DELETE — paired with NIP-17 message deletion
// ============================================================================

/// Build a BUD-01 DELETE authorization header (kind-24242, verb=delete).
async fn build_delete_auth_header<T>(
    signer: &T,
    hash: Sha256Hash,
) -> Result<HeaderValue, String>
where
    T: VectorSigner,
{
    let expiration = Timestamp::now() + std::time::Duration::from_secs(300);
    let auth = BlossomAuthorization::new(
        "Blossom delete authorization".to_string(),
        expiration,
        BlossomAuthorizationVerb::Delete,
        BlossomAuthorizationScope::BlobSha256Hashes(vec![hash]),
    );

    let auth_event: Event = auth
        .finalize_async(signer)
        .await
        .map_err(|e| format!("Failed to sign auth event: {}", e))?;

    let encoded_auth = base64_simd::STANDARD.encode_to_string(auth_event.as_json());
    let value = format!("Nostr {}", encoded_auth);

    HeaderValue::try_from(value)
        .map_err(|e| format!("Failed to create header value: {}", e))
}

/// Delete a blob from a Blossom server. 2xx and 404 both count as
/// success (idempotent: "blob is gone" is the goal). 401/403/5xx and
/// network errors propagate.
pub async fn delete_blob<T>(
    signer: T,
    server_url: &Url,
    hash: Sha256Hash,
) -> Result<(), String>
where
    T: VectorSigner + Clone,
{
    let auth_header = build_delete_auth_header(&signer, hash).await?;

    let mut url = server_url.clone();
    // BUD-01 DELETE endpoint: `<origin>/<hash>`.
    url.set_path(&format!("/{}", hash));

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);

    let client = crate::net::build_http_client(std::time::Duration::from_secs(30))?;

    let response = client
        .delete(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("Blossom DELETE request failed: {}", e))?;

    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else {
        let body = response.text().await.unwrap_or_else(|_| "<no body>".into());
        // "with status N" phrasing so `parse_status_from_error` can read the
        // code (the probe uses it to detect deletion-refusal). 404 already
        // counts as success above (blob gone = effectively deleted).
        Err(format!("Blossom DELETE failed with status {}: {}", status, body))
    }
}

/// Parse a Blossom blob URL into its server origin and SHA-256 hash
/// (last non-empty path segment, optional `.ext` stripped).
pub fn parse_blob_url(url_str: &str) -> Result<(Url, Sha256Hash), String> {
    let parsed = Url::parse(url_str)
        .map_err(|e| format!("Invalid Blossom URL: {}", e))?;
    let last_segment = parsed
        .path_segments()
        .and_then(|segs| segs.rev().find(|s| !s.is_empty()))
        .ok_or_else(|| "Blossom URL has no path segment".to_string())?;
    let hash_str = last_segment.split('.').next().unwrap_or("");
    let hash = Sha256Hash::from_str(hash_str)
        .map_err(|e| format!("Path is not a SHA-256 hash: {}", e))?;
    let mut origin = parsed.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok((origin, hash))
}

/// Verify a downloaded body against its URL's content address.
/// `Some(true)` = bytes match the blob hash, `Some(false)` = the source served
/// the wrong bytes, `None` = the URL carries no content address to check.
pub fn verify_blob_content(url_str: &str, bytes: &[u8]) -> Option<bool> {
    let (_, expected) = parse_blob_url(url_str).ok()?;
    Some(Sha256Hash::hash(bytes) == expected)
}

/// Parse a Blossom blob URL into (origin, hash) and DELETE that blob.
/// Awaitable single-URL variant of `delete_blobs_best_effort` — caller
/// drives sequencing + per-URL UI feedback.
pub async fn delete_blob_by_url<T>(signer: T, url_str: &str) -> Result<(), String>
where
    T: VectorSigner + Clone,
{
    let (origin, hash) = parse_blob_url(url_str)?;

    crate::log_info!("[Blossom] DELETE {} from {}", hash, origin);
    // Hard ceiling — a black-holed server must not hang the caller's
    // UI (e.g. the pack creator's "Deleting…" overlay) indefinitely.
    // 15s is generous for a healthy server and short enough that a
    // misbehaving one fails over to the next blob in a batch quickly.
    let timeout = std::time::Duration::from_secs(15);
    match tokio::time::timeout(timeout, delete_blob(signer, &origin, hash)).await {
        Ok(Ok(())) => {
            crate::log_info!("[Blossom] DELETE successful: {} from {}", hash, origin);
            Ok(())
        }
        Ok(Err(e)) => {
            crate::log_warn!("[Blossom] DELETE failed: {} from {}: {}", hash, origin, e);
            Err(e)
        }
        Err(_) => {
            let msg = format!("DELETE timed out after {}s", timeout.as_secs());
            crate::log_warn!("[Blossom] {} ({} from {})", msg, hash, origin);
            Err(msg)
        }
    }
}

/// Derive BUD-03 hash-swap candidates: the same content-address on each of
/// `servers`. Blossom URLs are `<origin>/<sha256>[.ext]`, so any server in
/// the author's list may serve a blob whose embedded URLs have all died.
/// Servers matching `primary_url`'s origin (already tried) are skipped.
pub fn hash_swap_candidates(primary_url: &str, servers: &[String]) -> Vec<String> {
    let Ok((primary_origin, hash)) = parse_blob_url(primary_url) else {
        return Vec::new();
    };
    // Extension from the parsed path segment, never the raw string — a raw
    // rsplit would drag the query/fragment along into every derived URL.
    let ext = Url::parse(primary_url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|segs| segs.rev().find(|s| !s.is_empty()).map(|s| s.to_string()))
        })
        .and_then(|leaf| leaf.split_once('.').map(|(_, e)| e.to_string()))
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.bytes().all(|b| b.is_ascii_alphanumeric()));
    let leaf = match ext {
        Some(e) => format!("{}.{}", hash, e),
        None => hash.to_string(),
    };
    servers
        .iter()
        .filter_map(|s| {
            let base = Url::parse(&format!("{}/", s.trim_end_matches('/'))).ok()?;
            if base.origin() == primary_origin.origin() {
                return None;
            }
            base.join(&leaf).ok().map(|u| u.to_string())
        })
        .collect()
}

/// BUD-04: ask `target_server` to mirror the blob at `source_url` by pulling
/// it server-to-server (the client sends one small JSON request, never the
/// bytes). Authorized by the same hash-scoped upload event as a direct
/// upload. The mirror's ACK gets the same serve-check as an upload ACK.
/// Returns the mirror's URL for the blob.
pub async fn mirror_blob<T>(signer: T, target_server: &Url, source_url: &str) -> Result<String, String>
where
    T: VectorSigner + Clone,
{
    let (source_origin, hash) = parse_blob_url(source_url)?;
    if target_server.origin() == source_origin.origin() {
        return Err("Mirror target is the source server".to_string());
    }
    let auth_header = build_auth_header(&signer, hash).await?;
    let mirror_url = target_server
        .join("mirror")
        .map_err(|e| format!("Invalid mirror URL: {}", e))?;

    let client = crate::net::build_http_client(std::time::Duration::from_secs(30))?;
    let response = client
        .put(mirror_url)
        .header(AUTHORIZATION, auth_header)
        .header(CONTENT_TYPE, "application/json")
        .body(format!("{{\"url\":{}}}", serde_json::to_string(source_url).map_err(|e| e.to_string())?))
        .send()
        .await
        .map_err(|e| format!("Mirror request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_else(|_| "<no body>".into());
        return Err(format!("Mirror failed with status {}: {}", status, body));
    }
    let descriptor: BlobDescriptor = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse mirror response: {}", e))?;
    if descriptor.sha256 != hash {
        return Err(format!(
            "[INTEGRITY] {} mirrored a different hash (returned {}, expected {})",
            target_server, descriptor.sha256, hash,
        ));
    }
    let url = descriptor.url.to_string();
    // BUD-04: the mirror serves the blob ITSELF. A descriptor pointing at a
    // foreign origin, a downgraded scheme, or carrying a query string is a
    // protocol violation (or a tracking beacon) — never embed it in messages.
    let same_origin = Url::parse(&url)
        .map(|u| u.origin() == target_server.origin() && u.query().is_none())
        .unwrap_or(false);
    if !same_origin {
        return Err(format!("{} returned a foreign descriptor URL: {}", target_server, url));
    }
    if !uploaded_blob_serves(&url).await {
        return Err(format!("{} ACKed the mirror but does not serve it", target_server));
    }
    Ok(url)
}

/// Best-effort BUD-04 fan-out: mirror `source_url` onto up to `max_mirrors`
/// of the user's other servers, concurrently, under one wall-clock `budget`.
/// Returns only the mirror URLs that verifiably serve — the caller embeds
/// these as NIP-17 / imeta `fallback` sources. Never fails the send: an
/// empty Vec just means the message ships with no fallbacks.
pub async fn mirror_blob_to_servers<T>(
    signer: T,
    source_url: &str,
    server_urls: Vec<String>,
    max_mirrors: usize,
    budget: std::time::Duration,
) -> Vec<String>
where
    T: VectorSigner + Clone,
{
    let source_origin = match parse_blob_url(source_url) {
        Ok((origin, _)) => origin,
        Err(e) => {
            crate::log_debug!("[Blossom Mirror] unparseable source {}: {}", source_url, e);
            return Vec::new();
        }
    };
    let targets: Vec<Url> = server_urls
        .iter()
        .filter_map(|s| Url::parse(s).ok())
        .filter(|u| u.origin() != source_origin.origin())
        .take(max_mirrors)
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }

    let futures = targets.into_iter().map(|target| {
        let signer = signer.clone();
        let source = source_url.to_string();
        async move {
            // Per-target budget: a hung server cancels only itself. A mirror
            // that completed must keep its result — an unrecorded live copy
            // is invisible to every future deletion sweep.
            match tokio::time::timeout(budget, mirror_blob(signer, &target, &source)).await {
                Ok(Ok(url)) => {
                    crate::log_net_info!("[Blossom Mirror] {} now serves {}", target, url);
                    Some(url)
                }
                Ok(Err(e)) => {
                    crate::log_net_fail!("[Blossom Mirror] {} refused: {}", target, e);
                    None
                }
                Err(_) => {
                    crate::log_net_fail!("[Blossom Mirror] {} exceeded {:?} budget", target, budget);
                    None
                }
            }
        }
    });
    futures_util::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Fire-and-forget DELETE for each parseable blob URL. Pairs with
/// `delete_own_dm` so removing a NIP-17 file message also removes
/// the ciphertext from the server it was uploaded to.
pub fn delete_blobs_best_effort<T>(signer: T, urls: Vec<String>)
where
    T: VectorSigner + Clone + Send + Sync + 'static,
{
    for url_str in urls {
        let url = match Url::parse(&url_str) {
            Ok(u) => u,
            Err(_) => continue,
        };

        // Last non-empty path segment (trailing-slash URLs leave an empty tail).
        let last_segment = match url.path_segments()
            .and_then(|segs| segs.rev().find(|s| !s.is_empty()))
        {
            Some(s) => s,
            None => continue,
        };
        // Strip an optional `.ext` suffix some servers append.
        let hash_str = last_segment.split('.').next().unwrap_or("");
        let hash = match Sha256Hash::from_str(hash_str) {
            Ok(h) => h,
            Err(_) => continue,
        };

        let mut origin = url.clone();
        origin.set_path("/");
        origin.set_query(None);
        origin.set_fragment(None);

        let signer = signer.clone();
        // spawn-detached: deleting one probe blob from a server — signer in hand, no account storage.
        tokio::spawn(async move {
            if let Err(e) = delete_blob(signer, &origin, hash).await {
                crate::log_warn!("[Blossom delete] {} from {}: {}", hash, origin, e);
            }
        });
    }
}

/// Probe `(server, application/octet-stream, encrypted=true)` with a
/// 32-byte random blob to learn whether the server accepts the binary
/// uploads Vector produces for chat attachments. Single-shot per
/// (server,mime,encrypted). Successful probes are cleaned up via DELETE.
pub async fn probe_servers_for_octet_stream<T>(
    signer: T,
    server_urls: Vec<String>,
) -> Result<usize, String>
where
    T: VectorSigner + Clone,
{
    use rand::RngCore;
    if server_urls.is_empty() { return Ok(0); }

    const PROBE_MIME: &str = "application/octet-stream";
    let mut payload = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut payload[..]);
    let payload = Arc::new(payload);
    let payload_size = payload.len() as u64;

    let mut probed = 0usize;
    for server_url_str in &server_urls {
        if crate::blossom_capabilities::has_fresh_capability_for(server_url_str, PROBE_MIME, true) {
            continue;
        }
        let parsed = match Url::parse(server_url_str) {
            Ok(u) => u,
            Err(_) => continue,
        };
        // 4s per-server budget bounds worst-case probe pass.
        let no_op_progress: ProgressCallback = Arc::new(|_, _| Ok(()));
        match tokio::time::timeout(
            std::time::Duration::from_secs(4),
            upload_blob_with_progress(
                signer.clone(),
                &parsed,
                payload.clone(),
                Some(PROBE_MIME),
                no_op_progress,
                Some(0),
                None,
                None,
            ),
        ).await {
            Ok(Ok(url)) => {
                // Race-guard: server may have been disabled/removed
                // between spawn and now (purge_server already cleared).
                if !crate::blossom_servers::is_enabled_server(server_url_str) {
                    if let Some(hash) = extract_hash_from_blossom_url(&url) {
                        let _ = delete_blob(signer.clone(), &parsed, hash).await;
                    }
                    continue;
                }
                // Reaching here means the upload succeeded AND the returned hash
                // matched (upload_attempt's integrity gate) — so the server
                // accepts our encrypted type and stores it verbatim. Final gate:
                // it must honor BUD-01 deletion, else removing a message can't
                // remove its blob. The probe blob is deleted either way (cleanup);
                // a 4s budget bounds the wait, and only a definitive refusal
                // (403/405/501) sinks the server — transient failures stay optimistic.
                let delete_result = match extract_hash_from_blossom_url(&url) {
                    Some(hash) => tokio::time::timeout(
                        std::time::Duration::from_secs(4),
                        delete_blob(signer.clone(), &parsed, hash),
                    ).await.ok(),
                    None => None,
                };
                let refuses_deletion = matches!(
                    &delete_result,
                    Some(Err(e)) if matches!(parse_status_from_error(e), Some(403) | Some(405) | Some(501)),
                );
                if refuses_deletion {
                    if let Err(err) = crate::blossom_capabilities::record_rejected_mime(
                        server_url_str, PROBE_MIME, true,
                    ) {
                        crate::log_warn!("[Blossom Probe] record_rejected_mime failed: {}", err);
                    }
                    probed += 1;
                    crate::log_info!("[Blossom Probe] {} refuses deletion; routing around", server_url_str);
                } else {
                    if let Err(e) = crate::blossom_capabilities::record_accepted(
                        server_url_str, PROBE_MIME, true, payload_size,
                    ) {
                        crate::log_warn!("[Blossom Probe] record_accepted failed: {}", e);
                    }
                    probed += 1;
                    crate::log_info!("[Blossom Probe] {} validated (accepts + verbatim + deletes)", server_url_str);
                }
            }
            Ok(Err(e)) => {
                let status = parse_status_from_error(&e);
                // `[INTEGRITY]` = the server accepted but transformed our probe
                // blob; treat it as unsuitable, same as a hard MIME rejection.
                if e.contains("[INTEGRITY]") || crate::blossom_capabilities::is_mime_rejection(status, &e) {
                    if !crate::blossom_servers::is_enabled_server(server_url_str) {
                        continue;
                    }
                    if let Err(err) = crate::blossom_capabilities::record_rejected_mime(
                        server_url_str, PROBE_MIME, true,
                    ) {
                        crate::log_warn!("[Blossom Probe] record_rejected_mime failed: {}", err);
                    }
                    probed += 1;
                    crate::log_info!("[Blossom Probe] {} unsuitable; routing around: {}", server_url_str, e);
                } else {
                    // Transient — leave reputation unchanged so we re-probe later.
                    crate::log_debug!("[Blossom Probe] {} transient error (not cached): {}", server_url_str, e);
                }
            }
            Err(_) => {
                crate::log_debug!("[Blossom Probe] {} timed out, not cached", server_url_str);
            }
        }
    }
    Ok(probed)
}

/// Parse the sha256 out of `<origin>/<sha256>[.<ext>][/]`. Skips an
/// empty trailing segment when the URL came back with a trailing slash.
fn extract_hash_from_blossom_url(url: &str) -> Option<Sha256Hash> {
    let parsed = Url::parse(url).ok()?;
    let last = parsed.path_segments()?.rev().find(|s| !s.is_empty())?;
    let stem = last.split('.').next()?;
    Sha256Hash::from_str(stem).ok()
}

/// Extract the HTTP status from an error string. Anchored to the
/// `"with status NNN"` shape produced by `upload_blob_with_progress`
/// so unrelated `status` substrings don't false-match.
fn parse_status_from_error(msg: &str) -> Option<u16> {
    let key = "with status ";
    let i = msg.find(key)?;
    let tail = &msg[i + key.len()..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u16>().ok()
}

#[cfg(test)]
mod parse_status_tests {
    use super::parse_status_from_error;

    #[test]
    fn extracts_status_code() {
        assert_eq!(parse_status_from_error("Upload failed with status 500 Internal Server Error: x"), Some(500));
        assert_eq!(parse_status_from_error("Upload failed with status 413 Payload Too Large"), Some(413));
        assert_eq!(parse_status_from_error("Upload failed with status 415"), Some(415));
        // Cloudflare gateway timeouts render as "<unknown status code>" — the failover branch relies
        // on this still parsing to the numeric code.
        assert_eq!(parse_status_from_error("Upload failed with status 524 <unknown status code>: gateway"), Some(524));
    }

    #[test]
    fn returns_none_when_absent() {
        assert_eq!(parse_status_from_error("network error: timeout"), None);
    }
}

#[cfg(test)]
mod hash_swap_tests {
    use super::hash_swap_candidates;

    const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn derives_clean_leaves_and_skips_the_source_origin() {
        // Query + fragment on the primary must NOT leak into derived URLs,
        // the source origin is skipped, junk servers are dropped.
        let primary = format!("https://a.example/{}.png?utm=track#frag", HASH);
        let servers = vec![
            "https://a.example".to_string(),
            "https://b.example".to_string(),
            "not a url".to_string(),
        ];
        assert_eq!(
            hash_swap_candidates(&primary, &servers),
            vec![format!("https://b.example/{}.png", HASH)],
        );
    }

    #[test]
    fn extensionless_primary_yields_the_bare_hash() {
        let primary = format!("https://a.example/{}", HASH);
        assert_eq!(
            hash_swap_candidates(&primary, &["https://b.example/".to_string()]),
            vec![format!("https://b.example/{}", HASH)],
        );
    }

    #[test]
    fn unparseable_primary_yields_nothing() {
        assert!(hash_swap_candidates("https://a.example/not-a-hash.png", &["https://b.example".to_string()]).is_empty());
    }
}

#[cfg(test)]
mod hash_extract_tests {
    use super::extract_hash_from_blossom_url;

    const HASH_HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn plain_url() {
        let url = format!("https://srv.example/{}", HASH_HEX);
        assert!(extract_hash_from_blossom_url(&url).is_some());
    }

    #[test]
    fn with_extension() {
        let url = format!("https://srv.example/{}.jpg", HASH_HEX);
        assert!(extract_hash_from_blossom_url(&url).is_some());
    }

    #[test]
    fn trailing_slash_still_resolves() {
        // Some servers append a trailing slash to the descriptor URL.
        let url = format!("https://srv.example/{}/", HASH_HEX);
        assert!(extract_hash_from_blossom_url(&url).is_some());
    }

    #[test]
    fn malformed_returns_none() {
        assert!(extract_hash_from_blossom_url("https://srv.example/").is_none());
        assert!(extract_hash_from_blossom_url("not a url").is_none());
        assert!(extract_hash_from_blossom_url("https://srv.example/notahash").is_none());
    }

    #[test]
    fn x_sha256_simd_hex_matches_lowerhex() {
        use bitcoin_hashes::sha256::Hash as Sha256Hash;
        // The X-SHA-256 header swapped format!("{:x}") for the SIMD encoder; they MUST agree
        // byte-for-byte (sha256::Hash displays in forward order — a reversed-display hash type would
        // silently corrupt the upload header).
        let hash = Sha256Hash::hash(b"vector blossom x-sha-256 parity check");
        assert_eq!(
            crate::simd::hex::bytes_to_hex_32(&hash.to_byte_array()),
            format!("{:x}", hash),
        );
    }
}
