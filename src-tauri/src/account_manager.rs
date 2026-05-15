use std::path::PathBuf;
use std::sync::{Arc, RwLock, LazyLock};
use serde::Serialize;
use tauri::{AppHandle, Runtime, Manager};

/// Metadata for one account, surfaced to the frontend's account picker
/// (pre-login) and the in-app My Profile dropdown (post-login).
///
/// Reads come from the account's own `vector.db` opened read-only — no global
/// state is mutated and the active account's session is never disturbed.
#[derive(Debug, Clone, Serialize)]
pub struct AccountMetadata {
    pub npub: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub avatar_cached: Option<String>,
    pub has_encryption: bool,
    pub last_active: Option<i64>,
}

// ============================================================================
// Database — delegates to vector-core's single connection pool
// ============================================================================

/// Type aliases — all 149 call sites use these unchanged.
pub type ConnectionGuard = vector_core::db::ConnectionGuard;
pub type WriteConnectionGuard = vector_core::db::WriteConnectionGuard;

/// Set the app data directory (delegates to vector-core).
pub fn set_app_data_dir(path: PathBuf) {
    vector_core::db::set_app_data_dir(path);
}

/// Get the app data directory (delegates to vector-core).
pub fn get_app_data_dir() -> Result<&'static PathBuf, String> {
    vector_core::db::get_app_data_dir()
}

/// Get a READ connection guard (delegates to vector-core pool).
pub fn get_db_connection_guard<R: Runtime>(_handle: &AppHandle<R>) -> Result<ConnectionGuard, String> {
    vector_core::db::get_db_connection_guard_static()
}

/// Get the WRITE connection guard (delegates to vector-core pool).
pub fn get_write_connection_guard<R: Runtime>(_handle: &AppHandle<R>) -> Result<WriteConnectionGuard, String> {
    vector_core::db::get_write_connection_guard_static()
}

/// Get a READ connection guard using static path (delegates to vector-core pool).
pub fn get_db_connection_guard_static() -> Result<ConnectionGuard, String> {
    vector_core::db::get_db_connection_guard_static()
}

/// Get the WRITE connection guard using static path (delegates to vector-core pool).
pub fn get_write_connection_guard_static() -> Result<WriteConnectionGuard, String> {
    vector_core::db::get_write_connection_guard_static()
}

/// Close ALL database connections. Used when switching accounts.
pub fn close_db_connection() {
    vector_core::db::close_database();
}

/// Initialize the DB pool using static path (for headless/background service).
#[allow(dead_code)]
pub fn init_db_pool_static(_db_path: &std::path::Path) -> Result<(), String> {
    let npub = get_current_account()?;
    vector_core::db::init_database(&npub)
}

/// Pending account waiting for encryption (npub stored before database creation)
static PENDING_ACCOUNT: LazyLock<Arc<RwLock<Option<String>>>> = LazyLock::new(|| Arc::new(RwLock::new(None)));

/// Get the profile directory for a given npub (full npub, no truncation)
///
/// Returns: AppData/npub1qwertyuiop.../
pub fn get_profile_directory<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str
) -> Result<PathBuf, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    // Validate npub format
    if !npub.starts_with("npub1") {
        return Err(format!("Invalid npub format: {}", npub));
    }

    // Use full npub as directory name
    let profile_dir = app_data.join(npub);

    // Create directory if it doesn't exist
    if !profile_dir.exists() {
        std::fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
        println!("[Account Manager] Created profile directory: {}", profile_dir.display());
    }

    Ok(profile_dir)
}

/// Get the database path for a given npub
///
/// Returns: AppData/npub1qwerty.../vector.db
pub fn get_database_path<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str
) -> Result<PathBuf, String> {
    let profile_dir = get_profile_directory(handle, npub)?;
    Ok(profile_dir.join("vector.db"))
}

// ============================================================================
// Static Path Helpers (headless-safe — no AppHandle required)
// ============================================================================

/// Enumerate every valid account on disk by scanning the app-data root.
///
/// Returns npubs whose `vector.db` has a non-empty `pkey` row, sorted
/// lexicographically. Broken / unreadable directories are omitted; this
/// function never deletes. Use `prune_invalid_accounts` for cleanup.
pub fn list_accounts<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let mut accounts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            // Reject symlinks so a crafted `<app_data>/<valid-npub-name>`
            // pointing elsewhere can't redirect downstream `remove_dir_all`.
            let Ok(ft) = entry.file_type() else { continue; };
            if !ft.is_dir() || ft.is_symlink() { continue; }
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else { continue; };
            if !name.starts_with("npub1") { continue; }
            if let Ok(true) = account_has_valid_pkey(handle, &name) {
                accounts.push(name);
            }
        }
    }

    accounts.sort();
    Ok(accounts)
}

