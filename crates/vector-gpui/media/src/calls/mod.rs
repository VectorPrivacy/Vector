//! Calls, on the desktop client's own media engine: its modules compiled in unchanged, with the
//! webview's video socket replaced by an in-process link (this client draws video itself).

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

/// Where the session hands a video connection: the call's video track while one runs.
pub mod link {
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    pub use vector_core::calls::platform::LinkConn;

    static TAKER: Mutex<Option<mpsc::Sender<LinkConn>>> = Mutex::new(None);

    pub fn set_taker(taker: Option<mpsc::Sender<LinkConn>>) {
        *TAKER.lock().unwrap_or_else(|e| e.into_inner()) = taker;
    }

    /// Hands a new link to the running track; false when no call wants one.
    pub fn connect(conn: LinkConn) -> bool {
        let taker = TAKER.lock().unwrap_or_else(|e| e.into_inner()).clone();
        taker.is_some_and(|t| t.try_send(conn).is_ok())
    }
}
