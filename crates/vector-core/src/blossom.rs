use crate::signer::VectorSigner;
use crate::blossom_error::{host_of, summarise_failures, Refusal, UploadFailure};
use nostr_sdk::prelude::{Event, FinalizeEventAsync, Timestamp, Url};
use bitcoin_hashes::sha256::Hash as Sha256Hash;
use nostr_blossom::prelude::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Body, StatusCode};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use futures_util::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Progress callback function type
pub type ProgressCallback = std::sync::Arc<dyn Fn(Option<u8>, Option<u64>) -> Result<(), String> + Send + Sync>;

/// The upload body: slices of the one ciphertext buffer, counted as the socket pulls
/// them. A slice shares the buffer, so nothing is copied on the way out.
struct ProgressTrackingStream {
    data: bytes::Bytes,
    position: usize,
    bytes_sent: Arc<Mutex<u64>>,
}

/// Lends an `Arc`'d buffer to `Bytes` without copying it.
struct SharedBuffer(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl ProgressTrackingStream {
    /// Progress is observed per slice pulled, so the slice is the slowest rate that
    /// still reads as progress: 16 KB in a 60 s stall window is about 2 kbit/s.
    const SLICE: usize = 16 * 1024;

    fn new(data: Arc<Vec<u8>>, bytes_sent: Arc<Mutex<u64>>) -> Self {
        Self { data: bytes::Bytes::from_owner(SharedBuffer(data)), position: 0, bytes_sent }
    }
}

impl Stream for ProgressTrackingStream {
    type Item = Result<bytes::Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.position >= self.data.len() {
            return Poll::Ready(None);
        }
        let end = (self.position + Self::SLICE).min(self.data.len());
        let slice = self.data.slice(self.position..end);
        self.position = end;
        *self.bytes_sent.lock().unwrap() += slice.len() as u64;
        Poll::Ready(Some(Ok(slice)))
    }
}

/// After the last byte is handed to the socket there is nothing left to watch:
/// what remains is buffered bytes draining, an ingress relay forwarding the
/// blob to its origin, and the origin storing it — none of which is visible
/// from here. This bounds that wait.
///
/// Five minutes because the relay hop is real work: a gigabyte across a
/// datacenter backhaul at the ~6 MB/s a single stream manages is nearly three
/// of them, and giving up then would discard an upload that had in fact
/// arrived.
const RESPONSE_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Why an upload was abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stall {
    /// The socket accepted nothing for the whole stall window.
    NoProgress { idle: std::time::Duration },
    /// Every byte was handed over and the server never answered.
    NoResponse { waited: std::time::Duration },
}

/// Progress-based watchdog for an upload: gives up on a transfer that stops
/// moving, never on one that is merely slow.
struct StallWatch {
    total: u64,
    last_bytes: u64,
    last_progress: tokio::time::Instant,
    stall_limit: std::time::Duration,
    response_limit: std::time::Duration,
}

impl StallWatch {
    fn new(total: u64, stall_limit: std::time::Duration, response_limit: std::time::Duration) -> Self {
        Self {
            total,
            last_bytes: 0,
            last_progress: tokio::time::Instant::now(),
            stall_limit,
            response_limit,
        }
    }

    /// Feed the latest byte count; `Some` means give up.
    fn observe(&mut self, bytes_sent: u64) -> Option<Stall> {
        let now = tokio::time::Instant::now();
        if bytes_sent > self.last_bytes {
            self.last_bytes = bytes_sent;
            self.last_progress = now;
            return None;
        }
        let idle = now.duration_since(self.last_progress);
        if bytes_sent >= self.total {
            (idle > self.response_limit).then_some(Stall::NoResponse { waited: idle })
        } else {
            (idle > self.stall_limit).then_some(Stall::NoProgress { idle })
        }
    }
}

/// A progress callback's refusal, as an upload failure: it says "Upload
/// cancelled" when the user pressed stop, and that must stay a cancellation
/// rather than an error.
fn callback_failure(e: String) -> UploadFailure {
    if e == "Upload cancelled" {
        UploadFailure::Cancelled
    } else {
        UploadFailure::Other(e)
    }
}

