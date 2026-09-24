//! Unified audio engine — persistent cpal output stream with multi-source mixing,
//! streaming decode, and precomputed FFT waveform data.
//!
//! Architecture:
//! - Single cpal output stream created once, lives for app lifetime
//! - Mixer callback sums all active sources per buffer with on-the-fly rate conversion
//! - Sources: voice message playback + notification oneshots (desktop)
//! - Non-WAV files stream-decode in background (playback starts immediately)
//! - Files past STREAM_MIN_SECS play from a window around the playhead instead
//! - FFT waveform sent via Tauri event: built during the decode, or by its own pass
//!   for a windowed file

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

use cpal::traits::{DeviceTrait, StreamTrait};
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
/// Files at least this long split their decode across parallel segment workers.
const PARALLEL_MIN_SECS: u64 = 30;
/// Head span decoded first with progressive flushes so playback starts within
/// milliseconds; the tail decodes in parallel behind it.
const PARALLEL_HEAD_SECS: u64 = 12;
/// Seek-back margin before each segment. The discarded run-in re-primes the
/// codec's inter-frame state (MP3 bit reservoir, IMDCT overlap-add) so kept
/// samples match a straight-through decode.
const SEGMENT_WARMUP_SECS: f64 = 0.3;
/// Cap on parallel segment workers (plus the head decode on the coordinator).
const MAX_DECODE_WORKERS: usize = 6;
/// Files at least this long play from a window around the playhead instead of being
/// decoded whole: an album or a mix would otherwise hold a gigabyte, and a seek deep into
/// it would wait for everything before.
const STREAM_MIN_SECS: u64 = 8 * 60;
/// How far ahead of the playhead a window keeps decoded.
const WINDOW_AHEAD_SECS: u64 = 20;
/// How much already-played audio a window keeps, for a short seek back.
const WINDOW_BEHIND_SECS: u64 = 4;
/// Played audio is let go once this much has built up behind the playhead.
const WINDOW_TRIM_SECS: u64 = 12;


// ============================================================================
// Global singleton
// ============================================================================

static ENGINE: OnceLock<AudioEngine> = OnceLock::new();

// ============================================================================
// Types
// ============================================================================

pub struct AudioEngine {
    shared: Arc<SharedState>,
    /// The output stream, held so it keeps playing; replaced when the default
    /// device changes. Touched only by init and the device watchdog thread.
    stream: std::sync::Mutex<Option<cpal::Stream>>,
    /// Names of the default devices the streams were opened on, for the watchdog.
    output_name: std::sync::Mutex<String>,
    input_name: std::sync::Mutex<String>,
    /// Told when either device changes, after the output is rebuilt.
    device_listeners: std::sync::Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    /// The watchdog sleeps on this; a dying stream wakes it at once.
    wake: (std::sync::Mutex<bool>, std::sync::Condvar),
    /// Bumped whenever the input device changes, for capture loops to notice.
    input_generation: AtomicU64,
}

// SAFETY: cpal::Stream stores a boxed callback that is !Send+!Sync. The stream
// is only ever created, replaced or dropped from init and the watchdog thread,
// never concurrently; the mixer callback runs on cpal's audio thread using only
// the Arc<SharedState> (which is Send+Sync). All other methods operate on SharedState.
unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

/// A live call on the mixer: `play` is what the call wants heard, `tap` is what the
/// speaker actually gets (everything mixed), the echo canceller's reference. A shared
/// screen's sound is a second source, stereo and with its own volume, and `share_tap`
/// is the same reference again for the canceller that keeps our own output out of
/// the sound we capture from the screen.
pub struct LiveLink {
    pub play: crate::calls::ring::SpscRing,
    pub tap: crate::calls::ring::SpscRing,
    /// Listener-side volume for the call, as f32 bits; 1.0 is unity.
    pub gain: AtomicU32,
    /// Interleaved stereo at the device rate.
    pub share_play: crate::calls::ring::SpscRing,
    pub share_tap: crate::calls::ring::SpscRing,
    pub share_gain: AtomicU32,
}