/// Explicit maintenance: remove account directories whose `pkey` row is
/// positively empty/missing. Intended for a user-triggered "clean up
/// broken accounts" flow; never invoked from boot / picker / swap paths.
///
/// Connection-open failures leave the directory alone — we only delete
/// on positive proof of invalidity.
#[allow(dead_code)]
pub fn prune_invalid_accounts<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let mut pruned = Vec::new();

    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue; };
            if !ft.is_dir() || ft.is_symlink() { continue; }
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else { continue; };
            if !name.starts_with("npub1") { continue; }

            if matches!(account_has_valid_pkey(handle, &name), Ok(false)) {
                let dir = entry.path();
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    eprintln!("[Account Manager] prune_invalid_accounts: failed to remove {}: {}", dir.display(), e);
                } else {
                    pruned.push(name);
                }
            }
        }
    }

    Ok(pruned)
}

/// Read display metadata for one account by opening its vector.db read-only.
/// Never mutates global state — safe to call before the user has chosen an
/// account. Any read failure returns a minimally populated record so the
/// frontend can still render the npub.
pub fn read_account_metadata<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str,
) -> Result<AccountMetadata, String> {
    let db_path = get_database_path(handle, npub)?;
    Ok(read_account_metadata_at(&db_path, npub))
}

/// Same as `read_account_metadata` but takes the database path directly.
/// Lives outside the Tauri AppHandle path so unit tests can hit a temp DB.
pub fn read_account_metadata_at(db_path: &std::path::Path, npub: &str) -> AccountMetadata {
    let mut metadata = AccountMetadata {
        npub: npub.to_string(),
        display_name: None,
        avatar_url: None,
        avatar_cached: None,
        has_encryption: false,
        last_active: None,
    };

    if !db_path.exists() {
        return metadata;
    }

    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return metadata,
    };

    if let Ok((nickname, display, name, avatar, avatar_cached)) = conn.query_row(
        "SELECT nickname, display_name, name, avatar, avatar_cached FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        )),
    ) {
        let chosen = [nickname, display, name]
            .into_iter()
            .find(|s| !s.is_empty());
        metadata.display_name = chosen;
        if !avatar.is_empty() { metadata.avatar_url = Some(avatar); }
        if !avatar_cached.is_empty() { metadata.avatar_cached = Some(avatar_cached); }
    }

    // Use the canonical resolver — accounts with `security_type` set but
    // no `encryption_enabled` row must be treated as encrypted.
    {
        let enc_row = conn.query_row::<String, _, _>(
            "SELECT value FROM settings WHERE key = 'encryption_enabled'",
            [],
            |row| row.get(0),
        ).ok();
        let sec_row = conn.query_row::<String, _, _>(
            "SELECT value FROM settings WHERE key = 'security_type'",
            [],
            |row| row.get(0),
        ).ok();
        metadata.has_encryption = vector_core::state::resolve_encryption_enabled(
            enc_row.as_deref(),
            sec_row.as_deref(),
        );
    }

    if let Ok(value) = conn.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'last_active'",
        [],
        |row| row.get(0),
    ) {
        if let Ok(parsed) = value.parse::<i64>() {
            metadata.last_active = Some(parsed);
        }
    }

    metadata
}

/// Tauri command — enumerate every valid account with display metadata.
/// Used by both the pre-login picker and the post-login My Profile dropdown.
#[tauri::command]
pub fn list_accounts_with_metadata<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<Vec<AccountMetadata>, String> {
    let npubs = list_accounts(&handle)?;
    let mut out = Vec::with_capacity(npubs.len());
    for npub in npubs {
        if let Ok(meta) = read_account_metadata(&handle, &npub) {
            out.push(meta);
        }
    }
    out.sort_by(|a, b| b.last_active.unwrap_or(0).cmp(&a.last_active.unwrap_or(0)));
    Ok(out)
}

/// Persist a `last_active` epoch-second timestamp into the current account's settings.
/// Called on every successful login so the account picker can sort correctly.
pub fn touch_last_active() -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    vector_core::db::set_sql_setting("last_active".to_string(), now.to_string())
}

