//! What a client supplies for calls to run on it: the Iroh endpoint, the audio
//! devices, the video link, and a few platform extras. Everything else (the state
//! machine, the signalling, the video transport) is shared.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use tokio::sync::mpsc;

use super::share::ShareInput;
use super::stats::MediaStats;

/// One video link, split: what goes to the side that decodes and paints, and what
/// comes from the side that captures and encodes. Messages are `calls::link`'s.
pub struct LinkConn {
    pub to_web: mpsc::Sender<Bytes>,
    pub from_web: mpsc::Receiver<Bytes>,
}

/// How awake a call needs the machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AwakeLevel {
    /// No idle sleep; the display may go dark.
    System,
    /// The display stays on too: there is video.
    Display,
}

/// What the audio of a connected call starts with.
pub struct AudioStart {
    pub volume: f32,
    pub share_volume: f32,
    /// The call's counters, kept across a restart: fresh ones read as a peer gone quiet.
    pub stats: Option<Arc<MediaStats>>,
    pub share: Arc<ShareInput>,
}

/// A running audio engine; dropping it stops it.
pub trait CallAudio: Send {
    fn stats(&self) -> Arc<MediaStats>;
    fn set_muted(&self, on: bool);
    /// False when the call went ahead without a microphone, so it can only be heard muted.
    fn has_mic(&self) -> bool {
        true
    }
    /// Listener-side gains, 1.0 is unity.
    fn set_volume(&self, volume: f32);
    fn set_share_volume(&self, volume: f32);
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait CallPlatform: Send + Sync + 'static {
    /// The endpoint calls ride. It accepts `wire::CALL_ALPN` and hands those
    /// connections to `session::on_incoming`.
    async fn endpoint(&self) -> Result<Endpoint, String>;
    /// Our address, encoded, as a peer should dial it.
    async fn local_addr(&self) -> Result<String, String>;
    /// A peer's address as this platform may dial it; None if unusable.
    fn decode_addr(&self, addr: &str) -> Option<EndpointAddr>;
    /// Microphone and speaker for a connected call.
    async fn start_audio(&self, conn: Connection, start: AudioStart) -> Result<Box<dyn CallAudio>, String>;
    /// Where new video links go: the call's video track while one runs, else nowhere.
    fn set_video_taker(&self, taker: Option<mpsc::Sender<LinkConn>>);
    /// A hold on the machine for as long as the returned value lives.
    fn hold_awake(&self, _level: AwakeLevel) -> Option<Box<dyn Any + Send>> {
        None
    }
    /// A call this side took down after it rang or ran.
    fn ended(&self) {}
    /// Captures a shared screen's sound itself, for a picker that gave none.
    fn share_audio_native(&self) -> bool {
        false
    }
    async fn start_share_audio(&self, _share: Arc<ShareInput>) -> Result<(), String> {
        Err("Not available on this platform".into())
    }
    async fn stop_share_audio(&self) {}
    /// False once the system has refused screen capture for good.
    fn share_audio_permitted(&self) -> bool {
        true
    }
}

static PLATFORM: OnceLock<Arc<dyn CallPlatform>> = OnceLock::new();

/// Installed once at startup; calls are refused until it is.
pub fn install(platform: Arc<dyn CallPlatform>) {
    let _ = PLATFORM.set(platform);
}

pub(crate) fn get() -> Result<Arc<dyn CallPlatform>, String> {
    PLATFORM.get().cloned().ok_or_else(|| "Calls are not available".to_string())
}
