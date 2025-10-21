use nostr_sdk::{NostrSigner, Url, Event, EventBuilder, Timestamp, JsonUtil};
use nostr_sdk::hashes::{sha256::Hash as Sha256Hash, Hash};
use nostr_blossom::prelude::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Body, StatusCode};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use futures_util::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};
use base64::engine::general_purpose;
use base64::Engine;

/// Progress callback function type
pub type ProgressCallback = std::sync::Arc<dyn Fn(Option<u8>, Option<u64>) -> Result<(), String> + Send + Sync>;

/// Custom upload stream that tracks progress
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
    T: NostrSigner,
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
    let auth_event: Event = EventBuilder::blossom_auth(auth)
        .sign(signer)
        .await
        .map_err(|e| format!("Failed to sign auth event: {}", e))?;
    
    // Encode as base64
    let encoded_auth = general_purpose::STANDARD.encode(auth_event.as_json());
    let value = format!("Nostr {}", encoded_auth);
    
    HeaderValue::try_from(value)
        .map_err(|e| format!("Failed to create header value: {}", e))
}

/// Uploads data to a Blossom server with progress callback
///
/// This function implements Blossom file upload with progress reporting
/// via a callback function that is called periodically during the upload process.
///
/// # Retry Parameters
/// - `retry_count`: Optional number of retry attempts (default: 0)
/// - `retry_spacing`: Optional delay between retry attempts (default: 1s)
pub async fn upload_blob_with_progress<T>(
    signer: T,
    server_url: &Url,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
) -> Result<String, String>
where
    T: NostrSigner + Clone,
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
            signer.clone(),
            server_url,
            file_data.clone(),
            mime_type,
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
    Err(last_error.unwrap_or_else(|| "No upload attempts were made".to_string()))
}

/// Internal function that performs a single upload attempt with progress tracking
async fn upload_attempt<T>(
    signer: T,
    server_url: &Url,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    progress_callback: &ProgressCallback,
) -> Result<String, String>
where
    T: NostrSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| format!("Invalid server URL: {}", e))?;
    
    let total_size = file_data.len() as u64;
    let hash = Sha256Hash::hash(&file_data);
    
    // Report initial progress (0%)
    progress_callback(Some(0), Some(0)).map_err(|e| e)?;
    
    // Build authorization header
    let auth_header = build_auth_header(&signer, hash).await?;
    
    // Create shared counter for tracking upload progress
    let bytes_sent = Arc::new(Mutex::new(0u64));
    let bytes_sent_clone = Arc::clone(&bytes_sent);
    
    // Create the streaming body with progress tracking
    let tracking_stream = ProgressTrackingStream::new(file_data, bytes_sent_clone);
    let body = Body::wrap_stream(tracking_stream);
    
    // Build headers
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| format!("Invalid content type: {}", e))?
        );
    }
    
    // Create HTTP client
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300)) // 5 minute timeout
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
    
    // Start the upload request
    let mut request_future = Box::pin(client
        .put(upload_url.clone())
        .headers(headers)
        .body(body)
        .send());
    
    // Monitor progress while upload is in progress
    let mut last_percentage = 0;
    let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
    
    let response = loop {
        tokio::select! {
            // Check if the response is ready
            response = &mut request_future => {
                break response.map_err(|e| format!("Upload request failed: {}", e))?;
            },
            // Report progress periodically
            _ = poll_interval.tick() => {
                let current_bytes = *bytes_sent.lock().unwrap();
                let percentage = if total_size > 0 {
                    ((current_bytes as f64 / total_size as f64) * 100.0) as u8
                } else {
                    0
                };
                
                // Report every percentage change
                if percentage != last_percentage {
                    if let Err(e) = progress_callback(Some(percentage), Some(current_bytes)) {
                        return Err(e);
                    }
                    last_percentage = percentage;
                }
            }
        }
    };
    
    // Ensure we report 100% if we haven't already (in case the loop exited before catching it)
    let final_bytes = *bytes_sent.lock().unwrap();
    if final_bytes == total_size && last_percentage < 100 {
        progress_callback(Some(100), Some(total_size)).map_err(|e| e)?;
    }
    
    // Check response status
    match response.status() {
        StatusCode::OK => {
            let descriptor: BlobDescriptor = response.json().await
                .map_err(|e| format!("Failed to parse response: {}", e))?;
            Ok(descriptor.url.to_string())
        }
        status => {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            eprintln!("[Blossom Error] Upload failed with status {}: {}", status, error_text);
            Err(format!("Upload failed with status {}: {}", status, error_text))
        }
    }
}

