//! Remote images the UI shows from a local copy: avatars, banners, emojis,
//! inline images. Fetched through the media proxy when one is offered, kept in
//! OPFS under `/cache/<kind>/`, and served by the service worker.
//!
//! When the fetch can't be made (no proxy, and the host sends no CORS headers) the
//! image stays a placeholder: the page never loads a remote image itself, since the
//! browser would draw whatever it got, SVG included, outside the Rust renderer.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageReader};
use vector_core::{db, SlimProfile, STATE};

const MAX_BYTES: usize = 10 * 1024 * 1024;
const MAX_EMOJI_BYTES: usize = 1024 * 1024;
const AVATAR_THUMB: u32 = 160;
const FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

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

    /// Longest side an SVG of this kind is rendered at: its largest display, at up to 3x density.
    fn svg_dim(self) -> u32 {
        match self {
            Kind::Avatar => 512,
            Kind::Banner | Kind::InlineImage => 2048,
            Kind::MiniAppIcon | Kind::Emoji | Kind::EmojiPackIcon => 256,
        }
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

/// A copy stored before SVGs were rendered: rewritten in place as a wrapped render, so the
/// remembered path keeps working, or blanked when it refuses to render. Once per path a session.
async fn defuse(path: &str, kind: Kind) {
    thread_local! {
        static CHECKED: std::cell::RefCell<std::collections::HashSet<String>> = Default::default();
    }
    if !path.ends_with(".svg") || !CHECKED.with(|c| c.borrow_mut().insert(path.to_string())) {
        return;
    }
    let mut paths = vec![path.to_string()];
    if kind == Kind::Avatar {
        paths.push(path.replacen("/cache/avatars/", "/cache/avatars/thumbs/", 1));
    }
    for p in paths {
        let Ok(bytes) = vector_core::webfiles::read(Path::new(&p)).await else { continue };
        if vector_core::svg::is_raster_wrapped(&bytes) {
            continue;
        }
        let out = vector_core::svg::rasterize_wrapped(&bytes, kind.svg_dim()).unwrap_or_else(|_| vector_core::svg::empty_wrapped());
        let _ = vector_core::webfiles::write(Path::new(&p), &out).await;
    }
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

pub(crate) async fn fetch(url: &str, cap: usize) -> Result<Vec<u8>, String> {
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

/// A displayable src for `url`: the local copy's path, or a picture in a chat held in memory
/// while the account doesn't keep media.
pub async fn cache(url: &str, kind: Kind) -> Result<String, String> {
    vector_core::net::validate_url_not_private(url).map_err(str::to_string)?;
    if kind == Kind::InlineImage && !vector_core::media::keep_media() {
        return hold(url).await;
    }
    if let Some(path) = remembered(url, kind) {
        defuse(&path, kind).await;
        return Ok(path);
    }
    while IN_FLIGHT.with(|f| f.borrow().contains(url)) {
        vector_core::rt::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if let Some(path) = remembered(url, kind) {
        defuse(&path, kind).await;
        return Ok(path);
    }
    IN_FLIGHT.with(|f| f.borrow_mut().insert(url.to_string()));
    let result = vector_core::rt::time::timeout(FETCH_BUDGET, fetch_and_store(url, kind))
        .await
        .unwrap_or_else(|_| Err("timed out".into()));
    IN_FLIGHT.with(|f| f.borrow_mut().remove(url));
    let path = result?;
    remember(url, kind, &path);
    Ok(path)
}

/// Fetch a picture into memory under its link: what a chat shows follows the attachment rules.
async fn hold(url: &str) -> Result<String, String> {
    let key = vector_core::media::url_key("inline-image", url);
    if vector_core::media::ram::cache().contains(&key) {
        return Ok(vector_core::media::MediaRef::held(&key));
    }
    let bytes = vector_core::rt::time::timeout(FETCH_BUDGET, fetch(url, MAX_BYTES))
        .await
        .unwrap_or_else(|_| Err("timed out".into()))?;
    let bytes = if vector_core::svg::looks_like_svg(&bytes) { vector_core::svg::rasterize_png(&bytes, Kind::InlineImage.svg_dim())? } else { bytes };
    let mime = vector_core::crypto::mime_from_magic_bytes(&bytes);
    extension_for(mime).ok_or("Not an image")?;
    vector_core::media::ram::insert(key, bytes, mime).map_err(|_| "Too much media is open to show this one".to_string())?;
    Ok(vector_core::media::MediaRef::held(&key))
}

async fn fetch_and_store(url: &str, kind: Kind) -> Result<String, String> {
    let cap = if matches!(kind, Kind::Emoji | Kind::EmojiPackIcon) { MAX_EMOJI_BYTES } else { MAX_BYTES };
    let bytes = fetch(url, cap).await?;
    store(url, kind, &bytes).await
}

async fn store(url: &str, kind: Kind, bytes: &[u8]) -> Result<String, String> {
    // An SVG is kept as the pixels it draws: its markup never reaches the page.
    let rendered;
    let bytes = if vector_core::svg::looks_like_svg(bytes) {
        rendered = vector_core::svg::rasterize_png(bytes, kind.svg_dim())?;
        &rendered[..]
    } else {
        bytes
    };
    let ext = extension_for(vector_core::crypto::mime_from_magic_bytes(bytes)).ok_or("Not an image")?;
    let key = &vector_core::crypto::sha256_hex(url.as_bytes())[..16];
    let path = format!("/cache/{}/{key}.{ext}", kind.dir());
    vector_core::webfiles::write(Path::new(&path), bytes).await?;
    if kind == Kind::Avatar {
        let small = if ext == "gif" { None } else { thumb(bytes) };
        let thumb_path = format!("/cache/avatars/thumbs/{key}.{ext}");
        vector_core::webfiles::write(Path::new(&thumb_path), small.as_deref().unwrap_or(bytes)).await?;
    }
    Ok(path)
}

/// Keep bytes we already hold as `url`'s local copy, so it shows without a fetch.
pub async fn precache(url: &str, kind: Kind, bytes: &[u8]) {
    match store(url, kind, bytes).await {
        Ok(path) => remember(url, kind, &path),
        Err(e) => vector_core::log_debug!("[Images] precache {url}: {e}"),
    }
}

/// Drop every local copy and the index pointing at them.
pub async fn clear_all() {
    KNOWN.with(|k| k.borrow_mut().clear());
    if let Some(dir) = app_data() {
        let _ = db::webfs::remove_tree(&dir, Path::new("/image-index"));
    }
    vector_core::webfiles::remove(Path::new("/cache")).await;
}

/// Forget the link pictures kept for chats: their files and the index that hands them out.
pub async fn forget_inline() {
    KNOWN.with(|k| k.borrow_mut().retain(|(_, kind), _| *kind != Kind::InlineImage as u8));
    if let Some(dir) = app_data() {
        let _ = db::webfs::remove_tree(&dir, Path::new("/image-index/inline_images"));
    }
    vector_core::webfiles::remove(Path::new("/cache/inline_images")).await;
}

/// Delete blocked profiles' pictures: each file, an avatar's thumb, and the index entry that
/// would hand the path out again.
pub async fn forget(images: Vec<vector_core::profile::CachedImage>) {
    for img in images {
        let kind = if img.banner { Kind::Banner } else { Kind::Avatar };
        if !img.path.starts_with(&format!("/cache/{}/", kind.dir())) {
            continue;
        }
        KNOWN.with(|k| k.borrow_mut().remove(&(img.url.clone(), kind as u8)));
        if let Some(dir) = app_data() {
            let _ = db::webfs::remove(&dir, &index_path(&img.url, kind));
        }
        vector_core::webfiles::remove(Path::new(&img.path)).await;
        if kind == Kind::Avatar {
            let thumb = img.path.replacen("/cache/avatars/", "/cache/avatars/thumbs/", 1);
            vector_core::webfiles::remove(Path::new(&thumb)).await;
        }
    }
}

/// Blocked, or waiting on us to accept their DM request: their pictures aren't fetched.
async fn withheld(npub: &str) -> bool {
    let state = STATE.lock().await;
    state.get_profile(npub).is_some_and(|p| p.flags.is_blocked()) || state.profile_held(npub)
}

/// Cache a profile's pictures and repaint it when either landed.
pub async fn cache_profile_images(npub: &str, avatar_url: &str, banner_url: &str) {
    if withheld(npub).await {
        return;
    }
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
            drop(state);
            let lost = [(avatar_url, avatar, false), (banner_url, banner, true)]
                .into_iter()
                .filter_map(|(url, path, banner)| Some(vector_core::profile::CachedImage { url: url.to_string(), path: path?, banner }))
                .collect();
            forget(lost).await;
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

/// Every profile whose pictures have no local copy yet, e.g. after the first sync.
pub async fn cache_all_profile_images() {
    let pending: Vec<(String, String, String)> = {
        let state = STATE.lock().await;
        state
            .profiles
            .iter()
            .filter(|p| (!p.avatar().is_empty() && !p.has_avatar_cached()) || (!p.banner().is_empty() && !p.has_banner_cached()))
            .filter_map(|p| {
                let npub = state.interner.resolve(p.id)?.to_string();
                Some((npub, p.avatar().to_string(), p.banner().to_string()))
            })
            .collect()
    };
    for (npub, avatar, banner) in pending {
        cache_profile_images(&npub, &avatar, &banner).await;
    }
}
