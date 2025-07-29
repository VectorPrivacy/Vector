use nostr_sdk::{
    nips::nip96::{Error, ServerConfig, UploadResponse, UploadResponseStatus},
    nips::nip98::{HttpData, HttpMethod},
    NostrSigner, TagKind, TagStandard, Url,
};
use nostr_sdk::hashes::{sha256::Hash as Sha256Hash, Hash};
use reqwest::{
    multipart::{self, Part},
    Body, Client
};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Makes a reqwest client
fn make_client(proxy: Option<SocketAddr>) -> Result<Client, Error> {
    let client: Client = {
        let mut builder = Client::builder()
            // Set connection timeout to 5 seconds - this only applies to initial connection
            .connect_timeout(std::time::Duration::from_secs(5))
            // No read timeout - we'll rely on stall detection instead
            // This allows large files to upload without timing out
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(2);
            
        if let Some(proxy) = proxy {
            let proxy = format!("socks5h://{proxy}");
            use reqwest::Proxy;
            builder = builder.proxy(Proxy::all(proxy).map_err(|_e| Error::InvalidURL)?);
        }
        builder.build().map_err(|_e| Error::InvalidURL)?
    };

    Ok(client)
}

/// Custom upload stream that allows tracking progress
struct ProgressTrackingStream {
    bytes_sent: Arc<Mutex<u64>>,
    inner: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
}

impl ProgressTrackingStream {
    fn new(data: Vec<u8>, bytes_sent: Arc<Mutex<u64>>) -> Self {
        let (tx, rx) = mpsc::channel(8); // Buffer size of 8 chunks
        
        // Spawn a background task to feed the stream
        tokio::spawn(async move {
            let chunk_size = 64 * 1024; // 64 KB chunks
            let mut position = 0;
            
            while position < data.len() {
                let end = std::cmp::min(position + chunk_size, data.len());
                let chunk = data[position..end].to_vec();
                let chunk_size = chunk.len();
                
                // Send chunk through channel
                if tx.send(Ok(chunk)).await.is_err() {
                    break; // Receiver was dropped
                }
                
                position += chunk_size;
            }
        });
        
        Self {
            bytes_sent,
            inner: rx,
        }
    }
}

