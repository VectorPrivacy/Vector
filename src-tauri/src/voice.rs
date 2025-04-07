use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use once_cell::sync::OnceCell;
use std::sync::mpsc;
use rubato::{
    SincInterpolationParameters, SincInterpolationType, Resampler, 
    SincFixedIn, WindowFunction
};

static RECORDER: OnceCell<AudioRecorder> = OnceCell::new();

// Standard sample rate for voice recording with good quality-to-size ratio
const TARGET_SAMPLE_RATE: u32 = 16000;

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
        let device = host.default_input_device()
            .ok_or("No input device found")?;

        let config = device.default_input_config()
            .map_err(|e| e.to_string())?;
            
        *self.device_sample_rate.lock().unwrap() = config.sample_rate().0;
        let samples = Arc::clone(&self.samples);
        let recording = Arc::clone(&self.recording);
        
        self.recording.store(true, Ordering::SeqCst);
        
        std::thread::spawn(move || {
            let stream = device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| {
                    if recording.load(Ordering::SeqCst) {
                        if let Ok(mut guard) = samples.lock() {
                            guard.extend(data.iter().map(|&x| (x * 32768.0) as i16));
                        }
                    }
                },
                |err| eprintln!("Error: {}", err),
                None
            ).unwrap();

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
            let samples = self.samples.lock()
                .map_err(|_| "Failed to get samples")?;

            if samples.is_empty() {
                return Err("No audio data recorded".to_string());
            }

            let device_sample_rate = *self.device_sample_rate.lock().unwrap();
            
            // Resample audio to target sample rate to ensure consistent quality
            let resampled_samples = self.resample_audio(&samples, device_sample_rate)?;
    
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: TARGET_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
    
            let mut buffer: Vec<u8> = Vec::new();
            {
                let mut writer = hound::WavWriter::new(
                    std::io::Cursor::new(&mut buffer),
                    spec
                ).map_err(|e| e.to_string())?;
    
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
    
    /// Resample audio using Rubato's high-quality resampling
    fn resample_audio(&self, samples: &[i16], source_rate: u32) -> Result<Vec<i16>, String> {
        // If sample rates are already the same, return the original samples
        if source_rate == TARGET_SAMPLE_RATE {
            return Ok(samples.to_vec());
        }
        
        // Convert i16 samples to f32 for Rubato
        let samples_f32: Vec<f32> = samples.iter()
            .map(|&s| (s as f32) / 32768.0)
            .collect();
            
        // Since Rubato works with separate channels, wrap our mono audio in a Vec of Vecs
        let input_frames = vec![samples_f32];
        
        // Create a Sinc resampler with good quality settings for voice
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        
        let mut resampler = SincFixedIn::<f32>::new(
            TARGET_SAMPLE_RATE as f64 / source_rate as f64,
            1.0,
            params,
            samples.len(),
            1, // mono audio (1 channel)
        ).map_err(|e| format!("Failed to create resampler: {}", e))?;
        
        // Process the audio
        let output_frames = resampler.process(
            &input_frames,
            None
        ).map_err(|e| format!("Failed to resample audio: {}", e))?;
        
        // Convert back to i16 from f32 (first channel only since we're using mono)
        let resampled_samples = output_frames[0]
            .iter()
            .map(|&s| (s * 32767.0) as i16)
            .collect();
            
        Ok(resampled_samples)
    }
}
