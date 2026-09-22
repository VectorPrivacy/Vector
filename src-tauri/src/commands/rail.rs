//! The community rail's arrangement: order and folders, synced across the
//! account's own devices as its own settings document (`vector/rail`).
//!
//! Every edit commits locally and returns at once; the publish waits for the
//! dragging to stop (see `synced_prefs::save_rail_debounced`).

use vector_core::rail_layout::{self, RailDrag, RailDrop, RailLayout};
use vector_core::synced_prefs::{self, Pref};

/// The arrangement as this device holds it, for the rail's first paint.
#[tauri::command]
pub async fn get_rail_layout() -> Result<RailLayout, String> {
    vector_core::db::scoped(async move { Ok(synced_prefs::load_rail()) }).await
}

/// A drag let go. `live` is every community on the rail in its painted order:
/// the drop lands on what the user SAW, and a rail that was never arranged
/// stores nothing until now, so the first drag is what fixes its order.
#[tauri::command]
pub async fn rail_apply_drop(
    source: RailDrag,
    target: RailDrop,
    live: Vec<String>,
) -> Result<RailLayout, String> {
    edit(move |l| {
        let base = l.merged_with(&live);
        let mut next = base.clone();
        next.apply_drop(&source, &target, &rail_layout::new_folder_id())?;
        // Let go where it started: freezing the order is not worth a write.
        if next != base {
            *l = next;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn rail_rename_folder(folder_id: String, name: String) -> Result<RailLayout, String> {
    edit(|l| l.rename_folder(&folder_id, &name)).await
}

/// `hue` in degrees, or null for the default grey.
#[tauri::command]
pub async fn rail_set_folder_hue(folder_id: String, hue: Option<u16>) -> Result<RailLayout, String> {
    edit(|l| l.set_folder_hue(&folder_id, hue)).await
}

#[tauri::command]
pub async fn rail_dissolve_folder(folder_id: String) -> Result<RailLayout, String> {
    edit(|l| l.dissolve_folder(&folder_id)).await
}

/// Forget a community the user left, so a rejoin lands at the end rather than
/// back in its old folder. Writes nothing for a rail that never placed it.
pub async fn forget_community(community_id: &str) {
    let id = community_id.to_string();
    let _ = edit(move |l| {
        if l.contains(&id) {
            l.remove_key(&id);
        }
        Ok(())
    })
    .await;
}

/// Load, change, and commit only if something moved.
///
/// Refused until the relay copy has been read: an edit before then would mark
/// the rail dirty, and a dirty rail is published OVER the relays at the next
/// chance, so a fresh login could flatten an arrangement made on another device.
async fn edit(f: impl FnOnce(&mut RailLayout) -> Result<(), String>) -> Result<RailLayout, String> {
    vector_core::db::scoped(async move {
        if !synced_prefs::is_hydrated(Pref::Rail) {
            return Err("Still syncing your communities, try again in a moment".to_string());
        }
        let before = synced_prefs::load_rail();
        let mut layout = before.clone();
        f(&mut layout)?;
        if layout != before {
            synced_prefs::save_rail_debounced(&layout)?;
            emit(&layout);
        }
        Ok(layout)
    })
    .await
}

/// Through `emit_event` so an arrangement belonging to an account since
/// swapped away paints nothing.
pub fn emit(layout: &RailLayout) {
    vector_core::traits::emit_event_json(
        "rail_layout_updated",
        serde_json::to_value(layout).unwrap_or_default(),
    );
}

// Handlers: get_rail_layout, rail_apply_drop, rail_rename_folder, rail_set_folder_hue, rail_dissolve_folder
