//! Audio engine Tauri command handlers.
//!
//! This module handles:
//! - Loading audio files into the engine
//! - Playback control (play/pause/seek/stop)
//! - Volume control
//! - Sending voice recordings without IPC audio data transfer

use crate::audio_engine::{self, AudioEngine, AudioLoadResult};

/// Probe an audio file for its duration without loading it for playback.
/// Fast: reads file headers only, no decoding.
#[tauri::command]
pub async fn audio_probe(path: String) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || {
        audio_engine::probe_duration(&path)
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

/// Load an audio file into the engine for playback.
/// Decodes, resamples, and precomputes FFT waveform.
/// Returns source ID, duration, and waveform data.
#[tauri::command]
pub async fn audio_load(path: String) -> Result<AudioLoadResult, String> {
    // Run decode/resample on blocking thread to avoid blocking the async runtime
    tokio::task::spawn_blocking(move || {
        AudioEngine::get()?.load_from_file(&path)
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

/// Start playback of a loaded source.
/// Returns current position_ms for frontend sync.
#[tauri::command]
pub fn audio_play(id: u32) -> Result<u64, String> {
    AudioEngine::get()?.play(id)
}

/// Pause playback of a source.
/// Returns paused position_ms.
#[tauri::command]
pub fn audio_pause(id: u32) -> Result<u64, String> {
    AudioEngine::get()?.pause(id)
}

/// Seek to a position in milliseconds.
#[tauri::command]
pub fn audio_seek(id: u32, position_ms: u64) -> Result<(), String> {
    AudioEngine::get()?.seek(id, position_ms)
}

/// Stop and remove a source, freeing memory.
#[tauri::command]
pub fn audio_stop(id: u32) -> Result<(), String> {
    AudioEngine::get()?.stop(id)
}

/// Stop and remove all non-oneshot sources (e.g. when leaving a chat).
#[tauri::command]
pub fn audio_stop_all() -> Result<(), String> {
    AudioEngine::get()?.stop_all()
}

/// Set volume for a source (0.0–1.0).
#[tauri::command]
pub fn audio_set_volume(id: u32, volume: f32) -> Result<(), String> {
    AudioEngine::get()?.set_volume(id, volume)
}

/// Send a pending voice recording without passing audio data over IPC.
/// Encodes WAV from stashed i16 samples and sends via existing voice_message path.
#[tauri::command]
pub async fn send_recording(receiver: String, replied_to: String) -> Result<crate::message::MessageSendResult, String> {
    use crate::voice::AudioRecorder;

    let pending = AudioRecorder::global()
        .take_pending()
        .ok_or("No pending recording")?;

    // Stop the engine source (preview no longer needed)
    let _ = AudioEngine::get().map(|e| e.stop(pending.source_id));

    // Encode WAV from stashed i16 samples
    let wav_bytes = pending.encode_wav()?;

    // Send via existing voice_message path
    crate::message::voice_message(receiver, replied_to, wav_bytes).await
}

/// Metadata extracted from an audio file's tags (ID3, Vorbis, MP4 atoms).
#[derive(serde::Serialize)]
pub struct AudioMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Base64 data URI for embedded cover art, e.g. "data:image/jpeg;base64,..."
    pub cover_art: Option<String>,
}

/// Extract metadata (title, artist, album, cover art) from an audio file's tags.
/// Returns `None` if the file has no useful metadata (no title AND no cover art).
#[tauri::command]
pub async fn get_audio_metadata(path: String) -> Result<Option<AudioMetadata>, String> {
    tokio::task::spawn_blocking(move || {
        use lofty::file::TaggedFileExt;
        use lofty::tag::Accessor;

        let tagged_file = lofty::read_from_path(&path)
            .map_err(|e| format!("Failed to read tags: {}", e))?;

        let tag = match tagged_file.primary_tag().or_else(|| tagged_file.first_tag()) {
            Some(t) => t,
            None => return Ok(None),
        };

        let title = tag.title().map(|s| s.to_string());
        let artist = tag.artist().map(|s| s.to_string());
        let album = tag.album().map(|s| s.to_string());

        // Extract first embedded picture as base64 data URI
        let cover_art = tag.pictures().first().map(|pic| {
            let mime = pic.mime_type().unwrap_or(&lofty::picture::MimeType::Jpeg);
            let mime_str = match mime {
                lofty::picture::MimeType::Png => "image/png",
                lofty::picture::MimeType::Bmp => "image/bmp",
                lofty::picture::MimeType::Gif => "image/gif",
                lofty::picture::MimeType::Tiff => "image/tiff",
                _ => "image/jpeg",
            };
            let b64 = base64_simd::STANDARD.encode_to_string(pic.data());
            format!("data:{};base64,{}", mime_str, b64)
        });

        // Only return if there's something useful to display
        if title.is_none() && cover_art.is_none() {
            return Ok(None);
        }

        Ok(Some(AudioMetadata { title, artist, album, cover_art }))
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

// Handler list for this module:
// - audio_load
// - audio_play
// - audio_pause
// - audio_seek
// - audio_stop
// - audio_stop_all
// - audio_set_volume
// - send_recording
// - get_audio_metadata
