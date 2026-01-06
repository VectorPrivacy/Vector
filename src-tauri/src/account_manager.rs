use std::path::PathBuf;
use std::sync::{Arc, RwLock, Mutex};
use lazy_static::lazy_static;
use tauri::{AppHandle, Runtime, Manager};

lazy_static! {
    /// Global state tracking the currently active account (npub)
    static ref CURRENT_ACCOUNT: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    /// Pending account waiting for encryption (npub stored before database creation)
    static ref PENDING_ACCOUNT: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    /// Persistent database connection pool (one per account)
    /// Keeps connection open to avoid repeated open/close overhead
    static ref DB_CONNECTION_POOL: Arc<Mutex<Option<(String, rusqlite::Connection)>>> =
        Arc::new(Mutex::new(None));
}

/// SQL Schema for Vector database
///
/// This schema uses selective encryption:
/// - Encrypted: message content, private keys, seed phrases, MLS secrets
/// - Plaintext: timestamps, IDs, metadata, profiles (for indexing and performance)
pub const SQL_SCHEMA: &str = r#"
-- Profiles table (plaintext - public data)
CREATE TABLE IF NOT EXISTS profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    npub TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    nickname TEXT NOT NULL DEFAULT '',
    lud06 TEXT NOT NULL DEFAULT '',
    lud16 TEXT NOT NULL DEFAULT '',
    banner TEXT NOT NULL DEFAULT '',
    avatar TEXT NOT NULL DEFAULT '',
    about TEXT NOT NULL DEFAULT '',
    website TEXT NOT NULL DEFAULT '',
    nip05 TEXT NOT NULL DEFAULT '',
    status_content TEXT NOT NULL DEFAULT '',
    status_url TEXT NOT NULL DEFAULT '',
    muted INTEGER NOT NULL DEFAULT 0,
    bot INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_profiles_npub ON profiles(npub);
CREATE INDEX IF NOT EXISTS idx_profiles_name ON profiles(name);

-- Chats table (plaintext - metadata)
CREATE TABLE IF NOT EXISTS chats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_identifier TEXT UNIQUE NOT NULL,
    chat_type INTEGER NOT NULL,
    participants TEXT NOT NULL,
    last_read TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    muted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_chats_identifier ON chats(chat_identifier);
CREATE INDEX IF NOT EXISTS idx_chats_created ON chats(created_at DESC);

-- Messages table (content encrypted, metadata plaintext)
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    chat_id INTEGER NOT NULL,
    content_encrypted TEXT NOT NULL,
    replied_to TEXT NOT NULL DEFAULT '',
    preview_metadata TEXT,
    attachments TEXT NOT NULL DEFAULT '[]',
    reactions TEXT NOT NULL DEFAULT '[]',
    at INTEGER NOT NULL,
    mine INTEGER NOT NULL,
    user_id INTEGER,
    wrapper_event_id TEXT,
    FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES profiles(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, at);
CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_user ON messages(user_id);
CREATE INDEX IF NOT EXISTS idx_messages_wrapper ON messages(wrapper_event_id);

-- Settings table (key-value pairs)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- MLS Groups table
CREATE TABLE IF NOT EXISTS mls_groups (
    group_id TEXT PRIMARY KEY,
    engine_group_id TEXT NOT NULL DEFAULT '',
    creator_pubkey TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    avatar_ref TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    evicted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_mls_groups_evicted_updated ON mls_groups(evicted, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_mls_groups_creator ON mls_groups(creator_pubkey);

-- MLS Key Packages table
CREATE TABLE IF NOT EXISTS mls_keypackages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_pubkey TEXT NOT NULL,
    device_id TEXT NOT NULL,
    keypackage_ref TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_keypackages_owner ON mls_keypackages(owner_pubkey);

-- MLS Event Cursors table
CREATE TABLE IF NOT EXISTS mls_event_cursors (
    group_id TEXT PRIMARY KEY,
    last_seen_event_id TEXT NOT NULL,
    last_seen_at INTEGER NOT NULL
);

-- Mini Apps history table (tracks recently used Mini Apps)
CREATE TABLE IF NOT EXISTS miniapps_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    src_url TEXT NOT NULL,
    attachment_ref TEXT NOT NULL,
    open_count INTEGER NOT NULL DEFAULT 1,
    last_opened_at INTEGER NOT NULL,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    categories TEXT NOT NULL DEFAULT '',
    marketplace_id TEXT DEFAULT NULL
);
"#;

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

/// Get the MLS directory for a given npub
///
/// Returns: AppData/npub1qwerty.../mls/
pub fn get_mls_directory<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str
) -> Result<PathBuf, String> {
    let profile_dir = get_profile_directory(handle, npub)?;
    let mls_dir = profile_dir.join("mls");

    if !mls_dir.exists() {
        std::fs::create_dir_all(&mls_dir)
            .map_err(|e| format!("Failed to create MLS directory: {}", e))?;
        println!("[Account Manager] Created MLS directory: {}", mls_dir.display());
    }

    Ok(mls_dir)
}

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
    CURRENT_ACCOUNT.read()
        .map_err(|e| format!("Failed to read current account: {}", e))?
        .clone()
        .ok_or_else(|| "No account selected".to_string())
}

