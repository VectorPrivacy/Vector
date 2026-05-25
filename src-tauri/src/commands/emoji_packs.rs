//! Custom emoji pack commands (NIP-30 / NIP-51).
//!
//! Phase 1 surfaces a read-only API: load the locally-cached packs
//! for fast startup, optionally refresh from relays in the background.

use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

use image::AnimationDecoder;
use image::ImageEncoder;
use serde::{Deserialize, Serialize};

use vector_core::emoji_packs::{self, EmojiPack};
use tauri::Manager;

/// Return every locally-cached emoji pack. Frontend hits this on
/// picker open so the sidebar renders instantly off the local mirror;
/// a background refresh keeps it current.
#[tauri::command]
pub async fn list_emoji_packs() -> Result<Vec<EmojiPack>, String> {
    emoji_packs::load_all_packs()
}

/// Re-fetch the user's kind 10030 list and every referenced pack,
/// updating the local mirror in place. Returns the freshly hydrated
/// pack list so the frontend can swap atomically.
#[tauri::command]
pub async fn refresh_emoji_packs() -> Result<Vec<EmojiPack>, String> {
    emoji_packs::refresh_subscribed_packs().await
}

/// Preview-only fetch of a pack from its NIP-19 `naddr`. Does NOT
/// persist or subscribe — frontend uses this to render the in-chat
/// preview card before the user commits.
#[tauri::command]
pub async fn fetch_emoji_pack_by_naddr(naddr: String) -> Result<EmojiPack, String> {
    emoji_packs::fetch_pack_by_naddr(&naddr).await
}


/// Subscribe to a pack by `naddr`: fetch + persist + add to local
/// subscription list, then debounce-publish kind 10030.
#[tauri::command]
pub async fn subscribe_emoji_pack(naddr: String) -> Result<EmojiPack, String> {
    emoji_packs::subscribe_pack(&naddr).await
}

/// Remove a pack from the local subscription list and republish kind
/// 10030. Pack data stays cached so existing reactions still resolve.
#[tauri::command]
pub async fn unsubscribe_emoji_pack(id: String) -> Result<(), String> {
    emoji_packs::unsubscribe_pack(&id).await
}

/// Register the active theme pack's emoji (shortcode + url) with the send
/// resolver so its shortcodes get NIP-30 tags even though it isn't a real
/// subscription. Pass an empty list to clear (e.g. on a theme with no pack,
/// or when the user is genuinely subscribed and the DB already covers it).
#[tauri::command]
pub fn set_theme_emoji_pack(emojis: Vec<emoji_packs::PackEmoji>) -> Result<(), String> {
    emoji_packs::set_theme_emoji_tags(
        emojis.into_iter().map(|e| (e.shortcode, e.url)).collect(),
    );
    Ok(())
}

// ============================================================================
// Pack creator (own packs)
// ============================================================================

/// Max bytes per uploaded emoji image — picker.js mirrors this for the
/// pre-upload gate but we enforce server-side too so a tampered frontend
/// can't push oversized blobs onto user's Blossom servers.
const MAX_EMOJI_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
pub struct EmojiPackEmojiInput {
    pub shortcode: String,
    pub url: String,
}

#[derive(Deserialize)]
pub struct EmojiPackCreateInput {
    /// Optional — when empty, the backend generates a fresh identifier.
    /// When set, an existing pack's `d` tag is reused (update path).
    pub identifier: Option<String>,
    pub title: String,
    pub image_url: Option<String>,
    pub description: Option<String>,
    pub emojis: Vec<EmojiPackEmojiInput>,
}

fn generate_pack_identifier() -> String {
    // 12 url-safe chars from the system PRNG — fits comfortably in a
    // NIP-19 naddr and is short enough to scan in logs.
    use rand::{Rng, thread_rng, distributions::Alphanumeric};
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect()
}

