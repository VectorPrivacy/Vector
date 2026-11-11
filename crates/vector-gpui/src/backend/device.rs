//! This device's own preferences (the Tauri client keeps these in `localStorage`): a JSON
//! map in the data directory, never synced and never per account.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Map, Value};

static STORE: Mutex<Option<(Option<PathBuf>, Map<String, Value>)>> = Mutex::new(None);

/// Load the map from `dir`; without one (fixtures) it lives in memory only.
pub fn init(dir: Option<PathBuf>) {
    let path = dir.map(|d| d.join("device.json"));
    let map = path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Map<String, Value>>(&b).ok())
        .unwrap_or_default();
    *STORE.lock().unwrap() = Some((path, map));
}

pub fn get(key: &str) -> Option<String> {
    let guard = STORE.lock().unwrap();
    guard
        .as_ref()?
        .1
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub fn set(key: &str, value: impl Into<String>) {
    let mut guard = STORE.lock().unwrap();
    let (path, map) = guard.get_or_insert_with(|| (None, Map::new()));
    map.insert(key.to_string(), Value::String(value.into()));
    if let Some(path) = path {
        if let Ok(bytes) = serde_json::to_vec(map) {
            let _ = std::fs::write(path, bytes);
        }
    }
}
