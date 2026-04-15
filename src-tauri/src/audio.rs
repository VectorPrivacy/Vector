//! Audio processing module for Vector
//!
//! Provides centralized audio functionality:
//! - Audio decoding (symphonia) - supports mp3, wav, flac, ogg, m4a
//! - Audio resampling (SIMD linear interpolation) - NEON/SSE accelerated
//! - Audio playback (cpal) - cross-platform output (desktop only)
//!
//! Used by: notification sounds (desktop), voice recording, whisper transcription

// Shared import for audio file decoding (all platforms - used by whisper on iOS)
use std::fs::File;

// Desktop-only imports for notification sound playback
#[cfg(desktop)]
use cpal::traits::{DeviceTrait, HostTrait};
#[cfg(desktop)]
use serde::{Deserialize, Serialize};
#[cfg(desktop)]
use std::io::{Read, Write};
#[cfg(desktop)]
use std::path::PathBuf;
#[cfg(desktop)]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(desktop)]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(desktop)]
use std::time::{Duration, Instant};
#[cfg(desktop)]
use tauri::{command, AppHandle, Manager, Runtime};
#[cfg(desktop)]
use crate::db;

use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// ============================================================================
// Notification Sound Cache (Desktop Only)
// ============================================================================

#[cfg(desktop)]
/// Cache TTL - samples expire 10 minutes after last notification
const CACHE_TTL_SECS: u64 = 600;

#[cfg(desktop)]
/// Cached device sample rate (atomic for lock-free reads)
static CACHED_DEVICE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);

#[cfg(desktop)]
/// In-memory cache for decoded and resampled notification samples
struct SoundCache {
    /// Pre-resampled samples ready to play (at device sample rate)
    samples: Option<Arc<Vec<f32>>>,
    /// Which sound is currently cached
    cached_sound: Option<NotificationSound>,
    /// When the cache was last used (for TTL expiry)
    last_used: Option<Instant>,
    /// Device sample rate the cached samples are resampled to
    cached_at_rate: u32,
}

#[cfg(desktop)]
impl Default for SoundCache {
    fn default() -> Self {
        Self {
            samples: None,
            cached_sound: None,
            last_used: None,
            cached_at_rate: 0,
        }
    }
}

#[cfg(desktop)]
/// Global sound cache (protected by mutex)
static SOUND_CACHE: OnceLock<Mutex<SoundCache>> = OnceLock::new();

#[cfg(desktop)]
fn get_sound_cache() -> &'static Mutex<SoundCache> {
    SOUND_CACHE.get_or_init(|| Mutex::new(SoundCache::default()))
}

#[cfg(desktop)]
/// Get the cached device sample rate, or query and cache it
fn get_device_sample_rate() -> Result<u32, String> {
    // Try to read cached rate first (lock-free)
    let cached = CACHED_DEVICE_SAMPLE_RATE.load(Ordering::Relaxed);
    if cached != 0 {
        return Ok(cached);
    }

    // Query the device
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No output device found")?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("Failed to get output config: {}", e))?;

    let rate = config.sample_rate().0;

    // Cache it (relaxed ordering is fine for this)
    CACHED_DEVICE_SAMPLE_RATE.store(rate, Ordering::Relaxed);

    Ok(rate)
}

#[cfg(desktop)]
/// Clear the device sample rate cache (call when audio device might have changed)
pub fn invalidate_device_sample_rate_cache() {
    CACHED_DEVICE_SAMPLE_RATE.store(0, Ordering::Relaxed);
}

#[cfg(desktop)]
/// Purge the in-memory sound cache
pub fn purge_sound_cache() {
    if let Ok(mut cache) = get_sound_cache().lock() {
        #[cfg(debug_assertions)]
        let bytes_freed = cache.samples.as_ref().map(|s| s.len() * 4).unwrap_or(0);

        // Drop the Arc - if this is the last reference, memory is freed
        cache.samples = None;
        cache.cached_sound = None;
        cache.last_used = None;

        #[cfg(debug_assertions)]
        if bytes_freed > 0 {
            println!(
                "[Maintenance] Purged notification sound cache (~{} KB freed)",
                bytes_freed / 1024
            );
        }
    }
}

