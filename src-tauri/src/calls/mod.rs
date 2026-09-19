//! Native 1:1 voice calls: Opus frames as QUIC datagrams over the Mini Apps' Iroh
//! endpoint, signalled with gift-wrapped rumors, echo cancelled before encoding.
//!
//! Fullband (48 kHz) is the engine rate: what the devices run at, so the microphone
//! and speaker paths usually need no resampling, and Opus keeps everything the
//! microphone heard. The canceller runs at the same rate, as Mumble's does.

pub mod aec;
pub mod codec;
pub mod commands;
pub mod link;
pub mod media;
pub mod session;
pub mod settings;
pub mod video;

// The platform-free half lives in vector-core; the device, codec and Tauri halves here.
pub use vector_core::calls::wire as transport;
pub use vector_core::calls::{declick, jitter, rate, resample, ring, stats};
pub use vector_core::calls::{AEC_TAIL_MS, ENGINE_RATE, FRAME, FRAME_MS};
