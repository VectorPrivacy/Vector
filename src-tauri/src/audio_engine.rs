//! Unified audio engine — persistent cpal output stream with multi-source mixing,
//! streaming decode, and precomputed FFT waveform data.
//!
//! Architecture:
//! - Single cpal output stream created once, lives for app lifetime
//! - Mixer callback sums all active sources per buffer with on-the-fly rate conversion
//! - Sources: voice message playback + notification oneshots (desktop)
//! - Non-WAV files stream-decode in background (playback starts immediately)
//! - FFT waveform precomputed after decode completes, sent via Tauri event

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use tauri::Emitter;

use crate::TAURI_APP;

// ============================================================================
// Constants
// ============================================================================

/// FFT window size (matches current fftSize = 256 in JS)
const FFT_WINDOW_SIZE: usize = 256;
/// Number of output frequency bins per frame
const WAVEFORM_BINS: usize = 64;
/// Frames per second for waveform data
const WAVEFORM_FPS: u32 = 30;
/// Temporal smoothing factor (matches current smoothingTimeConstant = 0.85)
const SMOOTHING_FACTOR: f32 = 0.85;
/// Maximum number of decoded sources kept in memory
const MAX_LOADED_SOURCES: usize = 5;
/// Crossfade length after seek (samples at device rate). ~10ms at 48kHz — old audio
/// fades out while new audio fades in, eliminating clicks from sample discontinuities.
const CROSSFADE_SAMPLES: u32 = 480;
/// Number of decoded samples to accumulate before flushing to the source (streaming decode)
const DECODE_BATCH_SIZE: usize = 16384;

// ============================================================================
// Global singleton
// ============================================================================

static ENGINE: OnceLock<AudioEngine> = OnceLock::new();

// ============================================================================
// Types
// ============================================================================

pub struct AudioEngine {
    shared: Arc<SharedState>,
    _stream: cpal::Stream, // kept alive
}

// SAFETY: cpal::Stream stores a boxed callback that is !Send+!Sync.
// We never access _stream after creation — it's kept alive in the OnceLock
// solely to prevent the stream from being dropped. The mixer callback runs
// on cpal's audio thread using only the Arc<SharedState> (which is Send+Sync).
// All public methods operate on SharedState, never touching _stream.
unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

struct SharedState {
    sources: std::sync::Mutex<HashMap<u32, AudioSource>>,
    device_sample_rate: u32,
    #[allow(dead_code)]
    device_channels: u16,
    next_id: AtomicU32,
    /// Channel for deferring audio_ended events off the real-time audio thread
    ended_tx: mpsc::Sender<u32>,
}

struct AudioSource {
    #[allow(dead_code)]
    id: u32,
    samples: Vec<f32>,           // decoded mono samples (at source_sample_rate)
    source_sample_rate: u32,     // native sample rate of the audio
    rate_ratio: f64,             // source_sample_rate / device_sample_rate
    position: f64,               // current position in source samples (fractional for interpolation)
    playing: bool,
    volume: f32,                 // 0.0–1.0
    duration_ms: u64,            // estimated until decode completes, then actual
    oneshot: bool,               // notification sounds — auto-remove on finish
    crossfade: Option<Crossfade>,
    decode_complete: bool,       // true when all samples have been decoded
}

/// Active crossfade state: blends old position out while new position fades in
struct Crossfade {
    old_position: f64,   // playback position before seek (in source samples, fractional)
    remaining: u32,      // device-rate samples left in crossfade
}

/// Result returned from audio_load to frontend.
/// Waveform data arrives separately via `audio_waveform` event (computed in background).
#[derive(Serialize, Clone)]
pub struct AudioLoadResult {
    pub id: u32,
    pub duration_ms: u64,
    pub waveform_fps: u8,
    pub bins: u8,
}

/// Event payload emitted when a source finishes playing
#[derive(Serialize, Clone)]
struct AudioEndedPayload {
    id: u32,
}

