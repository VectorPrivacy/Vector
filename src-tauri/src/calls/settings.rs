//! The voice processing switches: read every frame by the capture thread, so a
//! change lands mid-call; persisted per account.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct AudioSettings {
    pub auto_gain: bool,
    pub echo_cancel: bool,
    pub noise_suppress: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { auto_gain: true, echo_cancel: true, noise_suppress: true }
    }
}

const AGC: u8 = 1;
const AEC: u8 = 2;
const NS: u8 = 4;
const KEY: &str = "calls_audio";

static FLAGS: AtomicU8 = AtomicU8::new(AGC | AEC | NS);

impl AudioSettings {
    fn to_flags(self) -> u8 {
        (self.auto_gain as u8 * AGC) | (self.echo_cancel as u8 * AEC) | (self.noise_suppress as u8 * NS)
    }
    fn from_flags(f: u8) -> Self {
        Self { auto_gain: f & AGC != 0, echo_cancel: f & AEC != 0, noise_suppress: f & NS != 0 }
    }
}

/// What the capture thread applies right now.
pub fn current() -> AudioSettings {
    AudioSettings::from_flags(FLAGS.load(Ordering::Relaxed))
}

/// Applies at once and remembers for next time.
pub fn set(s: AudioSettings) -> Result<(), String> {
    FLAGS.store(s.to_flags(), Ordering::Relaxed);
    let json = serde_json::to_string(&s).map_err(|e| e.to_string())?;
    vector_core::db::set_sql_setting(KEY.to_string(), json)
}

/// Loads the account's saved switches into the live flags; defaults when unset.
pub fn load() -> AudioSettings {
    let s = vector_core::db::get_sql_setting(KEY.to_string())
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<AudioSettings>(&j).ok())
        .unwrap_or_default();
    FLAGS.store(s.to_flags(), Ordering::Relaxed);
    s
}
