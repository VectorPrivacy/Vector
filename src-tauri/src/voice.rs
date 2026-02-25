use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::audio;

static RECORDER: OnceLock<AudioRecorder> = OnceLock::new();

/// Trim leading and trailing silence from i16 audio samples.
/// Uses adaptive RMS threshold based on the recording's noise floor
/// with a 250ms padding buffer to avoid clipping trailing speech.
fn trim_silence_i16(samples: &[i16], sample_rate: u32) -> Vec<i16> {
    if samples.is_empty() {
        return Vec::new();
    }

    let chunk_size = (sample_rate as usize * 20) / 1000; // 20ms windows
    let pad_samples = (sample_rate as usize * 250) / 1000; // 250ms padding

    let rms = |chunk: &[i16]| -> f64 {
        let sum: f64 = chunk.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / chunk.len() as f64).sqrt()
    };

    // Single pass: compute RMS for every chunk, reuse for threshold + scanning
    let chunk_rms_vals: Vec<f64> = samples.chunks(chunk_size).map(|c| rms(c)).collect();

    // Adaptive threshold from the quietest 10% of chunks (noise floor)
    let mut sorted = chunk_rms_vals.clone();
    sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let bottom_10 = std::cmp::max(1, sorted.len() / 10);
    let noise_floor: f64 = sorted[..bottom_10].iter().sum::<f64>() / bottom_10 as f64;
    let threshold = (noise_floor * 3.0).clamp(80.0, 400.0);

    // Scan from the start to find first non-silent chunk
    let start_chunk = chunk_rms_vals.iter().position(|&v| v > threshold).unwrap_or(chunk_rms_vals.len());
    let start = start_chunk * chunk_size;

    // Scan from the end to find last non-silent chunk
    let end_chunk = chunk_rms_vals.iter().rposition(|&v| v > threshold).map(|i| i + 1).unwrap_or(0);
    let end = std::cmp::min(end_chunk * chunk_size, samples.len());

    // Apply padding (don't clip into speech)
    let start = start.saturating_sub(pad_samples);
    let end = std::cmp::min(end + pad_samples, samples.len());

    // Safety: don't produce empty output from a non-empty input
    if start >= end {
        return samples.to_vec();
    }

    let trimmed = &samples[start..end];
    let original_ms = samples.len() * 1000 / sample_rate as usize;
    let trimmed_ms = trimmed.len() * 1000 / sample_rate as usize;
    if original_ms != trimmed_ms {
        println!("[Voice] Trimmed silence: {}ms -> {}ms (removed {}ms)", original_ms, trimmed_ms, original_ms - trimmed_ms);
    }

    trimmed.to_vec()
}

// Standard sample rate for voice recording with good quality-to-size ratio
const TARGET_SAMPLE_RATE: u32 = 22000;

pub struct AudioRecorder {
    recording: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<i16>>>,
    device_sample_rate: Arc<Mutex<u32>>,
    stop_tx: Arc<Mutex<Option<mpsc::Sender<()>>>>,
}

impl AudioRecorder {
    pub fn global() -> &'static AudioRecorder {
        RECORDER.get_or_init(|| AudioRecorder::new())
    }

    fn new() -> Self {
        AudioRecorder {
            recording: Arc::new(AtomicBool::new(false)),
            samples: Arc::new(Mutex::new(Vec::new())),
            device_sample_rate: Arc::new(Mutex::new(48000)),
            stop_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&self) -> Result<(), String> {
        if self.recording.load(Ordering::SeqCst) {
            return Ok(());
        }

        let (tx, rx) = mpsc::channel();
        *self.stop_tx.lock().unwrap() = Some(tx);

        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("No input device found")?;

        let supported_config = device.default_input_config().map_err(|e| e.to_string())?;

        *self.device_sample_rate.lock().unwrap() = supported_config.sample_rate().0;

        let config: cpal::StreamConfig = supported_config.into();
        let channels = config.channels as usize;

        let samples = Arc::clone(&self.samples);
        let recording = Arc::clone(&self.recording);

        self.recording.store(true, Ordering::SeqCst);

        let recording_flag = Arc::clone(&self.recording);
        std::thread::spawn(move || {
            let stream = match device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    if recording.load(Ordering::SeqCst) {
                        if let Ok(mut guard) = samples.lock() {
                            guard.extend(data.chunks(channels).map(|chunk| {
                                let sum: f32 = chunk.iter().sum();
                                let avg = sum / channels as f32;
                                (avg.clamp(-1.0, 1.0) * 32767.0) as i16
                            }));
                        }
                    }
                },
                |err| eprintln!("Error: {}", err),
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[Voice] Failed to build input stream: {}", e);
                    recording_flag.store(false, Ordering::SeqCst);
                    return;
                }
            };

            if let Err(e) = stream.play() {
                eprintln!("[Voice] Failed to start audio stream: {}", e);
                recording_flag.store(false, Ordering::SeqCst);
                return;
            }

            // Wait for stop signal
            rx.recv().unwrap_or(());
        });

        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<u8>, String> {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }

        self.recording.store(false, Ordering::SeqCst);

        let wav_buffer = {
            let samples = self.samples.lock().map_err(|_| "Failed to get samples")?;

            if samples.is_empty() {
                return Err("No audio data recorded".to_string());
            }

            let device_sample_rate = *self.device_sample_rate.lock().unwrap();
            let resampled_samples = audio::resample_mono_i16(&samples, device_sample_rate, TARGET_SAMPLE_RATE)?;
            let resampled_samples = trim_silence_i16(&resampled_samples, TARGET_SAMPLE_RATE);

            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: TARGET_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            let mut buffer: Vec<u8> = Vec::new();
            {
                let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buffer), spec)
                    .map_err(|e| e.to_string())?;

                for &sample in resampled_samples.iter() {
                    writer.write_sample(sample).map_err(|e| e.to_string())?;
                }
                writer.finalize().map_err(|e| e.to_string())?;
            }
            buffer
        };

        self.samples.lock().unwrap().clear();

        Ok(wav_buffer)
    }
}
