use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};
use tauri_plugin_store::StoreBuilder;
use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VectorDB {
    pub theme: Option<String>,
    pub pkey: Option<String>,
}

const DB_PATH: &str = "vector.json";

fn get_store<R: Runtime>(handle: &AppHandle<R>) -> Arc<tauri_plugin_store::Store<R>> {
    StoreBuilder::new(handle, PathBuf::from(DB_PATH))
        .auto_save(Duration::from_millis(100))
        .build()
        .unwrap()
}

#[command]
pub fn get_db<R: Runtime>(handle: AppHandle<R>) -> Result<VectorDB, String> {
    let store = get_store(&handle);
    
    // Extract optional fields
    let theme = match store.get("theme") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    let pkey = match store.get("pkey") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    Ok(VectorDB {
        theme,
        pkey,
    })
}

#[command]
pub fn set_theme<R: Runtime>(handle: AppHandle<R>, theme: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("theme".to_string(), serde_json::json!(theme));
    Ok(())
}

#[command]
pub fn get_theme<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("theme") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_pkey<R: Runtime>(handle: AppHandle<R>, pkey: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("pkey".to_string(), serde_json::json!(pkey));
    Ok(())
}

#[command]
pub fn get_pkey<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("pkey") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn remove_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<bool, String> {
    let store = get_store(&handle);
    let deleted = store.delete(&key);
    Ok(deleted)
}