/// Send `file_data` as the body of `request`, watching progress rather than
/// the clock.
///
/// There is no total time limit. A transfer moving at any rate runs to
/// completion; one that stops for `stall_limit` is abandoned, as is one whose
/// server goes silent after receiving everything.
async fn send_upload(
    request: reqwest::RequestBuilder,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    stall_limit: std::time::Duration,
    cancel_flag: Option<&Arc<AtomicBool>>,
    progress_callback: Option<&ProgressCallback>,
) -> Result<reqwest::Response, UploadFailure> {
    let total_size = file_data.len() as u64;
    let bytes_sent = Arc::new(Mutex::new(0u64));
    let tracking_stream = ProgressTrackingStream::new(file_data, Arc::clone(&bytes_sent));
    let mut request_future = Box::pin(request.body(Body::wrap_stream(tracking_stream)).send());

    let mut watch = StallWatch::new(total_size, stall_limit, RESPONSE_WAIT);
    let mut last_percentage = 0;
    let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));

    let response = loop {
        tokio::select! {
            response = &mut request_future => {
                break response.map_err(|e| UploadFailure::Transport(format!("Upload request failed: {}", e)))?;
            },
            _ = poll_interval.tick() => {
                if let Some(flag) = cancel_flag {
                    if flag.load(Ordering::Relaxed) {
                        return Err(UploadFailure::Cancelled);
                    }
                }

                let current_bytes = *bytes_sent.lock().unwrap();
                match watch.observe(current_bytes) {
                    Some(Stall::NoProgress { idle }) => {
                        return Err(UploadFailure::Transport(format!(
                            "Upload stalled: {} accepted nothing for {}s ({} of {} bytes sent)",
                            server_url, idle.as_secs(), current_bytes, total_size,
                        )));
                    }
                    Some(Stall::NoResponse { waited }) => {
                        return Err(UploadFailure::Transport(format!(
                            "Upload stalled: {} received all {} bytes but gave no answer in {}s",
                            server_url, total_size, waited.as_secs(),
                        )));
                    }
                    None => {}
                }

                if let Some(cb) = progress_callback {
                    let percentage = if total_size > 0 {
                        ((current_bytes as f64 / total_size as f64) * 100.0) as u8
                    } else {
                        0
                    };
                    if percentage != last_percentage {
                        cb(Some(percentage), Some(current_bytes)).map_err(callback_failure)?;
                        last_percentage = percentage;
                    }
                }
            }
        }
    };

    if let Some(cb) = progress_callback {
        let final_bytes = *bytes_sent.lock().unwrap();
        if final_bytes == total_size && last_percentage < 100 {
            cb(Some(100), Some(total_size)).map_err(callback_failure)?;
        }
    }
    Ok(response)
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
) -> Result<String, UploadFailure>
where
    T: VectorSigner + Clone,
{
    upload_with_retries(signer, server_url, file_data, mime_type, progress_callback, retry_count, retry_spacing, cancel_flag, true).await
}

/// [`upload_blob_with_progress`], with the BUD-06 preflight optional: a caller that
/// already knows the server takes this blob spares the round trip.
#[allow(clippy::too_many_arguments)]
async fn upload_with_retries<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
    cancel_flag: Option<Arc<AtomicBool>>,
    preflight_first: bool,
) -> Result<String, UploadFailure>
where
    T: VectorSigner + Clone,
{
    let retry_count = retry_count.unwrap_or(0);
    let retry_spacing = retry_spacing.unwrap_or(std::time::Duration::from_secs(1));

    let mut last_error = None;
    let mut next_delay = retry_spacing;

    for attempt in 0..=retry_count {
        if attempt > 0 {
            tokio::time::sleep(next_delay).await;
            next_delay = retry_spacing;
        }

        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::Relaxed) {
                return Err(UploadFailure::Cancelled);
            }
        }

        match upload_attempt(
            signer.clone(),
            server_url,
            file_data.clone(),
            mime_type,
            &progress_callback,
            cancel_flag.clone(),
            preflight_first,
        ).await {
            Ok(url) => return Ok(url),
            Err(UploadFailure::Cancelled) => return Err(UploadFailure::Cancelled),
            Err(e) => {
                crate::log_warn!(
                    "[Blossom] Attempt {}/{} to {} failed: {}",
                    attempt + 1, retry_count + 1, server_url, e,
                );
                let again = match &e {
                    // The server said what it meant; believe it. A coded
                    // `later` names how long, and a plain server is retried
                    // unless its status is one that never changes.
                    UploadFailure::Refused(r) => r.worth_retrying_here(),
                    // On large uploads, mid-stream drops are almost always a
                    // size policy; don't burn retries. Below 8MB, treat as a
                    // genuine transient blip and retry.
                    UploadFailure::Transport(_) => {
                        !(e.is_mid_stream_drop() && file_data.len() > 8 * 1024 * 1024)
                    }
                    UploadFailure::Integrity(_) | UploadFailure::Other(_) | UploadFailure::Cancelled => false,
                };
                if !again {
                    if let Some(r) = e.refusal() {
                        if crate::blossom_error::is_gateway_status(r.status) && r.code().is_none() {
                            crate::log_warn!(
                                "[Blossom] {} origin unreachable (status {}) on {} bytes; routing to the next server",
                                server_url, r.status, file_data.len(),
                            );
                        }
                    } else if e.is_mid_stream_drop() {
                        crate::log_warn!(
                            "[Blossom] {} dropped the connection mid-upload of {} bytes, treating as permanent",
                            server_url, file_data.len(),
                        );
                    }
                    return Err(e);
                }
                if let Some(wait) = e.refusal().and_then(|r| r.retry_after()) {
                    next_delay = next_delay.max(wait);
                }
                last_error = Some(e);
            }
        }
    }

    // All attempts failed, return the last error
    Err(last_error.unwrap_or_else(|| UploadFailure::Other("No upload attempts were made".to_string())))
}