/// Event payload emitted when waveform precomputation completes
#[derive(Serialize, Clone)]
struct AudioWaveformPayload {
    id: u32,
    waveform: Vec<u8>,
    waveform_fps: u8,
    bins: u8,
}

/// Event payload emitted when actual duration is known after streaming decode completes
#[derive(Serialize, Clone)]
struct AudioDurationPayload {
    id: u32,
    duration_ms: u64,
}

// ============================================================================
// Precomputed Hann window (computed once per process)
// ============================================================================

static HANN_WINDOW: OnceLock<Vec<f32>> = OnceLock::new();

fn get_hann_window() -> &'static Vec<f32> {
    HANN_WINDOW.get_or_init(|| {
        (0..FFT_WINDOW_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_WINDOW_SIZE - 1) as f32).cos())
            })
            .collect()
    })
}

// ============================================================================
// Implementation
// ============================================================================

impl AudioEngine {
    /// Initialize the audio engine. Call once during app setup.
    pub fn init() {
        if ENGINE.get().is_some() {
            return; // Already initialized
        }

        match Self::create() {
            Ok(engine) => {
                let _ = ENGINE.set(engine);
                println!("[AudioEngine] Initialized successfully");
            }
            Err(e) => {
                eprintln!("[AudioEngine] Failed to initialize: {}", e);
            }
        }
    }

