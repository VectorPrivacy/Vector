//! The widescreen Start Menu's pins and folders, synced across the account's own
//! devices as their own settings document (`vector/start`). Keys and pinning rules
//! are vector-core's `start_menu`; the arrangement is the rail's shape.

use vector_core::rail_layout::{self, RailDrag, RailDrop, RailLayout};
use vector_core::start_menu;
use vector_core::synced_prefs::{self, Pref};

#[tauri::command]
pub async fn get_start_layout() -> Result<RailLayout, String> {
    vector_core::db::scoped(async move { Ok(synced_prefs::load_start()) }).await
}

#[tauri::command]
pub async fn start_pin(key: String) -> Result<RailLayout, String> {
    edit(move |l| start_menu::pin(l, &key).map(|_| ())).await
}

#[tauri::command]
pub async fn start_unpin(key: String) -> Result<RailLayout, String> {
    edit(move |l| {
        start_menu::unpin(l, &key);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn start_apply_drop(source: RailDrag, target: RailDrop) -> Result<RailLayout, String> {
    edit(move |l| l.apply_drop(&source, &target, &rail_layout::new_folder_id())).await
}

#[tauri::command]
pub async fn start_rename_folder(folder_id: String, name: String) -> Result<RailLayout, String> {
    edit(|l| l.rename_folder(&folder_id, &name)).await
}

#[tauri::command]
pub async fn start_dissolve_folder(folder_id: String) -> Result<RailLayout, String> {
    edit(|l| l.dissolve_folder(&folder_id)).await
}

/// Load, change, and commit only if something moved. Refused until the relay copy
/// has been read: a dirty layout is published over the relays, so an edit before
/// then could flatten pins arranged on another device.
async fn edit(f: impl FnOnce(&mut RailLayout) -> Result<(), String>) -> Result<RailLayout, String> {
    vector_core::db::scoped(async move {
        if !synced_prefs::is_hydrated(Pref::Start) {
            return Err("Still syncing your pins, try again in a moment".to_string());
        }
        let (layout, changed) = synced_prefs::edit_layout(Pref::Start, f)?;
        if changed {
            emit(&layout);
        }
        Ok(layout)
    })
    .await
}

/// Through `emit_event` so pins belonging to an account since swapped away paint nothing.
pub fn emit(layout: &RailLayout) {
    vector_core::traits::emit_event_json("start_layout_updated", serde_json::to_value(layout).unwrap_or_default());
}

// Handlers: get_start_layout, start_pin, start_unpin, start_apply_drop, start_rename_folder, start_dissolve_folder