/// Check if an account has a valid pkey in its database.
///
/// `Ok(true)` = real account. `Ok(false)` = DB opens but pkey row is
/// missing/empty. `Err` = transient failure (lock, AV scanner) — callers
/// that delete on `Ok(false)` MUST NOT treat `Err` the same way.
///
/// Opens read-only + `NO_MUTEX` so probing an inactive account never
/// creates WAL/SHM sidecar files and never blocks the active account's
/// writer.
fn account_has_valid_pkey<R: Runtime>(handle: &AppHandle<R>, npub: &str) -> Result<bool, String> {
    let db_path = get_database_path(handle, npub)?;

    if !db_path.exists() {
        return Ok(false);
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("Failed to open database: {}", e))?;

    // Check if the pkey exists in settings table and is not empty
    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["pkey"],
        |row| row.get(0)
    ).ok();

    Ok(result.map(|s| !s.is_empty()).unwrap_or(false))
}

/// Check if any account exists
pub fn has_any_account<R: Runtime>(handle: &AppHandle<R>) -> bool {
    let sql_accounts = list_accounts(handle).unwrap_or_default();
    !sql_accounts.is_empty()
}

/// Get the currently active account
#[tauri::command]
pub fn get_current_account() -> Result<String, String> {
    vector_core::db::get_current_account()
}

/// Pick the account to boot into. Resolution rules:
///   1. If `<app_data>/active_account` marker is set AND points to an account
///      that still exists locally → select it.
///   2. Else if exactly one valid account exists → select it (preserves the
///      single-account UX from before multi-account landed). We deliberately
///      do NOT pick "most recently active" when N>=2, because the explicit
///      picker is the right UX for ambiguous boots.
///   3. Else return None and let the frontend show the picker.
///
/// If the marker pointed at a deleted account, this falls through to (2)/(3)
/// silently — `read_active_account_file` returns None for stale markers.
///
/// Also ensures the database schema and migrations are up-to-date before
/// any other code (background tasks, frontend IPC) can access the DB.
pub fn boot_select_account<R: Runtime>(handle: &AppHandle<R>) -> Result<Option<String>, String> {
    // Idempotency guard: this runs in Tauri setup, but the dev hot-reload
    // path can re-enter the boot flow. If CURRENT_ACCOUNT is already populated,
    // just re-run schema ready and exit.
    if let Ok(current) = get_current_account() {
        ensure_schema_ready(handle, &current)?;
        // Re-seed the encryption atomic from the chosen account's DB. Idempotent.
        vector_core::state::init_encryption_enabled();
        return Ok(Some(current));
    }

    let accounts = list_accounts(handle)?;
    if accounts.is_empty() {
        let _ = vector_core::db::clear_active_account_file();
        return Ok(None);
    }

    let marker = vector_core::db::read_active_account_file()?;
    let chosen = match pick_account(&accounts, marker.as_deref()) {
        Some(n) => n,
        None => return Ok(None),
    };

    set_current_account(chosen.clone())?;
    ensure_schema_ready(handle, &chosen)?;
    // Seed the encryption atomic before any background task or frontend
    // command can read it (otherwise bg-sync could mis-route encrypt /
    // decrypt during the boot-to-foreground window).
    vector_core::state::init_encryption_enabled();
    Ok(Some(chosen))
}

/// Pure boot-resolution helper. Extracted so the priority logic is unit-testable
/// without a Tauri runtime, AppHandle, or filesystem.
pub(crate) fn pick_account(accounts: &[String], marker: Option<&str>) -> Option<String> {
    if accounts.is_empty() {
        return None;
    }
    if let Some(m) = marker {
        if accounts.iter().any(|a| a == m) {
            return Some(m.to_string());
        }
    }
    if accounts.len() == 1 {
        return Some(accounts[0].clone());
    }
    None
}


/// Ensure the database schema and all migrations are applied for an existing account.
/// Delegates to vector-core's init_database (idempotent — safe to call multiple times).
fn ensure_schema_ready<R: Runtime>(handle: &AppHandle<R>, npub: &str) -> Result<(), String> {
    let db_path = get_database_path(handle, npub)?;

    // No DB file = new account, nothing to migrate (init_profile_database will create it)
    if !db_path.exists() {
        return Ok(());
    }

    vector_core::db::init_database(npub)
}

/// Set the currently active account.
/// Clears the connection pool if switching to a different account.
pub fn set_current_account(npub: String) -> Result<(), String> {
    // Close pool if switching to a different account
    if let Ok(current) = vector_core::db::get_current_account() {
        if current != npub {
            close_db_connection();
        }
    }
    vector_core::db::set_current_account(npub)
}

