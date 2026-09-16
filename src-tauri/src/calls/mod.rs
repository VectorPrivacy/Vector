//! Native 1:1 voice calls: Opus frames as QUIC datagrams over the Mini Apps' Iroh
//! endpoint, signalled with gift-wrapped rumors, echo cancelled before encoding.
//!
//! Wideband (16 kHz) is the engine rate: the canceller's sweet spot and what voice
//! notes already use. Opus wideband at 24 kbit/s is ordinary call quality.

pub mod aec;
pub mod codec;
pub mod commands;
pub mod media;
pub mod resample;
pub mod ring;
pub mod session;
pub mod transport;

pub const ENGINE_RATE: u32 = 16_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME: usize = (ENGINE_RATE * FRAME_MS / 1000) as usize;
/// How late the echo may arrive after its far-end reference: device buffers plus room.
pub const AEC_TAIL_MS: u32 = 200;
