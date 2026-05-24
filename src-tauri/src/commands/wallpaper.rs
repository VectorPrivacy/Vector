//! Per-DM wallpaper Tauri commands — thin shims over `vector_core::wallpaper`.

use serde::Serialize;
use vector_core::wallpaper;

#[derive(Serialize)]
pub struct WallpaperPreviewResult {
    pub path: String,
    pub was_animated: bool,
    pub recommended_dim: u8,
}

/// Validate + prepare a picked image and return the local cached path for
/// the chat to display while the user decides whether to confirm. Accepts
/// EITHER a file_path (desktop Tauri dialog) OR raw bytes (Android, where
/// the WebView file input only gives us a Blob).
#[tauri::command]
pub async fn preview_wallpaper(
    chat_id: String,
    file_path: Option<String>,
    bytes: Option<Vec<u8>>,
    filename: Option<String>,
) -> Result<WallpaperPreviewResult, String> {
    let preview = if let Some(path) = file_path {
        wallpaper::prepare_wallpaper_preview(&chat_id, &path)?
    } else if let Some(buf) = bytes {
        // Stage bytes into a temp file the validator can mmap. The temp file
        // is unlinked immediately after — only the preview slot under
        // wallpapers/ lives on past this call.
        let safe_name = filename
            .as_deref()
            .unwrap_or("wallpaper-pick")
            .replace(['/', '\\', '\0'], "_");
        let tmp = std::env::temp_dir().join(format!("vector-wallpaper-{}", safe_name));
        std::fs::write(&tmp, &buf)
            .map_err(|e| format!("Failed to stage wallpaper bytes: {}", e))?;
        let result = wallpaper::prepare_wallpaper_preview(&chat_id, &tmp.to_string_lossy());
        let _ = std::fs::remove_file(&tmp);
        result?
    } else {
        return Err("preview_wallpaper needs either file_path or bytes".to_string());
    };
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
