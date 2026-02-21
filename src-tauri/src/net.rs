use std::cmp::min;

use futures_util::StreamExt;
use reqwest::{self, Client};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::simd::html_meta;

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
    #[allow(dead_code)]
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

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SiteMetadata {
    pub domain: String,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub og_url: Option<String>,
    pub og_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub favicon: Option<String>,
}

/// Fetch metadata specifically for Twitter/X posts using their oEmbed API
async fn fetch_twitter_metadata(url: &str) -> Result<SiteMetadata, String> {
    // Use Twitter's oEmbed API for reliable metadata extraction
    let encoded_url = url.replace("&", "%26").replace("?", "%3F").replace("=", "%3D");
    let oembed_url = format!("https://publish.twitter.com/oembed?url={}", encoded_url);
    
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;
    
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

    let client = reqwest::Client::new();
    let mut response = client
        .get(url)
        .header("Range", "bytes=0-32768")
        .send()
        .await
        .map_err(|e| e.to_string())?;

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
            }
            None => break,
        }
    }

    let html_string = String::from_utf8(html_chunk).map_err(|e| e.to_string())?;
    let parsed = html_meta::extract_html_meta(&html_string);

    let mut metadata = SiteMetadata {
        domain: domain.clone(),
        og_title: parsed.og_title.map(|c| c.into_owned()),
        og_description: parsed.og_description.map(|c| c.into_owned()),
        og_image: parsed.og_image.map(|u| normalize_url(&u, &domain)),
        og_url: parsed.og_url.map(|c| c.into_owned()).or(Some(url.to_string())),
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

/// Check if a URL is live and accessible
/// Returns true if the URL responds with a success status (2xx)
pub async fn check_url_live(url: &str) -> Result<bool, &'static str> {
    // Create a client with a reasonable timeout for checking
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "Failed to create HTTP client")?;
    
    // Try a HEAD request first (more efficient)
    match client.head(url).send().await {
        Ok(response) => {
            // Check if status is 2xx (success)
            Ok(response.status().is_success())
        }
        Err(_) => {
            // If HEAD fails, try a GET request with minimal range
            // Some servers don't support HEAD requests
            match client
                .get(url)
                .header("Range", "bytes=0-1")
                .send()
                .await
            {
                Ok(response) => {
                    // Accept both 200 (full content) and 206 (partial content)
                    let status = response.status();
                    Ok(status.is_success() || status.as_u16() == 206)
                }
                Err(_) => Ok(false), // URL is not accessible
            }
        }
    }
}