    /// Get reference to the global engine singleton
    pub fn get() -> Result<&'static AudioEngine, String> {
        ENGINE.get().ok_or_else(|| "Audio engine not initialized".to_string())
    }

    fn create() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No audio output device found")?;

        let default_config = device
            .default_output_config()
            .map_err(|e| format!("Failed to get output config: {}", e))?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels();

        let config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Channel for deferring audio_ended events off the cpal real-time thread
        let (ended_tx, ended_rx) = mpsc::channel::<u32>();

        // Background thread emits Tauri events (avoids allocations on the audio thread)
        std::thread::Builder::new()
            .name("audio-events".into())
            .spawn(move || {
                while let Ok(id) = ended_rx.recv() {
                    if let Some(app) = TAURI_APP.get() {
                        let _ = app.emit("audio_ended", AudioEndedPayload { id });
                    }
                }
            })
            .ok();

        let shared = Arc::new(SharedState {
            sources: std::sync::Mutex::new(HashMap::new()),
            device_sample_rate,
            device_channels,
            next_id: AtomicU32::new(1),
            ended_tx,
        });

        let shared_for_callback = Arc::clone(&shared);
        let channels = device_channels as usize;

        let stream = device
            .build_output_stream(
                &config,
                move |output: &mut [f32], _: &_| {
                    mixer_callback(output, &shared_for_callback, channels);
                },
                |err| eprintln!("[AudioEngine] Stream error: {}", err),
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start output stream: {}", e))?;

        Ok(AudioEngine {
            shared,
            _stream: stream,
        })
    }

    /// Load audio from file path. WAV uses instant decode; other formats stream-decode
    /// in a background thread so playback can begin within milliseconds.
    pub fn load_from_file(&self, path: &str) -> Result<AudioLoadResult, String> {
        let path_buf = std::path::PathBuf::from(path);
        if !path_buf.exists() {
            return Err("Audio file not found".to_string());
        }

        // Read file into memory (stays in page cache, fast)
        let file_bytes = std::fs::read(&path_buf)
            .map_err(|e| format!("Failed to read audio file: {}", e))?;

        // Try WAV fast path — synchronous, instant decode
        if let Some((mono_samples, sample_rate)) = crate::audio::wav_fast_decode_for_engine(&file_bytes) {
            return self.load_from_samples(mono_samples, sample_rate);
        }

        // Non-WAV: probe metadata, create source, stream-decode in background
        self.load_streaming(file_bytes, &path_buf)
    }

    /// Stream-decode a non-WAV audio file. Probes metadata synchronously (fast),
    /// creates an empty source, then spawns a background thread for packet-by-packet decode.
    fn load_streaming(&self, file_bytes: Vec<u8>, path: &std::path::Path) -> Result<AudioLoadResult, String> {
        // Probe metadata from file (re-opens from disk, already in page cache — <1ms)
        let (sample_rate, channels, est_frames) = probe_audio_metadata(path)?;
        if sample_rate == 0 || self.shared.device_sample_rate == 0 {
            return Err("Invalid sample rate".to_string());
        }

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let rate_ratio = sample_rate as f64 / self.shared.device_sample_rate as f64;
        let duration_ms = if sample_rate > 0 { est_frames * 1000 / sample_rate as u64 } else { 0 };

        self.evict_if_needed();

        let source = AudioSource {
            id,
            samples: Vec::with_capacity(est_frames as usize),
            source_sample_rate: sample_rate,
            rate_ratio,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms,
            oneshot: false,
            crossfade: None,
            decode_complete: false,
        };

        self.shared
            .sources
            .lock()
            .map_err(|_| "Lock poisoned")?
            .insert(id, source);

        // Spawn background decode thread
        let shared = Arc::clone(&self.shared);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
        std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                stream_decode_worker(id, file_bytes, &ext, channels, sample_rate, &shared);
            })
            .map_err(|e| format!("Failed to spawn decode thread: {}", e))?;

        Ok(AudioLoadResult {
            id,
            duration_ms,
            waveform_fps: WAVEFORM_FPS as u8,
            bins: WAVEFORM_BINS as u8,
        })
    }

    /// Load audio from pre-decoded f32 samples (already mono, at native sample rate).
    /// No resampling needed — mixer handles rate conversion on-the-fly.
    pub fn load_from_samples(&self, samples: Vec<f32>, sample_rate: u32) -> Result<AudioLoadResult, String> {
        self.load_from_samples_internal(samples, sample_rate)
    }

    /// Internal: create a fully-decoded source at the given sample rate.
    fn load_from_samples_internal(&self, samples: Vec<f32>, sample_rate: u32) -> Result<AudioLoadResult, String> {
        if samples.is_empty() {
            return Err("No audio data".to_string());
        }
        if sample_rate == 0 || self.shared.device_sample_rate == 0 {
            return Err("Invalid sample rate".to_string());
        }

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let rate_ratio = sample_rate as f64 / self.shared.device_sample_rate as f64;
        let duration_ms = (samples.len() as u64 * 1000) / sample_rate as u64;

        // Compute waveform in background — playback can start immediately
        let samples_for_fft = samples.clone();
        std::thread::Builder::new()
            .name("audio-fft".into())
            .spawn(move || {
                let waveform = precompute_fft_waveform(&samples_for_fft, sample_rate);
                if let Some(app) = TAURI_APP.get() {
                    let _ = app.emit("audio_waveform", AudioWaveformPayload {
                        id,
                        waveform,
                        waveform_fps: WAVEFORM_FPS as u8,
                        bins: WAVEFORM_BINS as u8,
                    });
                }
            })
            .ok();

        // Evict oldest paused sources if at capacity
        self.evict_if_needed();

        let source = AudioSource {
            id,
            samples,
            source_sample_rate: sample_rate,
            rate_ratio,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms,
            oneshot: false,
            crossfade: None,
            decode_complete: true, // All samples already available
        };

        self.shared
            .sources
            .lock()
            .map_err(|_| "Lock poisoned")?
            .insert(id, source);

        Ok(AudioLoadResult {
            id,
            duration_ms,
            waveform_fps: WAVEFORM_FPS as u8,
            bins: WAVEFORM_BINS as u8,
        })
    }

    /// Start playback of a source. Returns current position_ms.
    pub fn play(&self, id: u32) -> Result<u64, String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        let source = sources.get_mut(&id).ok_or("Source not found")?;
        // Auto-rewind if at end (so replay works without explicit seek)
        // Mixer stops when pos_floor + 1 >= samples.len(), so position ends at len-1
        if source.decode_complete && (source.position as usize) + 1 >= source.samples.len() {
            source.position = 0.0;
        }
        source.playing = true;
        let pos_ms = (source.position / source.source_sample_rate as f64 * 1000.0) as u64;
        Ok(pos_ms)
    }

    /// Pause playback. Returns paused position_ms.
    pub fn pause(&self, id: u32) -> Result<u64, String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        let source = sources.get_mut(&id).ok_or("Source not found")?;
        source.playing = false;
        let pos_ms = (source.position / source.source_sample_rate as f64 * 1000.0) as u64;
        Ok(pos_ms)
    }

    /// Seek to position in milliseconds.
    pub fn seek(&self, id: u32, position_ms: u64) -> Result<(), String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        let source = sources.get_mut(&id).ok_or("Source not found")?;
        let sample_pos = position_ms as f64 * source.source_sample_rate as f64 / 1000.0;
        let old_position = source.position;
        // During streaming decode, allow seeking beyond decoded data — the mixer
        // outputs silence until decode catches up. For fully-decoded sources,
        // clamp to file length to prevent seeking past the end.
        source.position = if source.decode_complete {
            sample_pos.min(source.samples.len() as f64)
        } else {
            sample_pos
        };
        // Only crossfade if the new position has decoded audio to blend into.
        // Seeking beyond decoded data during streaming → no crossfade (just silence).
        if source.playing && (source.position as usize) + 1 < source.samples.len() {
            source.crossfade = Some(Crossfade {
                old_position,
                remaining: CROSSFADE_SAMPLES,
            });
        } else {
            source.crossfade = None;
        }
        Ok(())
    }

    /// Stop and remove a source, freeing memory.
    pub fn stop(&self, id: u32) -> Result<(), String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        sources.remove(&id);
        Ok(())
    }

    /// Stop and remove all non-oneshot sources (e.g. when navigating away from a chat).
    pub fn stop_all(&self) -> Result<(), String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        sources.retain(|_, s| s.oneshot);
        Ok(())
    }

    /// Set volume for a source (0.0–1.0).
    pub fn set_volume(&self, id: u32, volume: f32) -> Result<(), String> {
        let mut sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        let source = sources.get_mut(&id).ok_or("Source not found")?;
        source.volume = volume.clamp(0.0, 1.0);
        Ok(())
    }

    /// Play a oneshot sound (for notifications). Auto-removes when finished.
    /// Expects samples pre-resampled to device sample rate (rate_ratio = 1.0).
    #[allow(dead_code)] // Called from #[cfg(desktop)] notification sound code
    pub fn play_oneshot(&self, samples: Vec<f32>) -> Result<(), String> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let duration_ms = (samples.len() as u64 * 1000) / self.shared.device_sample_rate as u64;

        let source = AudioSource {
            id,
            samples,
            source_sample_rate: self.shared.device_sample_rate,
            rate_ratio: 1.0, // already at device rate
            position: 0.0,
            playing: true, // start immediately
            volume: 1.0,
            duration_ms,
            oneshot: true,
            crossfade: None,
            decode_complete: true,
        };

        self.shared
            .sources
            .lock()
            .map_err(|_| "Lock poisoned")?
            .insert(id, source);

        Ok(())
    }

    /// Get current playback position in milliseconds.
    #[allow(dead_code)]
    pub fn get_position(&self, id: u32) -> Result<u64, String> {
        let sources = self.shared.sources.lock().map_err(|_| "Lock poisoned")?;
        let source = sources.get(&id).ok_or("Source not found")?;
        let pos_ms = (source.position / source.source_sample_rate as f64 * 1000.0) as u64;
        Ok(pos_ms)
    }

    /// Get the device sample rate
    #[allow(dead_code)]
    pub fn device_sample_rate(&self) -> u32 {
        self.shared.device_sample_rate
    }

    /// Evict oldest paused sources if we're at capacity
    fn evict_if_needed(&self) {
        if let Ok(mut sources) = self.shared.sources.lock() {
            while sources.len() >= MAX_LOADED_SOURCES {
                // Find oldest paused, non-oneshot source to evict
                let evict_id = sources
                    .iter()
                    .filter(|(_, s)| !s.playing && !s.oneshot)
                    .map(|(id, _)| *id)
                    .min(); // lowest ID = oldest

                if let Some(id) = evict_id {
                    sources.remove(&id);
                } else {
                    break; // all sources are playing or oneshot, can't evict
                }
            }
        }
    }
}