impl LiveLink {
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
    pub fn set_gain(&self, g: f32) {
        self.gain.store(g.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }
    pub fn share_gain(&self) -> f32 {
        f32::from_bits(self.share_gain.load(Ordering::Relaxed))
    }
    pub fn set_share_gain(&self, g: f32) {
        self.share_gain.store(g.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }
}

struct SharedState {
    sources: std::sync::Mutex<HashMap<u32, AudioSource>>,
    live: std::sync::RwLock<Option<Arc<LiveLink>>>,
    /// The current output device's rate; sources keep their ratio against it.
    device_sample_rate: AtomicU32,
    next_id: AtomicU32,
    /// Channel for deferring audio_ended events off the real-time audio thread
    ended_tx: mpsc::Sender<u32>,
}

struct AudioSource {
    #[allow(dead_code)]
    id: u32,
    samples: Vec<f32>,           // decoded samples at source_sample_rate; interleaved when src_channels == 2
    src_channels: usize,         // 1 (voice/oneshots) or 2 (music keeps its stereo field)
    source_sample_rate: u32,     // native sample rate of the audio
    rate_ratio: f64,             // source_sample_rate / device_sample_rate
    position: f64,               // current position in source FRAMES (fractional for interpolation)
    playing: bool,
    volume: f32,                 // 0.0–1.0
    duration_ms: u64,            // estimated until decode completes, then actual
    oneshot: bool,               // notification sounds — auto-remove on finish
    crossfade: Option<Crossfade>,
    decode_complete: bool,       // true when all samples have been decoded (a window: up to the file's end)
    /// The file frame `samples` starts at: 0 for a whole decode, the window's start otherwise.
    base_frame: u64,
    /// A streamed file's decoder, which a seek outside the window redirects.
    stream: Option<Arc<StreamCtl>>,
    /// Bumped by each redirect, so a batch decoded for the old place is never added.
    window_gen: u64,
    /// Where a redirect sent the window, until its decoder takes it (with the generation,
    /// in one critical section, so a later redirect is never mistaken for this one).
    pending_seek: Option<u64>,
}

/// How the engine redirects a streamed file's decoder.
struct StreamCtl {
    /// A hint that a redirect is waiting, so a batch for the old place is abandoned early.
    /// The redirect itself lives on the source, under its lock.
    redirected: std::sync::atomic::AtomicBool,
    wake: (std::sync::Mutex<bool>, std::sync::Condvar),
}

impl StreamCtl {
    fn nudge(&self) {
        let (lock, cv) = &self.wake;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }
    fn wait(&self, dur: std::time::Duration) {
        let (lock, cv) = &self.wake;
        let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
        if !*woken {
            woken = cv.wait_timeout(woken, dur).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        *woken = false;
    }
}

/// Active crossfade state: blends old position out while new position fades in
struct Crossfade {
    old_position: f64,   // playback position before seek (in source frames, fractional)
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
    /// Base64 of `bins` bytes per frame: a number array is several times larger as JSON.
    waveform: String,
    waveform_fps: f32,
    bins: u8,
}

/// At most this many frames reach the webview (20 minutes at 30 fps): past it frames are
/// pooled, keeping each group's peak, and the rate drops to match. Android's IPC is JSON.
const MAX_WAVEFORM_FRAMES: usize = 36_000;

impl AudioWaveformPayload {
    fn new(id: u32, waveform: Vec<u8>) -> Self {
        let frames = waveform.len() / WAVEFORM_BINS;
        let (data, fps) = if frames > MAX_WAVEFORM_FRAMES {
            let factor = frames.div_ceil(MAX_WAVEFORM_FRAMES);
            let mut pooled = Vec::with_capacity(frames.div_ceil(factor) * WAVEFORM_BINS);
            for group in (0..frames).step_by(factor) {
                let end = (group + factor).min(frames);
                for bin in 0..WAVEFORM_BINS {
                    pooled.push((group..end).map(|f| waveform[f * WAVEFORM_BINS + bin]).max().unwrap_or(0));
                }
            }
            (pooled, WAVEFORM_FPS as f32 / factor as f32)
        } else {
            (waveform, WAVEFORM_FPS as f32)
        };
        AudioWaveformPayload { id, waveform: base64_simd::STANDARD.encode_to_string(&data), waveform_fps: fps, bins: WAVEFORM_BINS as u8 }
    }
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
                if let Some(engine) = ENGINE.get() {
                    engine.start_device_watchdog();
                }
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

    /// Opens the mixer's output stream on the resolved output device.
    fn open_output(shared: &Arc<SharedState>) -> Result<(cpal::Stream, u32, String), String> {
        let device = crate::audio_devices::resolve_output().ok_or("No audio output device found")?;
        let name = device.name().unwrap_or_default();

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

        let shared_for_callback = Arc::clone(shared);
        let channels = device_channels as usize;

        let stream = device
            .build_output_stream(
                &config,
                move |output: &mut [f32], _: &_| {
                    mixer_callback(output, &shared_for_callback, channels);
                },
                |err| {
                    // The device went away under us: reopen now, not at the next poll.
                    eprintln!("[AudioEngine] Stream error: {}", err);
                    AudioEngine::kick();
                },
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start output stream: {}", e))?;
        Ok((stream, device_sample_rate, name))
    }

    /// Wakes the watchdog: a stream reported its device gone, or a preference changed.
    pub fn kick() {
        if let Some(engine) = ENGINE.get() {
            let (lock, cv) = &engine.wake;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
    }

    pub fn input_generation() -> u64 {
        ENGINE.get().map_or(0, |e| e.input_generation.load(Ordering::Relaxed))
    }

    fn create() -> Result<Self, String> {
        // cpal's AAudio host reads device params through `ndk_context`, which
        // panics unregistered (service-only process, or pre-registration) — and a
        // panic here can abort. Refuse with an error instead.
        #[cfg(target_os = "android")]
        if !crate::android::utils::context_registered() {
            return Err("Audio unavailable: Android context not registered".to_string());
        }

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
            live: std::sync::RwLock::new(None),
            device_sample_rate: AtomicU32::new(0),
            next_id: AtomicU32::new(1),
            ended_tx,
        });

        let (stream, rate, output_name) = Self::open_output(&shared)?;
        shared.device_sample_rate.store(rate, Ordering::Relaxed);

        Ok(AudioEngine {
            shared,
            stream: std::sync::Mutex::new(Some(stream)),
            output_name: std::sync::Mutex::new(output_name),
            input_name: std::sync::Mutex::new(crate::audio_devices::resolved_input_name()),
            device_listeners: std::sync::Mutex::new(Vec::new()),
            wake: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
            input_generation: AtomicU64::new(0),
        })
    }

    /// Watches the resolved devices: the OS does not tell cpal when the user picks
    /// another output or microphone, and a stream opened on the old one plays to
    /// nothing. It polls every second and wakes at once when a stream dies or a
    /// preference changes; on a change the output is reopened and the listeners told.
    fn start_device_watchdog(&'static self) {
        // The listeners spawn tasks, and a plain thread carries no runtime.
        let rt = tauri::async_runtime::handle();
        std::thread::Builder::new()
            .name("audio-devices".into())
            .spawn(move || {
                let _rt = rt.inner().enter();
                loop {
                    let kicked = {
                        let (lock, cv) = &self.wake;
                        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        let (mut guard, _) = cv
                            .wait_timeout_while(guard, std::time::Duration::from_secs(1), |k| !*k)
                            .unwrap_or_else(|e| e.into_inner());
                        std::mem::replace(&mut *guard, false)
                    };
                    let output_now = crate::audio_devices::resolved_output_name();
                    let input_now = crate::audio_devices::resolved_input_name();
                    let output_changed = {
                        let mut held = self.output_name.lock().unwrap_or_else(|e| e.into_inner());
                        if *held != output_now && !output_now.is_empty() {
                            *held = output_now.clone();
                            true
                        } else {
                            false
                        }
                    };
                    let input_changed = {
                        let mut held = self.input_name.lock().unwrap_or_else(|e| e.into_inner());
                        if *held != input_now && !input_now.is_empty() {
                            *held = input_now.clone();
                            true
                        } else {
                            false
                        }
                    };
                    // A kick with no visible change still means a stream died: reopen anyway.
                    if output_changed || kicked {
                        match self.rebuild_output() {
                            Ok(()) => println!("[AudioEngine] Output on {output_now}"),
                            Err(e) => eprintln!("[AudioEngine] Could not open the output device: {e}"),
                        }
                    }
                    if input_changed || kicked {
                        self.input_generation.fetch_add(1, Ordering::Relaxed);
                        println!("[AudioEngine] Input on {input_now}");
                    }
                    if output_changed || input_changed || kicked {
                        for f in self.device_listeners.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                            // A panicking listener must not take the watchdog with it:
                            // the thread dying ends device following for the process.
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                                eprintln!("[AudioEngine] A device listener panicked");
                            }
                        }
                        if let Some(app) = TAURI_APP.get() {
                            let _ = app.emit("audio_devices_changed", ());
                        }
                    }
                }
            })
            .ok();
    }

    /// Reopens the output on the current default device and rebases every
    /// source's rate ratio on the new rate.
    fn rebuild_output(&self) -> Result<(), String> {
        let (stream, rate, _) = Self::open_output(&self.shared)?;
        {
            let mut sources = self.shared.sources.lock().unwrap_or_else(|e| e.into_inner());
            for src in sources.values_mut() {
                src.rate_ratio = src.source_sample_rate as f64 / rate as f64;
            }
            self.shared.device_sample_rate.store(rate, Ordering::Relaxed);
        }
        let old = self.stream.lock().unwrap_or_else(|e| e.into_inner()).replace(stream);
        drop(old);
        // Only desktop keeps a sample-rate cache to invalidate.
        #[cfg(desktop)]
        crate::audio::invalidate_device_sample_rate_cache();
        Ok(())
    }

    /// Registers a callback for default device changes.
    pub fn on_device_change(&self, f: Box<dyn Fn() + Send + Sync>) {
        self.device_listeners.lock().unwrap_or_else(|e| e.into_inner()).push(f);
    }

    /// One call at a time on the mixer.
    pub fn attach_live(&self, link: Arc<LiveLink>) -> Result<(), String> {
        let mut slot = self.shared.live.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("A call is already on the mixer".into());
        }
        *slot = Some(link);
        Ok(())
    }

    pub fn detach_live(&self) {
        *self.shared.live.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Load audio from file path. WAV uses instant decode; other formats stream-decode
    /// in a background thread so playback can begin within milliseconds.
    pub fn load_from_file(&self, path: &str) -> Result<AudioLoadResult, String> {
        let path_buf = std::path::PathBuf::from(path);
        if !path_buf.exists() {
            return Err("Audio file not found".to_string());
        }

        // Only a WAV is read whole; every other decoder opens its own handle on the file.
        if has_wav_header(&path_buf) {
            let file_bytes = std::fs::read(&path_buf)
                .map_err(|e| format!("Failed to read audio file: {}", e))?;
            if let Some((mono_samples, sample_rate)) = crate::audio::wav_fast_decode_for_engine(&file_bytes) {
                return self.load_from_samples(mono_samples, sample_rate);
            }
        }
        self.load_streaming(&path_buf)
    }

    /// Stream-decode a non-WAV audio file. Probes metadata synchronously (fast),
    /// creates an empty source, then spawns a background thread for packet-by-packet decode.
    fn load_streaming(&self, path: &std::path::Path) -> Result<AudioLoadResult, String> {
        // Probe metadata from file (re-opens from disk, already in page cache — <1ms)
        let (sample_rate, channels, est_frames) = probe_audio_metadata(path)?;
        if sample_rate == 0 || self.shared.device_sample_rate.load(Ordering::Relaxed) == 0 {
            return Err("Invalid sample rate".to_string());
        }

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let rate_ratio = sample_rate as f64 / self.shared.device_sample_rate.load(Ordering::Relaxed) as f64;
        let duration_ms = if sample_rate > 0 { est_frames * 1000 / sample_rate as u64 } else { 0 };
        // Music keeps its stereo field; >2ch sources carry front L/R.
        let src_channels = channels.clamp(1, 2);

        self.evict_if_needed();

        if est_frames >= STREAM_MIN_SECS * sample_rate as u64 {
            return self.load_windowed(id, path, sample_rate, channels, src_channels, rate_ratio, duration_ms);
        }

        let source = AudioSource {
            id,
            samples: Vec::with_capacity(est_frames as usize * src_channels),
            src_channels,
            source_sample_rate: sample_rate,
            rate_ratio,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms,
            oneshot: false,
            crossfade: None,
            decode_complete: false,
            base_frame: 0,
            stream: None,
            window_gen: 0,
            pending_seek: None,
        };

        self.shared
            .sources
            .lock()
            .map_err(|_| "Lock poisoned")?
            .insert(id, source);

        // Spawn background decode thread
        let shared = Arc::clone(&self.shared);
        let path = path.to_path_buf();
        std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                stream_decode_worker(id, &path, sample_rate, est_frames, &shared);
            })
            .map_err(|e| format!("Failed to spawn decode thread: {}", e))?;

