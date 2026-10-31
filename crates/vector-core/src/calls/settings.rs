//! The voice processing switches and the two call volumes: read live by the capture
//! thread and the mixer, so a change lands mid-call; persisted per account.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct AudioSettings {
    pub auto_gain: bool,
    pub echo_cancel: bool,
    pub noise_suppress: bool,
    /// What we send, 0.0 to 1.0, applied after the processing so a test hears it.
    #[serde(default = "unity")]
    pub mic_volume: f32,
    /// Everything a call plays, 0.0 to 1.0, on top of the in-call volume.
    #[serde(default = "unity")]
    pub speaker_volume: f32,
}

fn unity() -> f32 {
    1.0
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { auto_gain: true, echo_cancel: true, noise_suppress: true, mic_volume: 1.0, speaker_volume: 1.0 }
    }
}

const AGC: u8 = 1;
const AEC: u8 = 2;
const NS: u8 = 4;
const KEY: &str = "calls_audio";

static FLAGS: AtomicU8 = AtomicU8::new(AGC | AEC | NS);
static MIC_GAIN: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0
static SPEAKER_GAIN: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0

impl AudioSettings {
    fn to_flags(self) -> u8 {
        (self.auto_gain as u8 * AGC) | (self.echo_cancel as u8 * AEC) | (self.noise_suppress as u8 * NS)
    }
    fn store(self) {
        FLAGS.store(self.to_flags(), Ordering::Relaxed);
        MIC_GAIN.store(level(self.mic_volume).to_bits(), Ordering::Relaxed);
        SPEAKER_GAIN.store(level(self.speaker_volume).to_bits(), Ordering::Relaxed);
    }
}

/// A volume as the engine applies it: finite and within 0..=1.
fn level(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 }
}

/// What the capture thread applies right now.
pub fn current() -> AudioSettings {
    let f = FLAGS.load(Ordering::Relaxed);
    AudioSettings {
        auto_gain: f & AGC != 0,
        echo_cancel: f & AEC != 0,
        noise_suppress: f & NS != 0,
        mic_volume: mic_gain(),
        speaker_volume: speaker_gain(),
    }
}

pub fn mic_gain() -> f32 {
    f32::from_bits(MIC_GAIN.load(Ordering::Relaxed))
}

pub fn speaker_gain() -> f32 {
    f32::from_bits(SPEAKER_GAIN.load(Ordering::Relaxed))
}

/// Applies at once and remembers for next time.
pub fn set(s: AudioSettings) -> Result<(), String> {
    s.store();
    let json = serde_json::to_string(&current()).map_err(|e| e.to_string())?;
    crate::db::set_sql_setting(KEY.to_string(), json)
}

/// Your microphone and speakers as you left them, one switch for every call: a voice channel
/// or a DM call starts this way, and muting or deafening in either carries to the next.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MicState {
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

const MIC_KEY: &str = "calls_mic";

pub fn mic_state() -> MicState {
    crate::db::get_sql_setting(MIC_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Remember the switches for the calls that follow; the UI hears of a change wherever it was made.
pub fn remember_mic(s: MicState) {
    if mic_state() == s {
        return;
    }
    if let Ok(json) = serde_json::to_string(&s) {
        let _ = crate::db::set_sql_setting(MIC_KEY.to_string(), json);
    }
    crate::traits::emit_event("call_mic_state", &s);
}

/// What a call plays at `volume`: nothing while deafened.
pub fn heard(volume: f32) -> f32 {
    if mic_state().deafened { 0.0 } else { volume }
}

/// Loads the account's saved settings into the live values; defaults when unset.
pub fn load() -> AudioSettings {
    let s = crate::db::get_sql_setting(KEY.to_string())
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<AudioSettings>(&j).ok())
        .unwrap_or_default();
    s.store();
    current()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_saved_before_the_volumes_existed_reads_them_as_unity() {
        let s: AudioSettings = serde_json::from_str(r#"{"auto_gain":false,"echo_cancel":true,"noise_suppress":true}"#).unwrap();
        assert!(!s.auto_gain);
        assert_eq!((s.mic_volume, s.speaker_volume), (1.0, 1.0));
    }

    #[test]
    fn the_switches_carry_from_call_to_call_and_deafened_hears_nothing() {
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let account = nostr_sdk::prelude::ToBech32::to_bech32(&nostr_sdk::prelude::Keys::generate().public_key()).unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();

        assert_eq!(mic_state(), MicState::default(), "a first call starts open");
        remember_mic(MicState { muted: true, deafened: false });
        assert_eq!(mic_state(), MicState { muted: true, deafened: false });
        assert_eq!(heard(0.8), 0.8);
        remember_mic(MicState { muted: true, deafened: true });
        assert_eq!(heard(0.8), 0.0, "deafened, a call plays nothing");
    }

    #[test]
    fn a_volume_outside_its_range_is_held_to_it() {
        assert_eq!(level(1.7), 1.0);
        assert_eq!(level(-0.2), 0.0);
        assert_eq!(level(f32::NAN), 1.0);
        assert_eq!(level(0.35), 0.35);
    }
}