/// Simple upload without progress tracking
pub async fn upload_blob<T>(
    signer: T,
    server_url: &Url,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
) -> Result<String, String>
where
    T: NostrSigner,
{
    let upload_url = server_url.join("upload")
        .map_err(|e| format!("Invalid server URL: {}", e))?;
    
    let hash = Sha256Hash::hash(&file_data);
    
    // Build authorization header
    let auth_header = build_auth_header(&signer, hash).await?;
    
    // Build headers
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, auth_header);
    if let Some(ct) = mime_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|e| format!("Invalid content type: {}", e))?
        );
    }
    
    // Create HTTP client
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
    
    // Perform the upload
    let response = client
        .put(upload_url)
        .headers(headers)
        .body(file_data)
        .send()
        .await
        .map_err(|e| format!("Upload request failed: {}", e))?;
    
    // Check response status
    match response.status() {
        StatusCode::OK => {
            let descriptor: BlobDescriptor = response.json().await
                .map_err(|e| format!("Failed to parse response: {}", e))?;
            Ok(descriptor.url.to_string())
        }
        status => {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            Err(format!("Upload failed with status {}: {}", status, error_text))
        }
    }
}

/// Upload to multiple Blossom servers with automatic failover
/// Tries each server in the list until one succeeds
pub async fn upload_blob_with_failover<T>(
    signer: T,
    server_urls: Vec<String>,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
) -> Result<String, String>
where
    T: NostrSigner + Clone,
{
    let mut last_error = String::from("No servers available");
    
    for (index, server_url_str) in server_urls.iter().enumerate() {
        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                eprintln!("[Blossom Error] Invalid server URL '{}': {}", server_url_str, e);
                last_error = format!("Invalid server URL: {}", e);
                continue;
            }
        };
        
        eprintln!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, server_urls.len(), server_url_str);
        
        match upload_blob(signer.clone(), &server_url, file_data.clone(), mime_type).await {
            Ok(url) => {
                eprintln!("[Blossom] Upload successful to: {}", server_url_str);
                return Ok(url);
            }
            Err(e) => {
                eprintln!("[Blossom Error] Upload failed to {}: {}", server_url_str, e);
                last_error = e;
                // Continue to next server
            }
        }
    }
    
    // All servers failed
    Err(format!("All Blossom servers failed. Last error: {}", last_error))
}

/// Upload with progress tracking and automatic failover to multiple servers
/// Tries each server in the list until one succeeds, with progress reporting
pub async fn upload_blob_with_progress_and_failover<T>(
    signer: T,
    server_urls: Vec<String>,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    progress_callback: ProgressCallback,
    retry_count: Option<u32>,
    retry_spacing: Option<std::time::Duration>,
) -> Result<String, String>
where
    T: NostrSigner + Clone,
{
    let mut last_error = String::from("No servers available");
    
    for (index, server_url_str) in server_urls.iter().enumerate() {
        let server_url = match Url::parse(server_url_str) {
            Ok(url) => url,
            Err(e) => {
                eprintln!("[Blossom Error] Invalid server URL '{}': {}", server_url_str, e);
                last_error = format!("Invalid server URL: {}", e);
                continue;
            }
        };
        
        eprintln!("[Blossom] Attempting upload to server {} of {}: {}",
            index + 1, server_urls.len(), server_url_str);
        
        // Try uploading to this server with progress tracking and retries
        match upload_blob_with_progress(
            signer.clone(),
            &server_url,
            file_data.clone(),
            mime_type,
            progress_callback.clone(),
            retry_count,
            retry_spacing,
        ).await {
            Ok(url) => {
                eprintln!("[Blossom] Upload successful to: {}", server_url_str);
                return Ok(url);
            }
            Err(e) => {
                eprintln!("[Blossom Error] Upload failed to {}: {}", server_url_str, e);
                last_error = e;
                // Reset progress to 0 before trying next server
                let _ = progress_callback(Some(0), Some(0));
                // Continue to next server
            }
        }
    }
    
    // All servers failed
    Err(format!("All Blossom servers failed. Last error: {}", last_error))
}