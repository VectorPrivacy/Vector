//! The slice of the desktop's image cache the marketplace asks for: Mini App icons on disk.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};

use crate::net::{NoOpProgressReporter, download_to_memory};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    MiniAppIcon,
}

pub enum CacheResult {
    Cached(String),
    AlreadyCached(String),
    Failed(String),
}

const MAX_BYTES: u64 = 1024 * 1024;
const SVG_DIM: u32 = 256;

fn dir<R: Runtime>(handle: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = handle.path().app_data_dir().map_err(|e| e.to_string())?.join("cache").join("miniapp_icons");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn key(url: &str) -> String {
    vector_core::crypto::sha256_hex(url.as_bytes())[..32].to_string()
}

const EXTS: [&str; 6] = ["jpg", "png", "webp", "gif", "ico", "bmp"];

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    const SIGS: &[(&[u8], &str)] = &[
        (&[0x89, b'P', b'N', b'G'], "png"),
        (&[0xFF, 0xD8, 0xFF], "jpg"),
        (b"GIF8", "gif"),
        (&[0x00, 0x00, 0x01, 0x00], "ico"),
        (b"BM", "bmp"),
    ];
    if let Some((_, ext)) = SIGS.iter().find(|(sig, _)| bytes.starts_with(sig)) {
        return Some(ext);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    let head = &bytes[..bytes.len().min(512)];
    head.windows(4).any(|w| w == b"<svg").then_some("svg")
}

pub fn get_cached_path<R: Runtime>(handle: &AppHandle<R>, url: &str, _: ImageType) -> Option<String> {
    let dir = dir(handle).ok()?;
    let key = key(url);
    EXTS.iter()
        .map(|ext| dir.join(format!("{key}.{ext}")))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

pub async fn cache_image<R: Runtime>(handle: &AppHandle<R>, url: &str, kind: ImageType) -> CacheResult {
    if url.is_empty() {
        return CacheResult::Failed("Empty URL".into());
    }
    if let Some(p) = get_cached_path(handle, url, kind) {
        return CacheResult::AlreadyCached(p);
    }
    let bytes = match download_to_memory(url, &NoOpProgressReporter, None, MAX_BYTES).await {
        Ok(b) => b,
        Err(e) => return CacheResult::Failed(e.to_string()),
    };
    let Some(mut ext) = sniff(&bytes) else {
        return CacheResult::Failed("Invalid image".into());
    };
    // An SVG is kept as the pixels it draws.
    let bytes = if ext == "svg" {
        ext = "png";
        match tokio::task::spawn_blocking(move || vector_core::svg::rasterize_png(&bytes, SVG_DIM)).await {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => return CacheResult::Failed(e.to_string()),
            Err(e) => return CacheResult::Failed(e.to_string()),
        }
    } else {
        bytes
    };
    let path = match dir(handle) {
        Ok(d) => d.join(format!("{}.{ext}", key(url))),
        Err(e) => return CacheResult::Failed(e),
    };
    match tokio::fs::write(&path, &bytes).await {
        Ok(()) => CacheResult::Cached(path.to_string_lossy().into_owned()),
        Err(e) => CacheResult::Failed(e.to_string()),
    }
}

pub fn remove_cached_image<R: Runtime>(handle: &AppHandle<R>, url: &str, kind: ImageType) -> Result<(), String> {
    while let Some(p) = get_cached_path(handle, url, kind) {
        std::fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    Ok(())
}
