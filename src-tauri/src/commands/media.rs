//! Media-related Tauri commands.
//!
//! This module handles:
//! - Voice recording (start/stop)
//! - Audio transcription via Whisper (platform-specific)

use tauri::{AppHandle, Runtime};

use crate::voice::AudioRecorder;

#[cfg(target_os = "android")]
use crate::android;

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
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

/// Stop audio recording and return the recorded audio data
#[tauri::command]
pub async fn stop_recording() -> Result<Vec<u8>, String> {
    AudioRecorder::global().stop()
}

// ============================================================================
// Transcription Commands (Whisper)
// ============================================================================

/// Transcribe audio file using Whisper model
#[cfg(all(not(target_os = "android"), feature = "whisper"))]
#[tauri::command]
pub async fn transcribe<R: Runtime>(
    handle: AppHandle<R>,
    file_path: String,
    model_name: String,
    translate: bool,
) -> Result<whisper::TranscriptionResult, String> {
    // Convert the file path to a Path
    let path = std::path::Path::new(&file_path);

    // Check if the file exists
    if !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    // Decode and resample to 16kHz for Whisper
    match audio::decode_and_resample(path, 16000) {
        Ok(audio_data) => {
            // Pass the resampled audio to the whisper transcribe function
            match whisper::transcribe(&handle, &model_name, translate, audio_data).await {
                Ok(result) => Ok(result),
                Err(e) => Err(format!("Transcription error: {}", e.to_string())),
            }
        }
        Err(e) => Err(format!("Audio processing error: {}", e.to_string())),
    }
}

/// Transcribe audio file (stub for unsupported platforms)
#[cfg(any(target_os = "android", not(feature = "whisper")))]
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
#[cfg(all(not(target_os = "android"), feature = "whisper"))]
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
#[cfg(any(target_os = "android", not(feature = "whisper")))]
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