// ============================================================================
// Lightweight duration probe (no decode, no engine instance needed)
// ============================================================================

/// Probe an audio file for its duration without decoding.
/// WAV: parses RIFF header directly. Other formats: symphonia metadata probe.
pub fn probe_duration(path: &str) -> Result<u64, String> {
    let path = std::path::Path::new(path);
    if !path.exists() {
        return Err("Audio file not found".to_string());
    }

    // WAV fast path: parse RIFF header for data size + sample rate
    if let Some(duration_ms) = wav_probe_duration(path) {
        return Ok(duration_ms);
    }

    // Symphonia probe for other formats
    let (sample_rate, _channels, est_frames) = probe_audio_metadata(path)?;
    if sample_rate > 0 {
        Ok(est_frames * 1000 / sample_rate as u64)
    } else {
        Ok(0)
    }
}

/// Fast WAV duration probe: reads RIFF header, computes duration from data chunk size.
fn wav_probe_duration(path: &std::path::Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }

    let mut pos = 12;
    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut audio_format = 0u16;

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;

        if chunk_id == b"fmt " {
            if chunk_size < 16 || pos + 8 + 16 > bytes.len() { return None; }
            let d = &bytes[pos + 8..];
            audio_format = u16::from_le_bytes([d[0], d[1]]);
            channels = u16::from_le_bytes([d[2], d[3]]);
            sample_rate = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
            bits_per_sample = u16::from_le_bytes([d[14], d[15]]);
        } else if chunk_id == b"data" {
            if audio_format != 1 && audio_format != 3 { return None; }
            if channels == 0 || sample_rate == 0 || bits_per_sample == 0 { return None; }
            let bytes_per_sample = bits_per_sample as u32 / 8;
            let bytes_per_frame = bytes_per_sample * channels as u32;
            if bytes_per_frame == 0 { return None; }
            let total_frames = chunk_size as u64 / bytes_per_frame as u64;
            return Some(total_frames * 1000 / sample_rate as u64);
        }

        pos += 8 + chunk_size;
        if chunk_size % 2 != 0 { pos += 1; }
    }

    None
}