/// Publish (or replace) one of the user's own packs as a kind 30030
/// event. If `identifier` is omitted, a fresh one is generated; otherwise
/// the existing pack is overwritten (Nostr replaceable-event semantics).
#[tauri::command]
pub async fn emoji_pack_create(
    input: EmojiPackCreateInput,
) -> Result<emoji_packs::EmojiPack, String> {
    // Entry-level guard: catches a swap that landed between IPC dispatch
    // and command execution. publish_pack re-checks before persisting.
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("Account swap in progress.".to_string());
    }

    let identifier = input.identifier
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(generate_pack_identifier);

    let emojis: Vec<emoji_packs::PackEmoji> = input.emojis.into_iter()
        .filter(|e| !e.shortcode.trim().is_empty() && !e.url.trim().is_empty())
        .map(|e| emoji_packs::PackEmoji {
            shortcode: e.shortcode.trim().to_string(),
            url: e.url.trim().to_string(),
            sha256: None,
        })
        .collect();

    if emojis.is_empty() {
        return Err("A pack needs at least one emoji.".to_string());
    }

    // `pubkey` / `id` get overwritten by `publish_pack` based on the
    // active session; we just need any valid shape here.
    let pack = emoji_packs::EmojiPack {
        id: String::new(),
        pubkey: String::new(),
        identifier,
        title: input.title.trim().to_string(),
        image_url: input.image_url.unwrap_or_default().trim().to_string(),
        description: input.description.unwrap_or_default().trim().to_string(),
        emojis,
        is_own: true,
        updated_at: 0,
    };

    emoji_packs::publish_pack(&pack).await
}

/// Tombstone one of the user's own packs (publishes an empty kind 30030
/// + drops local state + republishes 10030). `id` is the naddr.
#[tauri::command]
pub async fn emoji_pack_delete(id: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("Account swap in progress.".to_string());
    }
    emoji_packs::delete_own_pack(&id).await
}

/// Delete a single Blossom blob by its URL. Frontend calls this once
/// per emoji during pack deletion to surface per-emoji progress before
/// the Nostr-level `emoji_pack_delete` runs the tombstone publish.
#[tauri::command]
pub async fn emoji_pack_delete_blob(url: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("Account swap in progress.".to_string());
    }
    let client = vector_core::state::nostr_client()
        .ok_or_else(|| "Nostr client not initialised".to_string())?;
    let signer = client.signer().await
        .map_err(|e| format!("Failed to get signer: {}", e))?;
    vector_core::blossom::delete_blob_by_url(signer, &url).await
}

/// Upload an emoji/pack image to one of the user's Blossom servers and
/// return the resulting URL. Frontend pipes file bytes through here so
/// the credential never leaves Rust — the signer is the active Nostr
/// client's signer (handles both local nsec and remote NIP-46 bunker).
// `kind` is 'emoji' (default) or 'emoji_pack_icon' — chooses the local
// cache subdir so pre-cached bytes land where the frontend's
// `bindCachedEmojiImg` looks them up.
#[tauri::command]
pub async fn emoji_pack_upload_image<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    bytes: Vec<u8>,
    mime: String,
    kind: Option<String>,
) -> Result<String, String> {
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("Account swap in progress.".to_string());
    }
    if bytes.len() > MAX_EMOJI_BYTES {
        return Err(format!(
            "File is {} KB, max is {} KB.",
            bytes.len() / 1024,
            MAX_EMOJI_BYTES / 1024,
        ));
    }
    if bytes.is_empty() {
        return Err("File is empty.".to_string());
    }

    let client = vector_core::state::nostr_client()
        .ok_or_else(|| "Nostr client not initialised".to_string())?;
    let signer = client.signer().await
        .map_err(|e| format!("Failed to get signer: {}", e))?;

    let servers = vector_core::blossom_servers::compute_enabled_servers();
    if servers.is_empty() {
        return Err("No Blossom servers configured.".to_string());
    }

    let mime_ref = if mime.is_empty() { "application/octet-stream" } else { mime.as_str() };
    // Wrap once + clone the Arc — upload moves ownership of the inner
    // Vec onto its task, we keep a reference for the post-upload
    // pre-cache write.
    let bytes_arc = std::sync::Arc::new(bytes);
    let upload_bytes = bytes_arc.clone();
    let url = vector_core::blossom::upload_blob_with_failover(
        signer,
        servers,
        upload_bytes,
        Some(mime_ref),
    ).await?;

    // Re-check after the upload — caller will plumb this URL into a pack
    // tied to the original session. Bail loudly if the account changed
    // so the URL never gets stitched into the wrong pack.
    if !session.is_valid() {
        return Err("Account swapped during upload — discard this URL.".to_string());
    }

    // Pre-cache the bytes locally under the URL we just got back from
    // Blossom. Any subsequent render of this URL (in the picker, in a
    // chat preview card, in a freshly-published pack landing in the
    // user's own subscriptions, etc.) will hit the local cache and
    // never need to re-download what we already had in hand.
    let image_type = match kind.as_deref() {
        Some("emoji_pack_icon") => crate::image_cache::ImageType::EmojiPackIcon,
        _ => crate::image_cache::ImageType::Emoji,
    };
    let _ = crate::image_cache::precache_image_bytes(
        &handle, &url, &bytes_arc, image_type,
    );

    Ok(url)
}