/// Set a pending account (before database creation)
pub fn set_pending_account(npub: String) -> Result<(), String> {
    *PENDING_ACCOUNT.write()
        .map_err(|e| format!("Failed to write pending account: {}", e))? = Some(npub);
    Ok(())
}

/// Get the pending account (if any)
pub fn get_pending_account() -> Result<Option<String>, String> {
    Ok(PENDING_ACCOUNT.read()
        .map_err(|e| format!("Failed to read pending account: {}", e))?
        .clone())
}

/// Clear the pending account
pub fn clear_pending_account() -> Result<(), String> {
    *PENDING_ACCOUNT.write()
        .map_err(|e| format!("Failed to clear pending account: {}", e))? = None;
    Ok(())
}


/// List all accounts (Tauri command)
#[tauri::command]
pub fn list_all_accounts<R: Runtime>(handle: AppHandle<R>) -> Result<Vec<String>, String> {
    list_accounts(&handle)
}

/// Check if any account exists - Tauri command
#[tauri::command]
pub fn check_any_account_exists<R: Runtime>(handle: AppHandle<R>) -> bool {
    has_any_account(&handle)
}

/// Initialize SQL database for a specific profile.
/// Delegates to vector-core's init_database (creates schema, runs migrations, warms pool).
pub async fn init_profile_database<R: Runtime>(
    _handle: &AppHandle<R>,
    npub: &str
) -> Result<(), String> {
    vector_core::db::init_database(npub)
}

/// Persist `npub` as the account that should be active on the next boot.
/// Frontend pairs this with `swap_session` to perform a clean account switch.
/// Validates that the account exists locally; returns an error otherwise.
#[tauri::command]
pub fn set_active_account<R: Runtime>(
    handle: AppHandle<R>,
    npub: String,
) -> Result<(), String> {
    let accounts = list_accounts(&handle)?;
    if !accounts.iter().any(|a| a == &npub) {
        return Err(format!("Unknown account: {}", npub));
    }
    vector_core::db::write_active_account_file(&npub)
}

/// Drop the active-account marker without resetting the session. Used by the
/// Add Profile flow: the frontend wants the next boot to fall through to the
/// "no account selected" path so it can show the login-start screen (without
/// touching any account data on disk).
#[tauri::command]
pub fn clear_active_account() -> Result<(), String> {
    vector_core::db::clear_active_account_file()
}

/// Commit point of the Add Profile flow. The frontend keeps the current
/// account alive while the user is just browsing the login-start screen
/// (so the Back button can be a free, instant UI restore). Only when they
/// actually click Create Account or Login do we tear down the existing
/// session — a fresh `Keys` install would otherwise be silently rejected
/// by the lock-and-check guards in `login` / `create_account`.
///
/// Differs from `swap_session` in that it does NOT emit `session_reload` —
/// the frontend stays on the same document and proceeds straight into
/// account creation/import.
#[tauri::command]
pub async fn enter_add_account_mode() -> Result<(), String> {
    refuse_if_migration_in_progress("add a new account")?;
    let _ = vector_core::db::clear_active_account_file();
    reset_session().await;
    Ok(())
}

/// Tear down the entire session in-process and notify the frontend to reload.
/// After the reload the boot flow reads `<app_data>/active_account` and logs
/// into whichever account the marker points at, exactly as if the OS process
/// had restarted.
///
/// This avoids `app.restart()` because that API is unreliable on Android
/// (battery-saver / Activity lifecycle quirks) and forbidden on iOS (Apple
/// guidelines). One universal path everywhere is the right tradeoff.
#[tauri::command]
pub async fn swap_session<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    use tauri::Emitter;

    refuse_if_migration_in_progress("switch accounts")?;
    reset_session().await;

    // Tell the frontend to reload now that backend is in a fresh-boot state.
    // Reload is total: a new `index.html` document with all module-level state
    // reinitialized, equivalent to a process restart from the user's POV.
    let _ = handle.emit("session_reload", ());
    Ok(())
}

/// Refuses the requested operation when an encryption migration
/// (encrypt / decrypt / rekey) is mid-flight.
///
/// Tearing down the DB pool while a transaction is open would either commit
/// a partially-migrated row (data loss) or leave `migration_state` stuck in
/// `encrypting`/`decrypting` on the next boot. Every entry point that calls
/// `reset_session()` should gate through this first so the user gets a
/// clear "try again in a moment" error rather than a corrupted account.
///
/// `op_label` is interpolated into the message ("Cannot {op_label} while
/// encryption migration is in progress.")
pub fn refuse_if_migration_in_progress(op_label: &str) -> Result<(), String> {
    if crate::commands::encryption::MIGRATION_IN_PROGRESS
        .load(std::sync::atomic::Ordering::Acquire)
    {
        Err(format!(
            "Cannot {} while encryption migration is in progress. Try again in a moment.",
            op_label,
        ))
    } else {
        Ok(())
    }
}

