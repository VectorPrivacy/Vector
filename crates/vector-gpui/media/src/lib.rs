//! The desktop client's audio engine, device preferences, microphone access and call media,
//! compiled from `src-tauri` as they are, over a slice of Tauri's API (`tauri-shim`).

#[macro_use]
extern crate vector_core;

#[allow(dead_code)]
pub mod audio;
#[path = "../../../../src-tauri/src/audio_codecs.rs"]
pub mod audio_codecs;
#[path = "../../../../src-tauri/src/audio_devices.rs"]
#[allow(dead_code, unused_imports)]
pub mod audio_devices;
#[path = "../../../../src-tauri/src/audio_engine.rs"]
#[allow(dead_code, clippy::all)]
pub mod audio_engine;
#[path = "../../../../src-tauri/src/awake.rs"]
#[allow(dead_code)]
pub mod awake;
#[allow(dead_code, unused_imports, clippy::all)]
pub mod calls;
#[path = "../../../../src-tauri/src/chapters.rs"]
#[allow(dead_code)]
pub mod chapters;
#[path = "../../../../src-tauri/src/lyrics.rs"]
#[allow(dead_code)]
pub mod lyrics;
pub mod tags;
#[path = "../../../../src-tauri/src/mic_access.rs"]
#[allow(dead_code)]
pub mod mic_access;
pub mod miniapps;
#[cfg(feature = "whisper")]
#[path = "../../../../src-tauri/src/whisper.rs"]
#[allow(dead_code)]
pub mod whisper;
#[path = "../../../../src-tauri/src/voice.rs"]
#[allow(dead_code, unexpected_cfgs)]
pub mod voice;

#[path = "../../../../src-tauri/src/simd/audio.rs"]
#[allow(dead_code)]
#[doc(hidden)]
pub mod simd_audio;

pub mod simd {
    pub use super::simd_audio as audio;
}

/// Where the engine's events go: set once by the host before any sound plays.
pub static TAURI_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