// ============================================================================
// Animated-emoji frame decoding (WKWebView lacks WebCodecs/ImageDecoder)
// ============================================================================
//
// The picker's per-section canvas renderer needs decoded frames + per-frame
// durations. WKWebView ships neither ImageDecoder nor WebP-frame access via
// `<img>`, so we decode in Rust (image crate handles animated WebP + GIF +
// APNG out of the box) and serve a single PNG spritesheet per emoji over
// IPC. The frontend creates one Image element per emoji and draws sub-rects.
//
// Spritesheet layout: frames stacked vertically, each cell `frame_size` ×
// `frame_size`, scaled to fit within `frame_size` while preserving aspect
// (letterboxed). Output is base64-encoded PNG so we can ship it through
// Tauri's JSON IPC without a binary protocol; emoji PNGs are tiny so the
// base64 overhead is negligible.

/// Square pixel size for each frame in the output spritesheet — chosen
/// to look crisp at the picker's 28px display target on retina displays.
const EMOJI_FRAME_SIZE: u32 = 56;

#[derive(Serialize, Deserialize, Clone)]
pub struct EmojiSpritesheet {
    pub png_base64: String,
    pub frame_count: u32,
    pub frame_size: u32,
    pub frame_durations_ms: Vec<u32>,
}

/// Internal decode result before base64/IPC encoding — carries the raw PNG so
/// it can be written to the on-disk spritesheet cache without a base64 detour.
struct DecodedSheet {
    png: Vec<u8>,
    frame_count: u32,
    frame_size: u32,
    durations: Vec<u32>,
}

// --- On-disk presized-spritesheet cache --------------------------------------
// One container file per emoji: frames are decoded + resized ONCE, then this
// file is read on every later open and across app launches (the in-memory LRU
// sits on top for the hot path). Layout:
//   ["VSPR"][ver u8][frame_size u32 LE][frame_count u32 LE]
//   [durations: frame_count × u32 LE][png_len u32 LE][png bytes]
// Self-healing: a truncated/garbage file fails validation in
// `deserialize_spritesheet` and is simply treated as a cache miss.
const VSPR_MAGIC: &[u8; 4] = b"VSPR";
const VSPR_VERSION: u8 = 1;

fn serialize_spritesheet(frame_size: u32, durations: &[u32], png: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + durations.len() * 4 + 4 + png.len());
    out.extend_from_slice(VSPR_MAGIC);
    out.push(VSPR_VERSION);
    out.extend_from_slice(&frame_size.to_le_bytes());
    out.extend_from_slice(&(durations.len() as u32).to_le_bytes());
    for d in durations {
        out.extend_from_slice(&d.to_le_bytes());
    }
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(png);
    out
}