/// What the server said before the body was sent.
enum Preflight {
    /// Send it.
    Proceed,
    /// The server already holds this exact blob and named its URL; there is
    /// nothing to send.
    AlreadyStored(String),
}

/// BUD-06 preflight: `HEAD /upload` with the blob's hash, size and type, so a
/// server that will refuse can say so before the bytes go out.
///
/// Best-effort: a server without BUD-06 answers 404/405 and is sent the body
/// regardless. A server with Magnitude's vocabulary puts `X-Error-Code` on the
/// refusal, and every such code is final for this server — a quota, a closed
/// client gate, a blocked hash are as final as a size limit, and sending the
/// body would only earn the same answer after the transfer. A plain server is
/// believed only on the two statuses BUD-06 defines (413, 415) and on a body
/// that reads as a type rejection.
///
/// A 2xx with `X-Already-Stored: true` and `X-Blob-URL` means the server has
/// these bytes already and the caller can stop here with the link.
async fn preflight(
    client: &reqwest::Client,
    upload_url: &Url,
    server_url: &Url,
    auth_header: &HeaderValue,
    hash: Sha256Hash,
    total_size: u64,
    mime_type: Option<&str>,
) -> Result<Preflight, UploadFailure> {
    let mut head_headers = HeaderMap::new();
    head_headers.insert(AUTHORIZATION, auth_header.clone());
    head_headers.insert(
        "X-Content-Length",
        HeaderValue::from_str(&total_size.to_string())
            .map_err(|e| UploadFailure::Other(format!("Invalid X-Content-Length: {}", e)))?,
    );
    // BUD-06 requires lowercase hex. SIMD encode of the 32-byte digest (sha256::Hash displays
    // in forward byte order, matching to_byte_array — see the parity test).
    head_headers.insert(
        "X-SHA-256",
        HeaderValue::from_str(&crate::simd::hex::bytes_to_hex_32(&hash.to_byte_array()))
            .map_err(|e| UploadFailure::Other(format!("Invalid X-SHA-256: {}", e)))?,
    );
    if let Some(ct) = mime_type {
        head_headers.insert(
            "X-Content-Type",
            HeaderValue::from_str(ct).map_err(|e| UploadFailure::Other(format!("Invalid X-Content-Type: {}", e)))?,
        );
    }
    let asked_at = std::time::Instant::now();
    let resp = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.head(upload_url.clone()).headers(head_headers).send(),
    ).await {
        Ok(Ok(resp)) => {
            crate::blossom_stats::record_latency(server_url.as_str(), asked_at.elapsed().as_secs_f64() * 1000.0);
            resp
        }
        Ok(Err(e)) => {
            crate::log_debug!("[Blossom Preflight] {} HEAD failed: {}, falling through to PUT", server_url, e);
            return Ok(Preflight::Proceed);
        }
        Err(_) => {
            crate::log_debug!("[Blossom Preflight] {} HEAD timed out (5s), falling through to PUT", server_url);
            return Ok(Preflight::Proceed);
        }
    };

    let status = resp.status();
    if status.is_success() {
        let already = resp.headers().get("x-already-stored")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let url = resp.headers().get("x-blob-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        if let (true, Some(url)) = (already, url) {
            // The link must name our hash: a server pointing at some other blob
            // would have us embed a stranger's bytes in a message.
            match parse_blob_url(&url) {
                Ok((_, stored)) if stored == hash => {
                    crate::log_info!(
                        "[Blossom Preflight] {} already holds {} ({} bytes); nothing to send",
                        server_url, hash, total_size,
                    );
                    return Ok(Preflight::AlreadyStored(url));
                }
                _ => crate::log_warn!(
                    "[Blossom Preflight] {} claims to hold the blob at {} which is not {}; uploading anyway",
                    server_url, url, hash,
                ),
            }
        }
        crate::log_debug!(
            "[Blossom Preflight] {} → {} ({} bytes); proceeding to PUT",
            server_url, status, total_size,
        );
        return Ok(Preflight::Proceed);
    }

    // A HEAD has no body: the headers are the whole answer. BUD-02 says the
    // prose in X-Reason is display-only; a body, if any, feeds the keyword
    // classifier for servers that 400 instead of 415.
    let headers = resp.headers().clone();
    let body = resp.text().await.unwrap_or_default();
    let refusal = Refusal::from_response(status.as_u16(), &headers, &body);
    let is_413 = status == StatusCode::PAYLOAD_TOO_LARGE;
    let is_415 = status == StatusCode::UNSUPPORTED_MEDIA_TYPE;
    let final_here = refusal.code().is_some()
        || is_413
        || is_415
        || (status.is_client_error() && refusal.is_mime());
    if final_here {
        crate::log_warn!(
            "[Blossom Preflight] {} REJECTED {} ({} bytes, {}): {}",
            server_url, status, total_size,
            mime_type.unwrap_or("(no mime)"), refusal.message,
        );
        return Err(UploadFailure::Refused(refusal));
    }
    crate::log_debug!(
        "[Blossom Preflight] {} → {} ({} bytes); proceeding to PUT",
        server_url, status, total_size,
    );
    Ok(Preflight::Proceed)
}

