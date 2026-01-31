//! Audio processing module for Vector
//!
//! Provides centralized audio functionality:
//! - Audio decoding (symphonia) - supports mp3, wav, flac, ogg, m4a
//! - Audio resampling (rubato) - high-quality sinc interpolation
//! - Audio playback (cpal) - cross-platform output (desktop only)
//!
//! Used by: notification sounds (desktop), voice recording, whisper transcription

// Shared import for audio file decoding (all platforms - used by whisper on iOS)
use std::fs::File;

// Desktop-only imports for notification sound playback
#[cfg(desktop)]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(desktop)]
use serde::{Deserialize, Serialize};
#[cfg(desktop)]
use std::io::{Read, Write};
#[cfg(desktop)]
use std::path::PathBuf;
#[cfg(desktop)]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
#[cfg(desktop)]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(desktop)]
use std::time::{Duration, Instant};
#[cfg(desktop)]
use tauri::{command, AppHandle, Manager, Runtime};
#[cfg(desktop)]
use crate::db;

// Shared imports for resampling (all platforms)
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
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
}

#[cfg(desktop)]
impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            global_mute: false,
            sound: NotificationSound::Default,
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

/// Get the standard high-quality resampling parameters used throughout the app
fn get_resampling_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Resample mono f32 audio samples to a target sample rate
///
/// Used by: notification sounds, general audio processing
#[allow(dead_code)]
pub fn resample_mono_f32(samples: Vec<f32>, from_rate: u32, to_rate: u32) -> Result<Vec<f32>, String> {
    if from_rate == to_rate {
        return Ok(samples);
    }

    let mut resampler = SincFixedIn::<f32>::new(
        to_rate as f64 / from_rate as f64,
        2.0,
        get_resampling_params(),
        samples.len(),
        1, // mono
    )
    .map_err(|e| format!("Failed to create resampler: {}", e))?;

    let waves_in = vec![samples];
    let waves_out = resampler
        .process(&waves_in, None)
        .map_err(|e| format!("Failed to resample: {}", e))?;

    Ok(waves_out.into_iter().next().unwrap_or_default())
}

/// Resample mono i16 audio samples to a target sample rate
///
/// Used by: voice recording (converts i16 -> f32 -> resample -> i16)
pub fn resample_mono_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Result<Vec<i16>, String> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }

    // Convert i16 to f32 (normalized to -1.0..1.0)
    let samples_f32: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut resampler = SincFixedIn::<f32>::new(
        to_rate as f64 / from_rate as f64,
        2.0,
        get_resampling_params(),
        samples.len(),
        1, // mono
    )
    .map_err(|e| format!("Failed to create resampler: {}", e))?;

    let waves_in = vec![samples_f32];
    let waves_out = resampler
        .process(&waves_in, None)
        .map_err(|e| format!("Failed to resample: {}", e))?;

    // Convert back to i16
    let resampled = waves_out
        .into_iter()
        .next()
        .unwrap_or_default()
        .iter()
        .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
        .collect();

    Ok(resampled)
}

