//! Profile pictures kept on disk, so the UI only ever draws local files.

use std::path::PathBuf;
use std::sync::LazyLock;

use vector_core::{db, SlimProfile, STATE};

const MAX_BYTES: usize = 10 * 1024 * 1024;
static DOWNLOADS: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(4));

#[derive(Clone, Copy)]
enum Kind {
    Avatar,
    Banner,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Avatar => "avatars",
            Kind::Banner => "banners",
        }
    }

    fn svg_dim(self) -> u32 {
        match self {
            Kind::Avatar => 512,
            Kind::Banner => 2048,
        }
    }
}

fn cache_dir(kind: Kind) -> Option<PathBuf> {
    let dir = db::get_app_data_dir().ok()?.join("cache").join(kind.dir());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn cache_key(url: &str) -> String {
    vector_core::crypto::sha256_hex(url.as_bytes())[..32].to_string()
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    const SIGS: &[(&[u8], &str)] = &[
        (&[0x89, b'P', b'N', b'G'], "png"),
        (&[0xFF, 0xD8, 0xFF], "jpg"),
        (b"GIF8", "gif"),
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

fn cached(url: &str, kind: Kind) -> Option<String> {
    let dir = cache_dir(kind)?;
    let key = cache_key(url);
    ["jpg", "png", "webp", "gif"].iter().map(|ext| dir.join(format!("{key}.{ext}"))).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let client = vector_core::net::shared_http_client();
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, url).await.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if out.len() + chunk.len() > MAX_BYTES {
            return Err("Image too large".into());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn cache(url: &str, kind: Kind) -> Result<String, String> {
    if let Some(path) = cached(url, kind) {
        return Ok(path);
    }
    let _permit = DOWNLOADS.acquire().await.map_err(|e| e.to_string())?;
    vector_core::net::validate_url_not_private(url)?;
    let bytes = fetch(url).await?;
    let mut ext = sniff(&bytes).ok_or("Invalid image")?;
    // An SVG is kept as the pixels it draws.
    let bytes = if ext == "svg" {
        ext = "png";
        let dim = kind.svg_dim();
        tokio::task::spawn_blocking(move || vector_core::svg::rasterize_png(&bytes, dim)).await.map_err(|e| e.to_string())??
    } else {
        bytes
    };
    let path = cache_dir(kind).ok_or("No cache directory")?.join(format!("{}.{ext}", cache_key(url)));
    tokio::fs::write(&path, &bytes).await.map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

pub async fn cache_profile_images(npub: &str, avatar_url: &str, banner_url: &str) {
    let avatar = if avatar_url.is_empty() { None } else { cache(avatar_url, Kind::Avatar).await.ok() };
    let banner = if banner_url.is_empty() { None } else { cache(banner_url, Kind::Banner).await.ok() };
    if avatar.is_none() && banner.is_none() {
        return;
    }
    let slim: Option<SlimProfile> = {
        let mut state = STATE.lock().await;
        let Some(id) = state.interner.lookup(npub) else { return };
        // Blocked while the download ran: nothing is kept.
        if state.get_profile_by_id(id).is_some_and(|p| p.flags.is_blocked()) {
            return;
        }
        let changed = state.get_profile_mut_by_id(id).is_some_and(|p| {
            let mut changed = false;
            if let Some(a) = &avatar {
                if *p.avatar_cached() != **a {
                    p.set_avatar_cached(a);
                    changed = true;
                }
            }
            if let Some(b) = &banner {
                if *p.banner_cached() != **b {
                    p.set_banner_cached(b);
                    changed = true;
                }
            }
            changed
        });
        if changed { state.serialize_profile(id) } else { None }
    };
    if let Some(slim) = slim {
        vector_core::emit_event("profile_update", &slim);
        let _ = db::profiles::set_profile(&slim);
    }
}

/// Every profile whose pictures have no local copy yet.
pub async fn cache_all_profile_images() {
    let pending: Vec<(String, String, String)> = {
        let state = STATE.lock().await;
        state
            .profiles
            .iter()
            .filter(|p| (!p.avatar().is_empty() && !p.has_avatar_cached()) || (!p.banner().is_empty() && !p.has_banner_cached()))
            .filter_map(|p| Some((state.interner.resolve(p.id)?.to_string(), p.avatar().to_string(), p.banner().to_string())))
            .collect()
    };
    for (npub, avatar, banner) in pending {
        cache_profile_images(&npub, &avatar, &banner).await;
    }
}

/// Keep bytes we already hold (our own upload) under `url`, so it shows without a download.
pub fn precache(url: &str, bytes: &[u8], banner: bool) {
    let kind = if banner { Kind::Banner } else { Kind::Avatar };
    if cached(url, kind).is_some() {
        return;
    }
    let (Some(ext), Some(dir)) = (sniff(bytes), cache_dir(kind)) else { return };
    let rendered;
    let (bytes, ext) = if ext == "svg" {
        let Ok(png) = vector_core::svg::rasterize_png(bytes, kind.svg_dim()) else { return };
        rendered = png;
        (&rendered[..], "png")
    } else {
        (bytes, ext)
    };
    let _ = std::fs::write(dir.join(format!("{}.{ext}", cache_key(url))), bytes);
}