// Top-level mutex ensuring that concurrent invocations of `reset_session()`
// serialize. Without this, a `logout` + `swap_session` racing pair (or any
// two destructive entry points dispatched in parallel) could interleave:
// thread A wins `take_nostr_client()` and starts `client.shutdown().await`
// while thread B proceeds past the take (sees None) and calls
// `close_database()` before A's relay-shutdown writes have finished. The
// generation gate masks most of the resulting damage, but explicit
// serialization is the defensible contract.
static RESET_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Tear down every per-session global so the backend looks like a freshly
/// launched process. Order matters:
///   1. Take and shut down the Nostr client BEFORE clearing other state, so
///      relay subscriptions detach cleanly and concurrent readers see None.
///   2. Stop Tor BEFORE closing the DB pool — Tor's last write may target the
///      per-account `tor/` cache and we want it to land on the right account.
///   3. Close the DB pool BEFORE clearing CURRENT_ACCOUNT so writes can't slip
///      in against a now-stale path.
///   4. Wipe key vaults, in-memory state, ID caches, and the FULL_SESSION flag.
///
/// Background tasks that captured `SessionGuard` at spawn time will see
/// their generation become invalid on the very first line of this function
/// and short-circuit before any side-effect. Tasks that did NOT capture a
/// guard rely on `NOSTR_CLIENT` going `None` during the reset window — which
/// works for the gap, but leaks across the reset/init boundary because the
/// new account installs a fresh client. Prefer the guard pattern for any new
/// spawn site.
pub async fn reset_session() {
    // Serialize all concurrent reset attempts behind a single mutex. The
    // mutex is per-process and lives forever; contention is rare (each
    // entry point is user-initiated UI) so this has no measurable cost.
    let _reset_lock = RESET_LOCK.lock().await;

    // FIRST: advance the session generation so every background task with a
    // captured `SessionGuard` becomes invalid before any teardown begins.
    // Tasks that wake up mid-reset see an invalid guard and exit instead of
    // writing partially-cleared state.
    vector_core::state::bump_session_generation();

    if let Some(client) = vector_core::take_nostr_client() {
        let _ = client.shutdown().await;
    }

    crate::commands::tor::stop_and_join_if_running().await;

    close_db_connection();

    // Cryptographic material — clear vaults and zeroize transient secrets.
    crate::ENCRYPTION_KEY.clear(&[&crate::MY_SECRET_KEY]);
    crate::MY_SECRET_KEY.clear(&[&crate::ENCRYPTION_KEY]);
    {
        use zeroize::Zeroize;
        if let Ok(mut g) = crate::MNEMONIC_SEED.lock() {
            if let Some(s) = g.as_mut() { s.zeroize(); }
            *g = None;
        }
        if let Ok(mut g) = crate::PENDING_NSEC.lock() {
            if let Some(s) = g.as_mut() { s.zeroize(); }
            *g = None;
        }
    }

    // NIP-46 bunker handle + client keypair vault. Drain first (clears state
    // under the lock atomically), then shut the connection down outside the
    // lock so its relay pool drains without blocking new-account install.
    if let Some(bunker) = vector_core::drain_bunker_state() {
        let _ = bunker.shutdown().await;
    }
    // Staged bunker metadata between connect → setup_encryption. Clearing
    // here defends against a swap that interrupts a half-completed bunker
    // login from leaking the previous bunker URL into the next account's
    // setup_encryption call.
    vector_core::clear_pending_bunker_setup();

    // In-memory state owned by vector-core's globals.
    {
        let mut state = crate::STATE.lock().await;
        state.profiles.clear();
        state.chats.clear();
        state.db_loaded = false;
        state.is_syncing = false;
    }
    { crate::WRAPPER_ID_CACHE.lock().await.clear(); }
    { crate::NOTIFIED_WELCOMES.lock().await.clear(); }
    { crate::state::PENDING_EVENTS.lock().await.clear(); }

    // Profile sync queue (long-lived processor loop services this queue
    // forever, so we drain it instead of cancelling the task).
    vector_core::profile::sync::clear_profile_sync_queue();

    // MLS subscription pointer — the new account will install its own.
    { *crate::services::subscription_handler::MLS_SUB_ID.lock().await = None; }

    // Pending invite captured during account creation (must NOT auto-execute
    // on the next account).
    crate::state::clear_pending_invite();

    // Per-session caches that hold message/file content or relay diagnostics.
    if let Ok(mut m) = crate::commands::relays::RELAY_METRICS.write() { m.clear(); }
    if let Ok(mut l) = crate::commands::relays::RELAY_LOGS.write() { l.clear(); }
    // Allow `monitor_relay_connections` to spawn a fresh subscriber against
    // the next session's client. Without this reset the frontend's relay
    // status UI freezes after the swap.
    crate::commands::relays::MONITOR_STARTED
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Custom Blossom upload servers — reset to the default list so account B
    // doesn't inherit account A's self-hosted upload destination silently.
    if let Some(servers) = vector_core::state::BLOSSOM_SERVERS.get() {
        if let Ok(mut s) = servers.lock() {
            *s = vector_core::state::init_blossom_servers();
        }
    }

    // Active Tor bridge socket addresses — stale after the old TorService
    // shut down. `current_circuit_hops` reads these to mark the Guard hop
    // as "via bridge"; without this clear it would lie about the new
    // account's circuit until Tor reboots and repopulates the slot.
    #[cfg(feature = "tor")]
    vector_core::tor::clear_active_bridge_addrs();

    // Mid-flight voice recording — drop the buffer AND any stashed
    // "pending" recording so a voice note prepared for account A doesn't
    // surface in account B's compose box.
    if let Some(rec) = crate::voice::RECORDER.get() {
        rec.cancel();
    }

    // MLS group failure counters and per-group sync locks — per-account
    // state that must NOT carry into the next session. Failure counters
    // (desync-detection threshold) keyed by group id could trigger
    // spurious desync detection on account B if the same id reappears.
    // Sync locks are just allocations but pile up across sessions.
    vector_core::mls::types::clear_group_failure_counts().await;
    vector_core::mls::service::clear_group_sync_locks();

    // Recipient relay-list cache — recipient-keyed, so technically
    // account-agnostic, but holds privacy-adjacent metadata about every
    // contact account A messaged. Drop on swap.
    vector_core::inbox_relays::clear_inbox_relay_cache();

    // PIVX address→balance cache — addresses derive from user keys, so
    // a cached entry from account A is meaningless (and slightly
    // privacy-leaky) under account B's session.
    crate::pivx::clear_balance_cache();

    // Miniapp marketplace state — install status / version info come from
    // the per-account DB, so the previous account's view would paint
    // stale "installed" indicators on the new account's marketplace UI.
    {
        let mut state = crate::miniapps::marketplace::MARKETPLACE_STATE.write().await;
        state.clear_for_session_reset();
    }

    // vector-core's MLS subscription pointer (separate from the
    // subscription_handler one cleared above). Carrying it across a swap
    // would mean the new account's `listen()` references a sub id that
    // belonged to account A's relay pool.
    vector_core::clear_mls_sub_id().await;

    if let Ok(mut u) = crate::message::upload_cancel_flags().lock() {
        for flag in u.values() {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
        u.clear();
    }
    crate::message::clear_all_message_caches().await;
    { crate::commands::attachments::ACTIVE_DOWNLOADS.lock().await.clear(); }
    { crate::image_cache::DOWNLOADS_IN_PROGRESS.lock().await.clear(); }

    // Identity caches.
    vector_core::db::clear_current_account_in_memory();
    vector_core::clear_my_public_key();
    crate::db::clear_id_caches();
    let _ = clear_pending_account();

    crate::commands::account::FULL_SESSION_INITIALIZED
        .store(false, std::sync::atomic::Ordering::Release);

    // ORDER MATTERS: clear the encryption-enabled atomic BEFORE reopening
    // the processing gate. If we opened the gate first, a background event
    // handler waking up in the gap between these two lines would read a
    // stale `true` and attempt to decrypt account-A's payload with an
    // already-cleared ENCRYPTION_KEY — burning log noise and re-trying
    // poisoned events. Clearing the flag first means any handler that
    // observes "gate open" simultaneously sees "no encryption".
    crate::state::set_encryption_enabled(false);
    crate::state::open_processing_gate();
}

/// Permanently delete an account: removes its data directory recursively.
/// If the deleted account is active, runs `reset_session()` first to
/// release file handles (required on Windows for `remove_dir_all` to
/// succeed). Returns `true` when the deleted account was active so the
/// frontend can issue `swap_session` to land on a remaining account.
///
/// When this delete leaves zero accounts on disk, ALSO wipes the shared
/// downloads dir and the legacy `<app_data>/mls/` folder — see the
/// last-account cascade below.
#[tauri::command]
pub async fn delete_account<R: Runtime>(
    handle: AppHandle<R>,
    npub: String,
) -> Result<bool, String> {
    // Validate via list_accounts (checks pkey row) so we can't delete a
    // phantom directory.
    if !list_accounts(&handle)?.iter().any(|a| a == &npub) {
        return Err(format!("Unknown account: {}", npub));
    }

    let was_active = matches!(get_current_account(), Ok(active) if active == npub);

    if was_active {
        // Non-active deletes don't touch the active DB, so the migration
        // gate only applies when deleting our own session.
        refuse_if_migration_in_progress("delete the active account")?;
        let _ = vector_core::db::clear_active_account_file();
        reset_session().await;
    } else if let Some(marker) = vector_core::db::read_active_account_file()? {
        if marker == npub {
            let _ = vector_core::db::clear_active_account_file();
        }
    }

    let dir = vector_core::db::account_dir(&npub)?;
    if dir.exists() {
        // Tolerate NotFound — a concurrent delete on the same npub may
        // have already removed the directory between our exists() and
        // remove. The caller's invariant ("the account is gone") still
        // holds.
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Failed to remove account directory: {}", e)),
        }
    }

    // LAST-ACCOUNT CASCADE: when this delete leaves zero accounts on disk,
    // wipe shared caches no remaining account needs — the downloads dir
    // (shared across every Vector account on the device) and the legacy
    // pre-multi-account MLS folder.
    //
    // Gate via `list_account_npubs()` (raw filesystem inventory) — NOT
    // `list_accounts` (which adds a pkey probe). The validated count can
    // transiently report zero when another account's DB is AV-locked /
    // SQLite-busy, and wiping downloads in that case would destroy
    // attachments referenced by the still-present-but-unreadable account.
    let remaining = vector_core::db::list_account_npubs()
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if remaining == 0 {
        let downloads = vector_core::db::get_download_dir();
        if downloads.exists() {
            let _ = std::fs::remove_dir_all(&downloads);
        }
        if let Ok(mls_dir) = handle.path().resolve("mls", tauri::path::BaseDirectory::AppData) {
            if mls_dir.exists() {
                let _ = std::fs::remove_dir_all(&mls_dir);
            }
        }
        println!("[delete_account] Last account removed — wiped shared downloads + legacy mls dirs.");
    }

    Ok(was_active)
}

