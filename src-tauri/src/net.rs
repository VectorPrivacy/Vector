use std::cmp::min;

use futures_util::StreamExt;
use reqwest::{self, Client};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use scraper::{Html, Selector};

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

pub async fn fetch_site_metadata(url: &str) -> Result<SiteMetadata, String> {
    // Extract and normalize domain
    let domain = {
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 3 {
            let mut domain = format!("{}://{}", parts[0].trim_end_matches(':'), parts[2]);
            if !domain.ends_with('/') {
                domain.push('/');
            }
            domain
        } else {
            let mut domain = url.to_string();
            if !domain.ends_with('/') {
                domain.push('/');
            }
            domain
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
    
    // Read the response in chunks
    loop {
        let chunk = response.chunk().await.map_err(|e| e.to_string())?;
        match chunk {
            Some(data) => {
                html_chunk.extend_from_slice(&data);
                
                if let Ok(current_html) = String::from_utf8(html_chunk.clone()) {
                    if let Some(head_end) = current_html.find("</head>") {
                        html_chunk.truncate(head_end + 7);
                        break;
                    }
                }
            }
            None => break,
        }
    }
    
    let html_string = String::from_utf8(html_chunk).map_err(|e| e.to_string())?;
    let document = Html::parse_document(&html_string);
    let meta_selector = Selector::parse("meta").unwrap();
    let link_selector = Selector::parse("link").unwrap();
    
    let mut metadata = SiteMetadata {
        domain: domain.clone(),
        og_title: None,
        og_description: None,
        og_image: None,
        og_url: Some(String::from(url)),
        og_type: None,
        title: None,
        description: None,
        favicon: None,
    };
    
    // Process favicon links
    let mut favicon_candidates = Vec::new();
    for link in document.select(&link_selector) {
        if let Some(rel) = link.value().attr("rel") {
            if let Some(href) = link.value().attr("href") {
                match rel.to_lowercase().as_str() {
                    "icon" | "shortcut icon" | "apple-touch-icon" => {
                        // Normalize the favicon URL
                        let favicon_url = if href.starts_with("https://") {
                            href.to_string()
                        } else if href.starts_with("//") {
                            format!("https:{}", href)
                        } else if href.starts_with('/') {
                            format!("{}{}", domain.trim_end_matches('/'), href)
                        } else {
                            format!("{}/{}", domain.trim_end_matches('/'), href)
                        };
                        
                        favicon_candidates.push((favicon_url, rel.to_lowercase()));
                    }
                    _ => {}
                }
            }
        }
    }

    // Set favicon with priority order
    if favicon_candidates.is_empty() {
        // If no favicon found in links, try the default /favicon.ico location
        metadata.favicon = Some(format!("{}/favicon.ico", domain.trim_end_matches('/')));
    } else {
        // Priority order:
        // 1. apple-touch-icon (highest quality)
        // 2. icon with .png extension
        // 3. shortcut icon with .png extension
        // 4. any other icon
        // 5. fallback to /favicon.ico
        
        let favicon = favicon_candidates.iter()
            .find(|(_url, rel)| 
                rel == "apple-touch-icon")
            .or_else(|| 
                favicon_candidates.iter()
                    .find(|(url, _)| 
                        url.ends_with(".png")))
            .or_else(|| 
                favicon_candidates.iter()
                    .find(|(_, rel)| 
                        rel == "icon" || rel == "shortcut icon"))
            .map(|(url, _)| url.clone())
            .or_else(|| 
                // Fallback to /favicon.ico
                Some(format!("{}/favicon.ico", domain.trim_end_matches('/')))
            );
        
        metadata.favicon = favicon;
    }
    
    // Process meta tags (existing code)
    for meta in document.select(&meta_selector) {
        let element = meta.value();
        
        if let Some(property) = element.attr("property") {
            if let Some(content) = element.attr("content") {
                match property {
                    "og:title" => metadata.og_title = Some(content.to_string()),
                    "og:description" => metadata.og_description = Some(content.to_string()),
                    "og:image" => {
                        let image_url = if content.starts_with("https://") {
                            content.to_string()
                        } else if content.starts_with("//") {
                            format!("https:{}", content)
                        } else if content.starts_with('/') {
                            format!("{}{}", domain.trim_end_matches('/'), content)
                        } else {
                            format!("{}{}", domain.trim_end_matches('/'), content)
                        };
                        metadata.og_image = Some(image_url);
                    },
                    "og:url" => metadata.og_url = Some(content.to_string()),
                    "og:type" => metadata.og_type = Some(content.to_string()),
                    _ => {}
                }
            }
        }
        
        if let Some(name) = element.attr("name") {
            if let Some(content) = element.attr("content") {
                match name {
                    "description" => metadata.description = Some(content.to_string()),
                    _ => {}
                }
            }
        }
    }
    
    // Extract title from title tag
    if let Some(title_element) = document.select(&Selector::parse("title").unwrap()).next() {
        metadata.title = Some(title_element.text().collect::<String>());
    }
    
    Ok(metadata)
}