        Ok(AudioLoadResult {
            id,
            duration_ms,
            waveform_fps: WAVEFORM_FPS as u8,
            bins: WAVEFORM_BINS as u8,
        })
    }

    /// A long file plays from a window around the playhead: a decoder follows the playhead
    /// (and jumps with a seek), and a separate pass builds the waveform, keeping none of
    /// the audio it decodes.
    #[allow(clippy::too_many_arguments)]
    fn load_windowed(
        &self,
        id: u32,
        path: &std::path::Path,
        sample_rate: u32,
        channels: usize,
        src_channels: usize,
        rate_ratio: f64,
        duration_ms: u64,
    ) -> Result<AudioLoadResult, String> {
        let ctl = Arc::new(StreamCtl {
            redirected: std::sync::atomic::AtomicBool::new(false),
            wake: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
        });
        let source = AudioSource {
            id,
            samples: Vec::with_capacity(window_capacity(sample_rate, src_channels)),
            src_channels,
            source_sample_rate: sample_rate,
            rate_ratio,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms,
            oneshot: false,
            crossfade: None,
            decode_complete: false,
            base_frame: 0,
            stream: Some(Arc::clone(&ctl)),
            window_gen: 0,
            pending_seek: None,
        };
        self.shared.sources.lock().map_err(|_| "Lock poisoned")?.insert(id, source);

        {
            let (path, shared) = (path.to_path_buf(), Arc::clone(&self.shared));
            std::thread::Builder::new()
                .name("audio-window".into())
                .spawn(move || window_decode_worker(id, &path, channels, sample_rate, ctl, &shared))
                .map_err(|e| format!("Failed to spawn window decoder: {}", e))?;
        }
        {
            let (path, shared) = (path.to_path_buf(), Arc::clone(&self.shared));
            let est_frames = duration_ms * sample_rate as u64 / 1000;
            std::thread::Builder::new()
                .name("audio-waveform".into())
                .spawn(move || waveform_pass(id, &path, sample_rate, est_frames, shared))
                .map_err(|e| format!("Failed to spawn waveform pass: {}", e))?;
        }
        Ok(AudioLoadResult { id, duration_ms, waveform_fps: WAVEFORM_FPS as u8, bins: WAVEFORM_BINS as u8 })
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
        if sample_rate == 0 || self.shared.device_sample_rate.load(Ordering::Relaxed) == 0 {
            return Err("Invalid sample rate".to_string());
        }

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let rate_ratio = sample_rate as f64 / self.shared.device_sample_rate.load(Ordering::Relaxed) as f64;
        let duration_ms = (samples.len() as u64 * 1000) / sample_rate as u64;

        // Compute waveform in background — playback can start immediately
        let samples_for_fft = samples.clone();
        std::thread::Builder::new()
            .name("audio-fft".into())
            .spawn(move || {
                let waveform = precompute_fft_waveform(&samples_for_fft, sample_rate);
                if let Some(app) = TAURI_APP.get() {
                    let _ = app.emit("audio_waveform", AudioWaveformPayload::new(id, waveform));
                }
            })
            .ok();

        // Evict oldest paused sources if at capacity
        self.evict_if_needed();

        let source = AudioSource {
            id,
            samples,
            src_channels: 1,
            source_sample_rate: sample_rate,
            rate_ratio,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms,
            oneshot: false,
            crossfade: None,
            decode_complete: true, // All samples already available
            base_frame: 0,
            stream: None,
            window_gen: 0,
            pending_seek: None,
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
        // Mixer stops when frame pos+1 isn't fully decoded, so position ends near the last frame
        let end_of_window = ((source.position - source.base_frame as f64).max(0.0) as usize + 2) * source.src_channels
            > source.samples.len();
        if source.decode_complete && end_of_window {
            source.position = 0.0;
            // A window that ends at the file's end holds no start to rewind to.
            if source.base_frame > 0 {
                redirect_window(source, 0);
            }
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
        // Never past the file: the mixer indexes from this position on the audio thread.
        let end_frame = (source.duration_ms.saturating_mul(source.source_sample_rate as u64) / 1000).max(1);
        let frame_pos = (position_ms as f64 * source.source_sample_rate as f64 / 1000.0).min((end_frame - 1) as f64);
        let old_position = source.position;
        if source.stream.is_some() {
            let frames_len = (source.samples.len() / source.src_channels) as u64;
            let target = frame_pos as u64;
            let in_window = target >= source.base_frame && target + 2 < source.base_frame + frames_len;
            if !in_window {
                source.position = target as f64;
                redirect_window(source, target);
                return Ok(());
            }
        }
        // During streaming decode, allow seeking beyond decoded data — the mixer
        // outputs silence until decode catches up. For fully-decoded sources,
        // clamp to file length to prevent seeking past the end.
        let base = source.base_frame as f64;
        let frames_end = base + (source.samples.len() / source.src_channels) as f64;
        source.position = if source.decode_complete {
            frame_pos.min(frames_end)
        } else {
            frame_pos
        };
        // Only crossfade if the new position has decoded audio to blend into.
        // Seeking beyond decoded data during streaming → no crossfade (just silence).
        if source.playing
            && source.position >= base
            && (((source.position - base) as usize) + 2) * source.src_channels <= source.samples.len()
        {
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
        let duration_ms = (samples.len() as u64 * 1000) / self.shared.device_sample_rate.load(Ordering::Relaxed) as u64;

        let source = AudioSource {
            id,
            samples,
            src_channels: 1,
            source_sample_rate: self.shared.device_sample_rate.load(Ordering::Relaxed),
            rate_ratio: 1.0, // already at device rate
            position: 0.0,
            playing: true, // start immediately
            volume: 1.0,
            duration_ms,
            oneshot: true,
            crossfade: None,
            decode_complete: true,
            base_frame: 0,
            stream: None,
            window_gen: 0,
            pending_seek: None,
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
        self.shared.device_sample_rate.load(Ordering::Relaxed)
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

fn has_wav_header(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && &head[0..4] == b"RIFF"
        && &head[8..12] == b"WAVE"
}

/// Fast WAV duration probe: reads RIFF header, computes duration from data chunk size.
fn wav_probe_duration(path: &std::path::Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    // Header chunks only: the data chunk's size gives the length without reading the audio.
    let mut file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut audio_format = 0u16;

    loop {
        let mut header = [0u8; 8];
        file.read_exact(&mut header).ok()?;
        let chunk_size = u32::from_le_bytes(header[4..8].try_into().ok()?) as u64;
        let mut skip = chunk_size + chunk_size % 2;

        if &header[0..4] == b"fmt " {
            if chunk_size < 16 { return None; }
            let mut d = [0u8; 16];
            file.read_exact(&mut d).ok()?;
            skip -= 16;
            audio_format = u16::from_le_bytes([d[0], d[1]]);
            channels = u16::from_le_bytes([d[2], d[3]]);
            sample_rate = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
            bits_per_sample = u16::from_le_bytes([d[14], d[15]]);
        } else if &header[0..4] == b"data" {
            if audio_format != 1 && audio_format != 3 { return None; }
            if channels == 0 || sample_rate == 0 || bits_per_sample == 0 { return None; }
            let bytes_per_frame = (bits_per_sample as u64 / 8) * channels as u64;
            if bytes_per_frame == 0 { return None; }
            return Some(chunk_size / bytes_per_frame * 1000 / sample_rate as u64);
        }
        file.seek(SeekFrom::Current(skip as i64)).ok()?;
    }
}

/// Send a streamed file's decoder to `frame`: the old window's audio stops at once and the
/// new place is silent for the moments it takes to decode.
fn redirect_window(source: &mut AudioSource, frame: u64) {
    let Some(ctl) = source.stream.clone() else { return };
    // Emptied here, under the lock: a second seek before the decoder wakes must see no
    // window to land in, or it would land in the old one.
    source.samples.clear();
    source.base_frame = frame;
    source.crossfade = None;
    source.decode_complete = false;
    source.window_gen += 1;
    source.pending_seek = Some(frame);
    ctl.redirected.store(true, Ordering::Relaxed);
    ctl.nudge();
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
        let src_ch = source.src_channels;
        // Positions are file frames; a window's samples begin at base_frame.
        let base = source.base_frame as f64;

        for frame in output.chunks_mut(channels) {
            // Before a window's start: its decoder is still filling the new place.
            if source.position < base {
                break;
            }
            let rel = source.position - base;
            let pos_floor = rel as usize;

            // Interpolation reads frames pos_floor and pos_floor+1 — both must
            // be fully decoded (frame-aligned in the interleaved buffer).
            if pos_floor.saturating_add(2).saturating_mul(src_ch) > source.samples.len() {
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

            let frac = (rel - pos_floor as f64) as f32;
            let base0 = pos_floor * src_ch;
            let base1 = base0 + src_ch;

            // Crossfade bookkeeping advances once per FRAME; each channel then
            // blends against the same old position. Mode: 2 = interpolate,
            // 1 = flat tail sample, 0 = past the end (silence).
            let (xf_t, xf_base, xf_frac, xf_mode) = if let Some(ref mut xfade) = source.crossfade {
                let t = xfade.remaining as f32 / CROSSFADE_SAMPLES as f32;
                let old_rel = (xfade.old_position - base).max(0.0);
                let old_floor = old_rel as usize;
                let old_frac = (old_rel - old_floor as f64) as f32;
                let mode: u8 = if xfade.old_position < base {
                    0
                } else if (old_floor + 2) * src_ch <= source.samples.len() {
                    2
                } else if (old_floor + 1) * src_ch <= source.samples.len() {
                    1
                } else {
                    0
                };
                let base = old_floor * src_ch;
                xfade.old_position += source.rate_ratio;
                xfade.remaining -= 1;
                let active = xfade.remaining > 0;
                if !active {
                    source.crossfade = None;
                }
                (t, base, old_frac, Some(mode))
            } else {
                (0.0, 0, 0.0, None)
            };

            for (out_ch, out) in frame.iter_mut().enumerate() {
                // Mono duplicates everywhere; stereo maps L/R, extra device
                // channels ride the last source channel.
                let c = out_ch.min(src_ch - 1);
                let s0 = source.samples[base0 + c];
                let s1 = source.samples[base1 + c];
                let new_sample = s0 + (s1 - s0) * frac;

                let sample = match xf_mode {
                    Some(mode) => {
                        let old_sample = match mode {
                            2 => {
                                let os0 = source.samples[xf_base + c];
                                let os1 = source.samples[xf_base + src_ch + c];
                                os0 + (os1 - os0) * xf_frac
                            }
                            1 => source.samples[xf_base + c],
                            _ => 0.0,
                        };
                        (old_sample * xf_t + new_sample * (1.0 - xf_t)) * source.volume
                    }
                    None => new_sample * source.volume,
                };
                *out += sample;
            }
            source.position += source.rate_ratio;
        }
    }

    // Handle finished oneshot sources
    for &(id, is_oneshot) in &finished_buf[..finished_count] {
        if is_oneshot {
            sources.remove(&id);
        }
    }

    // Drop the lock — file sources are mixed
    drop(sources);

    // A live call: its samples onto every channel, then the finished mix back to it.
    // try_read: the callback never waits on the attach/detach writer.
    if let Ok(slot) = shared.live.try_read() {
        if let Some(link) = slot.as_ref() {
            let mut mono = [0f32; 512];
            let mut stereo = [0f32; 1024];
            for block in output.chunks_mut(channels * mono.len()) {
                let frames = block.len() / channels;
                let got = link.play.pop(&mut mono[..frames]);
                mono[got..frames].fill(0.0);
                let gain = link.gain();
                let got2 = link.share_play.pop(&mut stereo[..frames * 2]);
                stereo[got2..frames * 2].fill(0.0);
                let share_gain = link.share_gain();
                for (i, frame) in block.chunks_mut(channels).enumerate() {
                    for (c, out) in frame.iter_mut().enumerate() {
                        let side = stereo[i * 2 + c.min(1)];
                        *out = (*out + mono[i] * gain + side * share_gain).clamp(-1.0, 1.0);
                    }
                    mono[i] = frame.iter().sum::<f32>() / channels as f32;
                }
                link.tap.push(&mono[..frames]);
                link.share_tap.push(&mono[..frames]);
            }
        }
    }

    // Clamp output to [-1.0, 1.0] before any non-audio work
    for s in output.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }

    // Defer audio_ended events to background thread (no allocations on RT thread)
    for &(id, is_oneshot) in &finished_buf[..finished_count] {
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
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
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

    let declared_rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let n_frames = track.codec_params.n_frames;
    let params = track.codec_params.clone();
    let track_id = track.id;

    // The header can lie: HE-AAC declares the 2x SBR extension rate, but
    // symphonia has no SBR support and decodes the AAC-LC core at half rate.
    // The decoder's own output spec is the truth, so decode one packet and
    // let it override the declared rate/channels.
    let mut sample_rate = declared_rate;
    let mut channels = params.channels.map(|c| c.count());
    if let Ok(mut decoder) = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()) {
        let mut format = probed.format;
        for _ in 0..16 {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(_) => break,
            };
            if packet.track_id() != track_id {
                continue;
            }
            if let Ok(buf) = decoder.decode(&packet) {
                let spec = buf.spec();
                if spec.rate > 0 {
                    sample_rate = spec.rate;
                }
                if spec.channels.count() > 0 {
                    channels = Some(spec.channels.count());
                }
                break;
            }
        }
    }

    let channels = channels.unwrap_or(2);
    // n_frames counts in declared-rate units, so it rescales with the rate.
    // Without it, estimate ~5 minutes.
    let est_frames = match n_frames {
        Some(n) => n.saturating_mul(sample_rate as u64) / declared_rate.max(1) as u64,
        None => sample_rate as u64 * 300,
    };

    Ok((sample_rate, channels, est_frames))
}

// ============================================================================
// Streaming decode worker (runs on background thread)
// ============================================================================

/// Background decode entry: long files split into parallel segments (each
/// core decodes its own slice, seam-primed by a warmup pre-roll); short files
/// take the simple sequential path.
fn stream_decode_worker(id: u32, path: &std::path::Path, sample_rate: u32, est_frames: u64, shared: &SharedState) {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let workers = cores.saturating_sub(2).min(MAX_DECODE_WORKERS);
    let est_secs = if sample_rate > 0 { est_frames / sample_rate as u64 } else { 0 };
    if workers >= 2 && est_secs >= PARALLEL_MIN_SECS {
        parallel_stream_decode(id, path, sample_rate, est_frames, workers, shared);
    } else {
        sequential_stream_decode(id, path, sample_rate, shared);
    }
}

/// One decoded segment: engine-layout samples plus the mono mix feeding the
/// FFT waveform (for mono sources the mix duplicates the samples).
struct SegmentOut {
    samples: Vec<f32>,
    mono: Vec<f32>,
}

/// Append `take` frames (after skipping `skip`) of an interleaved packet to
/// the engine-layout buffer + mono mix. >2ch sources keep front L/R.
fn append_packet_frames(
    samples: &[f32],
    channels: usize,
    skip_frames: usize,
    take_frames: usize,
    out: &mut Vec<f32>,
    mono: &mut Vec<f32>,
) {
    if channels <= 1 {
        let start = skip_frames.min(samples.len());
        let end = (start + take_frames).min(samples.len());
        out.extend_from_slice(&samples[start..end]);
        mono.extend_from_slice(&samples[start..end]);
        return;
    }
    let start = (skip_frames * channels).min(samples.len());
    let end = ((skip_frames + take_frames) * channels).min(samples.len());
    let slice = &samples[start..end];
    if channels == 2 {
        // Interleaved stereo IS the engine layout — one memcpy.
        out.extend_from_slice(slice);
        for pair in slice.chunks_exact(2) {
            mono.push((pair[0] + pair[1]) * 0.5);
        }
    } else {
        for chunk in slice.chunks_exact(channels) {
            out.push(chunk[0]);
            out.push(chunk[1]);
            let sum: f32 = chunk.iter().sum();
            mono.push(sum / channels as f32);
        }
    }
}

/// A demuxer and decoder over their own handle on the file, counting frames as they go.
struct PacketCursor {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    time_base: Option<symphonia::core::units::TimeBase>,
    buf: Option<symphonia::core::audio::SampleBuffer<f32>>,
    /// The frame after the last packet handed out.
    frame: u64,
}

impl PacketCursor {
    fn open(path: &std::path::Path) -> Result<Self, String> {
        use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(path).map_err(|e| format!("open: {}", e))?;
        let media_source = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .format(&hint, media_source, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| format!("probe: {}", e))?
            .format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("no audio track")?;
        let (track_id, time_base) = (track.id, track.codec_params.time_base);
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("decoder: {}", e))?;
        Ok(Self { format, decoder, track_id, time_base, buf: None, frame: 0 })
    }

    /// Seek a warmup margin before `frame`, counting from wherever the seek lands. The
    /// discarded run-in re-primes the codec's inter-frame state (MP3 bit reservoir, IMDCT
    /// overlap-add) so what follows matches a straight-through decode.
    fn seek_before(&mut self, frame: u64, sample_rate: u32) -> Result<(), String> {
        use symphonia::core::formats::{SeekMode, SeekTo};
        use symphonia::core::units::Time;
        let warmup = (SEGMENT_WARMUP_SECS * sample_rate as f64) as u64;
        let target_secs = frame.saturating_sub(warmup) as f64 / sample_rate as f64;
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Time { time: Time::from(target_secs), track_id: Some(self.track_id) })
            .map_err(|e| format!("seek: {}", e))?;
        self.decoder.reset();
        self.frame = match self.time_base {
            Some(tb) => {
                let t = tb.calc_time(seeked.actual_ts);
                ((t.seconds as f64 + t.frac) * sample_rate as f64).round() as u64
            }
            None => seeked.actual_ts,
        };
        Ok(())
    }

    /// The next packet as (interleaved samples, channels, first frame); None once the
    /// stream ends or can't go on.
    fn next(&mut self) -> Option<(&[f32], usize, u64)> {
        use symphonia::core::audio::SampleBuffer;
        use symphonia::core::errors::Error as SymphoniaError;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(_) => return None,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio_buf) => {
                    let channels = audio_buf.spec().channels.count().max(1);
                    let buf = self
                        .buf
                        .get_or_insert_with(|| SampleBuffer::<f32>::new(audio_buf.capacity() as u64, *audio_buf.spec()));
                    buf.copy_interleaved_ref(audio_buf);
                    let start = self.frame;
                    self.frame += (buf.samples().len() / channels) as u64;
                    return Some((buf.samples(), channels, start));
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return None,
            }
        }
    }
}