#[cfg(test)]
mod pick_account_tests {
    use super::pick_account;

    const A: &str = "npub16ye7evyevwnl0fc9hujsxf9zym72e063awn0pvde0huvpyec5nyq4dg4wn";
    const B: &str = "npub12w73tzcqgpr2pcy4el5x60d2emeud4cyeeayynzqgg2fefzgytaqm4ktz3";
    const STALE: &str = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";

    #[test]
    fn empty_account_list_yields_none() {
        assert_eq!(pick_account(&[], None), None);
        assert_eq!(pick_account(&[], Some(A)), None);
    }

    #[test]
    fn marker_hit_wins_over_single_account_fallback() {
        let accounts = vec![A.to_string(), B.to_string()];
        assert_eq!(pick_account(&accounts, Some(B)), Some(B.to_string()));
    }

    #[test]
    fn stale_marker_with_single_account_falls_back_to_that_account() {
        let accounts = vec![A.to_string()];
        // Marker pointed at a deleted account, but we only have one left;
        // the single-account fallback should still kick in.
        assert_eq!(pick_account(&accounts, Some(STALE)), Some(A.to_string()));
    }

    #[test]
    fn stale_marker_with_multiple_accounts_yields_none() {
        // Multiple accounts + bad marker → picker, no silent auto-pick.
        let accounts = vec![A.to_string(), B.to_string()];
        assert_eq!(pick_account(&accounts, Some(STALE)), None);
    }

