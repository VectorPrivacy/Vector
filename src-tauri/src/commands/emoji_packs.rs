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

/// The shareable form of a pack naddr: same coordinate, plus relay hints
/// naming the pack's own home (the author's NIP-65 write relays).
#[tauri::command]
pub async fn get_pack_share_naddr(naddr: String) -> Result<String, String> {
    emoji_packs::share_naddr(&naddr).await
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

/// Record one use of an emoji (stock unicode or custom pack emoji) into the
/// active account's frecency store. Fire-and-forget from the frontend on every
/// emoji selection. `kind` is "unicode" | "custom"; `url` is the pack image for
/// customs. Fully synchronous (load+save), so it operates on whatever the
/// current account is — no captured-npub staleness to guard.
#[tauri::command]
pub async fn bump_emoji_usage(kind: String, id: String, url: Option<String>) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(()); // no account → silently skip (fire-and-forget)
    }
    vector_core::emoji_usage::bump(&kind, &id, url.as_deref())
}

/// Record every distinct emoji of one sent message in a single load+save.
/// Called once per message (not per emoji) so a 10-emoji message is one IPC.
#[tauri::command]
pub async fn bump_emoji_usage_batch(entries: Vec<vector_core::emoji_usage::EmojiUse>) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(());
    }
    vector_core::emoji_usage::bump_batch(&entries)
}

/// Ranked emoji usage (highest decayed-frecency first) for the active account,
/// powering the picker's "recently used" row. `limit` caps the returned set.
#[tauri::command]
pub async fn get_emoji_usage(limit: Option<usize>) -> Result<Vec<vector_core::emoji_usage::EmojiUsageEntry>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(Vec::new());
    }
    Ok(vector_core::emoji_usage::ranked(limit))
}

/// Resolve the active theme's pinned pack cache-first: returns the persisted
/// copy instantly (refreshing in the background) or fetches + persists on a
/// cache miss. Returns null if uncached and not found on relays.
#[tauri::command]
pub async fn get_theme_emoji_pack(naddr: String) -> Result<Option<EmojiPack>, String> {
    emoji_packs::get_or_fetch_theme_pack(&naddr).await
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

/// Persist a user-defined display order for the equipped packs and republish
/// kind 10030 so it syncs across devices. `ordered_ids` is the full display
/// order: pack `id` naddrs for real packs, the literal "theme_slot" for the
/// theme marker.
#[tauri::command]
pub async fn reorder_emoji_packs(ordered_ids: Vec<String>) -> Result<(), String> {
    emoji_packs::reorder_emoji_packs(ordered_ids)
}

/// Return the theme-slot anchor as a naddr (the pack the theme slot renders
/// immediately after), or "" when the slot is at the top.
#[tauri::command]
pub async fn get_theme_slot_anchor() -> Result<String, String> {
    emoji_packs::get_theme_slot_anchor()
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
// Downscale floor when shrinking an oversized crop to fit the byte cap — an
// emoji this small is still legible, and nothing legible fails to fit under it.
const MIN_EMOJI_DIM: u32 = 48;

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
    vector_core::db::scoped(async move {
        // Entry-level guard: catches a swap that landed between IPC dispatch
        // and command execution. publish_pack re-checks before persisting.

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
            status: emoji_packs::PACK_STATUS_ACTIVE,
        };

        emoji_packs::publish_pack(&pack).await
    })
    .await
}

/// Tombstone one of the user's own packs (publishes an empty kind 30030
/// + drops local state + republishes 10030). `id` is the naddr.
#[tauri::command]
pub async fn emoji_pack_delete(id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        emoji_packs::delete_own_pack(&id).await
    })
    .await
}