#[cfg(desktop)]
/// Check and purge cache if TTL expired
pub fn check_cache_ttl() {
    if let Ok(mut cache) = get_sound_cache().lock() {
        if let Some(last_used) = cache.last_used {
            if last_used.elapsed() > Duration::from_secs(CACHE_TTL_SECS) {
                #[cfg(debug_assertions)]
                let bytes_freed = cache.samples.as_ref().map(|s| s.len() * 4).unwrap_or(0);
                #[cfg(debug_assertions)]
                let elapsed_secs = last_used.elapsed().as_secs();

                // Drop the Arc - if this is the last reference, memory is freed
                cache.samples = None;
                cache.cached_sound = None;
                cache.last_used = None;

                #[cfg(debug_assertions)]
                if bytes_freed > 0 {
                    println!(
                        "[Maintenance] TTL expired ({}s idle) - Purged notification sound cache (~{} KB freed)",
                        elapsed_secs,
                        bytes_freed / 1024
                    );
                }
            }
        }
    }
}

// ============================================================================
// Sound Cache Directory & File I/O (Desktop Only)
// ============================================================================

#[cfg(desktop)]
/// Get the sounds cache directory (cache/sounds/)
fn get_sound_cache_dir<R: Runtime>(handle: &AppHandle<R>) -> Result<PathBuf, String> {
    let app_data = handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    Ok(app_data.join("cache").join("sounds"))
}

#[cfg(desktop)]
/// Save samples to a .raw file
fn save_raw_samples(path: &Path, samples: &[f32]) -> Result<(), String> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create cache dir: {}", e))?;
    }

    // Write raw f32 samples as bytes
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();

    let mut file = File::create(path)
        .map_err(|e| format!("Failed to create cache file: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Failed to write cache file: {}", e))?;

    Ok(())
}

#[cfg(desktop)]
/// Load samples from a .raw file
fn load_raw_samples(path: &Path) -> Result<Vec<f32>, String> {
    let mut file = File::open(path)
        .map_err(|e| format!("Failed to open cache file: {}", e))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read cache file: {}", e))?;

    // Convert bytes back to f32 samples
    if bytes.len() % 4 != 0 {
        return Err("Invalid cache file size".to_string());
    }

    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    Ok(samples)
}

#[cfg(desktop)]
/// Represents the notification sound choice
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "path")]
pub enum NotificationSound {
    /// Default built-in sound - Prélude (notif-prelude.mp3)
    Default,
    /// Techno ping sound (notif-techno.mp3)
    Techno,
    /// No sound (silent)
    None,
    /// Custom user-selected sound file
    Custom(String),
}

#[cfg(desktop)]
impl Default for NotificationSound {
    fn default() -> Self {
        Self::Default
    }
}

#[cfg(desktop)]
/// Notification settings stored in the database
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Whether all notification sounds are muted globally
    pub global_mute: bool,
    /// The selected notification sound
    pub sound: NotificationSound,
    /// Whether @everyone pings are muted
    pub mute_everyone: bool,
}

#[cfg(desktop)]
impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            global_mute: false,
            sound: NotificationSound::Default,
            mute_everyone: false,
        }
    }
}

#[cfg(desktop)]
/// Decode an audio file into mono f32 samples (notification sounds)
///
/// Used by: notification sound playback
/// Returns (mono_samples, sample_rate)
fn decode_audio_file(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let (samples, sample_rate, _channels) = decode_audio_internal(path, true)?;
    Ok((samples, sample_rate))
}

// ============================================================================
// Audio Resampling (public API for use by other modules)
// ============================================================================

/// Resample mono f32 audio samples to a target sample rate (SIMD-accelerated)
///
/// Used by: notification sounds, general audio processing
#[allow(dead_code)]
pub fn resample_mono_f32(samples: Vec<f32>, from_rate: u32, to_rate: u32) -> Result<Vec<f32>, String> {
    if from_rate == to_rate {
        return Ok(samples);
    }
    Ok(crate::simd::audio::linear_resample_mono(&samples, from_rate, to_rate))
}

/// Resample mono i16 audio samples to a target sample rate (SIMD-accelerated)
///
/// Used by: voice recording (i16 → f32 resample → i16, all SIMD)
pub fn resample_mono_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Result<Vec<i16>, String> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }

    // SIMD i16 → f32 (normalized to -1.0..1.0)
    // Safety: i16 is 2 bytes, reinterpret as raw bytes for SIMD conversion.
    // This assumes little-endian byte order (true for all target platforms).
    #[cfg(not(target_endian = "little"))]
    compile_error!("i16-to-bytes reinterpret requires little-endian target");
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 2)
    };
    let samples_f32 = crate::simd::audio::i16_le_bytes_to_f32_mono(bytes);

    // SIMD resample
    let resampled_f32 = crate::simd::audio::linear_resample_mono(&samples_f32, from_rate, to_rate);

    // SIMD f32 → i16
    Ok(crate::simd::audio::f32_to_i16(&resampled_f32))
}