fn deserialize_spritesheet(buf: &[u8]) -> Option<EmojiSpritesheet> {
    if buf.len() < 13 || &buf[0..4] != VSPR_MAGIC || buf[4] != VSPR_VERSION {
        return None;
    }
    let frame_size = u32::from_le_bytes(buf[5..9].try_into().ok()?);
    let frame_count = u32::from_le_bytes(buf[9..13].try_into().ok()?);
    let mut off = 13usize;
    let dur_bytes = (frame_count as usize).checked_mul(4)?;
    if buf.len() < off + dur_bytes + 4 {
        return None;
    }
    let mut durations = Vec::with_capacity(frame_count as usize);
    for _ in 0..frame_count {
        durations.push(u32::from_le_bytes(buf[off..off + 4].try_into().ok()?));
        off += 4;
    }
    let png_len = u32::from_le_bytes(buf[off..off + 4].try_into().ok()?) as usize;
    off += 4;
    if buf.len() < off + png_len {
        return None;
    }
    Some(EmojiSpritesheet {
        png_base64: base64_simd::STANDARD.encode_to_string(&buf[off..off + png_len]),
        frame_count,
        frame_size,
        frame_durations_ms: durations,
    })
}

/// On-disk path for an emoji's presized spritesheet (creates the dir as needed).
/// Keyed by sha256(url): Blossom URLs embed the content hash, so a content edit
/// changes the key and auto-reprocesses; non-Blossom URLs use the URL itself as
/// their (best-effort) identity.
fn emoji_spritesheet_path<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    url: &str,
) -> Option<std::path::PathBuf> {
    let dir = handle
        .path()
        .app_data_dir()
        .ok()?
        .join("cache")
        .join("emoji_spritesheets");
    std::fs::create_dir_all(&dir).ok()?;
    let key = vector_core::crypto::sha256_hex(url.as_bytes());
    Some(dir.join(format!("{}.vspr", key)))
}

/// Soft cap on the on-disk spritesheet cache. Presized 56px sheets are small
/// (tens of KB), so this holds thousands of distinct emoji.
const SPRITESHEET_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Size-capped LRU eviction for the spritesheet cache, run after each new
/// write. Evicts oldest-by-mtime files once the dir exceeds the cap, down to
/// 80% (hysteresis so we don't re-prune on every subsequent write). This is
/// what keeps orphans (emoji from edited/removed packs) from accumulating —
/// reference-based pruning isn't viable since theme-pack URLs live only in the
/// frontend, so age-based eviction is both safe and self-maintaining.
fn prune_spritesheet_cache(dir: &std::path::Path) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("vspr") {
            continue;
        }
        if let Ok(m) = e.metadata() {
            total += m.len();
            entries.push((p, m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
        }
    }
    if total <= SPRITESHEET_CACHE_MAX_BYTES {
        return;
    }
    let target = SPRITESHEET_CACHE_MAX_BYTES * 4 / 5;
    entries.sort_by_key(|(_, t, _)| *t); // oldest first
    for (path, _, len) in entries {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Cache decoded spritesheets in-memory by URL. Picker reopens reuse the
/// decode — round-tripping a 50-frame WebP through libwebp + PNG encode
/// is ~50ms; doing that 54× per open is precisely what we're avoiding.
///
/// Bounded LRU: an unbounded HashMap kept the per-emoji PNG bytes
/// (typ. tens of KB each) around forever, so long sessions browsing many
/// packs leaked memory. Cap at MAX_SPRITESHEET_CACHE entries with simple
/// usage-order eviction — newest insertion goes to the back, oldest gets
/// dropped when we exceed the cap.
const MAX_SPRITESHEET_CACHE: usize = 500;

struct SpritesheetCache {
    /// URL → sheet. Insertion order doubles as access order: every `get`
    /// promotes to the back, every miss inserts at the back.
    entries: std::collections::VecDeque<(String, EmojiSpritesheet)>,
}

impl SpritesheetCache {
    fn new() -> Self {
        Self { entries: std::collections::VecDeque::with_capacity(MAX_SPRITESHEET_CACHE) }
    }
    fn get(&mut self, url: &str) -> Option<EmojiSpritesheet> {
        let pos = self.entries.iter().position(|(k, _)| k == url)?;
        let (k, v) = self.entries.remove(pos).unwrap();
        let clone = v.clone();
        self.entries.push_back((k, v));
        Some(clone)
    }
    fn insert(&mut self, url: String, sheet: EmojiSpritesheet) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &url) {
            self.entries.remove(pos);
        }
        if self.entries.len() >= MAX_SPRITESHEET_CACHE {
            self.entries.pop_front();
        }
        self.entries.push_back((url, sheet));
    }
}

