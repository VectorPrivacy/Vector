use futures_util::StreamExt;
use reqwest::{self, Client};
use serde_json::json;
use std::cmp::min;
use tauri::{AppHandle, Emitter};

/// Trait for reporting download progress
pub trait ProgressReporter {
    /// Report progress of a download
    fn report_progress(&self, percentage: Option<u8>, bytes_downloaded: Option<u64>) -> Result<(), &'static str>;
    
    /// Report completion of a download
    fn report_complete(&self) -> Result<(), &'static str>;
}

/// A no-op progress reporter that does nothing when progress is reported
pub struct NoOpProgressReporter;

impl NoOpProgressReporter {
    /// Create a new NoOpProgressReporter
    pub fn new() -> Self {
        Self {}
    }
}

impl ProgressReporter for NoOpProgressReporter {
    fn report_progress(&self, _percentage: Option<u8>, _bytes_downloaded: Option<u64>) -> Result<(), &'static str> {
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
    fn report_progress(&self, percentage: Option<u8>, bytes_downloaded: Option<u64>) -> Result<(), &'static str> {
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
        
        self.handle
            .emit("attachment_download_progress", payload)
            .map_err(|_| "Failed to emit event")
    }
    
    fn report_complete(&self) -> Result<(), &'static str> {
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

/// Downloads the file in-memory at the given URL with progress reporting
pub async fn download<R: tauri::Runtime>(
    content_url: &str,
    handle: &AppHandle<R>,
    attachment_id: &str,
    timeout: Option<std::time::Duration>,
) -> Result<Vec<u8>, &'static str> {
    let reporter = TauriProgressReporter::new(handle, attachment_id);
    download_with_reporter(content_url, &reporter, timeout).await
}

/// Downloads the file in-memory at the given URL without progress reporting
pub async fn download_silent(
    content_url: &str, 
    timeout: Option<std::time::Duration>,
) -> Result<Vec<u8>, &'static str> {
    let reporter = NoOpProgressReporter::new();
    download_with_reporter(content_url, &reporter, timeout).await
}

/// Generic download function that works with any progress reporter
pub async fn download_with_reporter(
    content_url: &str,
    reporter: &impl ProgressReporter,
    timeout: Option<std::time::Duration>,
) -> Result<Vec<u8>, &'static str> {
    // Create a client with the specified timeout
    let client = if let Some(duration) = timeout {
        Client::builder()
            .timeout(duration)
            .build()
            .map_err(|_| "Failed to create HTTP client")?
    } else {
        Client::new()
    };
    let mut total_size: Option<u64> = None;

    // Method 1: Try HEAD request
    if let Ok(head_res) = client.head(content_url).send().await {
        if let Some(length) = head_res.content_length() {
            if length > 0 {
                total_size = Some(length);
            }
        }
    }

    // Method 2: Try a small GET request to check if it accepts ranges and get size
    if total_size.is_none() {
        if let Ok(partial_res) = client
            .get(content_url)
            .header("Range", "bytes=0-1")
            .send()
            .await
        {
            // Check for Content-Range header which typically looks like "bytes 0-1/12345"
            if let Some(content_range) = partial_res.headers().get("content-range") {
                if let Ok(range_str) = content_range.to_str() {
                    if let Some(size_part) = range_str.split('/').nth(1) {
                        if let Ok(size) = size_part.parse::<u64>() {
                            total_size = Some(size);
                        }
                    }
                }
            }

            // Some servers provide complete size with partial request
            if let Some(length) = partial_res.content_length() {
                // Check if this is the full file or just the range
                // If it's much larger than our 2-byte request, it's likely the full file
                if length > 100 && total_size.is_none() {
                    total_size = Some(length);
                }
            }
        }
    }

    // Based on findings, choose the appropriate download method
    match total_size {
        Some(size) if supports_range(content_url, &client).await => {
            // Use range-based download with progress
            download_with_ranges(&client, content_url, size, reporter).await
        }
        Some(size) => {
            // Use streaming download with known size
            download_with_streaming(&client, content_url, Some(size), reporter).await
        }
        None => {
            // Use streaming download without known size
            download_with_streaming(&client, content_url, None, reporter).await
        }
    }
}

