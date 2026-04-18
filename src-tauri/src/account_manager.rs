use std::path::PathBuf;
use std::sync::{Arc, RwLock, LazyLock};
use tauri::{AppHandle, Runtime, Manager};

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

/// List all existing accounts by scanning directories
///
/// Returns: Vec of full npubs that have valid pkeys (not just directories)
/// Also cleans up invalid account directories without pkeys
pub fn list_accounts<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let mut accounts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // Check if it looks like an npub directory
                    if name.starts_with("npub1") {
                        // Validate that this account has a valid pkey in its database
                        if let Ok(has_pkey) = account_has_valid_pkey(handle, name) {
                            if has_pkey {
                                accounts.push(name.to_string());
                            } else {
                                // Clean up invalid account directory
                                let invalid_dir = entry.path();
                                if let Err(e) = std::fs::remove_dir_all(&invalid_dir) {
                                    eprintln!("[Account Manager] Failed to remove invalid account directory {}: {}", invalid_dir.display(), e);
                                } else {
                                    println!("[Account Manager] Cleaned up invalid account directory: {}", invalid_dir.display());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(accounts)
}

/// Check if an account has a valid pkey in its database
fn account_has_valid_pkey<R: Runtime>(handle: &AppHandle<R>, npub: &str) -> Result<bool, String> {
    // Try to get database connection for this account
    let db_path = get_database_path(handle, npub)?;

    // Check if database file exists
    if !db_path.exists() {
        return Ok(false);
    }

    // Try to open database connection
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

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

/// Auto-select the first available account if none is currently selected.
/// Also ensures the database schema and migrations are up-to-date before
/// any other code (background tasks, frontend IPC) can access the DB.
pub fn auto_select_account<R: Runtime>(handle: &AppHandle<R>) -> Result<Option<String>, String> {
    // Check if an account is already selected
    if let Ok(current) = get_current_account() {
        ensure_schema_ready(handle, &current)?;
        return Ok(Some(current));
    }

    // No account selected, try to find one
    let accounts = list_accounts(handle)?;

    if accounts.is_empty() {
        return Ok(None);
    }

    // Select the first account
    let first_account = accounts[0].clone();
    set_current_account(first_account.clone())?;

    // Ensure schema + migrations complete BEFORE anything else touches the DB.
    // This runs synchronously in Tauri setup(), so background tasks spawned
    // after this point always see a fully migrated schema.
    ensure_schema_ready(handle, &first_account)?;

    Ok(Some(first_account))
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

/// Switch to a different account
#[tauri::command]
pub async fn switch_account<R: Runtime>(
    handle: AppHandle<R>,
    npub: String
) -> Result<(), String> {
    if !npub.starts_with("npub1") {
        return Err(format!("Invalid npub format: {}", npub));
    }

    println!("[Account Manager] Switching to account: {}", npub);

    // Initialize database for this profile
    init_profile_database(&handle, &npub).await?;

    // Update current account
    set_current_account(npub.clone())?;

    // Clear old account's ID caches and preload new account's caches
    crate::db::clear_id_caches();
    if let Err(e) = crate::db::preload_id_caches().await {
        eprintln!("[Account Manager] Failed to preload ID caches: {}", e);
    }

    // Update MLS directory
    if let Ok(mls_dir) = vector_core::mls::get_mls_directory() {
        println!("[Account Manager] MLS directory: {}", mls_dir.display());
    }

    Ok(())
}