/// Decode audio for whisper: mono f32 at 16kHz, as fast as possible.
///
/// Fast path (WAV PCM): Bypasses Symphonia entirely — parses the RIFF header
/// directly and converts raw PCM bytes to f32 mono in a single pass. ~5ms.
///
/// Slow path (MP3/OGG/FLAC): Pre-reads file into RAM, Symphonia decode with
/// inline mono mixdown, pre-allocated buffers. ~180ms.
///
/// Both paths use rayon parallel SIMD linear interpolation resampling to 16kHz. ~2ms.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
pub fn decode_for_whisper(path: &Path) -> Result<Vec<f32>, String> {
    use rayon::prelude::*;

    let t0 = std::time::Instant::now();

    let file_bytes = std::fs::read(path)
        .map_err(|e| format!("Failed to read audio file: {}", e))?;

    // Try WAV fast path first — raw PCM byte conversion, no codec overhead
    let (mono_samples, sample_rate) = match wav_fast_decode(&file_bytes, 16000) {
        Some(result) => {
            let t_decode = t0.elapsed();
            println!("[Whisper]     decode: {:?} (WAV fast path)", t_decode);
            result
        }
        None => {
            // Symphonia needs owned Vec<u8>
            let result = symphonia_decode_mono(file_bytes, path)?;
            let t_decode = t0.elapsed();
            println!("[Whisper]     decode: {:?} (Symphonia)", t_decode);
            result
        }
    };

    if sample_rate == 16000 {
        println!("[Whisper]     resample: skipped (already 16kHz)");
        return Ok(mono_samples);
    }

    // Fast path: exact integer-ratio decimation (48kHz→16kHz = 3:1, 32kHz→16kHz = 2:1, etc.)
    // Simple box-filter average — adequate for speech; avoids all resampler library overhead.
    let t1 = std::time::Instant::now();
    let ratio_int = sample_rate / 16000;
    if ratio_int >= 2 && ratio_int * 16000 == sample_rate {
        let n = ratio_int as usize;
        let out_len = mono_samples.len() / n;
        let mut result = Vec::with_capacity(out_len + 1);
        let scale = 1.0 / n as f32;
        for chunk in mono_samples.chunks_exact(n) {
            let sum: f32 = chunk.iter().sum();
            result.push(sum * scale);
        }
        let remainder = mono_samples.chunks_exact(n).remainder();
        if !remainder.is_empty() {
            let sum: f32 = remainder.iter().sum();
            result.push(sum / remainder.len() as f32);
        }
        println!("[Whisper]     resample: {:?} ({}:1 integer decimation, {}Hz→16kHz)",
            t1.elapsed(), n, sample_rate);
        return Ok(result);
    }

    // Parallel SIMD linear interpolation — each rayon thread runs the SIMD lerp loop
    // on its chunk. Combines parallelism (fast in debug) with SIMD (fast in release).
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get()).unwrap_or(4);
    let chunk_size = (mono_samples.len() / n_threads).max(16000);

    let resampled: Vec<Vec<f32>> = mono_samples
        .par_chunks(chunk_size)
        .map(|chunk| {
            crate::simd::audio::linear_resample_mono(chunk, sample_rate, 16000)
        })
        .collect();

    let mut result = Vec::with_capacity(resampled.iter().map(|v| v.len()).sum());
    for chunk in resampled {
        result.extend_from_slice(&chunk);
    }
    println!("[Whisper]     resample: {:?} ({}Hz→16kHz, SIMD linear interp, {} threads)",
        t1.elapsed(), sample_rate, n_threads);

    Ok(result)
}

