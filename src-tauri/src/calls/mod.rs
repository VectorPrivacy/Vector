//! Native 1:1 voice calls: Opus frames as QUIC datagrams over the Mini Apps' Iroh
//! endpoint, signalled with gift-wrapped rumors, echo cancelled before encoding.
//!
//! Fullband (48 kHz) is the engine rate: what the devices run at, so the microphone
//! and speaker paths usually need no resampling, and Opus keeps everything the
//! microphone heard. The canceller runs at the same rate, as Mumble's does.

pub mod aec;
pub mod codec;
pub mod declick;
pub mod commands;
pub mod media;
pub mod rate;
pub mod resample;
pub mod ring;
pub mod session;
pub mod settings;
pub mod transport;

pub const ENGINE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME: usize = (ENGINE_RATE * FRAME_MS / 1000) as usize;
/// How late the echo may arrive after its far-end reference: device buffers plus room.
pub const AEC_TAIL_MS: u32 = 200;
