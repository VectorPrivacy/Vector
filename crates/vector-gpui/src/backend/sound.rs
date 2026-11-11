//! Notification sounds (`audio.rs` on desktop): the bundled pair decoded once, a custom pick
//! stored pre-resampled as `name_RATE.raw`, each played as a one-shot on the default output.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const PRELUDE: &[u8] = include_bytes!("../../../../src-tauri/resources/sounds/notif-prelude.mp3");
const TECHNO: &[u8] = include_bytes!("../../../../src-tauri/resources/sounds/notif-techno.mp3");
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_SECS: f32 = 10.0;
/// Decoded samples stay this long after their last play, for a burst of messages.
const TTL: Duration = Duration::from_secs(600);

/// The stored `notif_sound` value: default, techno, none, or `custom:<path>`.
#[derive(Clone, PartialEq)]
pub enum Sound {
    Default,
    Techno,
    None,
    Custom(PathBuf),
}

impl Sound {
    pub fn parse(v: &str) -> Self {
        match v {
            "techno" => Sound::Techno,
            "none" => Sound::None,
            v => v.strip_prefix("custom:").map(|p| Sound::Custom(p.into())).unwrap_or(Sound::Default),
        }
    }
}

struct Cached {
    sound: Sound,
    rate: u32,
    samples: std::sync::Arc<Vec<f32>>,
    used: Instant,
}

// Keyed by the sound itself, so it holds nothing an account owns.
static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

fn device_rate() -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let device = cpal::default_host().default_output_device().ok_or("No output device found")?;
    let config = device.default_output_config().map_err(|e| format!("Failed to get output config: {e}"))?;
    Ok((device, config))
}

fn decode(bytes: Vec<u8>, ext: Option<&str>) -> Result<(Vec<f32>, u32), String> {
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = ext {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Failed to probe audio format: {e}"))?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("No supported audio tracks found")?;
    let id = track.id;
    let mut rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| format!("Failed to create decoder: {e}"))?;
    let mut out = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut channels = 1;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let b = buf.get_or_insert_with(|| {
                    let spec = *decoded.spec();
                    if spec.rate > 0 {
                        rate = spec.rate;
                    }
                    channels = spec.channels.count().max(1);
                    SampleBuffer::new(decoded.capacity() as u64, spec)
                });
                b.copy_interleaved_ref(decoded);
                if channels > 1 {
                    out.extend(b.samples().chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32));
                } else {
                    out.extend_from_slice(b.samples());
                }
            }
            Err(SymErr::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }
    Ok((out, rate))
}

fn resample(samples: Vec<f32>, from: u32, to: u32) -> Vec<f32> {
    if from == to { samples } else { crate::simd::audio::linear_resample_mono(&samples, from, to) }
}

fn load_raw(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read cache file: {e}"))?;
    if bytes.len() % 4 != 0 {
        return Err("Invalid cache file size".into());
    }
    Ok(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

fn samples_for(sound: &Sound, rate: u32) -> Result<Vec<f32>, String> {
    let bundled = |bytes: &[u8]| decode(bytes.to_vec(), Some("mp3")).map(|(s, r)| resample(s, r, rate));
    match sound {
        Sound::None => Ok(Vec::new()),
        Sound::Default => bundled(PRELUDE),
        Sound::Techno => bundled(TECHNO),
        // A custom file gone missing falls back to the default rather than silence.
        Sound::Custom(path) if !path.exists() => bundled(PRELUDE),
        Sound::Custom(path) => {
            let stored = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.rsplit('_').next()).and_then(|r| r.parse().ok()).ok_or("Invalid cache filename format")?;
            Ok(resample(load_raw(path)?, stored, rate))
        }
    }
}

/// Play `sound` once, off the caller's thread.
pub fn play(sound: Sound) {
    if sound == Sound::None {
        return;
    }
    // spawn-detached: plays bytes in hand on the audio device; owns no account state.
    std::thread::spawn(move || {
        if let Err(e) = play_now(&sound) {
            log::warn!("notification sound: {e}");
        }
    });
}

fn play_now(sound: &Sound) -> Result<(), String> {
    let (device, config) = device_rate()?;
    let rate = config.sample_rate().0;
    let samples = {
        let mut cache = CACHE.lock().map_err(|_| "cache poisoned")?;
        if cache.as_ref().is_some_and(|c| c.used.elapsed() > TTL) {
            *cache = None;
        }
        match cache.as_mut().filter(|c| c.sound == *sound && c.rate == rate) {
            Some(c) => {
                c.used = Instant::now();
                c.samples.clone()
            }
            None => {
                let samples = std::sync::Arc::new(samples_for(sound, rate)?);
                *cache = Some(Cached { sound: sound.clone(), rate, samples: samples.clone(), used: Instant::now() });
                samples
            }
        }
    };
    if samples.is_empty() {
        return Err("No audio data decoded".into());
    }
    let channels = config.channels() as usize;
    let total = samples.len();
    let mut at = 0usize;
    let err = |e| log::warn!("notification stream: {e}");
    let stream_config: cpal::StreamConfig = config.config();
    macro_rules! build {
        ($t:ty, $conv:expr) => {
            device.build_output_stream(
                &stream_config,
                move |out: &mut [$t], _| {
                    for frame in out.chunks_mut(channels) {
                        let s = samples.get(at).copied().unwrap_or(0.0);
                        at += 1;
                        frame.fill($conv(s));
                    }
                },
                err,
                None,
            )
        };
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: f32| (s.clamp(-1., 1.) * i16::MAX as f32) as i16),
        cpal::SampleFormat::U16 => build!(u16, |s: f32| ((s.clamp(-1., 1.) + 1.) * 0.5 * u16::MAX as f32) as u16),
        f => return Err(format!("Unsupported sample format {f:?}")),
    }
    .map_err(|e| format!("Failed to open output: {e}"))?;
    stream.play().map_err(|e| format!("Failed to play: {e}"))?;
    let secs = (total as f32 / rate as f32).min(MAX_SECS);
    std::thread::sleep(Duration::from_secs_f32(secs + 0.15));
    Ok(())
}

/// Forget decoded samples, for a sound that changed or went quiet.
pub fn purge() {
    if let Ok(mut c) = CACHE.lock() {
        *c = None;
    }
}

/// Import a picked file: at most 1MB and 10 seconds, stored resampled for this device.
pub fn import(source: &Path, sounds_dir: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(source).map_err(|e| format!("Failed to read file: {e}"))?;
    if meta.len() > MAX_BYTES {
        return Err("FILE_TOO_LARGE".into());
    }
    let (_, config) = device_rate()?;
    let rate = config.sample_rate().0;
    let bytes = std::fs::read(source).map_err(|e| format!("Failed to read file: {e}"))?;
    let (samples, from) = decode(bytes, source.extension().and_then(|e| e.to_str()))?;
    if samples.len() as f32 / from as f32 > MAX_SECS {
        return Err("AUDIO_TOO_LONG".into());
    }
    let samples = resample(samples, from, rate);
    let stem = source.file_stem().ok_or("Invalid filename")?.to_string_lossy();
    std::fs::create_dir_all(sounds_dir).map_err(|e| format!("Failed to create sounds cache dir: {e}"))?;
    let dest = sounds_dir.join(format!("{stem}_{rate}.raw"));
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(&dest, bytes).map_err(|e| format!("Failed to write cache file: {e}"))?;
    Ok(dest)
}
