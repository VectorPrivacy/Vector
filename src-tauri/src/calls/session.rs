//! The call session is vector-core's (`vector_core::calls::session`); this is the
//! desktop's side of it: the Mini Apps' Iroh endpoint, the cpal media engine, the
//! webview's video socket, keeping the machine awake, and the ended chime.

pub use vector_core::calls::session::*;

use super::media::{MediaEngine, ShareInput};
use crate::miniapps::realtime::{decode_node_addr, encode_node_addr, IrohState};
use crate::miniapps::state::MiniAppsState;
use crate::TAURI_APP;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::Manager;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vector_core::calls::platform::{AudioStart, AwakeLevel, CallAudio, CallPlatform, LinkConn};

struct Desktop;

async fn iroh() -> Result<Arc<IrohState>, String> {
    let app = TAURI_APP.get().ok_or("App not ready")?;
    let state = app.state::<MiniAppsState>();
    state.realtime.get_or_init().await.map_err(|e| format!("Iroh unavailable: {e}"))
}

#[async_trait::async_trait]
impl CallPlatform for Desktop {
    async fn endpoint(&self) -> Result<Endpoint, String> {
        Ok(iroh().await?.endpoint.clone())
    }

    async fn local_addr(&self) -> Result<String, String> {
        encode_node_addr(&iroh().await?.get_node_addr()).map_err(|e| e.to_string())
    }

    fn decode_addr(&self, addr: &str) -> Option<EndpointAddr> {
        decode_node_addr(addr).ok()
    }

    async fn start_audio(&self, conn: Connection, start: AudioStart) -> Result<Box<dyn CallAudio>, String> {
        let engine = tokio::task::spawn_blocking(move || {
            MediaEngine::start(Some(conn), start.volume, start.stats, Some(start.share), start.share_volume)
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(Box::new(engine))
    }

    fn set_video_taker(&self, taker: Option<mpsc::Sender<LinkConn>>) {
        super::link::set_taker(taker);
    }

    fn hold_awake(&self, level: AwakeLevel) -> Option<Box<dyn std::any::Any + Send>> {
        let level = match level {
            AwakeLevel::System => crate::awake::Level::System,
            AwakeLevel::Display => crate::awake::Level::Display,
        };
        Some(Box::new(crate::awake::hold(level)))
    }

    fn ended(&self) {
        play_ended_chime();
    }

    fn share_audio_native(&self) -> bool {
        super::share_native::available()
    }

    async fn start_share_audio(&self, share: Arc<ShareInput>) -> Result<(), String> {
        super::share_native::start(share).await
    }

    async fn stop_share_audio(&self) {
        super::share_native::stop().await;
    }

    fn share_audio_permitted(&self) -> bool {
        super::share_native::permitted()
    }
}

/// Called once at startup, after the audio engine: calls run on this platform, and
/// when the default microphone or speaker changes an active call reopens its audio.
pub fn install() {
    vector_core::calls::platform::install(Arc::new(Desktop));
    if let Ok(mixer) = crate::audio_engine::AudioEngine::get() {
        mixer.on_device_change(Box::new(|| {
            if mic_test_running() {
                vector_core::db::spawn_bound(async move {
                    let _ = mic_test_start().await;
                });
            }
            if snapshot().is_some() {
                vector_core::db::spawn_bound(restart_audio());
            }
        }));
    }
}

/// Hang up from a synchronous context, waiting briefly for the Bye and the signal.
pub fn hangup_blocking() {
    if snapshot().is_none() {
        return;
    }
    let _ = tauri::async_runtime::block_on(tokio::time::timeout(Duration::from_secs(3), hangup()));
}

/// The ended chime rides the shared engine as a oneshot, so it plays over whatever
/// else is sounding and needs no device of its own.
static ENDED_WAV: &[u8] = include_bytes!("ended.wav");

fn play_ended_chime() {
    let Ok(engine) = crate::audio_engine::AudioEngine::get() else { return };
    let Some((samples, rate)) = crate::audio::wav_fast_decode_for_engine(ENDED_WAV) else { return };
    let played = crate::audio::resample_mono_f32(samples, rate, engine.device_sample_rate())
        .and_then(|s| engine.play_oneshot(s));
    if let Err(e) = played {
        log_warn!("[CALLS] Ended chime failed: {e}");
    }
}

/// The microphone test outside a call: the capture chain alone, feeding the meter.
struct MicTest {
    engine: MediaEngine,
    task: JoinHandle<()>,
}

static MIC_TEST: OnceLock<Mutex<Option<MicTest>>> = OnceLock::new();

fn mic_test_slot() -> &'static Mutex<Option<MicTest>> {
    MIC_TEST.get_or_init(|| Mutex::new(None))
}

pub fn mic_test_running() -> bool {
    mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

pub async fn mic_test_start() -> Result<(), String> {
    mic_test_stop();
    let engine = tokio::task::spawn_blocking(|| MediaEngine::start(None, 1.0, None, None, 1.0))
        .await
        .map_err(|e| e.to_string())??;
    let stats = Arc::clone(&engine.stats);
    let task = vector_core::db::spawn_bound(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            vector_core::traits::emit_event("mic_level", &LevelPayload {
                id: String::new(),
                mic: stats.mic_level(),
                peer: 0.0,
            });
        }
    });
    *mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(MicTest { engine, task });
    Ok(())
}

pub fn mic_test_stop() {
    let taken = mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(t) = taken {
        t.task.abort();
        drop(t.engine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The engine's WAV fast path is mono-only: a stereo replacement would go silent.
    #[test]
    fn the_ended_chime_decodes() {
        let (samples, rate) = crate::audio::wav_fast_decode_for_engine(ENDED_WAV).expect("mono WAV");
        assert!(rate > 0 && !samples.is_empty());
    }
}
