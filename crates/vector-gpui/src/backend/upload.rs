//! Our own profile pictures: prepared as the desktop client prepares them, then uploaded.

use std::path::PathBuf;
use std::sync::Arc;

use vector_core::image_crop::CropRect;

use crate::shared::image::{UploadImageKind, prepare_upload_image, upload_mime_for};

/// Only these upload as a profile picture: the picker can't filter, so a renamed file is caught here.
pub const PICTURE_EXTS: &[&str] = &["png", "jpeg", "jpg", "gif", "webp"];

pub fn picture_ext(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    PICTURE_EXTS.contains(&ext.as_str()).then_some(ext)
}

/// The picture exactly as it will upload: cropped, shrunk and re-encoded.
pub fn prepared(bytes: Vec<u8>, banner: bool, crop: Option<CropRect>) -> Result<(Vec<u8>, &'static str), String> {
    let source = match crop {
        Some(c) => crate::crop::crop_drawn(bytes, c)?,
        None => bytes,
    };
    let kind = if banner { UploadImageKind::Banner } else { UploadImageKind::Avatar };
    let p = prepare_upload_image(&source, kind)?;
    Ok((p.bytes, p.extension))
}

/// Upload a picked picture and point our profile at it; the URL it landed at.
pub async fn picture(path: PathBuf, banner: bool, crop: Option<CropRect>) -> Result<String, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|_| "Image couldn't be loaded from disk")?;
    picture_ext(&path).ok_or("File type is not allowed for avatars (only images are permitted)")?;
    let (bytes, ext) = tokio::task::spawn_blocking(move || prepared(bytes, banner, crop)).await.map_err(|e| e.to_string())??;
    let bytes = Arc::new(bytes);

    vector_core::state::nostr_client().ok_or("Not connected")?;
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
    let progress: vector_core::blossom::ProgressCallback = Arc::new(|_, _| Ok(()));
    let url = vector_core::blossom::upload_blob_with_progress_and_failover(
        signer,
        vector_core::state::get_blossom_servers(),
        bytes.clone(),
        Some(upload_mime_for(ext)),
        false,
        progress,
        None,
        None,
        None,
    )
    .await?
    .url;

    super::images::precache(&url, &bytes, banner);
    if let Some(me) = vector_core::state::my_public_key().and_then(|k| nostr_sdk::prelude::ToBech32::to_bech32(&k).ok()) {
        if banner {
            super::images::cache_profile_images(&me, "", &url).await;
        } else {
            super::images::cache_profile_images(&me, &url, "").await;
        }
    }
    let (avatar, banner_url) = if banner { (String::new(), url.clone()) } else { (url.clone(), String::new()) };
    if !vector_core::profile::sync::update_profile(String::new(), avatar, banner_url, String::new(), &super::sync::ProfileHandler).await {
        return Err("Failed to broadcast the update to the network.".into());
    }
    Ok(url)
}
