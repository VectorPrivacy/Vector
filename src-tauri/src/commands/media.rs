//! Media-related Tauri commands.
//!
//! This module handles:
//! - Voice recording (start/stop)
//! - Audio transcription via Whisper (platform-specific)

use tauri::{AppHandle, Runtime};

use crate::voice::AudioRecorder;

#[cfg(target_os = "android")]
use crate::android;

#[cfg(feature = "whisper")]
use crate::{audio, whisper};

// ============================================================================
// Voice Recording Commands
// ============================================================================

/// Start audio recording
#[tauri::command]
pub async fn start_recording() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        // Check if we already have permission
        if !android::permissions::check_audio_permission().unwrap() {
            // This will block until the user responds to the permission dialog
            let granted = android::permissions::request_audio_permission_blocking()?;

            if !granted {
                return Err("Audio permission denied by user".to_string());
            }
        }
    }

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
    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err("File not found".to_string());
    }

    // Decode to mono 16kHz for Whisper (fast resample — whisper doesn't need audiophile quality)
    let t0 = std::time::Instant::now();
    let audio_data = audio::decode_for_whisper(path)
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
        Err(e) => Err(format!("Model Download error: {}", e.to_string())),
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

// Handler list for this module (for reference):
// - start_recording
// - stop_recording
// - transcribe (platform-specific)
// - download_whisper_model (platform-specific)
