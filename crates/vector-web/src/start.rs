//! The widescreen Main Menu's pins and folders, synced as their own settings
//! document (`vector/start`), as on desktop (`src-tauri/src/commands/start.rs`).

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use vector_core::rail_layout::{self, RailDrag, RailDrop, RailLayout};
use vector_core::start_menu;
use vector_core::synced_prefs::{self, Pref};

use crate::commands::{to_value, Args};

pub fn dispatch<'a>(
    cmd: &'a str,
    a: &'a Args,
) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let result = match cmd {
            "get_start_layout" => Ok(synced_prefs::load_start()),
            "start_pin" => match a.str("key") {
                Ok(key) => edit(move |l| start_menu::pin(l, &key).map(|_| ())).await,
                Err(e) => Err(e),
            },
            "start_unpin" => match a.str("key") {
                Ok(key) => {
                    edit(move |l| {
                        start_menu::unpin(l, &key);
                        Ok(())
                    })
                    .await
                }
                Err(e) => Err(e),
            },
            "start_apply_drop" => match (a.de::<RailDrag>("source"), a.de::<RailDrop>("target")) {
                (Ok(source), Ok(target)) => edit(move |l| l.apply_drop(&source, &target, &rail_layout::new_folder_id())).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "start_rename_folder" => match (a.str("folderId"), a.str("name")) {
                (Ok(id), Ok(name)) => edit(|l| l.rename_folder(&id, &name)).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "start_dissolve_folder" => match a.str("folderId") {
                Ok(id) => edit(|l| l.dissolve_folder(&id)).await,
                Err(e) => Err(e),
            },
            _ => return None,
        };
        Some(result.and_then(to_value))
    })
}

/// Load, change, and commit only if something moved. Refused until the relay copy
/// has been read, as the rail's edits are.
async fn edit(f: impl FnOnce(&mut RailLayout) -> Result<(), String>) -> Result<RailLayout, String> {
    vector_core::db::scoped(async move {
        if !synced_prefs::is_hydrated(Pref::Start) {
            return Err("Still syncing your pins, try again in a moment".to_string());
        }
        let (layout, changed) = synced_prefs::edit_layout(Pref::Start, f)?;
        if changed {
            vector_core::traits::emit_event_json("start_layout_updated", serde_json::to_value(&layout).unwrap_or_default());
        }
        Ok(layout)
    })
    .await
}
