//! Media-related Tauri commands.
//!
//! This module handles:
//! - Voice recording (start/stop)
//! - Audio transcription via Whisper (platform-specific)

use tauri::{AppHandle, Runtime};

use crate::voice::AudioRecorder;

#[cfg(feature = "whisper")]
use crate::{audio, whisper};

// ============================================================================
// Voice Recording Commands
// ============================================================================

/// Start audio recording
#[tauri::command]
pub async fn start_recording() -> Result<(), String> {
    crate::mic_access::require().await?;
    AudioRecorder::global().start()
}

/// Stop audio recording and load into audio engine for preview.
/// Returns source ID, duration, and precomputed waveform data.
#[tauri::command]
pub async fn stop_recording() -> Result<crate::audio_engine::AudioLoadResult, String> {
    AudioRecorder::global().stop()
}

// ============================================================================
// Transcription Commands (Whisper)
// ============================================================================

/// Transcribe audio file using Whisper model
#[cfg(feature = "whisper")]
#[tauri::command]
pub async fn transcribe<R: Runtime>(
    handle: AppHandle<R>,
    file_path: String,
    model_name: String,
    translate: bool,
) -> Result<whisper::TranscriptionResult, String> {
    let input = crate::audio_engine::AudioInput::resolve(&file_path).map_err(|_| "File not found".to_string())?;

    // Decode to mono 16kHz for Whisper (fast resample — whisper doesn't need audiophile quality)
    let t0 = std::time::Instant::now();
    let audio_data = audio::decode_for_whisper(&input)
        .map_err(|e| format!("Audio processing error: {}", e))?;
    println!("[Whisper]   audio decode:  {:>10?}  ({} samples, {:.1}s)",
        t0.elapsed(), audio_data.len(), audio_data.len() as f64 / 16000.0);

    whisper::transcribe(&handle, &model_name, translate, audio_data).await
        .map_err(|e| format!("Transcription error: {}", e))
}

/// Transcribe audio file (stub for unsupported platforms)
#[cfg(not(feature = "whisper"))]
#[tauri::command]
pub async fn transcribe<R: Runtime>(
    _handle: AppHandle<R>,
    _file_path: String,
    _model_name: String,
    _translate: bool,
) -> Result<String, String> {
    Err("Whisper transcription is not supported on this platform".to_string())
}

/// Dictation: the recording waiting in the preview, as text. Never translated, as the
/// words are the user's own message.
#[cfg(feature = "whisper")]
#[tauri::command]
pub async fn transcribe_recording<R: Runtime>(
    handle: AppHandle<R>,
    model_name: String,
) -> Result<whisper::TranscriptionResult, String> {
    let audio_data = AudioRecorder::global().pending_for_whisper()?;
    whisper::transcribe(&handle, &model_name, false, audio_data).await
        .map_err(|e| format!("Transcription error: {}", e))
}

#[cfg(not(feature = "whisper"))]
#[tauri::command]
pub async fn transcribe_recording<R: Runtime>(
    _handle: AppHandle<R>,
    _model_name: String,
) -> Result<String, String> {
    Err("Whisper transcription is not supported on this platform".to_string())
}

/// Download a Whisper model for transcription
#[cfg(feature = "whisper")]
#[tauri::command]
pub async fn download_whisper_model<R: Runtime>(
    handle: AppHandle<R>,
    model_name: String,
) -> Result<String, String> {
    // Download (or simply return the cached path of) a Whisper Model
    match whisper::download_whisper_model(&handle, &model_name).await {
        Ok(path) => Ok(path),
        Err(e) => Err(format!("Model Download error: {}", e)),
    }
}

/// Download a Whisper model (stub for unsupported platforms)
#[cfg(not(feature = "whisper"))]
#[tauri::command]
pub async fn download_whisper_model<R: Runtime>(
    _handle: AppHandle<R>,
    _model_name: String,
) -> Result<String, String> {
    Err("Whisper model download is not supported on this platform".to_string())
}

/// A still for the animated image `src` (a `vmedia:` ref or a cached file), as a `vmedia:` ref or
/// a path, or None when it doesn't animate: the page shows it whenever the image may not move.
#[tauri::command]
pub async fn image_still(src: String) -> Result<Option<String>, String> {
    vector_core::db::blocking_bound(move || crate::image_cache::still_for(&src)).await?
}

/// A copy of the picture `src` (a `vmedia:` ref or a cached file) at most `dim` on its longest
/// side, as a `vmedia:` ref or a path, or None to show the original: a chat bubble never draws a
/// photo larger.
#[tauri::command]
pub async fn image_display(src: String, dim: u32) -> Result<Option<String>, String> {
    let dim = dim.clamp(256, 4096);
    let probe = src.clone();
    if let Some(kept) = vector_core::db::blocking_bound(move || crate::image_cache::display_kept(&probe, dim)).await? {
        return Ok(Some(kept));
    }
    let _turn = crate::image_cache::DISPLAY_COPIES.acquire().await.map_err(|e| e.to_string())?;
    vector_core::db::blocking_bound(move || crate::image_cache::display_for(&src, dim)).await?
}

// Handler list for this module (for reference):
// - start_recording
// - stop_recording
// - transcribe (platform-specific)
// - transcribe_recording (platform-specific)
// - download_whisper_model (platform-specific)
// - image_still
// - image_display
