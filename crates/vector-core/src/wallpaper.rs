//! Per-DM wallpaper feature.
//!
//! Wallpapers are static images attached to a 1:1 DM conversation. Either
//! party may set one; latest-write-wins by rumor `created_at`.
//!
//! On the wire: a NIP-17 gift-wrapped rumor with kind 30078 and the d tag
//! `vector-wallpaper`. The wallpaper bytes themselves are AES-256-GCM
//! encrypted onto Blossom — same crypto path Vector uses for normal file
//! attachments. The decryption key + nonce live in the rumor's tags; that's
//! safe because the rumor is already sealed inside the NIP-17 envelope
//! addressed only to the two participants.
//!
//! Rumor shape:
//! ```text
//!   kind:       30078 (APPLICATION_SPECIFIC)
//!   created_at: now (latest-write-wins tiebreaker)
//!   tags:
//!     ["d",                "vector-wallpaper"]
//!     ["url",              <blossom URL>]
//!     ["decryption-key",   <hex>]
//!     ["decryption-nonce", <hex>]
//!     ["x",                <plaintext sha256>]
//!     ["m",                "image/png"]   (optional)
//!     ["size",             <encrypted size in bytes>]
//!   content: "" (unused; metadata lives in tags)
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::stored_event::event_kind;

const WALLPAPER_DTAG_VALUE: &str = "vector-wallpaper";

/// Hard ceiling on a received (encrypted) wallpaper download. The send side
/// caps plaintext at 5 MB; 10 MB leaves headroom for encryption overhead
/// while still bounding memory against a malicious/oversized blob.
const MAX_WALLPAPER_DOWNLOAD_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum allowed source-image size (pre-encryption). Matches the user-
/// facing cap; enforced both at preview prep and at the picker UI.
pub const MAX_WALLPAPER_BYTES: usize = 5 * 1024 * 1024;

/// Per-account directory for cached wallpaper files. One active file per
/// chat + at most one preview-staging file per chat.
fn wallpapers_dir() -> Result<PathBuf, String> {
    let npub = crate::db::get_current_account()?;
    let dir = crate::db::account_dir(&npub)?.join("wallpapers");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create wallpapers dir: {}", e))?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallpaperPreview {
    /// Local cached path the frontend should render.
    pub path: String,
    /// True iff the picker had to extract a frame from an animated source.
    /// The UI uses this to surface a one-line "static-only" notice.
    pub was_animated: bool,
    /// Suggested initial brightness (0..=100) derived from the image's
    /// average luma — bright images get more dimming so text stays
    /// readable, dark images keep more of the original. User can still
    /// override with the slider before confirming.
    pub recommended_dim: u8,
}

/// Validate + prepare a picked image: enforce the 5 MB cap, image-only
/// mime, extract first frame for animated formats, write to the per-chat
/// preview slot. The returned path is what the chat background should
/// switch to while the Confirm/Cancel bar is showing.
pub fn prepare_wallpaper_preview(
    chat_npub: &str,
    file_path: &str,
) -> Result<WallpaperPreview, String> {
    let src = Path::new(file_path);
    let bytes = std::fs::read(src)
        .map_err(|e| format!("Failed to read image: {}", e))?;

    if bytes.len() > MAX_WALLPAPER_BYTES {
        return Err(format!(
            "Image is too large ({} MB). Wallpapers max out at {} MB.",
            bytes.len() / (1024 * 1024),
            MAX_WALLPAPER_BYTES / (1024 * 1024),
        ));
    }

    let mime = crypto::mime_from_magic_bytes(&bytes).to_string();
    if !mime.starts_with("image/") {
        return Err("Wallpapers must be image files.".to_string());
    }

    let (final_bytes, final_extension, was_animated) =
        extract_first_frame_if_animated(&bytes, &mime)?;

    // Sample luma so the slider lands somewhere readable instead of the
    // fixed 50% default. Cheap (downsamples first); failures fall back
    // to the static default so a weird-encoded image never blocks the
    // preview flow.
    let recommended_dim = estimate_brightness_for_white_text(&final_bytes).unwrap_or(50);

    // Clear any prior preview for this chat (different format or stale).
    clean_chat_files(chat_npub, FileKind::Preview, None)?;

    let preview = wallpapers_dir()?.join(format!("{}.preview.{}", chat_npub, final_extension));
    let tmp = preview.with_file_name(format!("{}.preview.{}.tmp", chat_npub, final_extension));
    std::fs::write(&tmp, &final_bytes)
        .map_err(|e| format!("Failed to stage preview file: {}", e))?;
    std::fs::rename(&tmp, &preview)
        .map_err(|e| format!("Failed to commit preview file: {}", e))?;

    Ok(WallpaperPreview {
        path: preview.to_string_lossy().to_string(),
        was_animated,
        recommended_dim,
    })
}