    #[test]
    fn no_marker_with_single_account_auto_selects() {
        let accounts = vec![A.to_string()];
        assert_eq!(pick_account(&accounts, None), Some(A.to_string()));
    }

    #[test]
    fn no_marker_with_multiple_accounts_yields_none() {
        let accounts = vec![A.to_string(), B.to_string()];
        assert_eq!(pick_account(&accounts, None), None);
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use tempfile::TempDir;

    const NPUB: &str = "npub16ye7evyevwnl0fc9hujsxf9zym72e063awn0pvde0huvpyec5nyq4dg4wn";
    const OTHER_NPUB: &str = "npub12w73tzcqgpr2pcy4el5x60d2emeud4cyeeayynzqgg2fefzgytaqm4ktz3";

    /// Build a vector.db with the minimum schema the metadata reader touches.
    /// Avoids pulling the full schema string just to keep tests focused on
    /// what `read_account_metadata_at` actually queries.
    fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("vector.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE profiles (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                npub TEXT UNIQUE NOT NULL,
                name TEXT NOT NULL DEFAULT '',
                display_name TEXT NOT NULL DEFAULT '',
                nickname TEXT NOT NULL DEFAULT '',
                avatar TEXT NOT NULL DEFAULT '',
                avatar_cached TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL DEFAULT ''
            );
        "#).unwrap();
        path
    }