// ============================================================================
// Mixer callback (runs on cpal audio thread)
// ============================================================================

fn mixer_callback(output: &mut [f32], shared: &SharedState, channels: usize) {
    // Zero the output buffer
    output.fill(0.0);

    // try_lock: if contended (during add/remove source or decode batch flush),
    // output silence for one buffer (~5ms)
    let mut sources = match shared.sources.try_lock() {
        Ok(s) => s,
        Err(_) => return, // output silence
    };

    // Stack-allocated array for finished source IDs — NO heap allocation on RT thread.
    // MAX_LOADED_SOURCES is 5, so this is always sufficient.
    let mut finished_buf = [(0u32, false); MAX_LOADED_SOURCES];
    let mut finished_count = 0usize;

    for (id, source) in sources.iter_mut() {
        if !source.playing {
            continue;
        }

        for frame in output.chunks_mut(channels) {
            let pos_floor = source.position as usize;

            // Need at least one sample beyond current position for interpolation
            if pos_floor + 1 >= source.samples.len() {
                if source.decode_complete {
                    // True end of file
                    source.playing = false;
                    if finished_count < finished_buf.len() {
                        finished_buf[finished_count] = (*id, source.oneshot);
                        finished_count += 1;
                    }
                }
                // Buffer underrun (decode in progress) or end of file — silence for rest
                break;
            }

            // Linear interpolation between adjacent samples for rate conversion
            let frac = (source.position - pos_floor as f64) as f32;
            let s0 = source.samples[pos_floor];
            let s1 = source.samples[pos_floor + 1];
            let new_sample = s0 + (s1 - s0) * frac;

            let sample = if let Some(ref mut xfade) = source.crossfade {
                // Crossfade: blend old position (fading out) with new position (fading in)
                let t = xfade.remaining as f32 / CROSSFADE_SAMPLES as f32;
                let old_floor = xfade.old_position as usize;
                let old_sample = if old_floor + 1 < source.samples.len() {
                    let old_frac = (xfade.old_position - old_floor as f64) as f32;
                    let os0 = source.samples[old_floor];
                    let os1 = source.samples[old_floor + 1];
                    os0 + (os1 - os0) * old_frac
                } else if old_floor < source.samples.len() {
                    source.samples[old_floor]
                } else {
                    0.0
                };
                xfade.old_position += source.rate_ratio;
                xfade.remaining -= 1;
                if xfade.remaining == 0 {
                    source.crossfade = None;
                }
                (old_sample * t + new_sample * (1.0 - t)) * source.volume
            } else {
                new_sample * source.volume
            };

            for ch in frame.iter_mut() {
                *ch += sample;
            }
            source.position += source.rate_ratio;
        }
    }

    // Handle finished oneshot sources
    for i in 0..finished_count {
        let (id, is_oneshot) = finished_buf[i];
        if is_oneshot {
            sources.remove(&id);
        }
    }

    // Clamp output to [-1.0, 1.0] before any non-audio work
    for s in output.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }

    // Drop the lock — audio buffer is finalized
    drop(sources);

    // Defer audio_ended events to background thread (no allocations on RT thread)
    for i in 0..finished_count {
        let (id, is_oneshot) = finished_buf[i];
        if !is_oneshot {
            let _ = shared.ended_tx.send(id);
        }
    }
}