/// Pick a starting brightness percent such that white chat text stays
/// readable against the image. Down-samples to 64×64 for speed, averages
/// the Rec. 709 luma, then maps `(avg_luma 0..=255)` to a brightness
/// percent in the `[25, 95]` range — bright images get dimmer defaults,
/// dark images stay bright. Returns `None` if the bytes can't be decoded.
fn estimate_brightness_for_white_text(bytes: &[u8]) -> Option<u8> {
    use ::image::{GenericImageView, ImageReader};
    use std::io::Cursor;

    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    let thumb = img.thumbnail(64, 64);
    let rgb = thumb.to_rgb8();
    let (w, h) = thumb.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    for pixel in rgb.pixels() {
        let r = pixel[0] as f32;
        let g = pixel[1] as f32;
        let b = pixel[2] as f32;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        sum += y as u64;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let avg = (sum / count) as f32; // 0..=255
    // Map luma → brightness with an aggressive curve, then halve to land
    // most photographs in the 15..30% range where white chat text is
    // clearly legible against the underlying image:
    //   white  (255) → ~12
    //   bright (200) → ~17
    //   mid    (128) → ~22
    //   dark   ( 60) → ~38
    //   black  (  0) → ~47
    let brightness = (95.0 - (avg / 255.0) * 70.0) / 2.0;
    Some(brightness.clamp(10.0, 50.0) as u8)
}

/// Delete the preview file (user cancelled before publishing).
pub fn cancel_wallpaper_preview(chat_npub: &str) -> Result<(), String> {
    clean_chat_files(chat_npub, FileKind::Preview, None)
}

/// Returns `(bytes, extension, was_animated)`. For non-animated formats,
/// passes the source through unchanged. For GIF / WebP the first frame is
/// re-encoded as PNG so it can be rendered in `background-image` without
/// animation.
fn extract_first_frame_if_animated(
    src: &[u8],
    mime: &str,
) -> Result<(Vec<u8>, String, bool), String> {
    use ::image::{ImageFormat, ImageReader};
    use std::io::Cursor;

    let is_gif = mime == "image/gif";
    let is_webp = mime == "image/webp";

    if !is_gif && !is_webp {
        let ext = crypto::extension_from_mime(mime);
        return Ok((src.to_vec(), ext, false));
    }

    // `image::ImageReader::decode()` yields frame 0 for animated GIF / WebP.
    let reader = ImageReader::with_format(
        Cursor::new(src),
        if is_gif { ImageFormat::Gif } else { ImageFormat::WebP },
    );
    let img = reader
        .decode()
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    let mut out: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| format!("Failed to re-encode wallpaper: {}", e))?;

    Ok((out, "png".to_string(), true))
}

#[derive(Copy, Clone)]
enum FileKind {
    Preview,
    Active,
}