/// Checks if the server supports range requests
async fn supports_range(url: &str, client: &Client) -> bool {
    if let Ok(res) = client.head(url).send().await {
        if let Some(accept_ranges) = res.headers().get("accept-ranges") {
            if let Ok(value) = accept_ranges.to_str() {
                return value.contains("bytes");
            }
        }
    }

    // Try a practical test with a range request
    if let Ok(res) = client.get(url).header("Range", "bytes=0-10").send().await {
        return res.status().as_u16() == 206; // 206 Partial Content
    }

    false
}

/// Downloads using HTTP range requests with progress reporting
async fn download_with_ranges(
    client: &Client,
    url: &str,
    total_size: u64,
    reporter: &impl ProgressReporter,
) -> Result<Vec<u8>, &'static str> {
    let mut result = Vec::with_capacity(total_size as usize);
    let mut downloaded: u64 = 0;
    let mut last_emitted_percentage: u8 = 0;

    // Download in chunks
    const CHUNK_SIZE: u64 = 256_000; // 256KB chunks (0.25MB)

    while downloaded < total_size {
        let end = min(downloaded + CHUNK_SIZE - 1, total_size - 1);

        let chunk_res = client
            .get(url)
            .header("Range", format!("bytes={}-{}", downloaded, end))
            .send()
            .await
            .map_err(|_| "Failed to download chunk")?;

        if chunk_res.status().as_u16() != 206 {
            return Err("Server did not honor range request");
        }

        let chunk = chunk_res
            .bytes()
            .await
            .map_err(|_| "Failed to read chunk bytes")?;

        result.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        // Calculate progress percentage
        let progress = (downloaded as f64 / total_size as f64) * 100.0;
        let current_percentage = progress as u8;

        // Only emit events when percentage changes (to reduce events)
        if current_percentage > last_emitted_percentage {
            reporter.report_progress(Some(current_percentage), Some(downloaded))?;
            last_emitted_percentage = current_percentage;
        }
    }

    // Ensure 100% is emitted at the end
    reporter.report_complete()?;

    Ok(result)
}

/// Downloads using a streaming approach with progress reporting
async fn download_with_streaming(
    client: &Client,
    url: &str,
    total_size: Option<u64>,
    reporter: &impl ProgressReporter,
) -> Result<Vec<u8>, &'static str> {
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Failed to download")?;

    // Create a buffer to store all data
    let capacity = total_size.unwrap_or(1024 * 1024) as usize; // 1MB default or known size
    let mut result = Vec::with_capacity(capacity);
    let mut downloaded: u64 = 0;
    let mut last_emitted_percentage: u8 = 0;
    let mut last_bytes_update: u64 = 0;

    // Get the stream and process it
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|_| "Error downloading chunk")?;

        result.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        // Report progress
        if let Some(size) = total_size {
            // We know the total size
            let progress = (downloaded as f64 / size as f64) * 100.0;
            let current_percentage = progress as u8;

            // Only emit events when percentage changes (to reduce events)
            if current_percentage > last_emitted_percentage {
                reporter.report_progress(Some(current_percentage), Some(downloaded))?;
                last_emitted_percentage = current_percentage;
            }
        } else {
            // Unknown size, emit progress updates at reasonable intervals
            // For example, every 256KB
            if downloaded - last_bytes_update >= 256 * 1024 {
            // We can't calculate percentage, but we can still show activity
            // Report with bytes downloaded instead of percentage
            reporter.report_progress(None, Some(downloaded))?;

                last_bytes_update = downloaded;
            }
        }
    }

    // Final event with complete status
    reporter.report_complete()?;

    Ok(result)
}
