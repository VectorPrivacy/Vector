//! Per-DM wallpaper Tauri commands — thin shims over `vector_core::wallpaper`.

use serde::Serialize;
use vector_core::wallpaper;

#[derive(Serialize)]
pub struct WallpaperPreviewResult {
    pub path: String,
    pub was_animated: bool,
    pub recommended_dim: u8,
}

/// Validate + prepare a picked image and return the local cached path for the
/// chat to display while the user decides whether to confirm. `file_path` is a
/// real filesystem path on desktop, or a `content://` URI on Android (which we
/// read natively — the WebView's `file.arrayBuffer()` returns nothing for those).
#[tauri::command]
pub async fn preview_wallpaper(
    chat_id: String,
    file_path: String,
) -> Result<WallpaperPreviewResult, String> {
    // Android content:// URIs aren't filesystem paths — read them natively
    // (ContentResolver) up front, since prepare_wallpaper_preview opens a path.
    #[cfg(target_os = "android")]
    let staged_bytes = if file_path.starts_with("content://") {
        Some(crate::android::filesystem::read_android_uri_bytes(file_path.clone())?.0)
    } else {
        None
    };
    #[cfg(not(target_os = "android"))]
    let staged_bytes: Option<Vec<u8>> = None;

    // decode + resize + re-encode is CPU-bound (hundreds of ms on a slow device),
    // so run it on a blocking thread — the frontend paints an overlay meanwhile.
    let preview = tokio::task::spawn_blocking(move || {
        if let Some(bytes) = staged_bytes {
            // Stage into a temp file the validator can open; unlink after.
            let tmp = std::env::temp_dir().join("vector-wallpaper-pick");
            std::fs::write(&tmp, &bytes)
                .map_err(|e| format!("Failed to stage wallpaper bytes: {}", e))?;
            let result = wallpaper::prepare_wallpaper_preview(&chat_id, &tmp.to_string_lossy());
            let _ = std::fs::remove_file(&tmp);
            result
        } else {
            wallpaper::prepare_wallpaper_preview(&chat_id, &file_path)
        }
    })
    .await
    .map_err(|e| format!("Image processing task failed: {e}"))??;
    Ok(WallpaperPreviewResult {
        path: preview.path,
        was_animated: preview.was_animated,
        recommended_dim: preview.recommended_dim,
    })
}

/// Publish the previously-prepared preview file as the chat's wallpaper.
/// `blur` and `dim` are the slider values from the preview UI (0..=30 and
/// 0..=100 respectively). They're carried as optional tags on the rumor so
/// every receiver applies the exact same visual settings.
#[tauri::command]
pub async fn publish_wallpaper(
    chat_id: String,
    blur: u8,
    dim: u8,
) -> Result<(), String> {
    wallpaper::publish_wallpaper(&chat_id, blur, dim).await
}

/// Drop the staged preview file (user cancelled).
#[tauri::command]
pub async fn cancel_wallpaper_preview(chat_id: String) -> Result<(), String> {
    wallpaper::cancel_wallpaper_preview(&chat_id)
}

/// Remove the chat's wallpaper, reverting both sides to the default theme.
/// Publishes a removal tombstone so the recipient + our other devices clear
/// it too, then DELETEs our blob and wipes local state.
#[tauri::command]
pub async fn remove_wallpaper(chat_id: String) -> Result<(), String> {
    wallpaper::remove_wallpaper(&chat_id).await
}