/// Resample multi-channel f32 audio to a target sample rate
///
/// Used by: whisper transcription (preserves channel layout)
/// Input: Vec of channels, each channel is Vec<f32>
/// Output: Vec of resampled channels
#[allow(dead_code)]
pub fn resample_multichannel_f32(
    channels_data: Vec<Vec<f32>>,
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<Vec<f32>>, String> {
    if from_rate == to_rate {
        return Ok(channels_data);
    }

    let num_channels = channels_data.len();
    if num_channels == 0 {
        return Ok(channels_data);
    }

    let chunk_size = channels_data[0].len();

    let mut resampler = SincFixedIn::<f32>::new(
        to_rate as f64 / from_rate as f64,
        2.0,
        get_resampling_params(),
        chunk_size,
        num_channels,
    )
    .map_err(|e| format!("Failed to create resampler: {}", e))?;

    let resampled = Resampler::process(&mut resampler, &channels_data, None)
        .map_err(|e| format!("Failed to resample: {}", e))?;

    Ok(resampled)
}

/// Decode an audio file and resample to a target rate
///
/// Used by: whisper transcription
/// Returns interleaved samples (for multi-channel) at the target sample rate
#[allow(dead_code)]
pub fn decode_and_resample(path: &Path, target_rate: u32) -> Result<Vec<f32>, String> {
    // Decode the file (get raw samples and metadata)
    let (samples, sample_rate, channels) = decode_audio_file_raw(path)?;

    // If already at target rate and mono, return directly
    if sample_rate == target_rate && channels == 1 {
        return Ok(samples);
    }

    // Organize interleaved samples into separate channels
    let mut channels_data: Vec<Vec<f32>> = vec![Vec::new(); channels];
    for (i, sample) in samples.iter().enumerate() {
        channels_data[i % channels].push(*sample);
    }

    // Resample each channel
    let resampled = resample_multichannel_f32(channels_data, sample_rate, target_rate)?;

    // Interleave the channels back together
    let output_len = resampled[0].len();
    let mut result = Vec::with_capacity(output_len * channels);
    for i in 0..output_len {
        for channel in &resampled {
            result.push(channel[i]);
        }
    }

    Ok(result)
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
// Audio Playback (desktop only)
// ============================================================================

#[cfg(desktop)]
/// Play pre-resampled audio samples (already at device sample rate)
/// Uses Arc to allow sharing cached samples without copying
fn play_samples_cached(samples: Arc<Vec<f32>>, device_sample_rate: u32) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No output device found")?;

    // Get the device's preferred channel count - Windows WASAPI often requires stereo
    let default_config = device
        .default_output_config()
        .map_err(|e| format!("Failed to get output config: {}", e))?;
    let device_channels = default_config.channels();

    let config = cpal::StreamConfig {
        channels: device_channels,
        sample_rate: cpal::SampleRate(device_sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Shared state for playback
    let position = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));

    let samples_clone = Arc::clone(&samples);
    let position_clone = Arc::clone(&position);
    let finished_clone = Arc::clone(&finished);

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &_| {
                let samples = &*samples_clone;
                let mut pos = position_clone.load(Ordering::Relaxed);
                let channels = device_channels as usize;

                // Fill buffer, duplicating mono sample to all channels (e.g., stereo)
                for frame in data.chunks_mut(channels) {
                    if pos < samples.len() {
                        let sample = samples[pos];
                        for channel_sample in frame.iter_mut() {
                            *channel_sample = sample;
                        }
                        pos += 1;
                    } else {
                        for channel_sample in frame.iter_mut() {
                            *channel_sample = 0.0;
                        }
                        finished_clone.store(true, Ordering::Relaxed);
                    }
                }

                position_clone.store(pos, Ordering::Relaxed);
            },
            |err| eprintln!("Audio playback error: {}", err),
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("Failed to start playback: {}", e))?;

    // Wait for playback to finish (with timeout)
    let start = Instant::now();
    let max_duration = Duration::from_secs(10); // 10 second max

    while !finished.load(Ordering::Relaxed) {
        if start.elapsed() > max_duration {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Small delay to ensure audio buffer is flushed
    std::thread::sleep(Duration::from_millis(100));

    Ok(())
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
    handle: &AppHandle<R>,
) -> Result<NotificationSettings, String> {
    let global_mute = match db::get_sql_setting(handle.clone(), "notif_global_mute".to_string()) {
        Ok(Some(val)) => val == "true",
        _ => false,
    };

    let sound = match db::get_sql_setting(handle.clone(), "notif_sound".to_string()) {
        Ok(Some(val)) => parse_notification_sound(&val),
        _ => NotificationSound::Default,
    };

    Ok(NotificationSettings { global_mute, sound })
}

#[cfg(desktop)]
fn save_notification_settings_internal<R: Runtime>(
    handle: &AppHandle<R>,
    settings: &NotificationSettings,
) -> Result<(), String> {
    db::set_sql_setting(
        handle.clone(),
        "notif_global_mute".to_string(),
        settings.global_mute.to_string(),
    )
    .map_err(|e| format!("Failed to save global_mute: {}", e))?;

    db::set_sql_setting(
        handle.clone(),
        "notif_sound".to_string(),
        serialize_notification_sound(&settings.sound),
    )
    .map_err(|e| format!("Failed to save sound: {}", e))?;

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