/// Remove every wallpaper artifact for `chat_npub` of the given kind. When
/// `extension_to_skip` is `Some`, files matching that extension are kept
/// (used to overwrite-in-place during same-format rewrites).
fn clean_chat_files(
    chat_npub: &str,
    kind: FileKind,
    extension_to_skip: Option<&str>,
) -> Result<(), String> {
    let dir = wallpapers_dir()?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    let preview_prefix = format!("{}.preview.", chat_npub);
    let active_prefix = format!("{}.", chat_npub);

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let matches = match kind {
            FileKind::Preview => name.starts_with(&preview_prefix),
            // Active = chat-prefixed but NOT preview-prefixed.
            FileKind::Active => name.starts_with(&active_prefix) && !name.starts_with(&preview_prefix),
        };
        if !matches {
            continue;
        }
        if let Some(ext) = extension_to_skip {
            let suffix = format!(".{}", ext);
            if name.ends_with(&suffix) {
                continue;
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

/// Publish the current preview file as the chat's wallpaper. Encrypts +
/// uploads to Blossom, builds the kind-30078 rumor, sends to the
/// counterparty (the gift-wrap helper fans out to self for cross-device
/// sync), promotes the preview file to the active slot, updates STATE +
/// DB, drops a `WallpaperChanged` system event, and emits
/// `wallpaper_updated` to the frontend.
///
/// `blur` and `dim` are the customisation knobs the user set on the
/// preview slider — clamped here to safe ranges and carried as optional
/// tags on the rumor so older clients without slider support still get a
/// usable wallpaper (falling back to their own defaults).
pub async fn publish_wallpaper(chat_npub: &str, blur: u8, dim: u8) -> Result<(), String> {
    // Capture session at entry — the upload + gift-wrap send below take
    // seconds, and a mid-publish account swap must not write this
    // wallpaper into the new account (re-checked before the STATE/DB write).
    let session = crate::state::SessionGuard::capture();

    let blur = blur.min(30);
    let dim = dim.min(100);
    // Find the preview file (we don't know its extension ahead of time).
    let dir = wallpapers_dir()?;
    let prefix = format!("{}.preview.", chat_npub);
    let mut preview_path: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| format!("Wallpapers dir: {}", e))?
        .flatten()
    {
        let p = entry.path();
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if n.starts_with(&prefix) {
            preview_path = Some(p);
            break;
        }
    }
    let preview = preview_path
        .ok_or_else(|| "No wallpaper preview to publish. Pick an image first.".to_string())?;
    let bytes = std::fs::read(&preview)
        .map_err(|e| format!("Failed to read preview file: {}", e))?;

    let extension = preview
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_string();
    let mime = crypto::mime_from_extension(&extension).to_string();
    let plaintext_hash = crypto::sha256_hex(&bytes);

    let params = crypto::generate_encryption_params();
    let encrypted = crypto::encrypt_data(&bytes, &params)?;

    let client = crate::state::nostr_client().ok_or("Not logged in")?;
    let signer = client
        .signer()
        .await
        .map_err(|e| format!("Signer: {}", e))?;
    let my_pk = crate::state::my_public_key().ok_or("Public key not set")?;
    // The chat the wallpaper belongs to, tagged on the rumor below. Without it,
    // the self-send copy (for multi-device sync) has no recipient, so the
    // inbound handler attributes it to our self-chat (Notes) instead of this
    // chat — a wallpaper set in any chat would also reskin Notes.
    let recipient_pk = PublicKey::from_bech32(chat_npub)
        .map_err(|e| format!("Invalid chat npub: {}", e))?;

    let servers = crate::state::get_blossom_servers();

    // Bridge Blossom upload progress to the frontend so the Set Wallpaper
    // button can render a real ring instead of an opaque disabled state.
    let chat_npub_for_progress = chat_npub.to_string();
    let progress_cb: crate::blossom::ProgressCallback = Arc::new(move |percentage, bytes| {
        crate::traits::emit_event(
            "wallpaper_upload_progress",
            &serde_json::json!({
                "chat_id": chat_npub_for_progress,
                "progress": percentage.unwrap_or(0),
                "bytes": bytes.unwrap_or(0),
            }),
        );
        Ok(())
    });

    let upload_url = crate::blossom::upload_blob_with_progress_and_failover(
        signer.clone(),
        servers,
        Arc::new(encrypted.clone()),
        Some(&mime),
        /* is_encrypted */ true,
        progress_cb,
        None, // default retry count
        None, // default retry spacing
        None, // no cancel flag (the picker flow doesn't expose cancel mid-upload)
    )
    .await
    .map_err(|e| format!("Wallpaper upload failed: {}", e))?;

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rumor = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), "")
        .tag(Tag::identifier(WALLPAPER_DTAG_VALUE))
        // Recipient tag — identifies which chat this wallpaper is for. The
        // inbound handler reads it to attribute self-sent (multi-device) copies
        // to the correct chat rather than defaulting to our self-chat.
        .tag(Tag::public_key(recipient_pk))
        .tag(Tag::custom(
            TagKind::Custom("url".into()),
            vec![upload_url.clone()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("decryption-key".into()),
            vec![params.key.clone()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("decryption-nonce".into()),
            vec![params.nonce.clone()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("x".into()),
            vec![plaintext_hash.clone()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("m".into()),
            vec![mime.clone()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("size".into()),
            vec![encrypted.len().to_string()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("blur".into()),
            vec![blur.to_string()],
        ))
        .tag(Tag::custom(
            TagKind::Custom("dim".into()),
            vec![dim.to_string()],
        ))
        .custom_created_at(Timestamp::from(created_at))
        .build(my_pk);

    // SEND FIRST, commit on success. Wallpaper is a sync feature — if the
    // recipient (and our other devices) can't see it, there's no value in
    // locally applying it.
    //
    // 3 attempts with 2s spacing (~6s max wait) keeps the dialog
    // responsive. self_send=true so other devices of ours pick it up via
    // their own NIP-17 inbox subscription.
    let pending_id = format!("pending-wallpaper-{}", created_at);
    let send_config = crate::sending::SendConfig {
        max_send_attempts: 3,
        retry_delay: std::time::Duration::from_secs(2),
        self_send: true,
        ..Default::default()
    };
    let send_callback: Arc<dyn crate::sending::SendCallback> =
        Arc::new(crate::sending::NoOpSendCallback);
    if let Err(e) = crate::sending::send_rumor_dm(
        chat_npub, &pending_id, rumor.clone(), &send_config, send_callback,
    ).await {
        log_warn!("[Wallpaper] send_rumor_dm to {} failed: {}", chat_npub, e);
        return Err(format!(
            "Couldn't send the wallpaper. Check that the relays you and your contact share are reachable, then try again. ({})",
            e
        ));
    }
    log_info!("[Wallpaper] rumor delivered to {}", chat_npub);

    // Account swapped during upload/send: the rumor already went to the
    // original recipient, but the local commit below (preview promotion,
    // STATE, DB) would land in the new account's storage. Skip it.
    if !session.is_valid() {
        return Ok(());
    }

    // Send succeeded — promote the preview file to the active slot.
    let active = wallpapers_dir()?.join(format!("{}.{}", chat_npub, extension));
    clean_chat_files(chat_npub, FileKind::Active, None)?;
    std::fs::rename(&preview, &active)
        .map_err(|e| format!("Failed to promote preview: {}", e))?;
    let active_str = active.to_string_lossy().to_string();

    let me_npub = my_pk.to_bech32().unwrap_or_default();

    // Persist to STATE + DB. Capture the previous Blossom URL + uploader
    // under the same lock so we can clean it up only if we owned it.
    let (slim, prev_url, prev_uploader) = {
        let mut state = crate::state::STATE.lock().await;
        let prev = state.get_chat(chat_npub).map(|c| {
            (c.wallpaper_url.clone(), c.wallpaper_uploader.clone())
        });
        if let Some(chat) = state.get_chat_mut(chat_npub) {
            chat.wallpaper_path = active_str.clone();
            chat.wallpaper_ts = created_at;
            chat.wallpaper_blur = blur;
            chat.wallpaper_dim = dim;
            chat.wallpaper_url = upload_url.clone();
            chat.wallpaper_uploader = me_npub.clone();
        }
        let slim = state
            .get_chat(chat_npub)
            .map(|c| crate::db::chats::SlimChatDB::from_chat(c, &state.interner));
        let (pu, puploader) = prev.unwrap_or_default();
        (slim, pu, puploader)
    };
    if let Some(slim) = slim {
        if let Err(e) = crate::db::chats::save_slim_chat(&slim) {
            log_warn!("[Wallpaper] save_slim_chat failed for {}: {}", chat_npub, e);
        }
    }

    // Fire-and-forget DELETE of the previous blob, only if we uploaded
    // it (server's auth challenge would reject otherwise). Multi-device
    // case: this runs on the device that does the replace, which may not
    // be the device that uploaded the previous one — the uploader check
    // is on the npub, not the device, so it still fires correctly.
    if !prev_url.is_empty() && prev_uploader == me_npub {
        let signer_clone = signer.clone();
        let prev_url_clone = prev_url.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::blossom::delete_blob_by_url(signer_clone, &prev_url_clone).await
            {
                log_warn!(
                    "[Wallpaper] DELETE prev blob {} failed: {}",
                    prev_url_clone, e
                );
            }
        });
    }

    let event_id = rumor.id.ok_or("Rumor missing id")?.to_hex();
    let inserted = match crate::db::events::save_system_event_by_id(
        &event_id,
        chat_npub,
        crate::stored_event::SystemEventType::WallpaperChanged,
        &me_npub,
        Some("You"),
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            log_warn!("[Wallpaper] save_system_event_by_id failed for {}: {}", event_id, e);
            false
        }
    };
    if inserted {
        crate::traits::emit_event("system_event", &serde_json::json!({
            "conversation_id": chat_npub,
            "event_id": event_id,
            "event_type": crate::stored_event::SystemEventType::WallpaperChanged.as_u8(),
            "member_pubkey": me_npub,
            "member_name": "You",
        }));
    } else {
        log_warn!("[Wallpaper] system event {} was not inserted (already exists or save failed)", event_id);
    }

    crate::traits::emit_event(
        "wallpaper_updated",
        &serde_json::json!({
            "chat_id": chat_npub,
            "path": active_str,
            "ts": created_at,
            "blur": blur,
            "dim": dim,
            "by_npub": me_npub,
            "event_id": event_id,
        }),
    );

    Ok(())
}

/// Apply a received wallpaper rumor. Drops the rumor if its timestamp is
/// not newer than the chat's current `wallpaper_ts` (latest-write-wins).
/// On a fresh rumor: downloads + decrypts the Blossom blob, caches it
/// locally, updates STATE + DB, saves a `WallpaperChanged` system event,
/// and emits `wallpaper_updated` to the frontend.
#[allow(clippy::too_many_arguments)]
pub async fn apply_received_wallpaper(
    chat_npub: &str,
    sender_npub: &str,
    created_at: u64,
    url: &str,
    decryption_key: &str,
    decryption_nonce: &str,
    plaintext_hash: Option<&str>,
    mime: Option<&str>,
    blur: Option<u8>,
    dim: Option<u8>,
    rumor_event_id: &str,
) -> Result<(), String> {
    // Capture session NOW — the download below can take seconds, and a
    // mid-fetch account swap must not let us write account A's wallpaper
    // into account B's STATE/DB (re-checked before every write below).
    let session = crate::state::SessionGuard::capture();

    let blur = blur.unwrap_or(0).min(30);
    let dim = dim.unwrap_or(50).min(100);
    // Latest-write-wins. Drop the rumor if we've already applied a newer
    // (or equal) one — typical during negentropy backfill.
    {
        let state = crate::state::STATE.lock().await;
        if let Some(chat) = state.get_chat(chat_npub) {
            if chat.wallpaper_ts >= created_at {
                return Ok(());
            }
        }
    }

    let mime_str = mime.unwrap_or("image/png").to_string();
    let extension = crypto::extension_from_mime(&mime_str);

    // SSRF guard: the URL is attacker-controlled (it arrives in a rumor),
    // and this fetch is zero-interaction. Block private/internal targets.
    crate::net::validate_url_not_private(url)?;

    let http = crate::net::build_http_client(Duration::from_secs(30))?;
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Wallpaper download failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Wallpaper HTTP {}", resp.status()));
    }
    // Reject by advertised length first (cheap), then enforce a hard cap
    // while streaming so a server that omits Content-Length can't OOM us.
    if let Some(len) = resp.content_length() {
        if len > MAX_WALLPAPER_DOWNLOAD_BYTES {
            return Err("Wallpaper too large".to_string());
        }
    }
    let mut resp = resp;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Read body: {}", e))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > MAX_WALLPAPER_DOWNLOAD_BYTES {
            return Err("Wallpaper too large".to_string());
        }
    }

    let plaintext = crypto::decrypt_data(&bytes, decryption_key, decryption_nonce)?;

    if let Some(want_hash) = plaintext_hash {
        let got_hash = crypto::sha256_hex(&plaintext);
        if !got_hash.eq_ignore_ascii_case(want_hash) {
            return Err("Wallpaper integrity check failed".to_string());
        }
    }

    // Account swap during the download invalidates everything below — the
    // chat npub, the DB pool, and the per-account wallpapers dir all belong
    // to a session that may no longer be active. Bail before any write.
    if !session.is_valid() {
        return Ok(());
    }

    let active = wallpapers_dir()?.join(format!("{}.{}", chat_npub, extension));
    clean_chat_files(chat_npub, FileKind::Active, None)?;
    let tmp = active.with_file_name(format!("{}.{}.tmp", chat_npub, extension));
    std::fs::write(&tmp, &plaintext).map_err(|e| format!("Write wallpaper: {}", e))?;
    std::fs::rename(&tmp, &active).map_err(|e| format!("Commit wallpaper: {}", e))?;
    let active_str = active.to_string_lossy().to_string();

    // Capture previous URL + uploader under the same lock — only DELETE
    // the prior blob if WE were the uploader (covers multi-device sync
    // where a different device of ours uploaded the previous wallpaper).
    let (slim, prev_url, prev_uploader) = {
        let mut state = crate::state::STATE.lock().await;
        let prev = state.get_chat(chat_npub).map(|c| {
            (c.wallpaper_url.clone(), c.wallpaper_uploader.clone())
        });
        if let Some(chat) = state.get_chat_mut(chat_npub) {
            chat.wallpaper_path = active_str.clone();
            chat.wallpaper_ts = created_at;
            chat.wallpaper_blur = blur;
            chat.wallpaper_dim = dim;
            chat.wallpaper_url = url.to_string();
            chat.wallpaper_uploader = sender_npub.to_string();
        }
        let slim = state
            .get_chat(chat_npub)
            .map(|c| crate::db::chats::SlimChatDB::from_chat(c, &state.interner));
        let (pu, puploader) = prev.unwrap_or_default();
        (slim, pu, puploader)
    };
    if let Some(slim) = slim {
        let _ = crate::db::chats::save_slim_chat(&slim);
    }

    if !prev_url.is_empty() {
        let me_npub = crate::state::my_public_key()
            .and_then(|pk| pk.to_bech32().ok())
            .unwrap_or_default();
        if !me_npub.is_empty() && prev_uploader == me_npub {
            if let Some(client) = crate::state::nostr_client() {
                if let Ok(signer) = client.signer().await {
                    let prev_url_clone = prev_url.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::blossom::delete_blob_by_url(signer, &prev_url_clone).await
                        {
                            log_warn!(
                                "[Wallpaper] DELETE prev blob {} failed: {}",
                                prev_url_clone, e
                            );
                        }
                    });
                }
            }
        }
    }

    // Resolve a display name for the system event. Fall back to the npub
    // (truncated by the frontend's formatter) when we don't know the peer
    // yet — the row still tells the user what happened.
    let sender_display = {
        let state = crate::state::STATE.lock().await;
        state
            .get_profile(sender_npub)
            .and_then(|p| {
                if !p.nickname.is_empty() {
                    Some(p.nickname.to_string())
                } else if !p.name.is_empty() {
                    Some(p.name.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| sender_npub.to_string())
    };
    let inserted = crate::db::events::save_system_event_by_id(
        rumor_event_id,
        chat_npub,
        crate::stored_event::SystemEventType::WallpaperChanged,
        sender_npub,
        Some(&sender_display),
    )
    .await
    .unwrap_or(false);
    if inserted {
        crate::traits::emit_event("system_event", &serde_json::json!({
            "conversation_id": chat_npub,
            "event_id": rumor_event_id,
            "event_type": crate::stored_event::SystemEventType::WallpaperChanged.as_u8(),
            "member_pubkey": sender_npub,
            "member_name": sender_display,
        }));
    }

    crate::traits::emit_event(
        "wallpaper_updated",
        &serde_json::json!({
            "chat_id": chat_npub,
            "path": active_str,
            "ts": created_at,
            "blur": blur,
            "dim": dim,
            "by_npub": sender_npub,
            "event_id": rumor_event_id,
        }),
    );

    Ok(())
}
