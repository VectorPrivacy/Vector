//! Calls, on the desktop client's own media engine: its modules compiled in unchanged, with the
//! webview's video socket left out (this client draws video itself).

#[path = "../../../../../src-tauri/src/calls/aec.rs"]
pub mod aec;
#[path = "../../../../../src-tauri/src/calls/codec.rs"]
pub mod codec;
#[path = "../../../../../src-tauri/src/calls/media.rs"]
pub mod media;
#[path = "../../../../../src-tauri/src/calls/session.rs"]
pub mod session;
#[path = "../../../../../src-tauri/src/calls/share_native.rs"]
pub mod share_native;
#[path = "../../../../../src-tauri/src/calls/voice.rs"]
pub mod voice;

pub use vector_core::calls::wire as transport;
pub use vector_core::calls::{declick, gate, jitter, rate, resample, ring, settings, stats};
pub use vector_core::calls::{AEC_TAIL_MS, ENGINE_RATE, FRAME, FRAME_MS};

/// Where the session hands a video connection; none is taken until video draws natively.
pub mod link {
    use tokio::sync::mpsc;
    pub use vector_core::calls::platform::LinkConn;

    pub fn set_taker(_taker: Option<mpsc::Sender<LinkConn>>) {}
}