/// Receives (interleaved samples, channels, frames to skip, frames to take).
type FrameSink<'a> = dyn FnMut(&[f32], usize, usize, usize) -> bool + 'a;

/// Decode frames [start_frame, end_frame), handing each packet's kept frames to `sink`
/// as (interleaved samples, channels, frames to skip, frames to take). The sink returns
/// false to stop.
fn decode_range(
    path: &std::path::Path,
    sample_rate: u32,
    start_frame: u64,
    end_frame: u64,
    cancel: &std::sync::atomic::AtomicBool,
    sink: &mut FrameSink<'_>,
) -> Result<(), String> {
    let mut cursor = PacketCursor::open(path)?;
    if start_frame > 0 {
        cursor.seek_before(start_frame, sample_rate)?;
    }
    while cursor.frame < end_frame && !cancel.load(Ordering::Relaxed) {
        let Some((samples, channels, start)) = cursor.next() else { break };
        let end = start + (samples.len() / channels) as u64;
        if end <= start_frame {
            continue; // still inside the warmup run-in
        }
        let skip = start_frame.saturating_sub(start) as usize;
        let take = (end.min(end_frame) - start) as usize - skip;
        if take > 0 && !sink(samples, channels, skip, take) {
            break;
        }
    }
    Ok(())
}

