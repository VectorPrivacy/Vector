//! The counters one call keeps for its whole life, shared by the media threads, the
//! liveness check and the stats line. Atomics so no thread waits on another.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Default)]
pub struct MediaStats {
    pub sent: AtomicU64,
    pub send_dropped: AtomicU64,
    pub received: AtomicU64,
    /// Frames skipped over: a hole wider than FEC covers, or a jump past a backlog.
    pub lost: AtomicU64,
    /// Frames the buffer discarded on purpose to take latency back.
    pub shed: AtomicU64,
    /// Frames rebuilt from the FEC data in their successor.
    pub rebuilt: AtomicU64,
    pub concealed: AtomicU64,
    pub late: AtomicU64,
    pub depth_ms: AtomicU32,
    pub jitter_ms: AtomicU32,
    /// Cable clicks cut out of the microphone before encoding.
    pub clicks_cut: AtomicU64,
    /// What the encoder is sending at, in kbit/s.
    pub bitrate_kbps: AtomicU32,
    /// Packets the network dropped on the way out, percent of the last second (f32 bits).
    pub net_loss: AtomicU32,
    /// Loudness of what the microphone sends after processing, 0 to 1 (f32 bits).
    pub mic_level: AtomicU32,
    /// Loudness of what the peer sends, 0 to 1 (f32 bits).
    pub peer_level: AtomicU32,
    /// The next outgoing sequence number (u16). Lives here, not in the capture
    /// loop, so a restarted engine keeps counting: the peer drops everything
    /// numbered below what it last played, and a reset silences us until the
    /// count catches back up.
    pub next_seq: AtomicU32,
}

impl MediaStats {
    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.mic_level.load(Ordering::Relaxed))
    }
    pub fn peer_level(&self) -> f32 {
        f32::from_bits(self.peer_level.load(Ordering::Relaxed))
    }
    pub fn net_loss(&self) -> f32 {
        f32::from_bits(self.net_loss.load(Ordering::Relaxed))
    }
}
