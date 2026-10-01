//! Files on the web, under `<path>` in whichever store the browser keeps: OPFS,
//! IndexedDB for a private window, or memory. The worker (`web/worker.js`) picks
//! the store and exposes it as `globalThis.vectorFiles`; the page is served the
//! files as `/vfs/<path>` by its service worker, or as blob URLs where it has none.
//!
//! Async where std::fs is sync: every browser store answers through promises.

use std::path::Path;

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export async function store_write(path, bytes) { await globalThis.vectorFiles.write(path, bytes); }
export async function store_read(path) { return globalThis.vectorFiles.read(path); }
export async function store_size(path) { return globalThis.vectorFiles.size(path); }
export async function store_remove(path) { return globalThis.vectorFiles.remove(path); }
export async function store_list(path) { return (await globalThis.vectorFiles.list(path, false)).map(([name]) => name); }
export async function store_tree(path, recursive) { return JSON.stringify(await globalThis.vectorFiles.list(path, recursive)); }
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn store_write(path: &str, bytes: Uint8Array) -> Result<(), JsValue>;
    async fn store_read(path: &str) -> JsValue;
    async fn store_size(path: &str) -> JsValue;
    async fn store_remove(path: &str) -> JsValue;
    async fn store_list(path: &str) -> JsValue;
    async fn store_tree(path: &str, recursive: bool) -> JsValue;
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub async fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    store_write(&key(path), Uint8Array::from(bytes))
        .await
        .map_err(|e| format!("File write {}: {e:?}", path.display()))
}

pub async fn read(path: &Path) -> Result<Vec<u8>, String> {
    let v = store_read(&key(path)).await;
    if v.is_null() {
        return Err(format!("{} not found", path.display()));
    }
    Ok(Uint8Array::from(v).to_vec())
}

/// Size in bytes, `None` if absent.
pub async fn size(path: &Path) -> Option<u64> {
    store_size(&key(path)).await.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64)
}

pub async fn exists(path: &Path) -> bool {
    size(path).await.is_some()
}

pub async fn remove(path: &Path) -> bool {
    store_remove(&key(path)).await.as_bool().unwrap_or(false)
}

/// Names of the files directly inside `dir`.
pub async fn list(dir: &Path) -> Vec<String> {
    let v = store_list(&key(dir)).await;
    js_sys::Array::from(&v).iter().filter_map(|n| n.as_string()).collect()
}

/// `(relative name, size)` of the files under `dir`; with `recursive`, names are
/// paths relative to it.
pub async fn tree(dir: &Path, recursive: bool) -> Vec<(String, u64)> {
    let json = store_tree(&key(dir), recursive).await.as_string().unwrap_or_default();
    serde_json::from_str(&json).unwrap_or_default()
}