/// Decode frames [start_frame, end_frame) with a fresh decoder, kept exactly as a straight-through decode would produce them.
/// `progressive` flushes playback batches into the source as they land (the head
/// segment's instant-start path).
fn decode_segment(
    path: &std::path::Path,
    sample_rate: u32,
    start_frame: u64,
    end_frame: u64,
    progressive: Option<(u32, &SharedState)>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<SegmentOut, String> {
    let mut out = SegmentOut { samples: Vec::new(), mono: Vec::new() };
    let mut removed = false;
    decode_range(path, sample_rate, start_frame, end_frame, cancel, &mut |samples, ch, skip, take| {
        append_packet_frames(samples, ch, skip, take, &mut out.samples, &mut out.mono);
        if let Some((id, shared)) = progressive {
            if out.samples.len() >= DECODE_BATCH_SIZE && !flush_decode_batch(id, &mut out.samples, shared) {
                removed = true;
                return false;
            }
        }
        true
    })?;
    if removed {
        cancel.store(true, Ordering::Relaxed);
        return Err("source removed".to_string());
    }
    Ok(out)
}

// ============================================================================
// Windowed playback: long files decode around the playhead
// ============================================================================

/// Room for the window at its largest (ahead, the trim margin, a spare batch), reserved
/// once so an append under the audio lock never reallocates.
fn window_capacity(sample_rate: u32, src_channels: usize) -> usize {
    ((WINDOW_AHEAD_SECS + WINDOW_TRIM_SECS + 4) * sample_rate as u64) as usize * src_channels + DECODE_BATCH_SIZE * 4
}

/// Keeps a streamed source's window filled: up to WINDOW_AHEAD_SECS past the playhead,
/// letting played audio go past WINDOW_TRIM_SECS, and starting over wherever a seek outside
/// the window sends it. A mirror of the window lives here, so letting audio go is a fresh
/// buffer built outside the lock and swapped in: the audio thread never waits on a copy.
fn window_decode_worker(
    id: u32,
    path: &std::path::Path,
    channels: usize,
    sample_rate: u32,
    ctl: Arc<StreamCtl>,
    shared: &SharedState,
) {
    let mut cursor = match PacketCursor::open(path) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[AudioEngine] Window decoder failed to open source {}: {}", id, e);
            mark_decode_complete(id, shared);
            return;
        }
    };
    let src_ch = channels.clamp(1, 2);
    let rate = sample_rate as u64;
    let (ahead, behind, trim) = (WINDOW_AHEAD_SECS * rate, WINDOW_BEHIND_SECS * rate, WINDOW_TRIM_SECS * rate);
    let cap = window_capacity(sample_rate, src_ch);
    let mut mirror: Vec<f32> = Vec::with_capacity(cap);
    let mut base: u64 = 0;
    let mut skip_until: u64 = 0;
    let mut gen: u64 = 0;
    let mut eof = false;
    let mut batch: Vec<f32> = Vec::with_capacity(DECODE_BATCH_SIZE * 2);
    let mut mono_scratch: Vec<f32> = Vec::new();

    loop {
        // Where the playhead is, and any redirect with its generation, read together.
        let (pos, redirect) = match shared.sources.lock() {
            Ok(mut sources) => match sources.get_mut(&id) {
                Some(src) => {
                    let redirect = src.pending_seek.take();
                    if redirect.is_some() {
                        ctl.redirected.store(false, Ordering::Relaxed);
                        gen = src.window_gen;
                    } else if src.position + 2.0 < src.base_frame as f64 {
                        // Behind the window with nothing on its way: go to the playhead.
                        let at = src.position.max(0.0) as u64;
                        redirect_window(src, at);
                        continue;
                    }
                    (src.position.max(0.0) as u64, redirect)
                }
                None => return,
            },
            Err(_) => return,
        };

        // Redirected: start again from the new place (the source's window is already empty).
        if let Some(target) = redirect {
            batch.clear();
            mirror.clear();
            base = target;
            skip_until = target;
            match cursor.seek_before(target, sample_rate) {
                Ok(()) => eof = false,
                Err(e) => {
                    // Nowhere to decode from (past the end, or a demuxer that can't seek):
                    // the window ends here rather than playing the old place as the new one.
                    eprintln!("[AudioEngine] Window seek failed for source {}: {}", id, e);
                    eof = true;
                    if let Ok(mut sources) = shared.sources.lock() {
                        if let Some(src) = sources.get_mut(&id) {
                            if src.window_gen == gen { src.decode_complete = true; }
                        }
                    }
                }
            }
            continue;
        }
        let end = base + (mirror.len() / src_ch) as u64;

        // Let go of what has played.
        if pos > base + trim && pos <= end {
            let keep_from = pos - behind;
            let cut = ((keep_from - base) as usize) * src_ch;
            let mut fresh = Vec::with_capacity(cap);
            fresh.extend_from_slice(&mirror[cut..]);
            let swapped = match shared.sources.lock() {
                Ok(mut sources) => match sources.get_mut(&id) {
                    // Not if the reader seeked back into what this would let go.
                    Some(src) if src.window_gen == gen && src.position >= keep_from as f64 => {
                        std::mem::swap(&mut src.samples, &mut fresh);
                        src.base_frame = keep_from;
                        true
                    }
                    Some(_) => false,
                    None => return,
                },
                Err(_) => return,
            };
            // `fresh` now holds the old window: freed here, off the lock.
            drop(fresh);
            if swapped {
                mirror.drain(..cut);
                base = keep_from;
            }
            continue;
        }

        if eof || end >= pos + ahead {
            ctl.wait(std::time::Duration::from_millis(40));
            continue;
        }

        // Decode one batch, abandoning it if a redirect lands meanwhile.
        while batch.len() < DECODE_BATCH_SIZE && !eof {
            if ctl.redirected.load(Ordering::Relaxed) {
                break;
            }
            let Some((samples, channels, start)) = cursor.next() else {
                eof = true;
                break;
            };
            let frames = samples.len() / channels;
            if start + frames as u64 <= skip_until {
                continue;
            }
            let skip = skip_until.saturating_sub(start) as usize;
            append_packet_frames(samples, channels, skip, frames - skip, &mut batch, &mut mono_scratch);
            mono_scratch.clear();
        }
        if ctl.redirected.load(Ordering::Relaxed) {
            batch.clear();
            continue;
        }

        let added = match shared.sources.lock() {
            Ok(mut sources) => match sources.get_mut(&id) {
                Some(src) if src.window_gen == gen => {
                    src.samples.extend_from_slice(&batch);
                    if eof {
                        src.decode_complete = true;
                    }
                    true
                }
                Some(_) => false,
                None => return,
            },
            Err(_) => return,
        };
        if added {
            mirror.extend_from_slice(&batch);
        }
        batch.clear();
    }
}