fn spritesheet_cache() -> &'static Mutex<SpritesheetCache> {
    static CACHE: OnceLock<Mutex<SpritesheetCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SpritesheetCache::new()))
}

/// Decode an animated emoji URL into a vertically-stacked PNG
/// spritesheet. Idempotent + cached.
///
/// Lookup order:
///   1. In-memory spritesheet cache (decoded PNG bytes ready to ship).
///   2. Local filesystem cache (emojis + emoji_pack_icons subdirs) —
///      pre-populated by `emoji_pack_upload_image` and by every
///      `bindCachedEmojiImg` hit on the same URL. Critical for
///      freshly-uploaded packs: Blossom may not have propagated the
///      URL yet, but we have the bytes in hand.
///   3. HTTP fetch from the URL (the original slow path).
#[tauri::command]
pub async fn decode_animated_emoji<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    url: String,
) -> Result<EmojiSpritesheet, String> {
    if let Some(cached) = spritesheet_cache().lock().unwrap().get(&url) {
        return Ok(cached);
    }

    // Persistent presized-spritesheet cache: a single file read, no decode and
    // no resize. Populated on first decode below and survives app restarts.
    if let Some(path) = emoji_spritesheet_path(&handle, &url) {
        if let Ok(buf) = std::fs::read(&path) {
            if let Some(sheet) = deserialize_spritesheet(&buf) {
                spritesheet_cache().lock().unwrap().insert(url.clone(), sheet.clone());
                return Ok(sheet);
            }
        }
    }

    // Try the local filesystem cache before going to the network. We
    // check both Emoji and EmojiPackIcon subdirs because we don't know
    // ahead of time which subdir the URL was originally cached under
    // (depends on whether the caller bound it as 'emoji' or
    // 'emoji_pack_icon'). The cached file's extension (.webp/.gif/etc.)
    // is the magic-byte-validated format from upload time — synthesise
    // a content-type from it so the sniffer below routes to the right
    // animated decoder.
    let local_hit = crate::image_cache::get_cached_path(
            &handle, &url, crate::image_cache::ImageType::Emoji,
        )
        .or_else(|| crate::image_cache::get_cached_path(
            &handle, &url, crate::image_cache::ImageType::EmojiPackIcon,
        ));
    let local_bytes_with_type = local_hit.and_then(|path| {
        let bytes = std::fs::read(&path).ok()?;
        let ct = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("image/{}", e.to_ascii_lowercase()))
            .unwrap_or_default();
        Some((bytes, ct))
    });

    let (bytes, content_type) = if let Some(t) = local_bytes_with_type {
        t
    } else {
        let client = vector_core::net::build_http_client(std::time::Duration::from_secs(10))?;
        let resp = client.get(&url).send().await
            .map_err(|e| format!("fetch: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let ct = resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let body = resp.bytes().await
            .map_err(|e| format!("read body: {}", e))?
            .to_vec();
        (body, ct)
    };

    let url_for_blocking = url.clone();
    let decoded = tokio::task::spawn_blocking(move || decode_to_spritesheet(&bytes, &content_type, &url_for_blocking))
        .await
        .map_err(|e| format!("decode join: {}", e))??;

    // Persist the presized spritesheet so future opens / launches skip the
    // decode + resize entirely. Best-effort: a write failure just means we
    // decode again next time. Written directly (no temp) — a partial file fails
    // validation on read and is treated as a miss.
    if let Some(path) = emoji_spritesheet_path(&handle, &url) {
        let blob = serialize_spritesheet(decoded.frame_size, &decoded.durations, &decoded.png);
        if std::fs::write(&path, &blob).is_ok() {
            if let Some(dir) = path.parent() {
                prune_spritesheet_cache(dir);
            }
        }
    }

    let sheet = EmojiSpritesheet {
        png_base64: base64_simd::STANDARD.encode_to_string(&decoded.png),
        frame_count: decoded.frame_count,
        frame_size: decoded.frame_size,
        frame_durations_ms: decoded.durations,
    };
    spritesheet_cache().lock().unwrap().insert(url, sheet.clone());
    Ok(sheet)
}

fn decode_to_spritesheet(bytes: &[u8], content_type: &str, url: &str) -> Result<DecodedSheet, String> {
    let inferred = if content_type.contains("webp") || url.ends_with(".webp") {
        "webp"
    } else if content_type.contains("gif") || url.ends_with(".gif") {
        "gif"
    } else if content_type.contains("apng") || url.ends_with(".apng") {
        "apng"
    } else if content_type.contains("png") || url.ends_with(".png") {
        "png"
    } else {
        // Fall back to image's content sniffer for everything else.
        "auto"
    };

    let frames: Vec<(image::RgbaImage, u32)> = match inferred {
        "webp" => decode_webp_frames(bytes)?,
        "gif" => decode_gif_frames(bytes)?,
        "apng" => decode_apng_frames(bytes)?,
        _ => decode_static_fallback(bytes)?,
    };

    if frames.is_empty() {
        return Err("no frames decoded".to_string());
    }

    let frame_size = EMOJI_FRAME_SIZE;
    let cols = 1u32;
    let rows = frames.len() as u32;
    let sheet_w = frame_size * cols;
    let sheet_h = frame_size * rows;

    let mut sheet = image::RgbaImage::new(sheet_w, sheet_h);
    let mut durations = Vec::with_capacity(frames.len());
    for (i, (frame, duration_ms)) in frames.iter().enumerate() {
        durations.push(*duration_ms);
        // Letterbox each frame into a frame_size × frame_size cell so
        // non-square emoji keep their aspect ratio.
        let resized = image::imageops::resize(
            frame,
            frame_size,
            frame_size,
            image::imageops::FilterType::Triangle,
        );
        let dst_y = i as u32 * frame_size;
        image::imageops::overlay(&mut sheet, &resized, 0, dst_y as i64);
    }

    let mut png_bytes = Vec::with_capacity((sheet_w * sheet_h * 4 / 8) as usize);
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png_bytes,
        image::codecs::png::CompressionType::Default,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        sheet.as_raw(),
        sheet_w,
        sheet_h,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| format!("png encode: {}", e))?;

    Ok(DecodedSheet {
        png: png_bytes,
        frame_count: frames.len() as u32,
        frame_size,
        durations,
    })
}