/// Delete a single Blossom blob by its URL. Frontend calls this once
/// per emoji during pack deletion to surface per-emoji progress before
/// the Nostr-level `emoji_pack_delete` runs the tombstone publish.
#[tauri::command]
pub async fn emoji_pack_delete_blob(url: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let session = vector_core::db::current_session();
        if !session.is_live() {
            return Err("Account swap in progress.".to_string());
        }
        let _client = vector_core::state::nostr_client()
            .ok_or_else(|| "Nostr client not initialised".to_string())?;
        let signer = vector_core::signer::active_signer()
            .map_err(|e| format!("Failed to get signer: {}", e))?;
        vector_core::blossom::delete_blob_by_url(signer, &url).await
    })
    .await
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
    bytes_b64: String,
    kind: Option<String>,
) -> Result<String, String> {
    vector_core::db::scoped(async move {
        // JSON base64 in: an async command doesn't reliably receive a raw ipc::Request
        // body on Android's WebView (only sync commands do), so bytes ride a base64 arg.
        let bytes = base64_simd::STANDARD
            .decode_to_vec(&bytes_b64)
            .map_err(|e| format!("bytes base64: {}", e))?;

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

        let _client = vector_core::state::nostr_client()
            .ok_or_else(|| "Nostr client not initialised".to_string())?;
        let signer = vector_core::signer::active_signer()
            .map_err(|e| format!("Failed to get signer: {}", e))?;

        let servers = vector_core::blossom_servers::compute_enabled_servers();
        if servers.is_empty() {
            return Err("No Blossom servers configured.".to_string());
        }

        // Strip metadata (+ resize/cap) before upload: static emojis are re-encoded
        // (dropping any EXIF), animated emotes (GIF / animated WebP / APNG) pass
        // through to keep their animation. The processed output drives the mime.
        let prepared = crate::shared::image::prepare_upload_image(
            &bytes,
            crate::shared::image::UploadImageKind::Emoji,
        )?;
        let mime_ref = crate::shared::image::upload_mime_for(prepared.extension);
        // Wrap once + clone the Arc — upload moves ownership of the inner
        // Vec onto its task, we keep a reference for the post-upload
        // pre-cache write.
        let bytes_arc = std::sync::Arc::new(prepared.bytes);
        let upload_bytes = bytes_arc.clone();
        let url = vector_core::blossom::upload_blob_with_failover(
            signer,
            servers,
            upload_bytes,
            Some(mime_ref),
            // Emojis are tiny (<=1MB) and should upload near-instantly: treat a server silent
            // for 10s as dead and fail over, instead of waiting out the full request timeout on
            // a broken Blossom host. Larger uploads (attachments, profiles) keep more leeway.
            Some(std::time::Duration::from_secs(10)),
        ).await?;

        // Re-check after the upload — caller will plumb this URL into a pack
        // tied to the original session. Bail loudly if the account changed
        // so the URL never gets stitched into the wrong pack.

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

        // Warm the presized-spritesheet cache the picker canvas reads for EVERY pack
        // emoji (static + animated), so first view of a freshly-saved pack is a
        // single file read: no fetch, no decode. Icons render as plain <img> and
        // skip this. Detached + best-effort — the bytes are already in hand.
        if !matches!(kind.as_deref(), Some("emoji_pack_icon")) {
            if let Some(dir) = sheet_cache_dir(&handle) {
                if read_sheet(&dir, &url).is_none() {
                    let warm_bytes = bytes_arc.clone();
                    let warm_ct = mime_ref.to_string();
                    let warm_url = url.clone();
                    // spawn-detached: global sheet cache maintenance on bytes in hand, no account state
                    tokio::task::spawn_blocking(move || {
                        if let Ok(decoded) = decode_to_spritesheet(&warm_bytes[..], &warm_ct, &warm_url) {
                            if write_sheet(&dir, &warm_url, &decoded).is_ok() {
                                prune_spritesheet_cache(&dir);
                            }
                        }
                    });
                }
            }
        }

        Ok(url)
    })
    .await
}

// ============================================================================
// Animated-emoji frame decoding (WKWebView lacks WebCodecs/ImageDecoder)
// ============================================================================
//
// The picker's per-section canvas renderer needs decoded frames + per-frame
// durations. WKWebView ships neither ImageDecoder nor WebP-frame access via
// `<img>`, so we decode in Rust (image crate handles animated WebP + GIF +
// APNG out of the box) into a presized PNG strip on disk. The webview loads the
// file (or the pack atlas it sits in) through the asset route and draws sub-rects;
// only a small descriptor crosses IPC.
//
// Spritesheet layout: frames stacked vertically, each cell `frame_size` ×
// `frame_size`, scaled to fit within `frame_size` while preserving aspect
// (letterboxed).

/// Square pixel size for each frame in the output spritesheet — chosen
/// to look crisp at the picker's 28px display target on retina displays.
const EMOJI_FRAME_SIZE: u32 = 56;

#[derive(Serialize, Deserialize, Clone)]
pub struct EmojiSheet {
    /// The PNG on disk holding this emoji's frames; the webview loads it through the
    /// asset route, so no pixel ever crosses IPC. Either the emoji's own sheet or a
    /// pack atlas shared with its neighbours, in which case `x`/`y` locate the strip.
    pub path: String,
    pub x: u32,
    pub y: u32,
    pub frame_count: u32,
    pub frame_size: u32,
    pub frame_durations_ms: Vec<u32>,
}

/// The sidecar next to each sheet PNG: everything the canvas needs besides the pixels.
#[derive(Serialize, Deserialize, Clone)]
struct SheetMeta {
    frame_count: u32,
    frame_size: u32,
    frame_durations_ms: Vec<u32>,
}

/// A decoded, presized sheet before it is written to the cache.
struct DecodedSheet {
    png: Vec<u8>,
    frame_count: u32,
    frame_size: u32,
    durations: Vec<u32>,
}

// --- On-disk presized-spritesheet cache --------------------------------------
// Two files per emoji, keyed by sha256(url): `<key>.png` (the vertically stacked
// frames, a plain PNG so the asset route can serve it) and `<key>.json` (frame
// metadata). Frames are decoded + resized ONCE; every later open, on every
// launch, is a stat and a small JSON read. The previous single-container
// format (`<key>.vspr`, "VSPR" magic) is converted on first read.
const VSPR_MAGIC: &[u8; 4] = b"VSPR";
const VSPR_VERSION: u8 = 1;
static SHEET_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A tmp path no concurrent writer of the same file can share.
fn sheet_tmp_path(target: &std::path::Path) -> std::path::PathBuf {
    let seq = SHEET_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    target.with_extension(format!("png.tmp-{}-{seq}", std::process::id()))
}