/// A streamed file's waveform, from a pass that decodes it in parallel and keeps nothing
/// but the band levels: 64 bytes per frame at 30 fps, not the audio itself.
/// Segments this many packets apart check the source still exists, so a pass for a song the
/// listener moved on from stops rather than decoding the rest of the file.
const WAVEFORM_LIVENESS_PACKETS: u32 = 256;

type WaveformPart = (Vec<f32>, u64);

fn waveform_pass(id: u32, path: &std::path::Path, sample_rate: u32, est_frames: u64, shared: Arc<SharedState>) {
    let t0 = std::time::Instant::now();
    let hop = WaveformComputer::new(sample_rate).hop_size as u64;
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let workers = cores.saturating_sub(2).clamp(1, MAX_DECODE_WORKERS);
    // Segments start on a hop so each one's frames line up with the whole file's.
    let per = ((est_frames / workers as u64) / hop).max(1) * hop;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<(usize, Result<WaveformPart, String>)>();
    for w in 0..workers {
        let start = per * w as u64;
        let end = if w + 1 == workers { u64::MAX } else { per * (w as u64 + 1) };
        let (path, tx, stop, shared) = (path.to_path_buf(), tx.clone(), Arc::clone(&cancel), Arc::clone(&shared));
        let spawned = std::thread::Builder::new()
            .name(format!("audio-waveform-{}", w))
            .spawn(move || {
                let mut computer = WaveformComputer::new(sample_rate);
                let mut pending: Vec<f32> = Vec::new();
                let mut scratch: Vec<f32> = Vec::new();
                let mut frames = 0u64;
                let mut packets = 0u32;
                let r = decode_range(&path, sample_rate, start, end, &stop, &mut |samples, ch, skip, take| {
                    packets += 1;
                    if packets.is_multiple_of(WAVEFORM_LIVENESS_PACKETS)
                        && !shared.sources.lock().map(|s| s.contains_key(&id)).unwrap_or(false)
                    {
                        stop.store(true, Ordering::Relaxed);
                        return false;
                    }
                    append_packet_frames(samples, ch, skip, take, &mut scratch, &mut pending);
                    scratch.clear();
                    frames += take as u64;
                    if pending.len() >= DECODE_BATCH_SIZE {
                        computer.feed(&mut pending);
                    }
                    true
                });
                computer.feed(&mut pending);
                let _ = tx.send((w, r.map(|_| (computer.levels, frames))));
            });
        // A missing segment would shift every level after it: no waveform beats a wrong one.
        if spawned.is_err() {
            cancel.store(true, Ordering::Relaxed);
            return;
        }
    }
    drop(tx);

    let mut parts: Vec<Option<WaveformPart>> = (0..workers).map(|_| None).collect();
    for (w, r) in rx {
        parts[w] = Some(match r {
            Ok(part) => part,
            // A segment that can't start is past the real end: a size estimate that overshot.
            Err(e) if w > 0 => {
                eprintln!("[AudioEngine] Waveform segment {} empty for source {}: {}", w, id, e);
                (Vec::new(), 0)
            }
            Err(e) => {
                eprintln!("[AudioEngine] Waveform pass failed for source {}: {}", id, e);
                cancel.store(true, Ordering::Relaxed);
                return;
            }
        });
    }
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let mut levels: Vec<f32> = Vec::new();
    let mut frames = 0u64;
    for (part_levels, part_frames) in parts.into_iter().flatten() {
        levels.extend(part_levels);
        frames += part_frames;
    }
    let waveform = normalise_levels(&levels, BAND_MAX_LIFT_DB);
    let duration_ms = frames * 1000 / sample_rate.max(1) as u64;
    let alive = match shared.sources.lock() {
        Ok(mut sources) => match sources.get_mut(&id) {
            Some(src) => {
                src.duration_ms = duration_ms;
                true
            }
            None => false,
        },
        Err(_) => false,
    };
    if !alive {
        return;
    }
    println!("[AudioEngine] Waveform pass for streamed source {}: {}ms, {} workers, took {:?}", id, duration_ms, workers, t0.elapsed());
    if let Some(app) = TAURI_APP.get() {
        let _ = app.emit("audio_waveform", AudioWaveformPayload::new(id, waveform));
        let _ = app.emit("audio_duration", AudioDurationPayload { id, duration_ms });
    }
}

/// Parallel decode: the head span decodes on this thread with progressive
/// flushes (instant playback), while N workers decode equal tail segments
/// concurrently. Segments splice strictly in order; any failure falls back to
/// finishing sequentially from the splice point, so output is always
/// gap-free. Wall clock ≈ sequential / workers.
#[allow(clippy::too_many_arguments)]
fn parallel_stream_decode(
    id: u32,
    path: &std::path::Path,
    sample_rate: u32,
    est_frames: u64,
    workers: usize,
    shared: &SharedState,
) {
    let t0 = std::time::Instant::now();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let head_end = (PARALLEL_HEAD_SECS * sample_rate as u64).min(est_frames);
    let span = (est_frames.saturating_sub(head_end)) / workers as u64;

    let (tx, rx) = mpsc::channel::<(usize, Result<SegmentOut, String>)>();
    for w in 0..workers {
        let start = head_end + span * w as u64;
        // The last segment runs to EOF so an estimate that undershoots never
        // truncates the file; overshooting estimates just yield empty tails.
        let end = if w + 1 == workers { u64::MAX } else { head_end + span * (w as u64 + 1) };
        let path = path.to_path_buf();
        let tx = tx.clone();
        let cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name(format!("audio-decode-{}", w))
            .spawn(move || {
                let r = decode_segment(&path, sample_rate, start, end, None, &cancel);
                let _ = tx.send((w, r));
            })
            .ok();
    }
    drop(tx);

    // Head: [0, head_end) with progressive flushes, exactly like the
    // sequential path's instant start.
    let mut waveform_computer = WaveformComputer::new(sample_rate);
    let mut mono_acc: Vec<f32> = Vec::new();
    let mut head = match decode_segment(path, sample_rate, 0, head_end, Some((id, shared)), &cancel) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[AudioEngine] Head decode failed for source {}: {}", id, e);
            cancel.store(true, Ordering::Relaxed);
            mark_decode_complete(id, shared);
            return;
        }
    };
    if !flush_decode_batch(id, &mut head.samples, shared) {
        cancel.store(true, Ordering::Relaxed);
        return;
    }
    mono_acc.append(&mut head.mono);
    waveform_computer.process(&mono_acc);

    // Splice tail segments strictly in order; early finishers park until their turn.
    let mut parked: HashMap<usize, Result<SegmentOut, String>> = HashMap::new();
    let mut next = 0usize;
    let mut failed = false;
    while next < workers {
        let entry = match parked.remove(&next) {
            Some(r) => r,
            None => match rx.recv() {
                Ok((w, r)) => {
                    if w != next {
                        parked.insert(w, r);
                        continue;
                    }
                    r
                }
                Err(_) => {
                    failed = true;
                    break;
                }
            },
        };
        match entry {
            Ok(mut seg) => {
                if !flush_decode_batch(id, &mut seg.samples, shared) {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                mono_acc.append(&mut seg.mono);
                waveform_computer.process(&mono_acc);
                next += 1;
            }
            Err(e) => {
                eprintln!("[AudioEngine] Segment {} failed for source {}: {} — finishing sequentially", next, id, e);
                failed = true;
                break;
            }
        }
    }

    if failed {
        // Cancel remaining workers and finish the rest in one sequential pass
        // from the exact splice point — output stays gap-free.
        cancel.store(true, Ordering::Relaxed);
        let resume = mono_acc.len() as u64;
        let fresh_cancel = std::sync::atomic::AtomicBool::new(false);
        match decode_segment(path, sample_rate, resume, u64::MAX, None, &fresh_cancel) {
            Ok(mut rest) => {
                if !flush_decode_batch(id, &mut rest.samples, shared) {
                    return;
                }
                mono_acc.append(&mut rest.mono);
                waveform_computer.process(&mono_acc);
            }
            Err(e) => eprintln!("[AudioEngine] Sequential fallback failed for source {}: {}", id, e),
        }
    }

    let waveform = waveform_computer.finish();
    let actual_duration_ms = if sample_rate > 0 {
        (mono_acc.len() as u64 * 1000) / sample_rate as u64
    } else {
        0
    };

    let source_exists = if let Ok(mut sources) = shared.sources.lock() {
        if let Some(source) = sources.get_mut(&id) {
            source.decode_complete = true;
            source.duration_ms = actual_duration_ms;
            println!(
                "[AudioEngine] Parallel decode complete for source {}: {} samples, {}ms, {} workers, took {:?}",
                id, source.samples.len(), actual_duration_ms, workers, t0.elapsed()
            );
            true
        } else {
            false
        }
    } else {
        false
    };

    if source_exists {
        if let Some(app) = TAURI_APP.get() {
            let _ = app.emit("audio_waveform", AudioWaveformPayload::new(id, waveform));
            let _ = app.emit("audio_duration", AudioDurationPayload {
                id,
                duration_ms: actual_duration_ms,
            });
        }
    }
}

