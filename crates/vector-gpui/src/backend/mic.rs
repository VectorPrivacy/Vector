//! Settings → Calls: the devices, the voice processing switches and volumes, Direct Connection,
//! and the microphone test.

use vector_core::calls::settings::{self, AudioSettings};

pub use crate::audio_devices::{DeviceList, DevicePrefs};

pub fn audio_settings() -> AudioSettings {
    settings::load()
}

pub fn set_audio_settings(s: AudioSettings) -> Result<(), String> {
    settings::set(s)
}

pub fn devices() -> DeviceList {
    crate::audio_devices::load();
    crate::audio_devices::list()
}

/// The engine follows the change, a running test included.
pub fn set_devices(prefs: DevicePrefs) -> Result<(), String> {
    crate::audio_devices::set(prefs)
}

pub fn direct() -> bool {
    vector_core::transport::realtime::direct()
}

pub async fn set_direct(on: bool) -> Result<(), String> {
    vector_core::transport::realtime::set_direct(on).await
}

/// The call's own capture chain (gain, echo, noise), metered by `mic_level` events until stopped.
pub async fn test_start() -> Result<(), String> {
    super::calls::mic_test_start().await
}

pub fn test_stop() {
    super::calls::mic_test_stop();
}