/// WAV fast path: parse RIFF header + convert raw PCM bytes to mono f32.
/// Returns None for non-WAV, non-PCM, or unsupported bit depths (falls back to Symphonia).
fn wav_fast_decode(bytes: &[u8], target_rate: u32) -> Option<(Vec<f32>, u32)> {
    // Validate RIFF/WAVE header
    if bytes.len() < 12 { return None; }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" { return None; }

    let mut pos = 12;
    let mut audio_format = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits_per_sample = 0u16;

    // Scan chunks (handles LIST, fact, etc. between fmt and data)
    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[pos + 4..pos + 8].try_into().ok()?
        ) as usize;

        if chunk_id == b"fmt " {
            if chunk_size < 16 || pos + 8 + 16 > bytes.len() { return None; }
            let d = &bytes[pos + 8..];
            audio_format = u16::from_le_bytes([d[0], d[1]]);
            channels = u16::from_le_bytes([d[2], d[3]]);
            sample_rate = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
            bits_per_sample = u16::from_le_bytes([d[14], d[15]]);
        } else if chunk_id == b"data" {
            // Only handle uncompressed PCM (format 1) or IEEE float (format 3)
            if audio_format != 1 && audio_format != 3 { return None; }
            if channels == 0 || sample_rate == 0 { return None; }

            let data_end = (pos + 8 + chunk_size).min(bytes.len());
            let data = &bytes[pos + 8..data_end];
            let ch = channels as usize;

            let mono = match (audio_format, bits_per_sample) {
                // 16-bit PCM (most common for voice recordings)
                (1, 16) => {
                    // Check if exact integer decimation to target rate is possible
                    // (e.g., 48kHz→16kHz = 3:1, 32kHz→16kHz = 2:1)
                    // If so, skip fused path — regular SIMD + integer decimation in caller is faster
                    let (_exact_ratio, is_exact) = if target_rate > 0 {
                        let r = sample_rate / target_rate;
                        (r, r >= 2 && r * target_rate == sample_rate)
                    } else {
                        (0, false)
                    };

                    // SIMD fused: stereo→mono + 2:1 decimation in one pass
                    // Only when exact integer decimation isn't possible (e.g., 44.1kHz→22.05kHz)
                    // Halves the output allocation (2.3MB vs 4.6MB) — fewer page faults
                    if ch == 2 && !is_exact && target_rate > 0 && sample_rate / target_rate >= 2 {
                        let output_rate = sample_rate / 2;
                        return Some((crate::simd::audio::i16_le_stereo_decimate2_mono(data), output_rate));
                    }

                    // Regular SIMD path (no decimation possible, or mono input)
                    if ch == 1 {
                        return Some((crate::simd::audio::i16_le_bytes_to_f32_mono(data), sample_rate));
                    } else if ch == 2 {
                        return Some((crate::simd::audio::i16_le_stereo_to_f32_mono(data), sample_rate));
                    }
                    // 3+ channels: scalar fallback
                    let frame_count = data.len() / (ch * 2);
                    let mut out = Vec::with_capacity(frame_count);
                    for frame in data.chunks_exact(ch * 2) {
                        let mut sum = 0i32;
                        for c in 0..ch {
                            sum += i16::from_le_bytes([frame[c * 2], frame[c * 2 + 1]]) as i32;
                        }
                        out.push(sum as f32 / (ch as f32 * 32768.0));
                    }
                    out
                }
                // 32-bit float PCM
                (3, 32) => {
                    let frame_count = data.len() / (ch * 4);
                    let mut out = Vec::with_capacity(frame_count);
                    if ch == 1 {
                        for quad in data.chunks_exact(4) {
                            out.push(f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]));
                        }
                    } else {
                        for frame in data.chunks_exact(ch * 4) {
                            let mut sum = 0.0f32;
                            for c in 0..ch {
                                sum += f32::from_le_bytes([
                                    frame[c * 4], frame[c * 4 + 1],
                                    frame[c * 4 + 2], frame[c * 4 + 3],
                                ]);
                            }
                            out.push(sum / ch as f32);
                        }
                    }
                    out
                }
                // 24-bit PCM
                (1, 24) => {
                    let frame_count = data.len() / (ch * 3);
                    let mut out = Vec::with_capacity(frame_count);
                    let scale = 1.0 / 8388608.0; // 2^23
                    if ch == 1 {
                        for triple in data.chunks_exact(3) {
                            let s = ((triple[0] as i32) | ((triple[1] as i32) << 8)
                                | ((triple[2] as i32) << 16)) << 8 >> 8; // sign-extend
                            out.push(s as f32 * scale);
                        }
                    } else {
                        for frame in data.chunks_exact(ch * 3) {
                            let mut sum = 0i32;
                            for c in 0..ch {
                                let b = &frame[c * 3..];
                                sum += ((b[0] as i32) | ((b[1] as i32) << 8)
                                    | ((b[2] as i32) << 16)) << 8 >> 8;
                            }
                            out.push(sum as f32 * scale / ch as f32);
                        }
                    }
                    out
                }
                _ => return None, // Unsupported bit depth — fall back to Symphonia
            };

            return Some((mono, sample_rate));
        }

        // Advance to next chunk (WAV chunks are 2-byte aligned)
        pos += 8 + chunk_size;
        if chunk_size % 2 != 0 { pos += 1; }
    }

    None
}