// ============================================================================
// Audio metadata probing (fast — header parsing only)
// ============================================================================

/// Probe audio file metadata without decoding. Returns (sample_rate, channels, estimated_frames).
fn probe_audio_metadata(path: &std::path::Path) -> Result<(u32, usize, u64), String> {
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open audio file for probe: {}", e))?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, media_source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Failed to probe audio format: {}", e))?;

    let track = probed.format.tracks().iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No supported audio tracks found")?;

    let sample_rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    // Use n_frames if available, otherwise estimate ~5 minutes
    let est_frames = track.codec_params.n_frames.unwrap_or(sample_rate as u64 * 300);

    Ok((sample_rate, channels, est_frames))
}

// ============================================================================
// Streaming decode worker (runs on background thread)
// ============================================================================

/// Background decode worker: decodes audio packets progressively and appends
/// decoded samples to the source in batches. When complete, triggers FFT
/// waveform computation and emits actual duration.
fn stream_decode_worker(
    id: u32,
    file_bytes: Vec<u8>,
    ext: &str,
    channels: usize,
    sample_rate: u32,
    shared: &SharedState,
) {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let t0 = std::time::Instant::now();

    let cursor = std::io::Cursor::new(file_bytes);
    let media_source = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }

    let probed = match symphonia::default::get_probe()
        .format(&hint, media_source, &FormatOptions::default(), &MetadataOptions::default())
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[AudioEngine] Decode probe failed for source {}: {}", id, e);
            mark_decode_complete(id, shared);
            return;
        }
    };

    let mut format = probed.format;
    let track = match format.tracks().iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    {
        Some(t) => t,
        None => {
            eprintln!("[AudioEngine] No audio track found for source {}", id);
            mark_decode_complete(id, shared);
            return;
        }
    };

    let track_id = track.id;
    let mut decoder = match symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[AudioEngine] Decoder creation failed for source {}: {}", id, e);
            mark_decode_complete(id, shared);
            return;
        }
    };

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut batch = Vec::with_capacity(DECODE_BATCH_SIZE);
    // Thread-local accumulator for FFT: keeps all decoded samples so we can
    // compute waveform frames incrementally without touching the engine's lock.
    let mut all_decoded = Vec::new();
    // Incremental FFT waveform — computed alongside decoding, not after
    let mut waveform_computer = WaveformComputer::new(sample_rate);

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(SymphoniaError::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    sample_buf = Some(SampleBuffer::<f32>::new(
                        audio_buf.capacity() as u64,
                        *audio_buf.spec(),
                    ));
                }
                if let Some(ref mut buf) = sample_buf {
                    buf.copy_interleaved_ref(audio_buf);
                    let samples = buf.samples();
                    if channels > 1 {
                        for chunk in samples.chunks_exact(channels) {
                            let sum: f32 = chunk.iter().sum();
                            let mono = sum / channels as f32;
                            batch.push(mono);
                            all_decoded.push(mono);
                        }
                    } else {
                        batch.extend_from_slice(samples);
                        all_decoded.extend_from_slice(samples);
                    }
                }
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        }

        // Flush batch to source periodically (minimize lock contention)
        if batch.len() >= DECODE_BATCH_SIZE {
            if !flush_decode_batch(id, &mut batch, shared) {
                return; // Source was removed, stop decoding
            }
            // Compute FFT frames for newly available samples (incremental)
            waveform_computer.process(&all_decoded);
        }
    }

    // Flush remaining samples + process final FFT frames
    flush_decode_batch(id, &mut batch, shared);
    waveform_computer.process(&all_decoded);
    let waveform = waveform_computer.finish();

    // Mark decode complete and update actual duration (brief lock, no cloning)
    let actual_duration_ms = if sample_rate > 0 {
        (all_decoded.len() as u64 * 1000) / sample_rate as u64
    } else {
        0
    };

    let source_exists = if let Ok(mut sources) = shared.sources.lock() {
        if let Some(source) = sources.get_mut(&id) {
            source.decode_complete = true;
            source.duration_ms = actual_duration_ms;
            println!(
                "[AudioEngine] Streaming decode complete for source {}: {} samples, {}ms, took {:?}",
                id, source.samples.len(), actual_duration_ms, t0.elapsed()
            );
            true
        } else {
            false
        }
    } else {
        false
    };

    // Emit waveform + duration immediately — no separate FFT thread needed
    if source_exists {
        if let Some(app) = TAURI_APP.get() {
            let _ = app.emit("audio_waveform", AudioWaveformPayload {
                id,
                waveform,
                waveform_fps: WAVEFORM_FPS as u8,
                bins: WAVEFORM_BINS as u8,
            });
            let _ = app.emit("audio_duration", AudioDurationPayload {
                id,
                duration_ms: actual_duration_ms,
            });
        }
    }
}