/// A cache hit is a use: bump the file's mtime so the age-ordered sweep keeps a pack
/// that is opened every day ahead of one that was added and forgotten.
fn touch(path: &std::path::Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

fn sheet_cache_dir<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> Option<std::path::PathBuf> {
    let dir = handle.path().app_data_dir().ok()?.join("cache").join("emoji_spritesheets");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Blossom URLs embed the content hash, so a content edit changes the key and
/// auto-reprocesses; other URLs use the URL itself as their identity.
fn sheet_key(url: &str) -> String {
    vector_core::crypto::sha256_hex(url.as_bytes())
}

fn sheet_files(dir: &std::path::Path, url: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let key = sheet_key(url);
    (dir.join(format!("{key}.png")), dir.join(format!("{key}.json")))
}

/// Persist a decoded sheet: PNG via tmp+rename, then the sidecar. The sidecar is
/// written last, so its presence means the PNG is complete. Callers sweep the dir
/// once per batch (`prune_spritesheet_cache`), not per file.
fn write_sheet(dir: &std::path::Path, url: &str, decoded: &DecodedSheet) -> Result<EmojiSheet, String> {
    let (png, json) = sheet_files(dir, url);
    let tmp = sheet_tmp_path(&png);
    std::fs::write(&tmp, &decoded.png).map_err(|e| format!("sheet write: {e}"))?;
    std::fs::rename(&tmp, &png).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("sheet place: {e}") })?;
    let meta = SheetMeta { frame_count: decoded.frame_count, frame_size: decoded.frame_size, frame_durations_ms: decoded.durations.clone() };
    let body = serde_json::to_vec(&meta).map_err(|e| e.to_string())?;
    std::fs::write(&json, body).map_err(|e| format!("sheet meta write: {e}"))?;
    Ok(EmojiSheet { path: png.to_string_lossy().into_owned(), x: 0, y: 0, frame_count: meta.frame_count, frame_size: meta.frame_size, frame_durations_ms: meta.frame_durations_ms })
}

/// A sheet already on disk, or None. Never touches the network.
fn read_sheet(dir: &std::path::Path, url: &str) -> Option<EmojiSheet> {
    let (png, json) = sheet_files(dir, url);
    if let Ok(buf) = std::fs::read(&json) {
        if png.is_file() {
            if let Ok(meta) = serde_json::from_slice::<SheetMeta>(&buf) {
                touch(&png);
                return Some(EmojiSheet { path: png.to_string_lossy().into_owned(), x: 0, y: 0, frame_count: meta.frame_count, frame_size: meta.frame_size, frame_durations_ms: meta.frame_durations_ms });
            }
        }
    }
    // Legacy container: split it into the two-file layout, and retire it only once
    // the new layout is on disk.
    let legacy = dir.join(format!("{}.vspr", sheet_key(url)));
    let buf = std::fs::read(&legacy).ok()?;
    let Some(decoded) = deserialize_legacy_sheet(&buf) else {
        let _ = std::fs::remove_file(&legacy);
        return None;
    };
    let sheet = write_sheet(dir, url, &decoded).ok()?;
    let _ = std::fs::remove_file(&legacy);
    Some(sheet)
}

/// A remembered descriptor is only good while its file is: the sweep may have taken it.
fn cached_sheet(url: &str) -> Option<EmojiSheet> {
    let hit = spritesheet_cache().lock().unwrap().get(url)?;
    if std::path::Path::new(&hit.path).is_file() {
        return Some(hit);
    }
    spritesheet_cache().lock().unwrap().remove(url);
    None
}

/// The retired single-file layout:
///   ["VSPR"][ver u8][frame_size u32 LE][frame_count u32 LE]
///   [durations: frame_count × u32 LE][png_len u32 LE][png bytes]
/// A truncated or garbage file is a cache miss, never a panic.
fn deserialize_legacy_sheet(buf: &[u8]) -> Option<DecodedSheet> {
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
    Some(DecodedSheet { png: buf[off..off + png_len].to_vec(), frame_count, frame_size, durations })
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
        if !matches!(p.extension().and_then(|x| x.to_str()), Some("png") | Some("vspr")) {
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
            let _ = std::fs::remove_file(path.with_extension("json"));
            total = total.saturating_sub(len);
        }
    }
}

/// URL → sheet descriptor (path + metadata, no pixels), so a picker reopen is a
/// map lookup rather than a stat and a JSON read. Bounded so long sessions
/// browsing many packs stay flat.
const MAX_SPRITESHEET_CACHE: usize = 2000;

struct SpritesheetCache {
    /// URL → sheet. Insertion order doubles as access order: every `get`
    /// promotes to the back, every miss inserts at the back.
    entries: std::collections::VecDeque<(String, EmojiSheet)>,
}