    #[test]
    fn missing_db_returns_minimal_record() {
        let tmp = TempDir::new().unwrap();
        let meta = read_account_metadata_at(&tmp.path().join("nope.db"), NPUB);
        assert_eq!(meta.npub, NPUB);
        assert_eq!(meta.display_name, None);
        assert_eq!(meta.avatar_url, None);
        assert_eq!(meta.avatar_cached, None);
        assert!(!meta.has_encryption);
        assert_eq!(meta.last_active, None);
    }

    #[test]
    fn corrupt_db_does_not_panic() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("vector.db");
        std::fs::write(&path, b"this is not a sqlite database").unwrap();
        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.npub, NPUB);
        assert!(meta.display_name.is_none());
    }

    #[test]
    fn nickname_wins_over_display_name_and_name() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name, nickname) VALUES (?1, 'low', 'mid', 'top')",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.display_name, Some("top".to_string()));
    }

    #[test]
    fn display_name_used_when_nickname_blank() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name, nickname) VALUES (?1, 'low', 'mid', '')",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.display_name, Some("mid".to_string()));
    }

    #[test]
    fn name_used_when_higher_priority_fields_blank() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name, nickname) VALUES (?1, 'low', '', '')",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.display_name, Some("low".to_string()));
    }

    #[test]
    fn all_blank_name_fields_yield_none() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub) VALUES (?1)",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.display_name, None);
    }

    #[test]
    fn empty_avatar_strings_become_none() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, avatar, avatar_cached) VALUES (?1, '', '')",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.avatar_url, None);
        assert_eq!(meta.avatar_cached, None);
    }

    #[test]
    fn populated_avatar_fields_pass_through() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, avatar, avatar_cached) VALUES (?1, 'https://x/a.png', '/cache/a.png')",
            rusqlite::params![NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.avatar_url.as_deref(), Some("https://x/a.png"));
        assert_eq!(meta.avatar_cached.as_deref(), Some("/cache/a.png"));
    }

    #[test]
    fn encryption_flag_handles_true_and_one() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();

        for value in &["true", "1"] {
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', ?1)",
                rusqlite::params![value],
            ).unwrap();
            let meta = read_account_metadata_at(&path, NPUB);
            assert!(meta.has_encryption, "value={} should yield true", value);
        }
    }

    #[test]
    fn encryption_flag_false_or_absent_is_not_encrypted() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();

        // Absent row.
        let meta = read_account_metadata_at(&path, NPUB);
        assert!(!meta.has_encryption);

        // Explicit 'false'.
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('encryption_enabled', 'false')",
            [],
        ).unwrap();
        let meta = read_account_metadata_at(&path, NPUB);
        assert!(!meta.has_encryption);
    }

    #[test]
    fn last_active_parsed_when_numeric() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('last_active', '1730000000')",
            [],
        ).unwrap();
        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.last_active, Some(1730000000));
    }

    #[test]
    fn last_active_ignored_when_garbage() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('last_active', 'not-a-number')",
            [],
        ).unwrap();
        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.last_active, None);
    }

    #[test]
    fn metadata_keyed_by_npub_does_not_leak_other_profiles() {
        let tmp = TempDir::new().unwrap();
        let path = make_db(tmp.path());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name) VALUES (?1, 'self')",
            rusqlite::params![NPUB],
        ).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name) VALUES (?1, 'someone-else')",
            rusqlite::params![OTHER_NPUB],
        ).unwrap();

        let meta = read_account_metadata_at(&path, NPUB);
        assert_eq!(meta.display_name, Some("self".to_string()));
    }
}