/// Auto-select the first available account if none is currently selected
/// This is useful when an account exists but isn't selected yet
pub fn auto_select_account<R: Runtime>(handle: &AppHandle<R>) -> Result<Option<String>, String> {
    // Check if an account is already selected
    if let Ok(current) = get_current_account() {
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

    Ok(Some(first_account))
}

/// Set the currently active account
pub fn set_current_account(npub: String) -> Result<(), String> {
    *CURRENT_ACCOUNT.write()
        .map_err(|e| format!("Failed to write current account: {}", e))? = Some(npub.clone());

    // Close old connection when switching accounts
    close_db_connection();

    Ok(())
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

/// Get or reuse database connection for the current account
/// This keeps the connection open to avoid repeated open/close overhead
pub fn get_db_connection<R: Runtime>(handle: &AppHandle<R>) -> Result<rusqlite::Connection, String> {
    let npub = get_current_account()?;

    // Try to reuse existing connection
    let mut pool = DB_CONNECTION_POOL.lock().unwrap();

    if let Some((cached_npub, _)) = pool.as_ref() {
        if cached_npub == &npub {
            // Same account, take the connection out
            if let Some((_, conn)) = pool.take() {
                return Ok(conn);
            }
        } else {
            // Different account, close old connection
            *pool = None;
        }
    }

    // Open new connection
    let db_path = get_database_path(handle, &npub)?;
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Enable WAL mode for better concurrency
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(|e| format!("Failed to set WAL mode: {}", e))?;

    // Run migrations to ensure schema is up to date
    // This is important for existing databases that may not have new columns
    run_migrations(&conn)?;

    Ok(conn)
}

/// Return connection to the pool for reuse
pub fn return_db_connection(conn: rusqlite::Connection) {
    if let Ok(npub) = get_current_account() {
        let mut pool = DB_CONNECTION_POOL.lock().unwrap();
        *pool = Some((npub, conn));
    }
}

/// Close the current database connection (e.g., when switching accounts)
pub fn close_db_connection() {
    let mut pool = DB_CONNECTION_POOL.lock().unwrap();
    *pool = None;
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

/// Initialize SQL database for a specific profile
/// Creates all tables if they don't exist
pub async fn init_profile_database<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str
) -> Result<(), String> {
    let db_path = get_database_path(handle, npub)?;
    println!("[Account Manager] Initializing database: {}", db_path.display());

    // Create the database directory if it doesn't exist
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create database directory: {}", e))?;
    }

    // Open connection and create schema
    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Execute the schema to create all tables
    conn.execute_batch(SQL_SCHEMA)
        .map_err(|e| format!("Failed to create database schema: {}", e))?;

    // Run migrations for existing databases
    run_migrations(&conn)?;

    println!("[Account Manager] Database schema created successfully");

    Ok(())
}

/// Run database migrations for schema updates
/// This handles adding new columns to existing tables
fn run_migrations(conn: &rusqlite::Connection) -> Result<(), String> {
    // Migration 1: Add wrapper_event_id column to messages table
    // This column stores the public giftwrap event ID for fast duplicate detection
    let has_wrapper_column: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'wrapper_event_id'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_wrapper_column {
        println!("[Migration] Adding wrapper_event_id column to messages table...");
        conn.execute(
            "ALTER TABLE messages ADD COLUMN wrapper_event_id TEXT",
            []
        ).map_err(|e| format!("Failed to add wrapper_event_id column: {}", e))?;

        // Create index for fast lookups
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_wrapper ON messages(wrapper_event_id)",
            []
        ).map_err(|e| format!("Failed to create wrapper_event_id index: {}", e))?;

        println!("[Migration] wrapper_event_id column added successfully");
    }

    // Migration 2: Create miniapps_history table if it doesn't exist
    let has_miniapps_history: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='miniapps_history'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_miniapps_history {
        println!("[Migration] Creating miniapps_history table...");
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS miniapps_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                src_url TEXT NOT NULL,
                attachment_ref TEXT,
                open_count INTEGER DEFAULT 1,
                last_opened_at INTEGER NOT NULL,
                is_favorite INTEGER NOT NULL DEFAULT 0,
                categories TEXT NOT NULL DEFAULT '',
                marketplace_id TEXT DEFAULT NULL
            )
            "#,
            []
        ).map_err(|e| format!("Failed to create miniapps_history table: {}", e))?;

        println!("[Migration] miniapps_history table created successfully");
    }

    // Migration 3: Add installed_version column to miniapps_history if it doesn't exist
    let has_installed_version: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('miniapps_history') WHERE name='installed_version'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_installed_version {
        println!("[Migration] Adding installed_version column to miniapps_history table...");
        conn.execute(
            "ALTER TABLE miniapps_history ADD COLUMN installed_version TEXT DEFAULT NULL",
            []
        ).map_err(|e| format!("Failed to add installed_version column: {}", e))?;

        println!("[Migration] installed_version column added successfully");
    }

    Ok(())
}

/// Switch to a different account
#[tauri::command]
pub async fn switch_account<R: Runtime>(
    handle: AppHandle<R>,
    npub: String
) -> Result<(), String> {
    // Validate npub
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
    if let Err(e) = crate::db::preload_id_caches(&handle).await {
        eprintln!("[Account Manager] Failed to preload ID caches: {}", e);
    }

    // Update MLS directory
    let mls_dir = get_mls_directory(&handle, &npub)?;
    println!("[Account Manager] MLS directory: {}", mls_dir.display());

    // TODO: Update MLS configuration to use new directory
    // This will be done when we update the MLS module

    Ok(())
}