/// Flush a batch of decoded samples into the source. Returns false if source was removed.
fn flush_decode_batch(id: u32, batch: &mut Vec<f32>, shared: &SharedState) -> bool {
    if batch.is_empty() {
        return true;
    }
    if let Ok(mut sources) = shared.sources.lock() {
        if let Some(source) = sources.get_mut(&id) {
            source.samples.extend_from_slice(batch);
            batch.clear();
            true
        } else {
            false // Source was removed
        }
    } else {
        false // Lock poisoned
    }
}

/// Mark a source as decode-complete (used on error paths)
fn mark_decode_complete(id: u32, shared: &SharedState) {
    if let Ok(mut sources) = shared.sources.lock() {
        if let Some(source) = sources.get_mut(&id) {
            source.decode_complete = true;
        }
    }
}

// ============================================================================
// FFT waveform computation (incremental — processes samples as they arrive)
// ============================================================================

/// Incremental FFT waveform computer. Processes samples as they arrive from
/// the streaming decoder, producing waveform data progressively. By the time
/// decode finishes, the waveform is already complete — no post-decode pass.
struct WaveformComputer {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    bin_ranges: Vec<(usize, usize)>,
    hop_size: usize,
    prev_frame: Vec<f32>,
    fft_buffer: Vec<rustfft::num_complex::Complex<f32>>,
    output: Vec<u8>,
    cursor: usize, // next sample index in the full stream to process
}