/// Read a rejected PUT into a refusal. The body feeds the classifier; the
/// headers carry the code when the server has one.
async fn refusal_from(response: reqwest::Response) -> Refusal {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    Refusal::from_response(status, &headers, &body)
}

/// Internal function that performs a single upload attempt with progress tracking
async fn upload_attempt<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    progress_callback: &ProgressCallback,
    cancel_flag: Option<Arc<AtomicBool>>,
    preflight_first: bool,
) -> Result<String, UploadFailure>
where
    T: VectorSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| UploadFailure::Other(format!("Invalid server URL: {}", e)))?;

    let total_size = file_data.len() as u64;
    let hash = Sha256Hash::hash(&*file_data);

    progress_callback(Some(0), Some(0)).map_err(callback_failure)?;

    // One auth event covers both HEAD preflight and PUT.
    let auth_header = build_auth_header(&signer, hash).await.map_err(UploadFailure::Other)?;

    // No deadlines: the upload is bounded by progress in `send_upload`.
    // Redirects disabled: a 3xx mid-PUT would re-issue as GET and drop the body.
    let client = crate::net::build_http_client_with_options(None, None, false)
        .map_err(UploadFailure::Other)?;

    if preflight_first {
        match preflight(&client, &upload_url, server_url, &auth_header, hash, total_size, mime_type).await? {
            Preflight::Proceed => {}
            Preflight::AlreadyStored(url) => {
                progress_callback(Some(100), Some(total_size)).map_err(callback_failure)?;
                return Ok(url);
            }
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| UploadFailure::Other(format!("Invalid content type: {}", e)))?
        );
    }
    // `Body::wrap_stream` is unknown-length so reqwest would default to
    // chunked encoding and omit Content-Length — some servers (e.g.
    // blossom.data.haus) then 411.
    headers.insert(CONTENT_LENGTH, HeaderValue::from(total_size));

    let started = std::time::Instant::now();
    let response = send_upload(
        client.put(upload_url.clone()).headers(headers),
        server_url,
        file_data,
        crate::net::TRANSFER_STALL,
        cancel_flag.as_ref(),
        Some(progress_callback),
    ).await?;

    // BUD-02: accept any 2xx (200 OK or 201 Created).
    let status = response.status();
    if status.is_success() {
        crate::blossom_stats::record_upload(server_url.as_str(), total_size, started.elapsed().as_secs_f64() * 1000.0);
        let descriptor: BlobDescriptor = response.json().await
            .map_err(|e| UploadFailure::Other(format!("Failed to parse response: {}", e)))?;
        // Integrity gate: a compliant server stores our bytes verbatim, so the
        // returned descriptor hash MUST equal what we uploaded. A mismatch means
        // the server transformed/re-encoded the blob, which is fatal for an
        // encrypted upload (corrupts the ciphertext). The failover loop routes
        // around the server like a hard rejection.
        if descriptor.sha256 != hash {
            return Err(UploadFailure::Integrity(format!(
                "[INTEGRITY] {} transformed the upload (returned {}, expected {})",
                server_url, descriptor.sha256, hash,
            )));
        }
        Ok(descriptor.url.to_string())
    } else {
        let refusal = refusal_from(response).await;
        crate::log_net_fail!(
            "[Blossom] upload rejected: HTTP {}{} — {}",
            status,
            refusal.code().map(|c| format!(" [{}]", c)).unwrap_or_default(),
            refusal.message,
        );
        Err(UploadFailure::Refused(refusal))
    }
}

