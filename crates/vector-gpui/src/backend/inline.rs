//! Pictures linked in messages (`cache_url_image`): fetched through the privacy egress, held in
//! memory like an attachment unless the account keeps media, and then kept only sealed.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde_json::json;

const MAX_BYTES: usize = 1024 * 1024 * 1024;
static DOWNLOADS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(4));

const SIGNATURES: &[(&[u8], &str)] = &[
    (&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], "png"),
    (&[0xFF, 0xD8, 0xFF], "jpg"),
    (b"GIF8", "gif"),
    (b"RIFF", "webp"),
    (b"BM", "bmp"),
    (&[0x00, 0x00, 0x01, 0x00], "ico"),
];

/// The format the bytes are, judged by them alone.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 8 {
        return None;
    }
    for (sig, ext) in SIGNATURES {
        if bytes.starts_with(sig) {
            if *ext == "webp" && (bytes.len() < 12 || &bytes[8..12] != b"WEBP") {
                continue;
            }
            return Some(ext);
        }
    }
    let head = &bytes[..bytes.len().min(512)];
    (head.windows(4).any(|w| w == b"<svg") || head.windows(5).any(|w| w == b"<?xml"))
        .then_some("svg")
}

fn cache_dir() -> Option<PathBuf> {
    let dir = vector_core::db::get_app_data_dir()
        .ok()?
        .join("cache")
        .join("inline_images");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn cache_stem(url: &str) -> String {
    vector_core::crypto::sha256_hex(url.as_bytes())[..32].to_string()
}

fn kept(url: &str) -> Option<PathBuf> {
    let (dir, stem) = (cache_dir()?, cache_stem(url));
    ["png", "jpg", "gif", "webp", "bmp", "ico"]
        .iter()
        .map(|e| dir.join(format!("{stem}.{e}")))
        .find(|p| vector_core::vcache::readable(p))
}

fn progress(url: &str, pct: i64) {
    vector_core::emit_event(
        "inline_image_progress",
        &json!({ "url": url, "progress": pct }),
    );
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let timeout = vector_core::net::tor_http_timeout(std::time::Duration::from_secs(10));
    let client = vector_core::net::build_http_client_with_options(
        Some(timeout),
        Some(vector_core::net::transfer_stall()),
        true,
    )?;
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, url)
        .await
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().filter(|&n| n > 0);
    if total.is_some_and(|n| n as usize > MAX_BYTES) {
        return Err("too large".into());
    }
    let mut body = Vec::with_capacity(total.unwrap_or(0).min(MAX_BYTES as u64) as usize);
    let mut last = -1;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_BYTES {
            return Err("too large".into());
        }
        body.extend_from_slice(&chunk);
        if let Some(t) = total {
            let pct = (body.len() as u64 * 100 / t).min(100) as i64;
            if pct > last {
                last = pct;
                progress(url, pct);
            }
        }
    }
    Ok(body)
}

/// The picture a message links to, as bytes ready to decode, or `None` when it isn't one.
pub async fn image(url: String) -> Option<Vec<u8>> {
    if !url.to_ascii_lowercase().starts_with("https://")
        || vector_core::net::validate_url_not_private(&url).is_err()
    {
        return None;
    }
    let held = vector_core::media::url_key("inline-image", &url);
    if vector_core::media::resident(&held)
        && let Some((bytes, _)) = vector_core::media::held_bytes(&held).await
    {
        return Some(bytes);
    }
    if let Some(path) = kept(&url) {
        return tokio::task::spawn_blocking(move || {
            vector_core::vcache::read(&path).ok().map(|b| b.to_vec())
        })
        .await
        .ok()
        .flatten();
    }
    let _permit = DOWNLOADS.acquire().await.ok()?;
    let bytes = fetch(&url).await.ok()?;
    let mut ext = sniff(&bytes)?;
    // An SVG is kept as the pixels it draws.
    let bytes = if ext == "svg" {
        ext = "png";
        tokio::task::spawn_blocking(move || vector_core::svg::rasterize_png(&bytes, 2048))
            .await
            .ok()?
            .ok()?
    } else {
        bytes
    };
    if vector_core::media::keep_media() {
        let path = cache_dir()?.join(format!("{}.{ext}", cache_stem(&url)));
        tokio::task::spawn_blocking(move || {
            vector_core::vcache::write(&path, &bytes)
                .ok()
                .map(|_| bytes)
        })
        .await
        .ok()
        .flatten()
    } else {
        let mime = vector_core::crypto::mime_from_extension(ext);
        vector_core::media::ram::insert(held, bytes.clone(), mime).ok()?;
        Some(bytes)
    }
}
