use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::audio;

static RECORDER: OnceCell<AudioRecorder> = OnceCell::new();

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

        std::thread::spawn(move || {
            let stream = device
                .build_input_stream(
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
                )
                .unwrap();

            stream.play().unwrap();

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