/// Simple upload without progress reporting.
///
/// `stall_timeout` is how long the server may accept nothing before it is
/// treated as dead and failover moves on; `None` is [`crate::net::TRANSFER_STALL`].
/// Small uploads (emoji, avatars) pass a short one so a broken host costs
/// seconds. There is no total time limit at any size.
pub async fn upload_blob<T>(
    signer: T,
    server_url: &Url,
    file_data: Arc<Vec<u8>>,
    mime_type: Option<&str>,
    stall_timeout: Option<std::time::Duration>,
) -> Result<String, UploadFailure>
where
    T: VectorSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| UploadFailure::Other(format!("Invalid server URL: {}", e)))?;

    let hash = Sha256Hash::hash(&*file_data);
    let total_size = file_data.len() as u64;

    let auth_header = build_auth_header(&signer, hash).await.map_err(UploadFailure::Other)?;

    // Redirects disabled so a 3xx mid-PUT doesn't re-issue as GET.
    let client = crate::net::build_http_client_with_options(None, None, false)
        .map_err(UploadFailure::Other)?;

    if let Preflight::AlreadyStored(url) =
        preflight(&client, &upload_url, server_url, &auth_header, hash, total_size, mime_type).await?
    {
        return Ok(url);
    }

    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| UploadFailure::Other(format!("Invalid content type: {}", e)))?
        );
    }
    headers.insert(CONTENT_LENGTH, HeaderValue::from(total_size));

    let started = std::time::Instant::now();
    let response = send_upload(
        client.put(upload_url).headers(headers),
        server_url,
        file_data,
        stall_timeout.unwrap_or(crate::net::TRANSFER_STALL),
        None,
        None,
    ).await?;

    // BUD-02: accept any 2xx (200 OK or 201 Created).
    let status = response.status();
    if status.is_success() {
        crate::blossom_stats::record_upload(server_url.as_str(), total_size, started.elapsed().as_secs_f64() * 1000.0);
        let descriptor: BlobDescriptor = response.json().await
            .map_err(|e| UploadFailure::Other(format!("Failed to parse response: {}", e)))?;
        // Integrity gate (see upload_attempt): reject a server that returns a
        // different hash than we uploaded — it re-encoded the blob.
        if descriptor.sha256 != hash {
            return Err(UploadFailure::Integrity(format!(
                "[INTEGRITY] {} transformed the upload (returned {}, expected {})",
                server_url, descriptor.sha256, hash,
            )));
        }
        Ok(descriptor.url.to_string())
    } else {
        Err(UploadFailure::Refused(refusal_from(response).await))
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
    matches!(crate::net::remote_status(url, timeout).await, Some(200..=299))
}

/// anything else passes, so a flaky HEAD can't sink a good upload.
async fn uploaded_blob_serves(url: &str) -> bool {
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        match uploaded_blob_status(url).await {
            Some(404) | Some(410) => continue,
            Some(_) => return true,
            None => {
                crate::log_net_info!("[Blossom] {} unreachable for post-upload verify — assuming stored", url);
                return true;
            }
        }
    }
    crate::log_net_fail!("[Blossom] {} ACKed the upload but serves 404/410 — treating as dropped", url);
    false
}

/// The status a just-uploaded blob answers with. Asked directly, it rides the
/// upload's pooled connection instead of opening a fresh one to the same host.
async fn uploaded_blob_status(url: &str) -> Option<u16> {
    let timeout = std::time::Duration::from_secs(10);
    if crate::net::egress(url).await.proxied() {
        return crate::net::remote_status(url, timeout).await;
    }
    let client = crate::net::build_http_client_with_options(None, None, false).ok()?;
    let status = tokio::time::timeout(timeout, client.head(url).send()).await.ok()?.ok()?.status();
    if status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_IMPLEMENTED {
        return crate::net::remote_status(url, timeout).await;
    }
    Some(status.as_u16())
}

/// What a failure says about the server's state for the status pill: gone
/// (nothing arrived) or full (a quota or the disk). A refusal about the file
/// itself says nothing about the server.
fn note_failure(server_url: &str, e: &UploadFailure) {
    match e {
        UploadFailure::Transport(_) => {
            crate::blossom_stats::record_failure(server_url, crate::blossom_stats::FAIL_OFFLINE)
        }
        UploadFailure::Refused(r) => {
            if matches!(r.code(), Some("quota_storage_exceeded" | "quota_daily_exceeded" | "server_full")) {
                crate::blossom_stats::record_failure(server_url, crate::blossom_stats::FAIL_MAXED);
            } else {
                crate::blossom_stats::record_ok(server_url);
            }
        }
        _ => {}
    }
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
    stall_timeout: Option<std::time::Duration>,
) -> Result<String, String>
where
    T: VectorSigner + Clone,
{
    let mut failures: Vec<(String, UploadFailure)> = Vec::new();

    for (index, server_url_str) in server_urls.iter().enumerate() {
        let host = host_of(server_url_str);
        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                crate::log_net_fail!("[Blossom] invalid server URL '{}': {}", server_url_str, e);
                failures.push((host, UploadFailure::Other(format!("Invalid server URL: {}", e))));
                continue;
            }
        };

        crate::log_info!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, server_urls.len(), server_url_str);

        match upload_blob(signer.clone(), &server_url, file_data.clone(), mime_type, stall_timeout).await {
            Ok(url) => {
                if !uploaded_blob_serves(&url).await {
                    crate::log_warn!(
                        "[Blossom Error] {} ACKed the upload but does not serve {} — failing over",
                        server_url_str, url,
                    );
                    failures.push((host, UploadFailure::Other(format!(
                        "{} accepted the upload but the blob is not retrievable", server_url_str,
                    ))));
                    continue;
                }
                crate::log_net_info!("[Blossom] upload OK via {}", server_url_str);
                crate::blossom_stats::record_ok(server_url_str);
                return Ok(url);
            }
            Err(e) => {
                crate::log_net_fail!("[Blossom] upload failed to {}: {}", server_url_str, e);
                note_failure(server_url_str, &e);
                failures.push((host, e));
            }
        }
    }

    let summary = summarise_failures(&failures);
    crate::log_net_fail!("[Blossom] ALL servers failed: {}", summary);
    Err(summary)
}

