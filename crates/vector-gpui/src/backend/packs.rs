//! Making and deleting our own emoji packs: the uploads, the publish, the cleanup.

use std::sync::Arc;

use vector_core::emoji_packs::{self, EmojiPack, PackEmoji};

use crate::shared::image::{UploadImageKind, prepare_upload_image, upload_mime_for};

pub const MAX_EMOJI_BYTES: usize = 256 * 1024;

/// What the picker accepts: web formats as they are, and what the image crate reads, as PNG.
pub const IMPORT_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "bmp", "ico"];

/// A picked file as the creator will hold it: web-safe bytes, never an SVG.
pub fn import(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if matches!(vector_core::crypto::mime_from_magic_bytes(&bytes), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        return Ok(bytes);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().map_err(|e| format!("sniff: {e}"))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader.decode().map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| format!("png encode: {e}"))?;
    Ok(png)
}

/// The picture's canvas size, animated or not.
pub fn dims(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// Upload one emoji or pack logo; the URL it landed at, its bytes kept as the local copy.
pub async fn upload(bytes: Arc<Vec<u8>>, icon: bool) -> Result<String, String> {
    if bytes.len() > MAX_EMOJI_BYTES {
        return Err(format!("File is {} KB, max is {} KB.", bytes.len() / 1024, MAX_EMOJI_BYTES / 1024));
    }
    if bytes.is_empty() {
        return Err("File is empty.".into());
    }
    vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Failed to get signer: {e}"))?;
    let servers = vector_core::blossom_servers::compute_enabled_servers();
    if servers.is_empty() {
        return Err("No Blossom servers configured.".into());
    }
    let source = bytes.clone();
    let prepared = tokio::task::spawn_blocking(move || prepare_upload_image(&source, UploadImageKind::Emoji)).await.map_err(|e| e.to_string())??;
    let mime = upload_mime_for(prepared.extension);
    let data = Arc::new(prepared.bytes);
    // Emojis are tiny: a server silent for 10s is treated as dead and failed over.
    let stall = vector_core::net::tor_http_timeout(std::time::Duration::from_secs(10));
    let url = vector_core::blossom::upload_blob_with_failover(signer, servers, data.clone(), Some(mime), Some(stall)).await?;
    super::images::precache_emoji(&url, &data, icon);
    Ok(url)
}

fn new_identifier() -> String {
    use rand::{Rng, distributions::Alphanumeric, thread_rng};
    thread_rng().sample_iter(&Alphanumeric).take(12).map(char::from).collect()
}

/// Publish the pack (`identifier` keeps an edit on the same coordinate).
pub async fn publish(identifier: Option<String>, title: String, image_url: String, emojis: Vec<(String, String)>) -> Result<EmojiPack, String> {
    let emojis: Vec<PackEmoji> = emojis
        .into_iter()
        .filter(|(s, u)| !s.trim().is_empty() && !u.trim().is_empty())
        .map(|(shortcode, url)| PackEmoji { shortcode: shortcode.trim().into(), url: url.trim().into(), sha256: None })
        .collect();
    if emojis.is_empty() {
        return Err("A pack needs at least one emoji.".into());
    }
    let pack = EmojiPack {
        id: String::new(),
        pubkey: String::new(),
        identifier: identifier.filter(|s| !s.trim().is_empty()).unwrap_or_else(new_identifier),
        title: title.trim().into(),
        image_url: image_url.trim().into(),
        description: String::new(),
        emojis,
        is_own: true,
        updated_at: 0,
        status: emoji_packs::PACK_STATUS_ACTIVE,
    };
    emoji_packs::publish_pack(&pack).await
}

pub async fn delete_blob(url: String) -> Result<(), String> {
    vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Failed to get signer: {e}"))?;
    vector_core::blossom::delete_blob_by_url(signer, &url).await
}

/// Tombstone our own pack and drop it locally.
pub async fn delete(id: String) -> Result<(), String> {
    emoji_packs::delete_own_pack(&id).await
}

/// The URLs whose host definitively deleted the file (404/410), past the warm local cache.
pub async fn gone(urls: Vec<String>) -> Vec<String> {
    use futures::StreamExt;
    let mut deduped: Vec<String> = Vec::new();
    for u in urls {
        if deduped.len() < 64 && !deduped.contains(&u) {
            deduped.push(u);
        }
    }
    futures::stream::iter(deduped.into_iter().map(|url| async move {
        let gone = vector_core::net::validate_url_not_private(&url).is_ok()
            && matches!(vector_core::net::remote_status(&url, std::time::Duration::from_secs(10)).await, Some(404 | 410));
        gone.then_some(url)
    }))
    .buffer_unordered(6)
    .filter_map(|u| async move { u })
    .collect()
    .await
}