fn decode_webp_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    // Animated WebPs go through libwebp directly. `image-webp` 0.2.x
    // returns per-frame raw pixels without applying the spec's disposal
    // + blend modes, so transparent regions flicker as residue from the
    // prior frame on a cleared canvas. libwebp is the reference decoder
    // and produces fully-composed RGBA frames.
    if let Ok(anim) = webp::AnimDecoder::new(bytes).decode() {
        if anim.has_animation() {
            let mut out = Vec::with_capacity(anim.len());
            let mut prev_ts: i32 = 0;
            for frame in anim.into_iter() {
                let w = frame.width();
                let h = frame.height();
                let layout = frame.get_layout();
                let pixels = frame.get_image();
                let rgba = if layout.is_alpha() {
                    image::RgbaImage::from_raw(w, h, pixels.to_vec())
                        .ok_or_else(|| "webp anim: malformed RGBA frame".to_string())?
                } else {
                    let mut buf = Vec::with_capacity((w * h * 4) as usize);
                    for px in pixels.chunks_exact(3) {
                        buf.extend_from_slice(px);
                        buf.push(255);
                    }
                    image::RgbaImage::from_raw(w, h, buf)
                        .ok_or_else(|| "webp anim: malformed RGB frame".to_string())?
                };
                let ts = frame.get_time_ms();
                // libwebp reports cumulative end-of-frame timestamps;
                // per-frame duration is the delta from the previous one.
                let duration = (ts - prev_ts).max(20) as u32;
                prev_ts = ts;
                out.push((rgba, duration));
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }

    // Still WebP — image-webp handles single-frame decoding correctly.
    let img = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP)
        .decode()
        .map_err(|e| format!("webp still: {}", e))?
        .into_rgba8();
    Ok(vec![(img, 100)])
}

