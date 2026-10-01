//! A shared screen's sound on its way to the engine: interleaved samples at the
//! capture's own rate. Lives on the call, not the engine, so a device change does
//! not lose what was in flight.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::ring::SpscRing;

pub struct ShareInput {
    ring: SpscRing,
    rate: AtomicU32,
    channels: AtomicU32,
    pub active: AtomicBool,
}

impl Default for ShareInput {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareInput {
    pub fn new() -> Self {
        Self {
            // Two seconds of stereo at 48 kHz.
            ring: SpscRing::new(192_000),
            rate: AtomicU32::new(48_000),
            channels: AtomicU32::new(2),
            active: AtomicBool::new(false),
        }
    }

    pub fn push(&self, rate: u32, channels: u8, samples: &[f32]) {
        self.rate.store(rate, Ordering::Relaxed);
        self.channels.store(channels as u32, Ordering::Relaxed);
        self.ring.push(samples);
    }

    pub fn ring(&self) -> &SpscRing {
        &self.ring
    }

    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn channels(&self) -> u32 {
        self.channels.load(Ordering::Relaxed)
    }
}
