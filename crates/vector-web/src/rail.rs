//! The community rail's arrangement: order and folders, synced across the
//! account's own devices as its own settings document (`vector/rail`), as on
//! desktop (`src-tauri/src/commands/rail.rs`).

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use vector_core::rail_layout::{self, RailDrag, RailDrop, RailLayout};
use vector_core::synced_prefs::{self, Pref};

use crate::commands::{to_value, Args};

pub fn dispatch<'a>(
    cmd: &'a str,
    a: &'a Args,
) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let result = match cmd {
            "rail_apply_drop" => apply_drop(a).await,
            "rail_rename_folder" => match (a.str("folderId"), a.str("name")) {
                (Ok(id), Ok(name)) => edit(|l| l.rename_folder(&id, &name)).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "rail_set_folder_hue" => match (a.str("folderId"), a.de::<Option<u16>>("hue")) {
                (Ok(id), Ok(hue)) => edit(|l| l.set_folder_hue(&id, hue)).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "rail_dissolve_folder" => match a.str("folderId") {
                Ok(id) => edit(|l| l.dissolve_folder(&id)).await,
                Err(e) => Err(e),
            },
            _ => return None,
        };
        Some(result.and_then(to_value))
    })
}

/// A drag let go. `live` is every community on the rail in its painted order:
/// the drop lands on what the user saw, and the first drag fixes that order.
async fn apply_drop(a: &Args) -> Result<RailLayout, String> {
    let source: RailDrag = a.de("source")?;
    let target: RailDrop = a.de("target")?;
    let live: Vec<String> = a.de("live")?;
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

/// Load, change, and commit only if something moved. Refused until the relay
/// copy has been read: a dirty rail is published over the relays, so an edit
/// before then could flatten an arrangement made on another device.
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
            vector_core::traits::emit_event_json(
                "rail_layout_updated",
                serde_json::to_value(&layout).unwrap_or_default(),
            );
        }
        Ok(layout)
    })
    .await
}