/// An upload that a server accepted.
///
/// `url` is the descriptor the server returned; `server` is the address Vector
/// actually talked to. They are frequently NOT the same host: an ingress relay
/// takes the bytes on a regional name and the origin hands back its own
/// canonical `public_url`. Both name the same server, and mirroring needs to
/// know that — a BUD-04 mirror aimed at either one asks a server to fetch a
/// blob it is already holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedUpload {
    pub url: String,
    pub server: String,
}

/// Open a pooled connection to the server an upload of this kind would try first,
/// so a send that follows starts on a live TLS session rather than a handshake.
/// Best-effort and bodiless: the answer is irrelevant, the connection is the point.
pub async fn warm_upload_connection(server_urls: Vec<String>, mime_type: &str, is_encrypted: bool, size_bytes: u64) {
    let ranked = crate::blossom_capabilities::rank_servers(server_urls, mime_type, is_encrypted, size_bytes);
    let Some(url) = ranked.first().and_then(|u| Url::parse(u).ok()) else { return };
    // The same options as `upload_attempt`, so the upload draws from this pool.
    let Ok(client) = crate::net::build_http_client_with_options(None, None, false) else { return };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), client.head(url).send()).await;
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
) -> Result<AcceptedUpload, String>
where
    T: VectorSigner + Clone,
{
    let mut failures: Vec<(String, UploadFailure)> = Vec::new();

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

        let host = host_of(server_url_str);
        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                crate::log_net_fail!("[Blossom] invalid server URL '{}': {}", server_url_str, e);
                failures.push((host, UploadFailure::Other(format!("Invalid server URL: {}", e))));
                continue;
            }
        };

        crate::log_info!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, ranked.len(), server_url_str);

        // A fresh random key makes every ciphertext new, so a preflight can only
        // refuse; it is skipped where this server has already taken its like.
        let preflight_first = !crate::blossom_capabilities::preflight_redundant(
            server_url_str, mime_for_routing, is_encrypted, size_bytes,
        );
        match upload_with_retries(
            signer.clone(),
            &server_url,
            file_data.clone(),
            mime_type,
            progress_callback.clone(),
            retry_count,
            retry_spacing,
            cancel_flag.clone(),
            preflight_first,
        ).await {
            Ok(url) => {
                if !uploaded_blob_serves(&url).await {
                    crate::log_warn!(
                        "[Blossom Error] {} ACKed the upload but does not serve {} — failing over",
                        server_url_str, url,
                    );
                    failures.push((host, UploadFailure::Other(format!(
                        "{} accepted the upload but the blob is not retrievable", server_url_str,
                    ))));
                    let _ = progress_callback(Some(0), Some(0));
                    continue;
                }
                crate::log_net_info!("[Blossom] upload OK via {}", server_url_str);
                if let Err(err) = crate::blossom_capabilities::record_accepted(
                    server_url_str, mime_for_routing, is_encrypted, size_bytes,
                ) {
                    crate::log_warn!("[Blossom Cap] record_accepted failed: {}", err);
                }
                crate::blossom_info::note_upload(server_url_str, size_bytes);
                crate::blossom_stats::record_ok(server_url_str);
                return Ok(AcceptedUpload {
                    url,
                    server: server_url_str.clone(),
                });
            }
            Err(UploadFailure::Cancelled) => {
                return Err("Upload cancelled".to_string());
            }
            Err(e) => {
                crate::log_net_fail!("[Blossom] upload failed to {}: {}", server_url_str, e);
                note_failure(server_url_str, &e);
                // Worth remembering about this server: a type it does not
                // take, a size it does not take, bytes it does not keep intact.
                // A quota, a rate limit, a closed gate or a bad clock say
                // nothing about the NEXT upload and are not cached.
                match &e {
                    UploadFailure::Integrity(_) => {
                        if let Err(err) = crate::blossom_capabilities::record_rejected_mime(
                            server_url_str, mime_for_routing, is_encrypted,
                        ) {
                            crate::log_warn!("[Blossom Cap] record_rejected_mime failed: {}", err);
                        }
                    }
                    UploadFailure::Refused(r) if r.is_mime() => {
                        if let Err(err) = crate::blossom_capabilities::record_rejected_mime(
                            server_url_str, mime_for_routing, is_encrypted,
                        ) {
                            crate::log_warn!("[Blossom Cap] record_rejected_mime failed: {}", err);
                        }
                    }
                    UploadFailure::Refused(r) if r.is_size_limit() => {
                        // A server that named its limit has told us the exact
                        // edge; one that only said 413 has told us this size.
                        let rejects_from = r.limit().map(|l| l.saturating_add(1)).unwrap_or(size_bytes).min(size_bytes);
                        if let Err(err) = crate::blossom_capabilities::record_rejected_size(
                            server_url_str, mime_for_routing, is_encrypted, rejects_from,
                        ) {
                            crate::log_warn!("[Blossom Cap] record_rejected_size failed: {}", err);
                        }
                    }
                    // Mid-stream drops aren't cached (too ambiguous); only
                    // an explicit size refusal sets min_rejected_size.
                    _ => {}
                }
                failures.push((host, e));
                let _ = progress_callback(Some(0), Some(0));
            }
        }
    }

    let summary = summarise_failures(&failures);
    crate::log_net_fail!("[Blossom] ALL servers failed: {}", summary);
    Err(summary)
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