/// Symphonia slow path: decode compressed audio (MP3, OGG, FLAC, etc.) to mono f32.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
fn symphonia_decode_mono(file_bytes: Vec<u8>, path: &Path) -> Result<(Vec<f32>, u32), String> {
    let cursor = std::io::Cursor::new(file_bytes);
    let media_source = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, media_source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Failed to probe audio format: {}", e))?;

    let mut format = probed.format;
    let track = format.tracks().iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No supported audio tracks found")?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let channels = track.codec_params.channels.ok_or("Unknown channel count")?.count();

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    let estimated_frames = track.codec_params.n_frames.unwrap_or(sample_rate as u64 * 120);
    let mut mono_samples = Vec::with_capacity(estimated_frames as usize);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::ResetRequired) => { decoder.reset(); continue; }
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        };

        if packet.track_id() != track_id { continue; }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    sample_buf = Some(SampleBuffer::<f32>::new(
                        audio_buf.capacity() as u64, *audio_buf.spec()));
                }
                if let Some(ref mut buf) = sample_buf {
                    buf.copy_interleaved_ref(audio_buf);
                    let samples = buf.samples();
                    if channels > 1 {
                        for chunk in samples.chunks_exact(channels) {
                            let sum: f32 = chunk.iter().sum();
                            mono_samples.push(sum / channels as f32);
                        }
                    } else {
                        mono_samples.extend_from_slice(samples);
                    }
                }
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }

    Ok((mono_samples, sample_rate))
}

/// Internal: Decode audio file with options
///
/// - `to_mono`: If true, multi-channel audio is mixed down to mono
/// - Returns: (samples, sample_rate, channel_count)
///   - When `to_mono` is true, channel_count is always 1
#[allow(dead_code)]
fn decode_audio_internal(path: &Path, to_mono: bool) -> Result<(Vec<f32>, u32, usize), String> {
    let file = File::open(path).map_err(|e| format!("Failed to open audio file: {}", e))?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension() {
        if let Some(extension_str) = extension.to_str() {
            hint.with_extension(extension_str);
        }
    }

    let format_opts = FormatOptions::default();
    let meta_opts = MetadataOptions::default();
    let probed = symphonia::default::get_probe()
        .format(&hint, media_source, &format_opts, &meta_opts)
        .map_err(|e| format!("Failed to probe audio format: {}", e))?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No supported audio tracks found")?;

    let track_id = track.id;
    let codec_params = &track.codec_params;

    let decoder_opts = DecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(codec_params, &decoder_opts)
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    let sample_rate = codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let channels = codec_params.channels.ok_or("Unknown channel count")?.count();

    let mut all_samples = Vec::new();
    let mut sample_buf = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    let spec = *audio_buf.spec();
                    let duration = audio_buf.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                }

                if let Some(ref mut buf) = sample_buf {
                    buf.copy_interleaved_ref(audio_buf);
                    all_samples.extend_from_slice(buf.samples());
                }
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }

    // Convert to mono if requested
    if to_mono && channels > 1 {
        let mono_samples: Vec<f32> = all_samples
            .chunks(channels)
            .map(|chunk| {
                let sum: f32 = chunk.iter().sum();
                sum / channels as f32
            })
            .collect();
        Ok((mono_samples, sample_rate, 1))
    } else {
        Ok((all_samples, sample_rate, channels))
    }
}

/// Decode audio file returning raw interleaved samples with metadata (preserves channels)
///
/// Used by: whisper transcription
#[allow(dead_code)]
fn decode_audio_file_raw(path: &Path) -> Result<(Vec<f32>, u32, usize), String> {
    decode_audio_internal(path, false)
}

// ============================================================================
// Public wrappers for audio_engine
// ============================================================================

/// WAV fast path for the audio engine (no target_rate decimation, just decode)
pub fn wav_fast_decode_for_engine(bytes: &[u8]) -> Option<(Vec<f32>, u32)> {
    wav_fast_decode(bytes, 0) // target_rate=0 disables fused decimation path
}


