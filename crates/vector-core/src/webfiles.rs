//! Files on the web: OPFS, under `files/<path>`, where the page's service worker
//! serves them back as `/vfs/<path>` (Vector Web's `convertFileSrc`).
//!
//! Async where std::fs is sync: OPFS hands out file handles only through promises.

use std::path::Path;

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
async function dirFor(path, create) {
    let dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('files', { create });
    const parts = path.split('/').filter(Boolean);
    const name = parts.pop();
    for (const p of parts) dir = await dir.getDirectoryHandle(p, { create });
    return [dir, name];
}
export async function opfs_write(path, bytes) {
    const [dir, name] = await dirFor(path, true);
    const handle = await (await dir.getFileHandle(name, { create: true })).createSyncAccessHandle();
    try {
        handle.truncate(0);
        handle.write(bytes, { at: 0 });
        handle.flush();
    } finally {
        handle.close();
    }
}
export async function opfs_read(path) {
    try {
        const [dir, name] = await dirFor(path, false);
        const file = await (await dir.getFileHandle(name)).getFile();
        return new Uint8Array(await file.arrayBuffer());
    } catch { return null; }
}
export async function opfs_size(path) {
    try {
        const [dir, name] = await dirFor(path, false);
        return (await (await dir.getFileHandle(name)).getFile()).size;
    } catch { return -1; }
}
export async function opfs_list(path) {
    try {
        let dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('files');
        for (const p of path.split('/').filter(Boolean)) dir = await dir.getDirectoryHandle(p);
        const names = [];
        for await (const [name, handle] of dir.entries()) if (handle.kind === 'file') names.push(name);
        return names;
    } catch { return []; }
}
export async function opfs_remove(path) {
    try {
        const [dir, name] = await dirFor(path, false);
        await dir.removeEntry(name, { recursive: true });
        return true;
    } catch { return false; }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn opfs_write(path: &str, bytes: Uint8Array) -> Result<(), JsValue>;
    async fn opfs_read(path: &str) -> JsValue;
    async fn opfs_size(path: &str) -> JsValue;
    async fn opfs_remove(path: &str) -> JsValue;
    async fn opfs_list(path: &str) -> JsValue;
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub async fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    opfs_write(&key(path), Uint8Array::from(bytes))
        .await
        .map_err(|e| format!("OPFS write {}: {e:?}", path.display()))
}

pub async fn read(path: &Path) -> Result<Vec<u8>, String> {
    let v = opfs_read(&key(path)).await;
    if v.is_null() {
        return Err(format!("{} not found", path.display()));
    }
    Ok(Uint8Array::from(v).to_vec())
}

/// Size in bytes, `None` if absent.
pub async fn size(path: &Path) -> Option<u64> {
    opfs_size(&key(path)).await.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64)
}

pub async fn exists(path: &Path) -> bool {
    size(path).await.is_some()
}

pub async fn remove(path: &Path) -> bool {
    opfs_remove(&key(path)).await.as_bool().unwrap_or(false)
}

/// Names of the files directly inside `dir`.
pub async fn list(dir: &Path) -> Vec<String> {
    let v = opfs_list(&key(dir)).await;
    js_sys::Array::from(&v).iter().filter_map(|n| n.as_string()).collect()
}
