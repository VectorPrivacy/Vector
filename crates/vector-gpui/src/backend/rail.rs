//! The community rail's arrangement, synced across the account's own devices (`vector/rail`).
//! Every edit commits locally at once; the publish waits for the dragging to stop.

use vector_core::rail_layout::{self, RailDrag, RailDrop, RailLayout};
use vector_core::synced_prefs::{self, Pref};

pub fn load() -> RailLayout {
    synced_prefs::load_rail()
}

/// A drag let go. `live` is the rail as painted: the drop lands on what the user saw, and a
/// rail never arranged stores nothing until its first drag fixes the order.
pub async fn drop(source: RailDrag, target: RailDrop, live: Vec<String>) -> Result<RailLayout, String> {
    edit(move |l| {
        let base = l.merged_with(&live);
        let mut next = base.clone();
        next.apply_drop(&source, &target, &rail_layout::new_folder_id())?;
        if next != base {
            *l = next;
        }
        Ok(())
    })
    .await
}

/// A community left: a rejoin lands fresh rather than back in its old folder.
pub async fn forget(community_id: String) {
    let _ = edit(move |l| {
        if l.contains(&community_id) {
            l.remove_key(&community_id);
        }
        Ok(())
    })
    .await;
}

/// Load, change, commit only if something moved. Refused until the relay copy is read, or a
/// fresh login could publish a flat rail over one arranged on another device.
pub async fn edit(f: impl FnOnce(&mut RailLayout) -> Result<(), String> + Send + 'static) -> Result<RailLayout, String> {
    vector_core::db::scoped(async move {
        if !synced_prefs::is_hydrated(Pref::Rail) {
            return Err("Still syncing your communities, try again in a moment".to_string());
        }
        let (layout, changed) = synced_prefs::edit_layout(Pref::Rail, f)?;
        if changed {
            vector_core::traits::emit_event_json(
                "rail_layout_updated",
                serde_json::to_value(&layout).unwrap_or_default(),
            );
        }
        Ok(layout)
    })
    .await
}