impl WaveformComputer {
    fn new(sample_rate: u32) -> Self {
        use rustfft::FftPlanner;

        let hop_size = if sample_rate >= WAVEFORM_FPS {
            (sample_rate / WAVEFORM_FPS) as usize
        } else {
            1
        };

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_WINDOW_SIZE);

        // Precompute log-spaced frequency bin boundaries
        let nyquist = sample_rate as f32 / 2.0;
        let min_freq = 100.0_f32;
        let max_freq = 8000.0_f32.min(nyquist);
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();
        let half_window = FFT_WINDOW_SIZE / 2;

        let bin_ranges: Vec<(usize, usize)> = (0..WAVEFORM_BINS)
            .map(|bin| {
                let log_freq_start = log_min + (log_max - log_min) * (bin as f32 / WAVEFORM_BINS as f32);
                let log_freq_end = log_min + (log_max - log_min) * ((bin + 1) as f32 / WAVEFORM_BINS as f32);
                let freq_start = 10.0f32.powf(log_freq_start);
                let freq_end = 10.0f32.powf(log_freq_end);
                let bin_start = ((freq_start / nyquist) * half_window as f32).floor() as usize;
                let bin_end = ((freq_end / nyquist) * half_window as f32).ceil() as usize;
                (bin_start.min(half_window), bin_end.min(half_window).max(bin_start.min(half_window) + 1))
            })
            .collect();

        WaveformComputer {
            fft,
            bin_ranges,
            hop_size,
            prev_frame: vec![0.0f32; WAVEFORM_BINS],
            fft_buffer: vec![rustfft::num_complex::Complex::new(0.0f32, 0.0f32); FFT_WINDOW_SIZE],
            output: Vec::new(),
            cursor: 0,
        }
    }

    /// Process all available FFT frames from the sample buffer.
    /// Call after appending new samples to `all_decoded`.
    fn process(&mut self, samples: &[f32]) {
        let hann = get_hann_window();

        while self.cursor + FFT_WINDOW_SIZE <= samples.len() {
            // Apply Hann window and fill FFT buffer
            for i in 0..FFT_WINDOW_SIZE {
                self.fft_buffer[i] = rustfft::num_complex::Complex::new(
                    samples[self.cursor + i] * hann[i], 0.0,
                );
            }

            // Run FFT in-place
            self.fft.process(&mut self.fft_buffer);

            // Log-bin magnitudes to WAVEFORM_BINS bands
            for (bin, &(bin_start, bin_end)) in self.bin_ranges.iter().enumerate() {
                let mut sum = 0.0f32;
                let count = bin_end - bin_start;
                for j in bin_start..bin_end {
                    sum += self.fft_buffer[j].norm_sqr();
                }
                let magnitude_sq = if count > 0 { sum / count as f32 } else { 0.0 };

                let smoothed = SMOOTHING_FACTOR * self.prev_frame[bin]
                    + (1.0 - SMOOTHING_FACTOR) * magnitude_sq;
                self.prev_frame[bin] = smoothed;

                let db = 10.0 * (smoothed.max(1e-20)).log10();
                // Range: -60dB → 0, 0dB → 0.75, +20dB → 1.0
                // Headroom above 0dB prevents music bass from clipping to 255
                let normalized = ((db + 60.0) / 80.0).clamp(0.0, 1.0);
                self.output.push((normalized * 255.0) as u8);
            }

            self.cursor += self.hop_size;
        }
    }

    /// Consume the computer and return the final waveform data.
    fn finish(self) -> Vec<u8> {
        if self.output.is_empty() {
            vec![0u8; WAVEFORM_BINS] // at least one frame of silence
        } else {
            self.output
        }
    }
}

/// Precompute FFT waveform for a complete sample buffer (batch mode).
/// Used by `load_from_samples_internal` for WAV files and voice recordings.
fn precompute_fft_waveform(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut computer = WaveformComputer::new(sample_rate);
    computer.process(samples);
    computer.finish()
}