// ============================================================================
// Audio Playback (desktop only)
// ============================================================================

#[cfg(desktop)]
/// Play pre-resampled audio samples through the unified audio engine.
/// Routes notification sounds as oneshots through the persistent cpal stream.
fn play_samples_cached(samples: Arc<Vec<f32>>, _device_sample_rate: u32) -> Result<(), String> {
    crate::audio_engine::AudioEngine::get()?.play_oneshot((*samples).clone())
}

#[cfg(desktop)]
/// Get the path to a bundled sound file
fn get_bundled_sound_path<R: Runtime>(
    #[allow(unused_variables)] handle: &AppHandle<R>,
    sound: &NotificationSound,
) -> Option<PathBuf> {
    // Dev: use local resources path
    #[cfg(debug_assertions)]
    let find_sound = |filename: &str| -> Option<PathBuf> {
        let dev_path = PathBuf::from("resources/sounds").join(filename);
        if dev_path.exists() {
            Some(dev_path)
        } else {
            None
        }
    };

    // Production: use bundled resource path
    #[cfg(not(debug_assertions))]
    let find_sound = |filename: &str| -> Option<PathBuf> {
        let resource_path = handle
            .path()
            .resource_dir()
            .ok()?
            .join("resources")
            .join("sounds")
            .join(filename);
        if resource_path.exists() {
            Some(resource_path)
        } else {
            None
        }
    };

    match sound {
        NotificationSound::Default => find_sound("notif-prelude.mp3"),
        NotificationSound::Techno => find_sound("notif-techno.mp3"),
        NotificationSound::Custom(path) => {
            // If path is empty or doesn't exist, fall back to default sound
            if path.is_empty() {
                return get_bundled_sound_path(handle, &NotificationSound::Default);
            }
            let p = PathBuf::from(path);
            if p.exists() {
                Some(p)
            } else {
                // Custom file not found - fall back to default instead of silent failure
                get_bundled_sound_path(handle, &NotificationSound::Default)
            }
        }
        NotificationSound::None => None,
    }
}

/// Play a notification sound (with smart caching)
///
/// Caching strategy:
/// - In-memory: Samples cached for 10 minutes after last notification (for rapid succession)
/// - Disk cache: Custom sounds are pre-resampled and cached on disk
/// - Device rate: Cached to avoid repeated device queries
#[cfg(desktop)]
pub fn play_notification_sound<R: Runtime>(
    handle: &AppHandle<R>,
    sound: &NotificationSound,
) -> Result<(), String> {
    // Don't play anything for None
    if matches!(sound, NotificationSound::None) {
        return Ok(());
    }

    // Check and purge expired cache
    check_cache_ttl();

    // Get device sample rate (cached, may be refreshed later if mismatch detected)
    let mut device_rate = get_device_sample_rate()?;

    // Try to get from in-memory cache
    let cached_samples = {
        let mut cache = get_sound_cache()
            .lock()
            .map_err(|_| "Cache lock poisoned")?;

        // Check if we have a valid cache hit
        let cache_hit = cache.samples.is_some()
            && cache.cached_sound.as_ref() == Some(sound)
            && cache.cached_at_rate == device_rate;

        if cache_hit {
            // Update last used time and return cached samples
            cache.last_used = Some(Instant::now());
            cache.samples.clone()
        } else {
            None
        }
    };

    if let Some(samples) = cached_samples {
        return play_samples_cached(samples, device_rate);
    }

    // Cache miss - need to decode/load
    let path = get_bundled_sound_path(handle, sound)
        .ok_or_else(|| "Sound file not found".to_string())?;

    // For custom sounds, load the pre-resampled .raw file directly
    let samples = if let NotificationSound::Custom(custom_path) = sound {
        // Custom sounds are stored as pre-resampled .raw files
        // Filename format: name_RATE.raw (e.g., discord_ping_48000.raw)
        let custom_file = Path::new(custom_path);
        if custom_file.exists() && custom_file.extension().map(|e| e == "raw").unwrap_or(false) {
            // Parse the cached sample rate from filename (e.g., "discord_ping_48000" -> 48000)
            let stem = custom_file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let cached_rate: u32 = stem.rsplit('_').next()
                .and_then(|r| r.parse().ok())
                .ok_or("Invalid cache filename format")?;

            // Load the cached samples
            let cached_samples = load_raw_samples(custom_file)?;

            // Resample if device rate changed since import
            if cached_rate != device_rate {
                // Device might have changed - invalidate cache and get fresh rate
                invalidate_device_sample_rate_cache();
                device_rate = get_device_sample_rate()?;
                resample_mono_f32(cached_samples, cached_rate, device_rate)?
            } else {
                cached_samples
            }
        } else {
            // File doesn't exist or isn't .raw - invalid setting
            return Err("Custom sound file not found".to_string());
        }
    } else {
        // Default sound - decode and resample
        let (raw_samples, sample_rate) = decode_audio_file(&path)?;
        if sample_rate != device_rate {
            resample_mono_f32(raw_samples, sample_rate, device_rate)?
        } else {
            raw_samples
        }
    };

    if samples.is_empty() {
        return Err("No audio data decoded".to_string());
    }

    // Store in memory cache
    let samples_arc = Arc::new(samples);
    {
        let mut cache = get_sound_cache()
            .lock()
            .map_err(|_| "Cache lock poisoned")?;
        cache.samples = Some(Arc::clone(&samples_arc));
        cache.cached_sound = Some(sound.clone());
        cache.cached_at_rate = device_rate;
        cache.last_used = Some(Instant::now());
    }

    play_samples_cached(samples_arc, device_rate)
}