/// Sequential decode worker: decodes audio packets progressively and appends
/// decoded samples to the source in batches. When complete, triggers FFT
/// waveform computation and emits actual duration.
fn sequential_stream_decode(id: u32, path: &std::path::Path, sample_rate: u32, shared: &SharedState) {
    let t0 = std::time::Instant::now();
    let mut cursor = match PacketCursor::open(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[AudioEngine] Decoder failed to open source {}: {}", id, e);
            mark_decode_complete(id, shared);
            return;
        }
    };

    let mut batch = Vec::with_capacity(DECODE_BATCH_SIZE);
    // A mono mix of everything decoded: the waveform's input and the true duration.
    let mut all_decoded = Vec::new();
    let mut waveform_computer = WaveformComputer::new(sample_rate);

    while let Some((samples, channels, _)) = cursor.next() {
        append_packet_frames(samples, channels, 0, samples.len() / channels, &mut batch, &mut all_decoded);
        // Flush in batches to keep lock contention low.
        if batch.len() >= DECODE_BATCH_SIZE {
            if !flush_decode_batch(id, &mut batch, shared) {
                return; // Source was removed, stop decoding
            }
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
            let _ = app.emit("audio_waveform", AudioWaveformPayload::new(id, waveform));
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

/// Incremental FFT waveform computer: fed as samples arrive, whole when they stop.
struct WaveformComputer {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    bin_ranges: Vec<(usize, usize)>,
    hop_size: usize,
    prev_frame: Vec<f32>,
    fft_buffer: Vec<rustfft::num_complex::Complex<f32>>,
    /// Each frame's band levels in dB, normalised once the whole file is known.
    levels: Vec<f32>,
    cursor: usize, // next sample index in the full stream to process
}

/// How far above the loudest band's ceiling a quiet band may be lifted: enough to even out
/// a voice's tilt toward the bass, not so much that near-silent treble reads as activity.
const BAND_MAX_LIFT_DB: f32 = 32.0;
/// The narrowest range a band is stretched over, so a steady tone doesn't flicker.
const BAND_MIN_RANGE_DB: f32 = 20.0;
/// The widest, so a band's rare dropouts don't flatten everything else it does.
const BAND_MAX_RANGE_DB: f32 = 48.0;
/// No band's ceiling sits below this: a near-silent file (or stretch of one) stays flat
/// rather than having its noise floor stretched into movement.
const SILENCE_CEILING_DB: f32 = -45.0;

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
            levels: Vec::new(),
            cursor: 0,
        }
    }

    /// Process all available FFT frames from the sample buffer.
    /// `samples` is everything fed so far; the cursor remembers where the last call stopped.
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

                self.levels.push(10.0 * (smoothed.max(1e-20)).log10());
            }

            self.cursor += self.hop_size;
        }
    }

    /// Process what `pending` holds and let go of what no later frame needs, so a whole
    /// file can pass through without being kept.
    fn feed(&mut self, pending: &mut Vec<f32>) {
        self.process(pending);
        let used = self.cursor.min(pending.len());
        pending.drain(..used);
        self.cursor -= used;
    }

    /// Consume the computer and return the final waveform data.
    ///
    /// A fixed dB window makes every file lean the same way: voices and most music carry
    /// far more energy low down, so the bass bands stand tall and the treble never moves,
    /// and a loud master sits near the top of the window everywhere. Instead each band is
    /// stretched over its own quiet-to-loud range across the file, within limits (see the
    /// BAND_* constants), so every band moves with what it actually does.
    fn finish(self) -> Vec<u8> {
        normalise_levels(&self.levels, BAND_MAX_LIFT_DB)
    }
}

/// See [`WaveformComputer::finish`]: each band over its own range, `max_lift` dB at most
/// above the loudest band's ceiling.
fn normalise_levels(levels: &[f32], max_lift: f32) -> Vec<u8> {
    if levels.is_empty() {
        return vec![0u8; WAVEFORM_BINS]; // at least one frame of silence
    }
    let frames = levels.len() / WAVEFORM_BINS;
    // One reused column and a partial selection: an hour of levels is tens of MB, and only
    // two ranks per band are ever read.
    let mut column: Vec<f32> = Vec::with_capacity(frames);
    let mut percentile = |bin: usize, p: f32| -> f32 {
        column.clear();
        column.extend((0..frames).map(|f| levels[f * WAVEFORM_BINS + bin]));
        let rank = ((frames - 1) as f32 * p) as usize;
        *column.select_nth_unstable_by(rank, |a, b| a.total_cmp(b)).1
    };
    let peaks: Vec<f32> = (0..WAVEFORM_BINS).map(|bin| percentile(bin, 0.97)).collect();
    // The loudest band's own ceiling: a percentile over every band would sink toward the
    // quiet majority and let them be stretched to full scale.
    let loudest = peaks.iter().copied().fold(f32::MIN, f32::max);
    let bands: Vec<(f32, f32)> = peaks
        .iter()
        .enumerate()
        .map(|(bin, &peak)| {
            let ceiling = peak.max(loudest - max_lift).max(SILENCE_CEILING_DB);
            let floor = percentile(bin, 0.2)
                .min(ceiling - BAND_MIN_RANGE_DB)
                .max(ceiling - BAND_MAX_RANGE_DB);
            (floor, ceiling)
        })
        .collect();
    levels
        .iter()
        .enumerate()
        .map(|(i, db)| {
            let (floor, ceiling) = bands[i % WAVEFORM_BINS];
            (((db - floor) / (ceiling - floor)).clamp(0.0, 1.0) * 255.0) as u8
        })
        .collect()
}

/// Precompute FFT waveform for a complete sample buffer (batch mode).
/// Used by `load_from_samples_internal` for WAV files and voice recordings.
fn precompute_fft_waveform(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut computer = WaveformComputer::new(sample_rate);
    computer.process(samples);
    computer.finish()
}



#[cfg(test)]
mod waveform_normalise_tests {
    #[test]
    fn a_waveform_fed_in_pieces_matches_one_fed_whole() {
        let rate = 44_100;
        let samples: Vec<f32> = (0..rate * 3).map(|i| ((i as f32) * 0.031).sin() * ((i as f32) * 0.0007).cos()).collect();
        let mut whole = WaveformComputer::new(rate as u32);
        whole.process(&samples);
        let mut pieces = WaveformComputer::new(rate as u32);
        let mut pending = Vec::new();
        for chunk in samples.chunks(7_919) {
            pending.extend_from_slice(chunk);
            pieces.feed(&mut pending);
        }
        assert_eq!(whole.levels.len(), pieces.levels.len());
        assert!(whole.levels.iter().zip(&pieces.levels).all(|(a, b)| (a - b).abs() < 1e-4));
    }
    use super::*;

