//! Vector Web: vector-core in a browser Web Worker.
//!
//! The page talks to this module the way the desktop frontend talks to Tauri:
//! `invoke(command, argsJson)` resolves to a JSON string, and backend events
//! arrive through the sink registered with `set_event_sink`. Commands mirror the
//! Tauri handlers' names and argument shapes so the frontend runs unmodified.

#![cfg(target_arch = "wasm32")]

mod account;
mod chat_ops;
mod community_ops;
mod network_ops;
mod profile_ops;
mod selfsync;
mod attachments;
mod clock;
mod commands;
mod community;
mod events;
mod files;
mod images;
mod emitter;
mod messaging;
mod sync;

use std::path::PathBuf;
use std::pin::Pin;

/// A feature module's commands: `None` when the command isn't its own.
type Module = for<'a> fn(&'a str, &'a commands::Args) -> Pin<Box<dyn std::future::Future<Output = Option<Result<serde_json::Value, String>>> + 'a>>;

/// Consulted in order for commands the core dispatcher doesn't answer.
const MODULES: &[Module] = &[chat_ops::dispatch, profile_ops::dispatch, network_ops::dispatch, community_ops::dispatch];

use wasm_bindgen::prelude::*;

/// Root of every account's files inside the OPFS pool.
const APP_DATA: &str = "/vector";

#[wasm_bindgen]
pub fn set_event_sink(sink: js_sys::Function) {
    emitter::set_sink(sink);
}

/// Mount OPFS storage and initialise core. Must run in a dedicated worker:
/// the pool VFS needs synchronous access handles, which pages don't get.
#[wasm_bindgen]
pub async fn start(version: String) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let cfg = sqlite_wasm_vfs::sahpool::OpfsSAHPoolCfgBuilder::new()
        .directory("vector-web")
        .build();
    sqlite_wasm_vfs::sahpool::install::<sqlite_wasm_rs::WasmOsCallback>(&cfg, true)
        .await
        .map_err(|e| JsValue::from_str(&format!("OPFS storage unavailable: {e:?}")))?;

    vector_core::db::set_app_version(version);
    vector_core::db::set_download_dir(PathBuf::from("/downloads"));
    vector_core::VectorCore::init(vector_core::CoreConfig {
        data_dir: PathBuf::from(APP_DATA),
        event_emitter: Some(Box::new(emitter::WebEmitter)),
    })
    .map_err(|e| JsValue::from_str(&e.to_string()))?;

    vector_core::db::spawn_bound(vector_core::profile::sync::start_profile_sync_processor(
        std::sync::Arc::new(sync::WebProfileSyncHandler),
    ));

    // Pick up the account a previous visit left active.
    if let Ok(Some(npub)) = vector_core::db::read_active_account_file() {
        if vector_core::db::set_current_account(npub.clone()).is_ok() {
            if let Err(e) = vector_core::db::init_database(&npub) {
                vector_core::log_warn!("[Web] account init failed: {e}");
            }
        }
    }
    Ok(())
}

/// Run one IPC command. `args` is the JSON object the frontend passed to
/// `invoke`; the result is the command's return value as JSON.
#[wasm_bindgen]
pub async fn invoke(cmd: String, args: String) -> Result<String, JsValue> {
    let args: serde_json::Value = serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
    match commands::dispatch(&cmd, commands::Args(args)).await {
        Ok(v) => Ok(v.to_string()),
        Err(e) => Err(JsValue::from_str(&e)),
    }
}

/// A raw-body command: `invoke(cmd, bytes, { headers })` on desktop.
#[wasm_bindgen]
pub async fn invoke_bytes(cmd: String, bytes: Vec<u8>, headers: String) -> Result<String, JsValue> {
    let headers: serde_json::Value = serde_json::from_str(&headers).unwrap_or_default();
    let header = |k: &str| headers.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    match cmd.as_str() {
        "cache_file_bytes" => {
            let name = base64_simd::STANDARD
                .decode_to_vec(header("file-name"))
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            Ok(files::cache_bytes(bytes, name, header("extension")).to_string())
        }
        _ => Err(JsValue::from_str(&format!("`{cmd}` does not take a raw body on Vector Web"))),
    }
}
