//! Which clock Vector stamps and judges by: the relays' agreed time, or this device's.

use vector_core::clock::{self, Status};

/// What the clock is doing, for Settings.
#[tauri::command]
pub fn get_clock_status() -> Status {
    clock::status()
}

/// Run on the relays' clock (`true`) or the device's, for this account.
#[tauri::command]
pub fn set_decentralised_clock(on: bool) -> Result<Status, String> {
    clock::set_decentralised(on)?;
    Ok(clock::status())
}
