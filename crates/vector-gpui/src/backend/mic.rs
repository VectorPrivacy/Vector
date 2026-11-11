//! Settings → Calls: the devices, the voice processing switches and volumes, Direct Connection,
//! and the microphone test.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use vector_core::calls::settings::{self, AudioSettings};

pub use crate::audio_devices::{DeviceList, DevicePrefs};

pub fn audio_settings() -> AudioSettings {
    settings::load()
}

pub fn set_audio_settings(s: AudioSettings) -> Result<(), String> {
    settings::set(s)
}

pub fn devices() -> DeviceList {
    crate::audio_devices::load();
    crate::audio_devices::list()
}

/// A running test reopens on the new device.
pub fn set_devices(prefs: DevicePrefs) -> Result<(), String> {
    crate::audio_devices::set(prefs)?;
    if TEST.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        test_start()?;
    }
    Ok(())
}

pub fn direct() -> bool {
    vector_core::transport::realtime::direct()
}

pub async fn set_direct(on: bool) -> Result<(), String> {
    vector_core::transport::realtime::set_direct(on).await
}

struct Test {
    stop: Arc<AtomicBool>,
}

static TEST: Mutex<Option<Test>> = Mutex::new(None);
static LEVEL: AtomicU32 = AtomicU32::new(0);

/// The meter's 0..1: -60 dBFS to full scale, as a call's own meter reads.
fn level_of(sum_sq: f64, n: usize) -> f32 {
    let rms = (sum_sq / n.max(1) as f64).sqrt();
    if rms <= 0.0 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0) as f32
}

pub fn level() -> f32 {
    f32::from_bits(LEVEL.load(Ordering::Relaxed))
}

/// Opens the microphone and meters it after the volume, until [`test_stop`].
pub fn test_start() -> Result<(), String> {
    test_stop();
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let flag = stop.clone();
    // The stream isn't Send on every host: it lives and dies on its own thread.
    std::thread::spawn(move || {
        let open = || -> Result<cpal::Stream, String> {
            let device = crate::audio_devices::resolve_input().ok_or("No microphone found")?;
            let config = device.default_input_config().map_err(|e| e.to_string())?;
            let channels = config.channels() as usize;
            let window = config.sample_rate().0 as usize / 50 * channels;
            let mut acc = (0.0f64, 0usize);
            let mut feed = move |s: f32| {
                let s = (s * settings::mic_gain()) as f64;
                acc.0 += s * s;
                acc.1 += 1;
                if acc.1 >= window {
                    LEVEL.store(level_of(acc.0, acc.1).to_bits(), Ordering::Relaxed);
                    acc = (0.0, 0);
                }
            };
            let err = |e| log::warn!("mic test: {e}");
            let cfg: cpal::StreamConfig = config.config();
            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => device.build_input_stream(&cfg, move |d: &[f32], _| d.iter().for_each(|s| feed(*s)), err, None),
                cpal::SampleFormat::I16 => device.build_input_stream(&cfg, move |d: &[i16], _| d.iter().for_each(|s| feed(*s as f32 / 32768.0)), err, None),
                cpal::SampleFormat::U16 => device.build_input_stream(&cfg, move |d: &[u16], _| d.iter().for_each(|s| feed((*s as f32 - 32768.0) / 32768.0)), err, None),
                f => return Err(format!("Unsupported sample format {f:?}")),
            }
            .map_err(|e| format!("Failed to open the microphone: {e}"))?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(stream)
        };
        let stream = match open() {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let _ = ready_tx.send(Ok(()));
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(stream);
        LEVEL.store(0, Ordering::Relaxed);
    });
    ready_rx.recv().map_err(|_| "The microphone test stopped".to_string())??;
    *TEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(Test { stop });
    Ok(())
}

pub fn test_stop() {
    if let Some(t) = TEST.lock().unwrap_or_else(|e| e.into_inner()).take() {
        t.stop.store(true, Ordering::Relaxed);
    }
    LEVEL.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    #[test]
    fn meter_spans_sixty_decibels() {
        assert_eq!(super::level_of(0.0, 10), 0.0);
        assert!((super::level_of(1.0, 1) - 1.0).abs() < 1e-6);
        assert!((super::level_of(1e-6, 1) - 0.0).abs() < 1e-6);
        assert!((super::level_of(1e-3, 1) - 0.5).abs() < 1e-6);
    }
}