    /// Frames of band levels in dB: `level(frame, band)`.
    fn levels(frames: usize, level: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        (0..frames).flat_map(|f| (0..WAVEFORM_BINS).map(move |b| (f, b))).map(|(f, b)| level(f, b)).collect()
    }

    fn band_mean(wave: &[u8], band: usize) -> f32 {
        let col: Vec<f32> = wave.chunks(WAVEFORM_BINS).map(|f| f[band] as f32).collect();
        col.iter().sum::<f32>() / col.len() as f32
    }

    #[test]
    fn a_spectrum_tilted_toward_the_bass_comes_out_level() {
        // 24 dB quieter at the top than the bottom, every band pulsing the same way.
        let tilted = levels(300, |f, b| -20.0 - b as f32 * 24.0 / WAVEFORM_BINS as f32 + if f % 10 < 5 { 0.0 } else { -20.0 });
        let wave = normalise_levels(&tilted, BAND_MAX_LIFT_DB);
        let (low, high) = (band_mean(&wave, 2), band_mean(&wave, WAVEFORM_BINS - 3));
        assert!((low - high).abs() < 8.0, "bass {low} vs treble {high}: the tilt should be gone");
        assert!(wave.iter().any(|&v| v > 240) && wave.iter().any(|&v| v < 15), "each band uses its whole range");
    }

    #[test]
    fn silence_stays_flat() {
        let quiet = levels(120, |f, b| -90.0 + ((f * 7 + b * 3) % 5) as f32);
        // Under the player's resting size, so it never reads as movement.
        assert!(normalise_levels(&quiet, BAND_MAX_LIFT_DB).iter().all(|&v| v < 30), "a noise floor must stay near the bottom");
    }

    #[test]
    fn a_band_that_barely_sounds_is_not_blown_up() {
        // One loud band, the rest 50 dB down: lifting them to full scale would fake activity.
        let sparse = levels(200, |f, b| if b == 10 { -10.0 + (f % 7) as f32 } else { -60.0 + (f % 3) as f32 });
        let wave = normalise_levels(&sparse, BAND_MAX_LIFT_DB);
        assert!(band_mean(&wave, 40) < 40.0, "a band far below the loudest stays low, not stretched to full scale");
    }
}


#[cfg(test)]
mod window_tests {
    use super::*;

    const RATE: u32 = 8_000;
    const SECS: u32 = 60;

    /// A mono 16-bit WAV whose every sample names its own frame (mod 30000), so any window
    /// can be checked against the place it claims to hold.
    fn stamped_wav() -> Vec<u8> {
        let frames = RATE * SECS;
        let data_len = frames * 2;
        let mut b = Vec::with_capacity(44 + data_len as usize);
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(RATE.to_le_bytes());
        b.extend((RATE * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        for f in 0..frames {
            b.extend(((f % 30_000) as i16).to_le_bytes());
        }
        b
    }

    #[test]
    fn a_wav_length_comes_from_its_headers_past_any_chunk_before_the_data() {
        let plain = stamped_wav();
        // An odd-sized LIST chunk between fmt and data, padded to even as RIFF requires.
        let mut listed = plain[..36].to_vec();
        listed.extend(b"LIST");
        listed.extend(5u32.to_le_bytes());
        listed.extend(b"INFO!\0");
        listed.extend(&plain[36..]);
        for (tag, bytes) in [("plain", plain), ("listed", listed)] {
            let path = std::env::temp_dir().join(format!("vector-wav-probe-{}-{tag}.wav", std::process::id()));
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(wav_probe_duration(&path), Some(SECS as u64 * 1000), "{tag}");
            let _ = std::fs::remove_file(&path);
        }
    }

    fn frame_of(sample: f32) -> u32 {
        (sample * 32_768.0).round() as u32
    }

    fn shared_with(ctl: &Arc<StreamCtl>) -> Arc<SharedState> {
        let (tx, _rx) = mpsc::channel();
        let shared = Arc::new(SharedState {
            sources: std::sync::Mutex::new(HashMap::new()),
            live: std::sync::RwLock::new(None),
            device_sample_rate: AtomicU32::new(RATE),
            next_id: AtomicU32::new(2),
            ended_tx: tx,
        });
        shared.sources.lock().unwrap().insert(1, AudioSource {
            id: 1,
            samples: Vec::with_capacity(window_capacity(RATE, 1)),
            src_channels: 1,
            source_sample_rate: RATE,
            rate_ratio: 1.0,
            position: 0.0,
            playing: false,
            volume: 1.0,
            duration_ms: SECS as u64 * 1000,
            oneshot: false,
            crossfade: None,
            decode_complete: false,
            base_frame: 0,
            stream: Some(Arc::clone(ctl)),
            window_gen: 0,
            pending_seek: None,
        });
        shared
    }

    /// Wait until the window starts at `base` and holds the playhead, then check that what it
    /// holds really is that stretch of the file.
    fn settles_at(shared: &SharedState, base: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let sources = shared.sources.lock().unwrap();
                let src = sources.get(&1).unwrap();
                let len = src.samples.len() as u64;
                if src.base_frame == base && len > 0 && src.pending_seek.is_none() {
                    for (k, s) in src.samples.iter().enumerate().step_by(997) {
                        assert_eq!(frame_of(*s), ((base + k as u64) % 30_000) as u32, "sample {k} after base {base}");
                    }
                    if base + len > src.position as u64 {
                        return;
                    }
                }
            }
            assert!(std::time::Instant::now() < deadline, "window never settled at {base}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_window_follows_seeks_trims_and_ends_with_its_source() {
        let ctl = Arc::new(StreamCtl {
            redirected: std::sync::atomic::AtomicBool::new(false),
            wake: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
        });
        let shared = shared_with(&ctl);
        let wav = std::env::temp_dir().join(format!("vector-window-{}.wav", std::process::id()));
        std::fs::write(&wav, stamped_wav()).unwrap();
        let worker = {
            let (shared, ctl, path) = (Arc::clone(&shared), Arc::clone(&ctl), wav.clone());
            std::thread::spawn(move || window_decode_worker(1, &path, 1, RATE, ctl, &shared))
        };
        settles_at(&shared, 0);

        // Two seeks before the decoder wakes: the second must win, never the first.
        {
            let mut sources = shared.sources.lock().unwrap();
            let src = sources.get_mut(&1).unwrap();
            for secs in [40u64, 10] {
                src.position = (secs * RATE as u64) as f64;
                redirect_window(src, secs * RATE as u64);
            }
        }
        settles_at(&shared, 10 * RATE as u64);

        // Playing on past the trim point lets the start of the window go, and nothing else.
        let played = (10 + WINDOW_TRIM_SECS + 2) * RATE as u64;
        shared.sources.lock().unwrap().get_mut(&1).unwrap().position = played as f64;
        ctl.nudge();
        settles_at(&shared, played - WINDOW_BEHIND_SECS * RATE as u64);

        shared.sources.lock().unwrap().remove(&1);
        ctl.nudge();
        worker.join().unwrap();
        let _ = std::fs::remove_file(&wav);
    }
}

#[cfg(test)]
mod waveform_payload_tests {
    use super::*;

    #[test]
    fn a_long_waveform_pools_to_the_frame_cap_keeping_its_peaks() {
        let frames = MAX_WAVEFORM_FRAMES * 2 + 1;
        let mut levels = vec![10u8; frames * WAVEFORM_BINS];
        levels[(frames - 1) * WAVEFORM_BINS + 5] = 250;   // a lone peak in the last frame
        let p = AudioWaveformPayload::new(7, levels);
        let bytes = base64_simd::STANDARD.decode_to_vec(&p.waveform).unwrap();
        assert!(bytes.len() / WAVEFORM_BINS <= MAX_WAVEFORM_FRAMES);
        assert_eq!(p.waveform_fps, WAVEFORM_FPS as f32 / 3.0);
        assert_eq!(bytes[bytes.len() - WAVEFORM_BINS + 5], 250);
    }

    #[test]
    fn a_short_waveform_travels_whole() {
        let p = AudioWaveformPayload::new(7, vec![1, 2, 3, 4].repeat(WAVEFORM_BINS));
        assert_eq!(base64_simd::STANDARD.decode_to_vec(&p.waveform).unwrap().len(), 4 * WAVEFORM_BINS);
        assert_eq!(p.waveform_fps, WAVEFORM_FPS as f32);
    }
}