impl SpritesheetCache {
    fn new() -> Self {
        Self { entries: std::collections::VecDeque::with_capacity(MAX_SPRITESHEET_CACHE) }
    }
    fn get(&mut self, url: &str) -> Option<EmojiSheet> {
        let pos = self.entries.iter().position(|(k, _)| k == url)?;
        let (k, v) = self.entries.remove(pos).unwrap();
        let clone = v.clone();
        self.entries.push_back((k, v));
        Some(clone)
    }
    fn remove(&mut self, url: &str) {
        self.entries.retain(|(u, _)| u != url);
    }

    fn insert(&mut self, url: String, sheet: EmojiSheet) {
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

/// Sheets stack into columns of at most this height; a single sheet taller than it
/// stands alone in its column. Keeps every column inside common texture limits.
const ATLAS_COLUMN_MAX_PX: u32 = 4096;
/// Total pixel budget of one atlas (64 MB of RGBA decoded). Past it the section is
/// served as separate sheets, which is what it was before atlases existed.
const ATLAS_MAX_PX: u64 = 16 * 1024 * 1024;
/// Frames per emoji sheet. A 1 MB GIF can carry thousands of tiny frames; the picker
/// shows a thumbnail, not the film.
const MAX_SHEET_FRAMES: usize = 256;

/// Transparent gutter between strips. drawImage samples past a source rect's edge when
/// it scales, so touching strips bleed their neighbour's pixels into the frame's border.
const ATLAS_PAD_PX: u32 = 2;
/// Bumped whenever the packing changes, so an atlas laid out the old way is rebuilt.
const ATLAS_LAYOUT_VERSION: u32 = 2;

/// Column packing for sheets of equal width: each is a vertical strip, stacked with a
/// gutter until the column would overflow. Returns the atlas size and one (x, y) per sheet.
fn atlas_layout(frame_size: u32, heights: &[u32]) -> (u32, u32, Vec<(u32, u32)>) {
    let step_x = frame_size + ATLAS_PAD_PX;
    let (mut col, mut y, mut tallest) = (0u32, 0u32, 0u32);
    let mut at = Vec::with_capacity(heights.len());
    for &h in heights {
        if y > 0 && y + h > ATLAS_COLUMN_MAX_PX {
            col += 1;
            y = 0;
        }
        at.push((col * step_x, y));
        tallest = tallest.max(y + h);
        y += h + ATLAS_PAD_PX;
        // A strip taller than a column closes its column behind it.
        if y > ATLAS_COLUMN_MAX_PX {
            col += 1;
            y = 0;
        }
    }
    let cols = if y == 0 && col > 0 { col } else { col + 1 };
    (cols * step_x - ATLAS_PAD_PX, tallest, at)
}

/// One PNG holding every sheet of a section, so the webview loads a pack in one
/// request instead of one per emoji. Keyed by the sheet set, cached next to the
/// sheets, rebuilt only when the set changes (an emoji arriving, a pack edit).
fn atlas_for(dir: &std::path::Path, sheets: &[EmojiSheet]) -> Option<(std::path::PathBuf, Vec<(u32, u32)>)> {
    let frame_size = sheets.first()?.frame_size;
    if sheets.iter().any(|s| s.frame_size != frame_size) {
        return None;
    }
    let heights: Vec<u32> = sheets.iter().map(|s| s.frame_count * frame_size).collect();
    let (width, height, at) = atlas_layout(frame_size, &heights);
    if width as u64 * height as u64 > ATLAS_MAX_PX {
        return None;
    }
    // The layout inputs are part of the key: a sheet rewritten at the same path with a
    // different frame count must never be read through an atlas laid out for the old one.
    let key = {
        let mut ident = String::new();
        ident.push_str(&format!("layout{ATLAS_LAYOUT_VERSION}\n"));
        for s in sheets {
            ident.push_str(&format!("{}|{}|{}\n", s.path, s.frame_count, s.frame_size));
        }
        vector_core::crypto::sha256_hex(ident.as_bytes())
    };
    let path = dir.join(format!("atlas-{key}.png"));
    if path.is_file() {
        touch(&path);
        return Some((path, at));
    }
    let mut atlas = image::RgbaImage::new(width, height);
    for (sheet, &(x, y)) in sheets.iter().zip(&at) {
        let bytes = std::fs::read(&sheet.path).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        image::imageops::overlay(&mut atlas, &img, x as i64, y as i64);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(atlas.as_raw(), width, height, image::ExtendedColorType::Rgba8)
    .ok()?;
    let tmp = sheet_tmp_path(&path);
    std::fs::write(&tmp, &png).ok()?;
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        if !path.is_file() {
            return None;
        }
    }
    Some((path, at))
}

/// Every sheet the on-disk cache already holds, in one crossing, and when two or
/// more are present, folded into one atlas so the webview fetches one file. A miss
/// is `None`: the caller fetches those one by one with `decode_animated_emoji`, so a
/// freshly added pack still streams in emoji by emoji while a known pack lands at once.
#[tauri::command]
pub async fn cached_emoji_sheets<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    urls: Vec<String>,
) -> Result<Vec<Option<EmojiSheet>>, String> {
    let Some(dir) = sheet_cache_dir(&handle) else { return Ok(vec![None; urls.len()]) };
    tokio::task::spawn_blocking(move || {
        let mut converted = false;
        let mut out: Vec<Option<EmojiSheet>> = urls
            .iter()
            .map(|url| {
                if let Some(hit) = cached_sheet(url) {
                    return Some(hit);
                }
                let sheet = read_sheet(&dir, url)?;
                converted = true;
                spritesheet_cache().lock().unwrap().insert(url.clone(), sheet.clone());
                Some(sheet)
            })
            .collect();
        if converted {
            prune_spritesheet_cache(&dir);
        }
        let hit_idx: Vec<usize> = out.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).collect();
        if hit_idx.len() >= 2 {
            let hits: Vec<EmojiSheet> = hit_idx.iter().map(|&i| out[i].clone().unwrap()).collect();
            if let Some((atlas, at)) = atlas_for(&dir, &hits) {
                let atlas = atlas.to_string_lossy().into_owned();
                for (k, &i) in hit_idx.iter().enumerate() {
                    if let Some(s) = out[i].as_mut() {
                        s.path = atlas.clone();
                        s.x = at[k].0;
                        s.y = at[k].1;
                    }
                }
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string())
}

/// Decode one emoji URL into a presized spritesheet on disk. Idempotent + cached.
///
/// Lookup order:
///   1. In-memory descriptor cache.
///   2. The on-disk sheet cache (a stat and a small JSON read).
///   3. Local image cache (emojis + emoji_pack_icons subdirs) —
///      pre-populated by `emoji_pack_upload_image` and by every
///      `bindCachedEmojiImg` hit on the same URL. Critical for
///      freshly-uploaded packs: Blossom may not have propagated the
///      URL yet, but we have the bytes in hand.
///   4. HTTP fetch from the URL (the original slow path).
#[tauri::command]
pub async fn decode_animated_emoji<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    url: String,
) -> Result<EmojiSheet, String> {
    if let Some(cached) = cached_sheet(&url) {
        return Ok(cached);
    }
    let sheet_dir = sheet_cache_dir(&handle);
    if let Some(dir) = &sheet_dir {
        if let Some(sheet) = read_sheet(dir, &url) {
            spritesheet_cache().lock().unwrap().insert(url.clone(), sheet.clone());
            return Ok(sheet);
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
        // SSRF + size guard: the emoji URL is attacker-controlled. Reject private/internal IP literals
        // + known-local hostnames, and cap at 1 MB (oversized emoji are hidden, never decoded).
        vector_core::net::validate_url_not_private(&url).map_err(|e| e.to_string())?;
        const MAX_EMOJI_DECODE_BYTES: usize = 1024 * 1024;
        let client = vector_core::net::build_http_client(std::time::Duration::from_secs(10))?;
        let mut resp = client.get(&url).send().await
            .map_err(|e| format!("fetch: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let ct = resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        if matches!(resp.content_length(), Some(len) if len as usize > MAX_EMOJI_DECODE_BYTES) {
            return Err("emoji too large".to_string());
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await.map_err(|e| format!("read body: {}", e))? {
                Some(chunk) => {
                    if body.len() + chunk.len() > MAX_EMOJI_DECODE_BYTES {
                        return Err("emoji too large".to_string());
                    }
                    body.extend_from_slice(&chunk);
                }
                None => break,
            }
        }
        (body, ct)
    };

    let url_for_blocking = url.clone();
    let url_for_sheet = url.clone();
    let decoded = tokio::task::spawn_blocking(move || decode_to_spritesheet(&bytes, &content_type, &url_for_blocking))
        .await
        .map_err(|e| format!("decode join: {}", e))??;

    // The sheet lives on disk from here on: future opens and launches are a stat,
    // and the webview reads the PNG itself.
    let dir = sheet_dir.ok_or_else(|| "no sheet cache directory".to_string())?;
    let sheet = tokio::task::spawn_blocking(move || {
        let sheet = write_sheet(&dir, &url_for_sheet, &decoded)?;
        prune_spritesheet_cache(&dir);
        Ok::<_, String>(sheet)
    })
    .await
    .map_err(|e| format!("sheet write join: {}", e))??;
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

    let mut frames: Vec<(image::RgbaImage, u32)> = match inferred {
        "webp" => decode_webp_frames(bytes)?,
        "gif" => decode_gif_frames(bytes)?,
        "apng" => decode_apng_frames(bytes)?,
        _ => decode_static_fallback(bytes)?,
    };

    if frames.is_empty() {
        return Err("no frames decoded".to_string());
    }
    frames.truncate(MAX_SHEET_FRAMES);

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
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP);
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader
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
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader
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
pub async fn emoji_crop_and_reencode(
    source_b64: String,
    mime: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<String, String> {
    // Fully-JSON (base64) IPC: Android's WebView is unreliable with raw request
    // bodies for this command and drops raw responses outright, so base64 in +
    // base64 out is the JSON path that works on every platform.
    let bytes = base64_simd::STANDARD
        .decode_to_vec(&source_b64)
        .map_err(|e| format!("source base64: {}", e))?;
    let input = EmojiCropInput { mime, x, y, w, h, bytes };
    if input.w != input.h {
        return Err("crop must be square".to_string());
    }
    if input.w == 0 {
        return Err("crop must be non-empty".to_string());
    }
    if input.bytes.len() > MAX_EMOJI_BYTES * 4 {
        // Source is allowed to be larger than the output cap — we shrink during
        // re-encode — but reject ridiculous inputs early.
        return Err("source too large".to_string());
    }

    let out = tokio::task::spawn_blocking(move || crop_and_reencode_blocking(input))
        .await
        .map_err(|e| format!("crop join: {}", e))??;
    Ok(base64_simd::STANDARD.encode_to_string(&out))
}

/// Import an image (a content URI on Android or a file path on desktop) for the
/// emoji/logo picker, normalized to a format every client + WebView can render.
/// Web-safe formats (PNG/JPEG/GIF/WebP, animation included) pass through; anything
/// else the image crate can decode (TIFF, BMP, ICO) is re-encoded to PNG. Reading
/// is native (ContentResolver / file I/O), so this also sidesteps the WebView's
/// broken content-URI reads. Bytes come back as a raw response.
#[tauri::command]
pub fn import_image_for_emoji(source: String) -> Result<String, String> {
    let bytes = read_emoji_image_source(&source)?;

    // Base64 out: a Vec<u8> return ships as a raw response, which Android's
    // WebView drops (only the inbound raw path works there).
    // Already web-safe (this set covers static + animated GIF/WebP): pass through.
    if matches!(
        vector_core::crypto::mime_from_magic_bytes(&bytes),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Ok(base64_simd::STANDARD.encode_to_string(&bytes));
    }

    // Anything else the image crate decodes (TIFF/BMP/ICO): normalize to PNG.
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader
        .decode()
        .map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {}", e))?;
    Ok(base64_simd::STANDARD.encode_to_string(&png))
}

fn read_emoji_image_source(source: &str) -> Result<Vec<u8>, String> {
    // Source is a full photo (pre-crop), so allow well over the emoji cap, but
    // bound it — a huge pick would otherwise be read + base64'd + shipped over
    // IPC only for the frontend's 256 KB gate to reject it.
    const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
    #[cfg(target_os = "android")]
    if source.starts_with("content://") {
        let (bytes, _) = crate::android::filesystem::read_android_uri_bytes(source.to_string())?;
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err("Image is too large.".to_string());
        }
        return Ok(bytes);
    }
    if std::fs::metadata(source).map(|m| m.len() as usize).unwrap_or(0) > MAX_SOURCE_BYTES {
        return Err("Image is too large.".to_string());
    }
    crate::shared::image::read_file_checked(source)
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

    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let dynimg = reader
        .decode()
        .map_err(|e| format!("decode: {}", e))?;
    let (sw, sh) = (dynimg.width(), dynimg.height());
    validate_crop_bounds(x, y, w, h, sw, sh)?;
    let cropped = dynimg.crop_imm(x, y, w, h).to_rgba8();
    encode_within_budget(&[(cropped, 0u32)], w, |scaled| encode_static(&scaled[0].0, format))
}

/// Encode square `frames` (side `src_dim`) via `encode`, shrinking the side
/// until the output fits `MAX_EMOJI_BYTES` (or the `MIN_EMOJI_DIM` floor). The
/// common path encodes once at native size; only an oversized result (e.g. a
/// GIF the re-encoder inflated past the cap) triggers downscaling. Emojis render
/// tiny, so shrinking is imperceptible and is the only way such an emote can
/// publish at all instead of being rejected.
fn encode_within_budget<F>(
    frames: &[(image::RgbaImage, u32)],
    src_dim: u32,
    encode: F,
) -> Result<Vec<u8>, String>
where
    F: Fn(&[(image::RgbaImage, u32)]) -> Result<Vec<u8>, String>,
{
    let out = encode(frames)?;
    // Fits, or already at/below the floor (shrinking a tiny input would only
    // upscale it) — take the native encode.
    if out.len() <= MAX_EMOJI_BYTES || src_dim <= MIN_EMOJI_DIM {
        return Ok(out);
    }
    let mut dim = src_dim;
    loop {
        // ~12% smaller per step (~0.77x area) — converges in a couple passes.
        dim = ((dim * 88) / 100).max(MIN_EMOJI_DIM);
        let scaled: Vec<(image::RgbaImage, u32)> = frames
            .iter()
            .map(|(img, d)| {
                (
                    image::imageops::resize(img, dim, dim, image::imageops::FilterType::Lanczos3),
                    *d,
                )
            })
            .collect();
        let out = encode(&scaled)?;
        if out.len() <= MAX_EMOJI_BYTES || dim <= MIN_EMOJI_DIM {
            return Ok(out);
        }
    }
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

    let cropped: Vec<(image::RgbaImage, u32)> = frames
        .into_iter()
        .map(|(rgba, duration)| {
            (image::imageops::crop_imm(&rgba, x, y, w, h).to_image(), duration.max(20))
        })
        .collect();

    encode_within_budget(&cropped, w, |scaled| {
        let dim = scaled[0].0.width();
        let mut config = webp::WebPConfig::new().map_err(|_| "webp config init failed".to_string())?;
        config.quality = 80.0;
        let mut encoder = webp::AnimEncoder::new(dim, dim, &config);
        encoder.set_loop_count(0);
        // libwebp wants cumulative end-of-frame timestamps (not per-frame
        // deltas). Decoder gives us deltas, so re-accumulate here.
        let mut cumulative: i32 = 0;
        for (img, duration) in scaled.iter() {
            cumulative = cumulative.saturating_add(*duration as i32);
            encoder.add_frame(webp::AnimFrame::from_rgba(img.as_raw(), dim, dim, cumulative));
        }
        let mem = encoder
            .try_encode()
            .map_err(|e| format!("webp anim encode: {:?}", e))?;
        Ok(mem.to_vec())
    })
}

fn crop_gif(bytes: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let frames = decode_gif_frames(bytes)?;
    if frames.is_empty() {
        return Err("gif: no frames".to_string());
    }
    let (fw, fh) = (frames[0].0.width(), frames[0].0.height());
    validate_crop_bounds(x, y, w, h, fw, fh)?;

    let cropped: Vec<(image::RgbaImage, u32)> = frames
        .into_iter()
        .map(|(rgba, duration_ms)| {
            (image::imageops::crop_imm(&rgba, x, y, w, h).to_image(), duration_ms)
        })
        .collect();

    encode_within_budget(&cropped, w, |scaled| {
        let mut out = Vec::new();
        {
            // Speed 10 = fastest encode at lowest CPU. Emoji-scale GIFs are
            // small enough that quality difference vs default is negligible.
            let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .map_err(|e| format!("gif repeat: {}", e))?;
            for (rgba, duration_ms) in scaled {
                let delay = image::Delay::from_numer_denom_ms((*duration_ms).max(20), 1);
                let frame = image::Frame::from_parts(rgba.clone(), 0, 0, delay);
                encoder
                    .encode_frame(frame)
                    .map_err(|e| format!("gif frame: {}", e))?;
            }
        }
        Ok(out)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vector-sheets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn legacy_container(frame_size: u32, durations: &[u32], png: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(VSPR_MAGIC);
        out.push(VSPR_VERSION);
        out.extend_from_slice(&frame_size.to_le_bytes());
        out.extend_from_slice(&(durations.len() as u32).to_le_bytes());
        for d in durations { out.extend_from_slice(&d.to_le_bytes()); }
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(png);
        out
    }

    #[test]
    fn atlas_columns_fill_to_the_cap_and_a_tall_sheet_stands_alone() {
        let step = 56 + ATLAS_PAD_PX;
        // Static 56 px strips with a gutter fill a column up to the cap; the next starts a column.
        let per_col = ((ATLAS_COLUMN_MAX_PX + ATLAS_PAD_PX) / step) as usize;
        let heights: Vec<u32> = std::iter::repeat(56).take(per_col + 1).collect();
        let (w, h, at) = atlas_layout(56, &heights);
        assert_eq!(w, 2 * step - ATLAS_PAD_PX);
        assert_eq!(h, per_col as u32 * step - ATLAS_PAD_PX);
        assert_eq!(at[per_col - 1], (0, (per_col as u32 - 1) * step));
        assert_eq!(at[per_col], (step, 0));
        // Strips never touch: every neighbour sits a gutter apart.
        assert_eq!(at[1].1 - at[0].1, step);
        // A 100-frame animation overflows a column: it stands alone, and its neighbours
        // stay in ordinary columns rather than stretching to its height.
        let (w, h, at) = atlas_layout(56, &[56, 100 * 56, 56]);
        assert_eq!(w, 3 * step - ATLAS_PAD_PX);
        assert_eq!(h, 100 * 56);
        assert_eq!(at, vec![(0, 0), (step, 0), (2 * step, 0)]);
        // Nothing but a tall sheet: one column, no empty trailing one.
        let (w, _, _) = atlas_layout(56, &[100 * 56]);
        assert_eq!(w, 56);
    }

    fn strip_png(frames: u32, shade: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(56, 56 * frames, image::Rgba([shade, shade, shade, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 56, 56 * frames, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn an_atlas_places_each_strip_where_its_descriptor_says() {
        let dir = scratch_dir("atlas");
        let a = write_sheet(&dir, "https://x/a.png", &DecodedSheet { png: strip_png(1, 10), frame_count: 1, frame_size: 56, durations: vec![0] }).unwrap();
        let b = write_sheet(&dir, "https://x/b.gif", &DecodedSheet { png: strip_png(3, 200), frame_count: 3, frame_size: 56, durations: vec![40; 3] }).unwrap();
        let (path, at) = atlas_for(&dir, &[a.clone(), b.clone()]).expect("two cached sheets fold");
        let gap = ATLAS_PAD_PX;
        assert_eq!(at, vec![(0, 0), (0, 56 + gap)]);
        let atlas = image::open(&path).unwrap().to_rgba8();
        assert_eq!(atlas.dimensions(), (56, 56 * 4 + gap));
        assert_eq!(atlas.get_pixel(3, 3)[0], 10, "a's pixels at a's offset");
        assert_eq!(atlas.get_pixel(3, 56)[3], 0, "the gutter between strips is transparent");
        assert_eq!(atlas.get_pixel(3, 56 + gap + 3)[0], 200, "b's first frame past the gutter");
        assert_eq!(atlas.get_pixel(3, 56 + gap + 56 * 2 + 3)[0], 200, "b's last frame at the bottom");
        let (again, _) = atlas_for(&dir, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(again, path, "same set, same file");
        let mut odd = b.clone();
        odd.frame_size = 48;
        assert!(atlas_for(&dir, &[a.clone(), odd]).is_none(), "mixed frame sizes are not folded");
        std::fs::remove_file(&b.path).unwrap();
        let c = write_sheet(&dir, "https://x/c.png", &DecodedSheet { png: strip_png(1, 90), frame_count: 1, frame_size: 56, durations: vec![0] }).unwrap();
        assert!(atlas_for(&dir, &[a, b, c]).is_none(), "a missing member means no atlas, not a hole");
    }

    #[test]
    fn a_sheet_round_trips_through_the_two_file_layout() {
        let dir = scratch_dir("roundtrip");
        let decoded = DecodedSheet { png: strip_png(3, 7), frame_count: 3, frame_size: 56, durations: vec![100, 120, 80] };
        let written = write_sheet(&dir, "https://x/a.gif", &decoded).unwrap();
        let read = read_sheet(&dir, "https://x/a.gif").expect("a written sheet reads back");
        assert_eq!(read.path, written.path);
        assert_eq!((read.frame_count, read.frame_size, read.frame_durations_ms.clone()), (3, 56, vec![100, 120, 80]));
        assert_eq!(std::fs::read(&read.path).unwrap(), decoded.png, "the PNG is served as-is");
        assert!(read_sheet(&dir, "https://x/missing.gif").is_none(), "a miss is None, not an error");
    }

    #[test]
    fn a_legacy_container_is_converted_once_and_retired() {
        let dir = scratch_dir("legacy");
        let png = vec![0xCDu8; 32];
        let legacy = dir.join(format!("{}.vspr", sheet_key("https://x/old.webp")));
        std::fs::write(&legacy, legacy_container(56, &[40, 40], &png)).unwrap();
        let sheet = read_sheet(&dir, "https://x/old.webp").expect("legacy converts on read");
        assert_eq!(sheet.frame_count, 2);
        assert_eq!(std::fs::read(&sheet.path).unwrap(), png);
        assert!(!legacy.exists(), "the container is gone after conversion");
        assert!(read_sheet(&dir, "https://x/old.webp").is_some(), "the converted layout serves the next read");

        // Truncated / garbage containers are a miss, never a panic.
        assert!(deserialize_legacy_sheet(&legacy_container(56, &[40], &png)[..20]).is_none());
        assert!(deserialize_legacy_sheet(b"nope").is_none());
        assert!(deserialize_legacy_sheet(&[]).is_none());
    }

    fn square_frames(dim: u32) -> Vec<(image::RgbaImage, u32)> {
        vec![(image::RgbaImage::new(dim, dim), 100u32)]
    }

    #[test]
    fn budget_encodes_once_when_it_fits() {
        // Fake encoder: output scales with frame area (64px * 4 bytes < cap).
        let calls = std::cell::Cell::new(0u32);
        let out = encode_within_budget(&square_frames(64), 64, |scaled| {
            calls.set(calls.get() + 1);
            let d = scaled[0].0.width();
            Ok(vec![0u8; (d * d * 4) as usize])
        })
        .unwrap();
        assert_eq!(out.len(), 64 * 64 * 4);
        assert_eq!(calls.get(), 1, "no resample on the fits-at-native path");
    }

    #[test]
    fn budget_shrinks_until_it_fits() {
        let out = encode_within_budget(&square_frames(400), 400, |scaled| {
            let d = scaled[0].0.width();
            Ok(vec![0u8; (d * d * 4) as usize])
        })
        .unwrap();
        assert!(out.len() <= MAX_EMOJI_BYTES, "shrank under the cap");
        assert!(out.len() < 400 * 400 * 4, "actually shrank from native");
    }

    #[test]
    fn budget_never_upscales_below_floor() {
        // 40px is under MIN_EMOJI_DIM and over budget: must return the native
        // encode, never upscale to the 48px floor (bigger, still over).
        let out = encode_within_budget(&square_frames(40), 40, |scaled| {
            let d = scaled[0].0.width();
            Ok(vec![0u8; (d * d * 200) as usize])
        })
        .unwrap();
        assert_eq!(out.len(), 40 * 40 * 200);
    }
}

