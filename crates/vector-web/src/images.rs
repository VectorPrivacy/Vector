//! Remote images the UI shows from a local copy: avatars, banners, emojis,
//! inline images. Fetched through the media proxy when one is offered, kept in
//! OPFS under `/cache/<kind>/`, and served by the service worker.
//!
//! When the fetch can't be made (no proxy, and the host sends no CORS headers),
//! the remote URL itself is returned: the page's `<img>` loads it directly,
//! which is the same direct fallback desktop takes when no proxy is offered.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageReader};
use vector_core::{db, SlimProfile, STATE};

const MAX_BYTES: usize = 10 * 1024 * 1024;
const MAX_EMOJI_BYTES: usize = 1024 * 1024;
const AVATAR_THUMB: u32 = 160;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Avatar,
    Banner,
    MiniAppIcon,
    Emoji,
    EmojiPackIcon,
    InlineImage,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "avatar" => Kind::Avatar,
            "banner" => Kind::Banner,
            "miniapp_icon" => Kind::MiniAppIcon,
            "emoji" => Kind::Emoji,
            "emoji_pack_icon" => Kind::EmojiPackIcon,
            "inline_image" => Kind::InlineImage,
            _ => return None,
        })
    }

    fn dir(self) -> &'static str {
        match self {
            Kind::Avatar => "avatars",
            Kind::Banner => "banners",
            Kind::MiniAppIcon => "miniapp_icons",
            Kind::Emoji => "emojis",
            Kind::EmojiPackIcon => "emoji_pack_icons",
            Kind::InlineImage => "inline_images",
        }
    }
}

thread_local! {
    static KNOWN: RefCell<HashMap<(String, u8), String>> = RefCell::new(HashMap::new());
    static IN_FLIGHT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

fn index_path(url: &str, kind: Kind) -> PathBuf {
    PathBuf::from(format!("/image-index/{}/{}", kind.dir(), &vector_core::crypto::sha256_hex(url.as_bytes())[..32]))
}

fn app_data() -> Option<PathBuf> {
    db::get_app_data_dir().ok().cloned()
}

fn remembered(url: &str, kind: Kind) -> Option<String> {
    if let Some(p) = KNOWN.with(|k| k.borrow().get(&(url.to_string(), kind as u8)).cloned()) {
        return Some(p);
    }
    let bytes = db::webfs::read(&app_data()?, &index_path(url, kind))?;
    let path = String::from_utf8(bytes).ok()?;
    KNOWN.with(|k| k.borrow_mut().insert((url.to_string(), kind as u8), path.clone()));
    Some(path)
}

fn remember(url: &str, kind: Kind, path: &str) {
    KNOWN.with(|k| k.borrow_mut().insert((url.to_string(), kind as u8), path.to_string()));
    if let Some(dir) = app_data() {
        let _ = db::webfs::write(&dir, &index_path(url, kind), path.as_bytes());
    }
}

fn extension_for(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/x-icon" => "ico",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

async fn fetch(url: &str, cap: usize) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    let client = vector_core::net::shared_http_client();
    let resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, url)
        .await
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut out = Vec::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        if out.len() > cap {
            return Err("Image too large".into());
        }
    }
    Ok(out)
}

fn thumb(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.decode().ok()?;
    let small = if img.width() > AVATAR_THUMB || img.height() > AVATAR_THUMB {
        img.thumbnail(AVATAR_THUMB, AVATAR_THUMB)
    } else {
        img
    };
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(small.to_rgba8())
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// A displayable src for `url`: the local copy's path, or the URL itself.
pub async fn cache(url: &str, kind: Kind) -> Result<String, String> {
    vector_core::net::validate_url_not_private(url).map_err(str::to_string)?;
    if let Some(path) = remembered(url, kind) {
        return Ok(path);
    }
    if !IN_FLIGHT.with(|f| f.borrow_mut().insert(url.to_string())) {
        return Ok(url.to_string());
    }
    let result = fetch_and_store(url, kind).await;
    IN_FLIGHT.with(|f| f.borrow_mut().remove(url));
    match result {
        Ok(path) => {
            remember(url, kind, &path);
            Ok(path)
        }
        Err(e) => {
            vector_core::log_debug!("[Images] {url}: {e}; the page loads it directly");
            Ok(url.to_string())
        }
    }
}

async fn fetch_and_store(url: &str, kind: Kind) -> Result<String, String> {
    let cap = if matches!(kind, Kind::Emoji | Kind::EmojiPackIcon) { MAX_EMOJI_BYTES } else { MAX_BYTES };
    let bytes = fetch(url, cap).await?;
    let ext = extension_for(vector_core::crypto::mime_from_magic_bytes(&bytes)).ok_or("Not an image")?;
    let key = &vector_core::crypto::sha256_hex(url.as_bytes())[..16];
    let path = format!("/cache/{}/{key}.{ext}", kind.dir());
    vector_core::webfiles::write(Path::new(&path), &bytes).await?;
    if kind == Kind::Avatar {
        let small = if ext == "gif" { None } else { thumb(&bytes) };
        let thumb_path = format!("/cache/avatars/thumbs/{key}.{ext}");
        vector_core::webfiles::write(Path::new(&thumb_path), small.as_deref().unwrap_or(&bytes)).await?;
    }
    Ok(path)
}

/// Cache a profile's pictures and repaint it when either landed.
pub async fn cache_profile_images(npub: &str, avatar_url: &str, banner_url: &str) {
    let avatar = if avatar_url.is_empty() { None } else { cache(avatar_url, Kind::Avatar).await.ok() };
    let banner = if banner_url.is_empty() { None } else { cache(banner_url, Kind::Banner).await.ok() };
    if avatar.is_none() && banner.is_none() {
        return;
    }
    let slim: Option<SlimProfile> = {
        let mut state = STATE.lock().await;
        let Some(id) = state.interner.lookup(npub) else { return };
        let changed = state.get_profile_mut_by_id(id).is_some_and(|p| {
            let mut changed = false;
            if let Some(a) = &avatar {
                if *p.avatar_cached != **a {
                    p.avatar_cached = a.clone().into_boxed_str();
                    changed = true;
                }
            }
            if let Some(b) = &banner {
                if *p.banner_cached != **b {
                    p.banner_cached = b.clone().into_boxed_str();
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

/// Every profile whose pictures have no local copy yet, e.g. after the first sync.
pub async fn cache_all_profile_images() {
    let pending: Vec<(String, String, String)> = {
        let state = STATE.lock().await;
        state
            .profiles
            .iter()
            .filter(|p| (!p.avatar.is_empty() && p.avatar_cached.is_empty()) || (!p.banner.is_empty() && p.banner_cached.is_empty()))
            .filter_map(|p| {
                let npub = state.interner.resolve(p.id)?.to_string();
                Some((npub, p.avatar.to_string(), p.banner.to_string()))
            })
            .collect()
    };
    for (npub, avatar, banner) in pending {
        cache_profile_images(&npub, &avatar, &banner).await;
    }
}