fn decode_gif_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("gif decoder: {}", e))?;
    let frames: Vec<image::Frame> = decoder.into_frames().collect_frames()
        .map_err(|e| format!("gif frames: {}", e))?;
    Ok(frames.into_iter().map(extract_frame).collect())
}

fn decode_apng_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("png decoder: {}", e))?
        .apng()
        .map_err(|e| format!("apng: {}", e))?;
    let frames: Vec<image::Frame> = decoder.into_frames().collect_frames()
        .map_err(|e| format!("apng frames: {}", e))?;
    Ok(frames.into_iter().map(extract_frame).collect())
}

fn decode_static_fallback(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let img = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?
        .decode()
        .map_err(|e| format!("decode: {}", e))?
        .into_rgba8();
    Ok(vec![(img, 100)])
}

fn extract_frame(f: image::Frame) -> (image::RgbaImage, u32) {
    // image::Frame::delay is `Delay` which gives (numer, denom) for ms.
    let (numer, denom) = f.delay().numer_denom_ms();
    let dur = if denom == 0 { 100 } else { (numer / denom).max(20) };
    (f.into_buffer(), dur)
}

// ============================================================================
// Crop + re-encode
// ============================================================================

#[derive(Deserialize)]
pub struct EmojiCropInput {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Crop an image to a square region and re-encode in the same format.
/// Supports PNG, JPEG, static WebP, GIF, and animated WebP. Animated
/// formats decode per-frame, crop each, then re-encode with the
/// original per-frame durations preserved.
///
/// `x`/`y`/`w`/`h` are source-pixel coords. Crop must be square (the
/// frontend enforces 1:1 lock); we sanity-check anyway.
#[tauri::command]
pub async fn emoji_crop_and_reencode(input: EmojiCropInput) -> Result<Vec<u8>, String> {
    if input.w != input.h {
        return Err("crop must be square".to_string());
    }
    if input.w == 0 {
        return Err("crop must be non-empty".to_string());
    }
    if input.bytes.len() > MAX_EMOJI_BYTES * 4 {
        // Source is allowed to be larger than the output cap — we'll
        // shrink during re-encode — but reject ridiculous inputs early.
        return Err("source too large".to_string());
    }

    tokio::task::spawn_blocking(move || crop_and_reencode_blocking(input))
        .await
        .map_err(|e| format!("crop join: {}", e))?
}

fn crop_and_reencode_blocking(input: EmojiCropInput) -> Result<Vec<u8>, String> {
    let EmojiCropInput { bytes, mime, x, y, w, h } = input;
    let mime = mime.to_lowercase();

    let format = if mime.contains("jpeg") || mime.contains("jpg") {
        "jpeg"
    } else if mime.contains("png") {
        "png"
    } else if mime.contains("gif") {
        "gif"
    } else if mime.contains("webp") {
        // Animated WebP routes to the dedicated AnimEncoder round-trip;
        // detect via libwebp itself rather than a header sniff so we
        // catch single-frame-anim edge cases correctly.
        if let Ok(anim) = webp::AnimDecoder::new(&bytes).decode() {
            if anim.has_animation() {
                return crop_animated_webp(&bytes, x, y, w, h);
            }
        }
        "webp"
    } else {
        return Err(format!("unsupported mime: {}", mime));
    };

    if format == "gif" {
        return crop_gif(&bytes, x, y, w, h);
    }

    let dynimg = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?
        .decode()
        .map_err(|e| format!("decode: {}", e))?;
    let (sw, sh) = (dynimg.width(), dynimg.height());
    validate_crop_bounds(x, y, w, h, sw, sh)?;
    let cropped = dynimg.crop_imm(x, y, w, h).to_rgba8();
    encode_static(&cropped, format)
}

fn crop_animated_webp(bytes: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    // Reuse the existing libwebp-based decoder — it already returns
    // fully-composed RGBA frames with per-frame deltas (the spec's
    // disposal + blend logic is handled inside libwebp).
    let frames = decode_webp_frames(bytes)?;
    if frames.is_empty() {
        return Err("webp: no frames".to_string());
    }
    let (fw, fh) = (frames[0].0.width(), frames[0].0.height());
    validate_crop_bounds(x, y, w, h, fw, fh)?;

    // Crop ahead of encoder construction so each AnimFrame's borrow of
    // the pixel buffer outlives `add_frame`.
    let cropped: Vec<(Vec<u8>, u32)> = frames
        .into_iter()
        .map(|(rgba, duration)| {
            let img = image::imageops::crop_imm(&rgba, x, y, w, h).to_image();
            (img.into_raw(), duration.max(20))
        })
        .collect();

    let mut config = webp::WebPConfig::new().map_err(|_| "webp config init failed".to_string())?;
    config.quality = 80.0;
    let mut encoder = webp::AnimEncoder::new(w, h, &config);
    encoder.set_loop_count(0);

    // libwebp wants cumulative end-of-frame timestamps (not per-frame
    // deltas). Decoder gives us deltas, so re-accumulate here.
    let mut cumulative: i32 = 0;
    for (pixels, duration) in &cropped {
        cumulative = cumulative.saturating_add(*duration as i32);
        let frame = webp::AnimFrame::from_rgba(pixels, w, h, cumulative);
        encoder.add_frame(frame);
    }

    let mem = encoder
        .try_encode()
        .map_err(|e| format!("webp anim encode: {:?}", e))?;
    Ok(mem.to_vec())
}

fn crop_gif(bytes: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let frames = decode_gif_frames(bytes)?;
    if frames.is_empty() {
        return Err("gif: no frames".to_string());
    }
    let (fw, fh) = (frames[0].0.width(), frames[0].0.height());
    validate_crop_bounds(x, y, w, h, fw, fh)?;

    let mut out = Vec::new();
    {
        // Speed 10 = fastest encode at lowest CPU. Emoji-scale GIFs are
        // small enough that quality difference vs default is negligible.
        let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .map_err(|e| format!("gif repeat: {}", e))?;
        for (rgba, duration_ms) in frames {
            let cropped = image::imageops::crop_imm(&rgba, x, y, w, h).to_image();
            let delay = image::Delay::from_numer_denom_ms(duration_ms.max(20), 1);
            let frame = image::Frame::from_parts(cropped, 0, 0, delay);
            encoder
                .encode_frame(frame)
                .map_err(|e| format!("gif frame: {}", e))?;
        }
    }
    Ok(out)
}

fn validate_crop_bounds(x: u32, y: u32, w: u32, h: u32, sw: u32, sh: u32) -> Result<(), String> {
    if x.saturating_add(w) > sw || y.saturating_add(h) > sh {
        return Err("crop outside source bounds".to_string());
    }
    Ok(())
}

fn encode_static(rgba: &image::RgbaImage, format: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity((rgba.width() * rgba.height()) as usize);
    match format {
        "png" => {
            image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("png encode: {}", e))?;
        }
        "jpeg" => {
            // JPEG has no alpha — composite onto white before encode so
            // transparent regions don't render as random noise.
            let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
            for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
                let a = src.0[3] as u32;
                let inv = 255 - a;
                dst.0[0] = ((src.0[0] as u32 * a + 255 * inv) / 255) as u8;
                dst.0[1] = ((src.0[1] as u32 * a + 255 * inv) / 255) as u8;
                dst.0[2] = ((src.0[2] as u32 * a + 255 * inv) / 255) as u8;
            }
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
                .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("jpeg encode: {}", e))?;
        }
        "webp" => {
            // Static WebP via libwebp — much smaller than image-webp's
            // lossless-only encoder for natural images.
            let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
            let mem = encoder.encode(88.0);
            out.extend_from_slice(&mem);
        }
        _ => return Err(format!("encode: unsupported format {}", format)),
    }
    Ok(out)
}