#[cfg(desktop)]
/// Play notification sound if enabled (checks settings)
/// Automatically purges cache if notifications are disabled
pub fn play_notification_if_enabled<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let settings = load_notification_settings_internal(handle)?;

    // Check global mute - purge cache since we won't need it
    if settings.global_mute {
        purge_sound_cache();
        return Ok(());
    }

    // Check if sound is None - purge cache since we won't need it
    if matches!(settings.sound, NotificationSound::None) {
        purge_sound_cache();
        return Ok(());
    }

    play_notification_sound(handle, &settings.sound)
}

// ============================================================================
// Settings persistence (Desktop Only)
// ============================================================================

#[cfg(desktop)]
fn load_notification_settings_internal<R: Runtime>(
    _handle: &AppHandle<R>,
) -> Result<NotificationSettings, String> {
    let global_mute = match db::get_sql_setting("notif_global_mute".to_string()) {
        Ok(Some(val)) => val == "true",
        _ => false,
    };

    let sound = match db::get_sql_setting("notif_sound".to_string()) {
        Ok(Some(val)) => parse_notification_sound(&val),
        _ => NotificationSound::Default,
    };

    let mute_everyone = match db::get_sql_setting("notif_mute_everyone".to_string()) {
        Ok(Some(val)) => val == "true",
        _ => false,
    };

    Ok(NotificationSettings { global_mute, sound, mute_everyone })
}

#[cfg(desktop)]
fn save_notification_settings_internal<R: Runtime>(
    _handle: &AppHandle<R>,
    settings: &NotificationSettings,
) -> Result<(), String> {
    db::set_sql_setting(
        "notif_global_mute".to_string(),
        settings.global_mute.to_string(),
    )
    .map_err(|e| format!("Failed to save global_mute: {}", e))?;

    db::set_sql_setting(
        "notif_sound".to_string(),
        serialize_notification_sound(&settings.sound),
    )
    .map_err(|e| format!("Failed to save sound: {}", e))?;

    db::set_sql_setting(
        "notif_mute_everyone".to_string(),
        settings.mute_everyone.to_string(),
    )
    .map_err(|e| format!("Failed to save mute_everyone: {}", e))?;

    Ok(())
}

#[cfg(desktop)]
fn parse_notification_sound(value: &str) -> NotificationSound {
    if value.starts_with("custom:") {
        NotificationSound::Custom(value[7..].to_string())
    } else {
        match value {
            "default" => NotificationSound::Default,
            "techno" => NotificationSound::Techno,
            "none" => NotificationSound::None,
            _ => NotificationSound::Default,
        }
    }
}

#[cfg(desktop)]
fn serialize_notification_sound(sound: &NotificationSound) -> String {
    match sound {
        NotificationSound::Default => "default".to_string(),
        NotificationSound::Techno => "techno".to_string(),
        NotificationSound::None => "none".to_string(),
        NotificationSound::Custom(path) => format!("custom:{}", path),
    }
}

// ============================================================================
// Tauri Commands (Desktop Only)
// ============================================================================

#[cfg(desktop)]
/// Get current notification settings
#[command]
pub fn get_notification_settings<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<NotificationSettings, String> {
    load_notification_settings_internal(&handle)
}