impl futures_util::Stream for ProgressTrackingStream {
    type Item = Result<Vec<u8>, std::io::Error>;
    
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        
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

/// Progress callback function type
pub type ProgressCallback = Box<dyn Fn(Option<u8>, Option<u64>) -> Result<(), String> + Send + Sync>;

/// Uploads data to a NIP-96 server with progress callback
///
/// This function implements NIP-96 file upload with progress reporting
/// via a callback function that is called periodically during the upload process.
///
/// # Retry Parameters
/// - `retry_count`: Optional number of retry attempts (default: 0)
/// - `retry_spacing`: Optional delay between retry attempts (default: 1s)
pub async fn upload_data_with_progress<T>(
    signer: &T,
    desc: &ServerConfig,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    proxy: Option<SocketAddr>,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
) -> Result<Url, Error>
where
    T: NostrSigner,
{
    let retry_count = retry_count.unwrap_or(0);
    let retry_spacing = retry_spacing.unwrap_or(std::time::Duration::from_secs(1));
    
    let mut last_error = None;
    
    for attempt in 0..=retry_count {
        // Log retry attempt if not the first attempt
        if attempt > 0 {
            // Sleep before retry
            tokio::time::sleep(retry_spacing).await;
        }
        
        match upload_attempt(
            signer,
            desc,
            file_data.clone(),
            mime_type,
            proxy,
            &progress_callback,
        ).await {
            Ok(url) => return Ok(url),
            Err(e) => {
                last_error = Some(e);
                // Continue to next retry attempt
            }
        }
    }
    
    // All attempts failed, return the last error
    Err(last_error.unwrap_or_else(|| Error::UploadError("No upload attempts were made".to_string())))
}

/// Internal function that performs a single upload attempt
async fn upload_attempt<T>(
    signer: &T,
    desc: &ServerConfig,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    proxy: Option<SocketAddr>,
    progress_callback: &ProgressCallback,
) -> Result<Url, Error>
where
    T: NostrSigner,
{
    // Build NIP98 Authorization header
    let payload: Sha256Hash = Sha256Hash::hash(&file_data);
    let data = HttpData::new(desc.api_url.clone(), HttpMethod::POST).payload(payload);
    let nip98_auth: String = data.to_authorization(signer).await?;

    // Create shared counter for tracking upload progress
    let bytes_sent = Arc::new(Mutex::new(0u64));
    let total_size = file_data.len() as u64;
    
    // Report initial progress (0%)
    progress_callback(Some(0), Some(0)).map_err(|e| Error::UploadError(e))?;

    // Make client
    let client: Client = make_client(proxy)?;

    // Create form with tracking stream
    let file_part = {
        let tracking_stream = ProgressTrackingStream::new(file_data.clone(), bytes_sent.clone());
        let body = Body::wrap_stream(tracking_stream);
        
        let mut part = Part::stream(body)
            .file_name("filename");
            
        // Set MIME type if provided
        if let Some(mime_str) = mime_type {
            part = part.mime_str(mime_str).map_err(|e| Error::UploadError(format!("Invalid MIME type: {}", e)))?;
        }
        
        part
    };
    
    let form = multipart::Form::new().part("file", file_part);

    // Launch upload as a future, but don't await it yet
    let mut response_future = client
        .post(desc.api_url.clone())
        .header("Authorization", nip98_auth)
        .multipart(form)
        .send();
    
    // Create a future that polls the bytes_sent counter periodically
    let mut last_percentage = 0;
    let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
    
    // Track stalled uploads
    let mut last_bytes_sent = 0u64;
    let mut stall_counter = 0;
    const STALL_THRESHOLD: u32 = 200; // 20 seconds (200 * 100ms) without progress
    
    // Use tokio::select to concurrently wait for the response and report progress
    let response = loop {
        tokio::select! {
            // Check if the response is ready
            response = &mut response_future => {
                break response.map_err(|e| Error::UploadError(format!("Request failed: {}", e)))?;
            },
            // Report progress periodically
            _ = poll_interval.tick() => {
                let current_bytes = *bytes_sent.lock().unwrap();
                let percentage = if total_size > 0 {
                    ((current_bytes as f64 / total_size as f64) * 100.0) as u8
                } else {
                    0
                };
                
                // Check if upload is stalled
                if current_bytes == last_bytes_sent && percentage < 100 && percentage > 0 {
                    stall_counter += 1;
                    if stall_counter >= STALL_THRESHOLD {
                        return Err(Error::UploadError("Upload stalled - no progress detected".to_string()));
                    }
                } else {
                    // Progress detected, reset stall counter
                    stall_counter = 0;
                    last_bytes_sent = current_bytes;
                }
                
                // Only report when percentage changes to reduce events
                if percentage > last_percentage {
                    if let Err(e) = progress_callback(Some(percentage), Some(current_bytes)) {
                        return Err(Error::UploadError(e));
                    }
                    last_percentage = percentage;
                }
            }
        }
    };
    
    // Report 100% completion
    progress_callback(Some(100), Some(total_size)).map_err(|e| Error::UploadError(e))?;
    
    // Decode response
    let res: UploadResponse = response.json().await.map_err(|_e| Error::ResponseDecodeError)?;

    // Check status
    if res.status == UploadResponseStatus::Error {
        return Err(Error::UploadError(res.message));
    }

    // Extract url
    let nip94_event = res.nip94_event.ok_or(Error::ResponseDecodeError)?;
    match nip94_event.tags.find_standardized(TagKind::Url) {
        Some(TagStandard::Url(url)) => Ok(url.clone()),
        _ => Err(Error::ResponseDecodeError),
    }
}
