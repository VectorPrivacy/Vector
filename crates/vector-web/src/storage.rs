//! Where this browser keeps what Vector stores, as `web/worker.js` found it, and
//! the few operations that differ by it: mounting the databases, deleting an
//! account's, and making sure writes land before the page reloads.

use sqlite_wasm_rs::WasmOsCallback;
use sqlite_wasm_vfs::relaxed_idb::{self, RelaxedIdbCfg};
use sqlite_wasm_vfs::sahpool::{self, OpfsSAHPoolCfgBuilder};
use wasm_bindgen::JsValue;

/// One slot per database file and per journal; the default six run out by the fifth account.
const POOL_SLOTS: u32 = 48;

static STORAGE: std::sync::OnceLock<Storage> = std::sync::OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Storage {
    /// OPFS: kept until the user clears it.
    Persistent,
    /// IndexedDB in a private window: kept until the browser closes.
    Session,
    /// Nothing: a reload starts over.
    Memory,
}

impl Storage {
    pub(crate) fn current() -> Self {
        STORAGE.get().copied().unwrap_or(Storage::Persistent)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Storage::Persistent => "persistent",
            Storage::Session => "session",
            Storage::Memory => "memory",
        }
    }
}

fn opfs_cfg() -> sahpool::OpfsSAHPoolCfg {
    OpfsSAHPoolCfgBuilder::new().directory("vector-web").build()
}

/// Mount the databases where `level` says, as SQLite's default storage.
pub(crate) async fn install(level: &str) -> Result<(), JsValue> {
    let storage = match level {
        "session" => Storage::Session,
        "memory" => Storage::Memory,
        _ => Storage::Persistent,
    };
    match storage {
        Storage::Persistent => {
            let pool = sahpool::install::<WasmOsCallback>(&opfs_cfg(), true)
                .await
                .map_err(|e| JsValue::from_str(&format!("OPFS storage unavailable: {e:?}")))?;
            pool.reserve_minimum_capacity(POOL_SLOTS)
                .await
                .map_err(|e| JsValue::from_str(&format!("OPFS storage unavailable: {e:?}")))?;
        }
        Storage::Session => {
            relaxed_idb::install::<WasmOsCallback>(&RelaxedIdbCfg::default(), true)
                .await
                .map_err(|e| JsValue::from_str(&format!("Browser storage unavailable: {e}")))?;
        }
        // SQLite's own default on the web keeps databases in memory.
        Storage::Memory => {}
    }
    // Neither IndexedDB mirroring nor memory syncs to disk, and neither shares memory for WAL.
    vector_core::db::set_relaxed_durability(storage != Storage::Persistent);
    let _ = STORAGE.set(storage);
    Ok(())
}

/// Remove `npub`'s databases (main file and journal) from wherever they live.
pub(crate) async fn delete_databases(npub: &str) -> Result<(), String> {
    let marker = format!("/{npub}/");
    match Storage::current() {
        Storage::Persistent => {
            // Installing again only hands back the pool `install` registered.
            let pool = sahpool::install::<WasmOsCallback>(&opfs_cfg(), true)
                .await
                .map_err(|e| format!("OPFS storage unavailable: {e:?}"))?;
            for name in pool.list().into_iter().filter(|n| n.contains(&marker)) {
                pool.delete_db(&name).map_err(|e| format!("Failed to remove {name}: {e:?}"))?;
            }
        }
        Storage::Session => {
            let util = relaxed_idb::install::<WasmOsCallback>(&RelaxedIdbCfg::default(), true)
                .await
                .map_err(|e| format!("Browser storage unavailable: {e}"))?;
            for name in util.list().into_iter().filter(|n| n.contains(&marker)) {
                util.delete_db(&name)
                    .map_err(|e| format!("Failed to remove {name}: {e}"))?
                    .await
                    .map_err(|e| format!("Failed to remove {name}: {e}"))?;
            }
        }
        Storage::Memory => {
            let util = sqlite_wasm_rs::MemVfsUtil::<WasmOsCallback>::new();
            for name in util.list().into_iter().filter(|n| n.contains(&marker)) {
                util.delete_db(&name);
            }
        }
    }
    Ok(())
}

/// Wait until every database write so far has reached IndexedDB, which commits
/// behind SQLite: a reload would otherwise drop the last of them.
pub(crate) async fn flush() {
    if Storage::current() != Storage::Session {
        return;
    }
    let Ok(util) = relaxed_idb::install::<WasmOsCallback>(&RelaxedIdbCfg::default(), true).await else { return };
    // Commits run in order, so a no-op queued now resolves after all of them.
    if let Ok(done) = util.delete_db("\u{1}flush") {
        let _ = done.await;
    }
}

/// Reload the page onto whichever account is marked active, once storage has it.
pub(crate) async fn reload() {
    crate::miniapps::end_sessions().await;
    flush().await;
    crate::emitter::emit("session_reload", &());
}