/// Which of the user's servers are worth mirroring to.
///
/// A server that already holds the blob gains nothing from being told to fetch
/// it, and mirroring exists for redundancy across DISTINCT servers — a server
/// that just accepted the upload is by definition not adding any.
///
/// The source URL alone does not identify those servers. An upload sent to an
/// ingress relay comes back carrying the origin's canonical `public_url`, so
/// `asia.magnitude.jskitty.com` and `magnitude.jskitty.com` are one server
/// wearing two names, and no amount of string comparison relates them. The
/// caller knows which address it actually uploaded to, so it passes that in
/// via `already_held_by` and both names are excluded.
fn mirror_targets(
    source_url: &str,
    server_urls: &[String],
    max_mirrors: usize,
    already_held_by: &[String],
) -> Vec<Url> {
    let source_origin = match parse_blob_url(source_url) {
        Ok((origin, _)) => origin,
        Err(e) => {
            crate::log_debug!("[Blossom Mirror] unparseable source {}: {}", source_url, e);
            return Vec::new();
        }
    };
    let excluded: Vec<url::Origin> = std::iter::once(source_origin.origin())
        .chain(
            already_held_by
                .iter()
                .filter_map(|s| Url::parse(s).ok())
                .map(|u| u.origin()),
        )
        .collect();
    server_urls
        .iter()
        .filter_map(|s| Url::parse(s).ok())
        .filter(|u| !excluded.contains(&u.origin()))
        .take(max_mirrors)
        .collect()
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
    already_held_by: &[String],
) -> Vec<String>
where
    T: VectorSigner + Clone,
{
    let targets = mirror_targets(source_url, &server_urls, max_mirrors, already_held_by);
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
                // A transformed probe blob is as unsuitable as a hard MIME
                // rejection. A closed gate, a quota or a bad clock is not: the
                // server may take the next upload, so its reputation is left
                // alone and the probe runs again later.
                let unsuitable = matches!(e, UploadFailure::Integrity(_))
                    || e.refusal().is_some_and(|r| r.is_mime());
                if unsuitable {
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

#[cfg(test)]
mod mirror_target_tests {
    use super::mirror_targets;

    /// `parse_blob_url` demands a full 64-hex SHA-256, so the fixture carries
    /// a real one; a truncated hash makes every call return no targets.
    const BLOB: &str = "3ad3648700bc0000000000000000000000000000000000000000000000000000.bin";

    fn servers(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn names(t: Vec<url::Url>) -> Vec<String> {
        t.into_iter().map(|u| u.host_str().unwrap_or("").to_string()).collect()
    }

    #[test]
    fn the_server_that_accepted_the_upload_is_not_a_mirror_target() {
        // The case observed in production: the bytes went to the regional
        // ingress name and the descriptor came back on the origin's canonical
        // name. Both are the same server, and neither is worth mirroring to.
        // Comparing the descriptor's host alone does NOT catch this — the two
        // strings differ — which is why the accepting address is passed in.
        let t = mirror_targets(
            &format!("https://magnitude.jskitty.com/{BLOB}"),
            &servers(&["https://asia.magnitude.jskitty.com", "https://blossom.band"]),
            2,
            &["https://asia.magnitude.jskitty.com".to_string()],
        );
        assert_eq!(names(t), vec!["blossom.band"]);
    }

    #[test]
    fn genuinely_different_servers_are_still_mirrored_to() {
        let t = mirror_targets(
            &format!("https://magnitude.jskitty.com/{BLOB}"),
            &servers(&["https://blossom.band", "https://nostr.download"]),
            2,
            &["https://asia.magnitude.jskitty.com".to_string()],
        );
        assert_eq!(names(t), vec!["blossom.band", "nostr.download"]);
    }

    #[test]
    fn the_source_origin_is_excluded_even_with_nothing_passed_in() {
        // The pre-existing guarantee, unchanged by the new argument.
        let t = mirror_targets(
            &format!("https://magnitude.jskitty.com/{BLOB}"),
            &servers(&["https://magnitude.jskitty.com", "https://blossom.band"]),
            2,
            &[],
        );
        assert_eq!(names(t), vec!["blossom.band"]);
    }

    #[test]
    fn max_mirrors_is_applied_after_exclusion_not_before() {
        // Taking first and filtering second would yield nothing here, and the
        // message would ship with no fallback sources at all.
        let t = mirror_targets(
            &format!("https://magnitude.jskitty.com/{BLOB}"),
            &servers(&[
                "https://asia.magnitude.jskitty.com",
                "https://magnitude.jskitty.com",
                "https://blossom.band",
            ]),
            1,
            &["https://asia.magnitude.jskitty.com".to_string()],
        );
        assert_eq!(names(t), vec!["blossom.band"]);
    }

    #[test]
    fn a_port_or_scheme_difference_is_a_different_server() {
        // Origin comparison, not host comparison: a self-hosted server on
        // another port is genuinely somewhere else.
        let t = mirror_targets(
            &format!("https://example.com/{BLOB}"),
            &servers(&["https://example.com:8443"]),
            2,
            &[],
        );
        assert_eq!(names(t), vec!["example.com"]);
    }

    #[test]
    fn an_unparseable_source_mirrors_nowhere() {
        let t = mirror_targets("not a url", &servers(&["https://blossom.band"]), 2, &[]);
        assert!(t.is_empty());
    }
}

#[cfg(test)]
mod stall_watch_tests {
    use super::{Stall, StallWatch};
    use std::time::Duration;

    const STALL: Duration = Duration::from_secs(60);
    const RESPONSE: Duration = Duration::from_secs(300);

    #[tokio::test(start_paused = true)]
    async fn a_transfer_moving_at_any_rate_is_never_abandoned() {
        // One 16 KB chunk every 50 s is under 3 kbit/s, and the watchdog is
        // content: 300 s of wall time here would have killed it under a deadline.
        let mut w = StallWatch::new(1 << 20, STALL, RESPONSE);
        let mut sent = 0u64;
        for _ in 0..8 {
            tokio::time::advance(Duration::from_secs(50)).await;
            sent += 16 * 1024;
            assert_eq!(w.observe(sent), None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_transfer_that_stops_is_abandoned_after_the_window() {
        let mut w = StallWatch::new(1 << 20, STALL, RESPONSE);
        assert_eq!(w.observe(4096), None);
        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(w.observe(4096), None, "inside the window it is only slow");
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(w.observe(4096), Some(Stall::NoProgress { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn progress_resets_the_window() {
        let mut w = StallWatch::new(1 << 20, STALL, RESPONSE);
        w.observe(100);
        tokio::time::advance(Duration::from_secs(55)).await;
        w.observe(200);
        tokio::time::advance(Duration::from_secs(55)).await;
        assert_eq!(w.observe(200), None, "the clock restarted at the second byte");
    }

    #[tokio::test(start_paused = true)]
    async fn after_the_last_byte_the_server_gets_longer_but_not_forever() {
        let total = 1 << 20;
        let mut w = StallWatch::new(total, STALL, RESPONSE);
        w.observe(total);
        // Long enough for a relay to push a large blob to its origin.
        tokio::time::advance(Duration::from_secs(240)).await;
        assert_eq!(w.observe(total), None, "buffers drain and the blob is stored");
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(matches!(w.observe(total), Some(Stall::NoResponse { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_upload_is_waiting_on_the_server_from_the_start() {
        let mut w = StallWatch::new(0, STALL, RESPONSE);
        tokio::time::advance(STALL + Duration::from_secs(1)).await;
        assert_eq!(w.observe(0), None, "nothing to send is not a stall");
        tokio::time::advance(RESPONSE).await;
        assert!(matches!(w.observe(0), Some(Stall::NoResponse { .. })));
    }
}