#[cfg(desktop)]
/// Save notification settings
/// Auto-purges cache when notifications are disabled/muted
#[command]
pub fn set_notification_settings<R: Runtime>(
    handle: AppHandle<R>,
    settings: NotificationSettings,
) -> Result<(), String> {
    // Purge in-memory cache if notifications are being disabled
    if settings.global_mute || matches!(settings.sound, NotificationSound::None) {
        purge_sound_cache();
    } else {
        // Sound changed - purge cache so new sound gets loaded
        let current = load_notification_settings_internal(&handle).ok();
        if let Some(current) = current {
            if current.sound != settings.sound {
                purge_sound_cache();
            }
        }
    }

    save_notification_settings_internal(&handle, &settings)
}

#[cfg(desktop)]
/// Preview a notification sound (plays it immediately)
#[command]
pub fn preview_notification_sound<R: Runtime>(
    handle: AppHandle<R>,
    sound: NotificationSound,
) -> Result<(), String> {
    // Run in a separate thread to not block
    let handle_clone = handle.clone();
    std::thread::spawn(move || {
        if let Err(e) = play_notification_sound(&handle_clone, &sound) {
            eprintln!("Failed to preview sound: {}", e);
        }
    });
    Ok(())
}

#[cfg(desktop)]
/// Open file picker to select a custom notification sound
/// Returns the path to the selected file after copying it to app data
#[command]
pub async fn select_custom_notification_sound<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    // Clone handle for use in spawn_blocking
    let handle_clone = handle.clone();

    // Run blocking file dialog in a separate thread to avoid blocking the async runtime
    let file_result = tokio::task::spawn_blocking(move || {
        handle_clone
            .dialog()
            .file()
            .add_filter("Audio Files", &["mp3", "wav", "flac", "ogg", "m4a"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?;

    match file_result {
        Some(path) => {
            let path_str = path.as_path().map(|p| p.to_string_lossy().to_string())
                .ok_or_else(|| "Invalid file path".to_string())?;

            let path_ref = Path::new(&path_str);

            // Check file size (max 1MB for notification sounds)
            const MAX_SIZE_BYTES: u64 = 1024 * 1024; // 1MB
            let metadata = std::fs::metadata(path_ref)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            if metadata.len() > MAX_SIZE_BYTES {
                return Err("FILE_TOO_LARGE".to_string());
            }

            // Import: decode, resample, and save as .raw
            let stored_path = import_custom_sound(&handle, &path_str)?;
            Ok(stored_path)
        }
        None => Err("No file selected".to_string()),
    }
}

#[cfg(desktop)]
/// Import a custom sound: decode, resample to device rate, and save as .raw
/// Returns the path to the cached .raw file
fn import_custom_sound<R: Runtime>(
    handle: &AppHandle<R>,
    source_path: &str,
) -> Result<String, String> {
    let sounds_dir = get_sound_cache_dir(handle)?;

    if !sounds_dir.exists() {
        std::fs::create_dir_all(&sounds_dir)
            .map_err(|e| format!("Failed to create sounds cache dir: {}", e))?;
    }

    // Get device sample rate for resampling
    let device_rate = get_device_sample_rate()?;

    // Decode the audio file
    let source = Path::new(source_path);
    let (samples, sample_rate) = decode_audio_file(source)?;

    // Check duration (max 10 seconds to match playback timeout)
    const MAX_DURATION_SECS: f32 = 10.0;
    let duration_secs = samples.len() as f32 / sample_rate as f32;
    if duration_secs > MAX_DURATION_SECS {
        return Err("AUDIO_TOO_LONG".to_string());
    }

    // Resample if needed
    let resampled = if sample_rate != device_rate {
        resample_mono_f32(samples, sample_rate, device_rate)?
    } else {
        samples
    };

    // Generate filename: original_name_RATE.raw
    let original_stem = source
        .file_stem()
        .ok_or("Invalid filename")?
        .to_string_lossy();
    let filename = format!("{}_{}.raw", original_stem, device_rate);
    let dest_path = sounds_dir.join(&filename);

    // Save resampled samples
    save_raw_samples(&dest_path, &resampled)?;

    #[cfg(debug_assertions)]
    println!("[Audio] Imported custom sound: {} samples at {}Hz -> {:?}", resampled.len(), device_rate, dest_path);

    Ok(dest_path.to_string_lossy().to_string())
}
