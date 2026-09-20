//! The GIF picker's service, fetched from Rust so the privacy setting
//! applies to it. The WebView used to talk to gifverse.net itself, which
//! left every trending list, every search term and every preview as a
//! request from this device even with proxying on; through here the
//! request takes the same egress as everything else.

const GIF_SERVICE: &str = "https://gifverse.net";

/// One GIF API query (`trending?...` or `search?q=...`), answered as the
/// service's JSON text. The path is fixed here; the caller only picks the
/// query, so nothing else on that host can be asked for through this.
#[tauri::command]
pub async fn gif_api(query: String) -> Result<String, String> {
    let (path, _) = query.split_once('?').unwrap_or((query.as_str(), ""));
    if !matches!(path, "trending" | "search") {
        return Err("unknown GIF query".to_string());
    }
    if query.contains("/") || query.contains("..") || query.contains('#') {
        return Err("unknown GIF query".to_string());
    }
    let url = format!("{GIF_SERVICE}/api/v1/{query}");
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(15))?;
    let resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, &url)
        .await
        .send()
        .await
        .map_err(|e| format!("gif service: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("gif service: HTTP {}", resp.status()));
    }
    // A listing is a few KB; a megabyte is already a broken answer.
    const MAX: usize = 1024 * 1024;
    let mut body = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("gif service: {e}"))? {
        if body.len() + chunk.len() > MAX {
            return Err("gif service: answer too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "gif service: answer is not text".to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn only_the_two_listings_can_be_asked_for() {
        for bad in ["media/abc/original.gif", "../admin", "search/x?q=1", "trending#x"] {
            assert!(super::gif_api(bad.to_string()).await.is_err(), "{bad}");
        }
    }
}
