//! The half of a Vector call that runs anywhere: the wire format, the jitter buffer,
//! the rate ladder, and the small signal-processing pieces that need no device.
//! Everything that touches a microphone, a speaker, a codec library or a socket
//! lives in the client that embeds this crate.
//!
//! No clock: every function that reasons about time takes milliseconds in, so the
//! same code runs under tokio, on a thread, and in a browser.

pub mod declick;
pub mod jitter;
pub mod rate;
pub mod resample;
pub mod ring;
pub mod stats;
pub mod wire;

/// Fullband: what the devices run at, so the microphone and speaker paths usually
/// need no resampling, and Opus keeps everything the microphone heard.
pub const ENGINE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME: usize = (ENGINE_RATE * FRAME_MS / 1000) as usize;
/// How late the echo may arrive after its far-end reference: device buffers plus room.
pub const AEC_TAIL_MS: u32 = 200;
