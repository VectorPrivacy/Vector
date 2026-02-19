use std::path::PathBuf;
use std::sync::{Arc, RwLock, Mutex, OnceLock};
use std::ops::{Deref, DerefMut};
use lazy_static::lazy_static;
use tauri::{AppHandle, Runtime, Manager};

// ============================================================================
// Static App Data Directory (headless-safe — no AppHandle required)
// ============================================================================

/// App data directory, set once at startup (Tauri setup) or by background service.
/// All DB/MLS path resolution can use this instead of `handle.path().app_data_dir()`.
static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set the app data directory (call once at Tauri startup or background service init).
pub fn set_app_data_dir(path: PathBuf) {
    let _ = APP_DATA_DIR.set(path);
}

/// Get the app data directory (headless-safe).
pub fn get_app_data_dir() -> Result<&'static PathBuf, String> {
    APP_DATA_DIR.get().ok_or_else(|| "App data directory not initialized".to_string())
}

lazy_static! {
    /// Global state tracking the currently active account (npub)
    static ref CURRENT_ACCOUNT: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    /// Pending account waiting for encryption (npub stored before database creation)
    static ref PENDING_ACCOUNT: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    /// Read connection pool — multiple connections for parallel reads (WAL mode).
    /// Pre-warmed at login so boot queries get instant connections.
    static ref DB_READ_POOL: Arc<Mutex<Vec<rusqlite::Connection>>> =
        Arc::new(Mutex::new(Vec::new()));

    /// Single write connection — all writes go through this to avoid SQLITE_BUSY.
    /// Protected by Mutex so only one write operation runs at a time.
    static ref DB_WRITE_CONN: Arc<Mutex<Option<rusqlite::Connection>>> =
        Arc::new(Mutex::new(None));
}

/// RAII guard for READ connections — auto-returns to the read pool on drop.
pub struct ConnectionGuard {
    conn: Option<rusqlite::Connection>,
}

impl ConnectionGuard {
    fn new(conn: rusqlite::Connection) -> Self {
        Self { conn: Some(conn) }
    }
}

impl Deref for ConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("Connection already taken")
    }
}

impl DerefMut for ConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().expect("Connection already taken")
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            return_db_connection(conn);
        }
    }
}

/// RAII guard for the WRITE connection — auto-returns to the write slot on drop.
pub struct WriteConnectionGuard {
    conn: Option<rusqlite::Connection>,
}

impl WriteConnectionGuard {
    fn new(conn: rusqlite::Connection) -> Self {
        Self { conn: Some(conn) }
    }
}

impl Deref for WriteConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("Write connection already taken")
    }
}

impl DerefMut for WriteConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().expect("Write connection already taken")
    }
}

impl Drop for WriteConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            return_write_connection(conn);
        }
    }
}

/// Get a READ connection wrapped in an RAII guard (auto-returned to read pool on drop).
pub fn get_db_connection_guard<R: Runtime>(handle: &AppHandle<R>) -> Result<ConnectionGuard, String> {
    let conn = get_db_connection(handle)?;
    Ok(ConnectionGuard::new(conn))
}

/// Get the WRITE connection wrapped in an RAII guard (auto-returned on drop).
pub fn get_write_connection_guard<R: Runtime>(handle: &AppHandle<R>) -> Result<WriteConnectionGuard, String> {
    let conn = get_write_connection(handle)?;
    Ok(WriteConnectionGuard::new(conn))
}

// ============================================================================
// Static Connection Guards (headless-safe — no AppHandle required)
// ============================================================================

/// Get a READ connection guard using static APP_DATA_DIR (no AppHandle needed).
pub fn get_db_connection_guard_static() -> Result<ConnectionGuard, String> {
    let conn = get_db_connection_static()?;
    Ok(ConnectionGuard::new(conn))
}

/// Get the WRITE connection guard using static APP_DATA_DIR (no AppHandle needed).
pub fn get_write_connection_guard_static() -> Result<WriteConnectionGuard, String> {
    let conn = get_write_connection_static()?;
    Ok(WriteConnectionGuard::new(conn))
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
    bot INTEGER NOT NULL DEFAULT 0,
    avatar_cached TEXT NOT NULL DEFAULT '',
    banner_cached TEXT NOT NULL DEFAULT ''
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
    description TEXT,
    avatar_ref TEXT,
    avatar_cached TEXT,
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
    created_at INTEGER,
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

-- Events table: flat, protocol-aligned storage for all Nostr events
-- Every event (message, reaction, attachment, etc.) is a separate row
-- This is the PRIMARY storage for all message data
CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    kind INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    user_id INTEGER,
    content TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '[]',
    reference_id TEXT,
    created_at INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    mine INTEGER NOT NULL DEFAULT 0,
    pending INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    wrapper_event_id TEXT,
    npub TEXT,
    preview_metadata TEXT,
    FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES profiles(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_events_chat_time ON events(chat_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS idx_events_reference ON events(reference_id) WHERE reference_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_wrapper ON events(wrapper_event_id) WHERE wrapper_event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_user ON events(user_id);

-- PIVX Promos table (for addressless PIVX payments via promo codes)
CREATE TABLE IF NOT EXISTS pivx_promos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    gift_code TEXT NOT NULL UNIQUE,
    address TEXT NOT NULL,
    privkey_encrypted TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    claimed_at INTEGER,
    amount_piv REAL,
    status TEXT NOT NULL DEFAULT 'active'
);
CREATE INDEX IF NOT EXISTS idx_pivx_promos_code ON pivx_promos(gift_code);
CREATE INDEX IF NOT EXISTS idx_pivx_promos_address ON pivx_promos(address);
CREATE INDEX IF NOT EXISTS idx_pivx_promos_status ON pivx_promos(status);
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

// ============================================================================
// Static Path Helpers (headless-safe — no AppHandle required)
// ============================================================================

/// Get the profile directory using static APP_DATA_DIR (no AppHandle needed).
pub fn get_profile_directory_static(npub: &str) -> Result<PathBuf, String> {
    let app_data = get_app_data_dir()?;

    if !npub.starts_with("npub1") {
        return Err(format!("Invalid npub format: {}", npub));
    }

    let profile_dir = app_data.join(npub);

    if !profile_dir.exists() {
        std::fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    }

    Ok(profile_dir)
}

/// Get the database path using static APP_DATA_DIR (no AppHandle needed).
pub fn get_database_path_static(npub: &str) -> Result<PathBuf, String> {
    let profile_dir = get_profile_directory_static(npub)?;
    Ok(profile_dir.join("vector.db"))
}

/// Get the MLS directory using static APP_DATA_DIR (no AppHandle needed).
pub fn get_mls_directory_static(npub: &str) -> Result<PathBuf, String> {
    let profile_dir = get_profile_directory_static(npub)?;
    let mls_dir = profile_dir.join("mls");

    if !mls_dir.exists() {
        std::fs::create_dir_all(&mls_dir)
            .map_err(|e| format!("Failed to create MLS directory: {}", e))?;
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
/// Opens a connection, runs SQL_SCHEMA (CREATE IF NOT EXISTS — safe for existing tables),
/// then runs all migrations (each is idempotent). The connection is pooled afterwards.
///
/// For new accounts (no DB file yet), this is a no-op — init_profile_database handles creation.
fn ensure_schema_ready<R: Runtime>(handle: &AppHandle<R>, npub: &str) -> Result<(), String> {
    let db_path = get_database_path(handle, npub)?;

    // No DB file = new account, nothing to migrate (init_profile_database will create it)
    if !db_path.exists() {
        return Ok(());
    }

    // If pool already has connections, schema was already ensured (e.g. second call)
    if DB_READ_POOL.lock().unwrap().len() > 0 {
        return Ok(());
    }

    println!("[Account Manager] Ensuring schema and migrations for {}", npub);

    let mut conn = open_db_connection(&db_path)?;
    conn.execute_batch(SQL_SCHEMA)
        .map_err(|e| format!("Failed to apply schema: {}", e))?;
    run_migrations(&mut conn)?;

    // Pool this connection so subsequent reads reuse it
    DB_READ_POOL.lock().unwrap().push(conn);

    println!("[Account Manager] Schema ready");
    Ok(())
}

/// Set the currently active account
/// Only clears the connection pool if actually switching to a different account.
pub fn set_current_account(npub: String) -> Result<(), String> {
    let mut current = CURRENT_ACCOUNT.write()
        .map_err(|e| format!("Failed to write current account: {}", e))?;

    // Only close pool if switching to a different account
    if current.as_ref() != Some(&npub) {
        close_db_connection();
    }

    *current = Some(npub);
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

// ============================================================================
// Read Connection Pool (multiple connections for parallel reads)
// ============================================================================

/// Get a READ connection from the pool. Falls back to opening a new one if pool is empty.
/// This is the standard path for all SELECT queries.
pub fn get_db_connection<R: Runtime>(handle: &AppHandle<R>) -> Result<rusqlite::Connection, String> {
    {
        let mut pool = DB_READ_POOL.lock().unwrap();
        if let Some(conn) = pool.pop() {
            return Ok(conn);
        }
    }
    let npub = get_current_account()?;
    let db_path = get_database_path(handle, &npub)?;
    open_db_connection(&db_path)
}

/// Return a READ connection to the pool (capped at 4 — excess connections are closed).
pub fn return_db_connection(conn: rusqlite::Connection) {
    let mut pool = DB_READ_POOL.lock().unwrap();
    if pool.len() < 4 {
        pool.push(conn);
    }
}

// ============================================================================
// Write Connection (single connection for serialized writes)
// ============================================================================

/// Get the dedicated WRITE connection. Only one write operation runs at a time.
/// Use this for INSERT, UPDATE, DELETE operations.
pub fn get_write_connection<R: Runtime>(handle: &AppHandle<R>) -> Result<rusqlite::Connection, String> {
    {
        let mut writer = DB_WRITE_CONN.lock().unwrap();
        if let Some(conn) = writer.take() {
            return Ok(conn);
        }
    }
    let npub = get_current_account()?;
    let db_path = get_database_path(handle, &npub)?;
    open_db_connection(&db_path)
}

/// Return the WRITE connection.
pub fn return_write_connection(conn: rusqlite::Connection) {
    let mut writer = DB_WRITE_CONN.lock().unwrap();
    *writer = Some(conn);
}

// ============================================================================
// Static Connection Functions (headless-safe — no AppHandle required)
// ============================================================================

/// Get a READ connection using static APP_DATA_DIR (no AppHandle needed).
fn get_db_connection_static() -> Result<rusqlite::Connection, String> {
    {
        let mut pool = DB_READ_POOL.lock().unwrap();
        if let Some(conn) = pool.pop() {
            return Ok(conn);
        }
    }
    let npub = get_current_account()?;
    let db_path = get_database_path_static(&npub)?;
    open_db_connection(&db_path)
}

/// Get the WRITE connection using static APP_DATA_DIR (no AppHandle needed).
fn get_write_connection_static() -> Result<rusqlite::Connection, String> {
    {
        let mut writer = DB_WRITE_CONN.lock().unwrap();
        if let Some(conn) = writer.take() {
            return Ok(conn);
        }
    }
    let npub = get_current_account()?;
    let db_path = get_database_path_static(&npub)?;
    open_db_connection(&db_path)
}

/// Initialize the DB pool using static path (for headless/background service).
pub fn init_db_pool_static(db_path: &std::path::Path) -> Result<(), String> {
    // Skip if pool already has connections
    if DB_READ_POOL.lock().unwrap().len() > 0 {
        return Ok(());
    }

    let mut conn = open_db_connection(db_path)?;
    conn.execute_batch(SQL_SCHEMA)
        .map_err(|e| format!("Failed to apply schema: {}", e))?;
    run_migrations(&mut conn)?;

    // Pre-warm read pool
    {
        let mut pool = DB_READ_POOL.lock().unwrap();
        for _ in 0..3 {
            if let Ok(extra) = open_db_connection(db_path) {
                pool.push(extra);
            }
        }
        pool.push(conn);
    }

    // Pre-warm write connection
    {
        if let Ok(writer) = open_db_connection(db_path) {
            *DB_WRITE_CONN.lock().unwrap() = Some(writer);
        }
    }

    println!("[Account Manager] Static DB pool initialized (4 readers + 1 writer)");
    Ok(())
}

// ============================================================================
// Connection Utilities
// ============================================================================

/// Open a new SQLite connection with standard pragmas.
fn open_db_connection(db_path: &std::path::Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .map_err(|e| format!("Failed to set pragmas: {}", e))?;
    Ok(conn)
}

/// Close ALL database connections (read pool + writer). Used when switching accounts.
pub fn close_db_connection() {
    DB_READ_POOL.lock().unwrap().clear();
    *DB_WRITE_CONN.lock().unwrap() = None;
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
/// The connection is pooled after init so subsequent get_db_connection calls reuse it
pub async fn init_profile_database<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str
) -> Result<(), String> {
    let db_path = get_database_path(handle, npub)?;

    // Fast path: if pool already has connections FOR THIS ACCOUNT, DB exists and schema is valid
    // (get_encryption_and_key already opened a connection and queried settings)
    // Guard: only use fast path when pool belongs to the same account (prevents wrong-DB on account switch)
    let pool_size = DB_READ_POOL.lock().unwrap().len();
    let same_account = get_current_account().map(|a| a == npub).unwrap_or(false);
    if pool_size > 0 && same_account {
        // Run migrations on existing pooled connection (usually all no-ops)
        let mut conn = get_db_connection(handle)?;
        run_migrations(&mut conn)?;
        return_db_connection(conn);

        // Warm remaining pool connections in background (not on critical path)
        let db_path_bg = db_path.clone();
        std::thread::spawn(move || {
            {
                let mut pool = DB_READ_POOL.lock().unwrap();
                let needed = 4usize.saturating_sub(pool.len());
                for _ in 0..needed {
                    if let Ok(c) = open_db_connection(&db_path_bg) {
                        pool.push(c);
                    }
                }
            }
            let mut writer = DB_WRITE_CONN.lock().unwrap();
            if writer.is_none() {
                if let Ok(w) = open_db_connection(&db_path_bg) {
                    *writer = Some(w);
                }
            }
        });

        return Ok(());
    }

    // Slow path: first time init (no existing connections)
    println!("[Account Manager] Initializing database: {}", db_path.display());

    // Create the database directory if it doesn't exist
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create database directory: {}", e))?;
    }

    // Open connection with standard pragmas (WAL + busy_timeout)
    let mut conn = open_db_connection(&db_path)?;

    // Execute the schema to create all tables
    conn.execute_batch(SQL_SCHEMA)
        .map_err(|e| format!("Failed to create database schema: {}", e))?;

    // Run migrations for existing databases (atomic - each migration is all-or-nothing)
    run_migrations(&mut conn)?;

    // Pre-warm read pool for parallel reads during boot
    {
        let mut pool = DB_READ_POOL.lock().unwrap();
        // 4 read connections for parallel boot queries
        for _ in 0..3 {
            if let Ok(extra) = open_db_connection(&db_path) {
                pool.push(extra);
            }
        }
        pool.push(conn); // Primary connection (used for migrations) joins the read pool
    }

    // Pre-warm dedicated write connection
    {
        if let Ok(writer) = open_db_connection(&db_path) {
            *DB_WRITE_CONN.lock().unwrap() = Some(writer);
        }
    }

    println!("[Account Manager] Database initialized (4 readers + 1 writer)");

    Ok(())
}

/// Check if a specific migration has already been applied
fn migration_applied(conn: &rusqlite::Connection, migration_id: u32) -> bool {
    conn.query_row(
        "SELECT 1 FROM schema_migrations WHERE id = ?1",
        rusqlite::params![migration_id],
        |_| Ok(())
    ).is_ok()
}

/// Mark a migration as applied (within a transaction)
fn mark_migration_applied(tx: &rusqlite::Transaction, migration_id: u32) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    tx.execute(
        "INSERT INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
        rusqlite::params![migration_id, now],
    ).map_err(|e| format!("[DB] Migration {}: Failed to record: {}", migration_id, e))?;

    Ok(())
}

/// Run a single migration atomically within a transaction.
///
/// GUARANTEES:
/// - If the migration succeeds: all changes are committed, migration is marked as applied
/// - If the migration fails: ALL changes are rolled back, database is unchanged
/// - No partial state is ever possible
///
/// This is the ONLY way migrations should be run.
fn run_atomic_migration<F>(
    conn: &mut rusqlite::Connection,
    id: u32,
    name: &str,
    migrate: F,
) -> Result<(), String>
where
    F: FnOnce(&rusqlite::Transaction) -> Result<(), String>,
{
    // Check if this specific migration was already applied.
    // We check each individually rather than using MAX(id) because bootstrap_legacy_migrations
    // can leave gaps (e.g. m1, m2, m8 applied but m3-m7 not) when SQL_SCHEMA creates tables
    // before bootstrap inspects them.
    if migration_applied(conn, id) {
        return Ok(());
    }

    println!("[DB] Migration {}: {}...", id, name);

    // Start transaction - this is the atomicity boundary
    let tx = conn.transaction()
        .map_err(|e| format!("[DB] Migration {}: Failed to start transaction: {}", id, e))?;

    // Run the migration within the transaction
    match migrate(&tx) {
        Ok(()) => {
            // Mark as applied WITHIN the same transaction
            mark_migration_applied(&tx, id)?;

            // Commit - if this fails, everything rolls back
            tx.commit()
                .map_err(|e| format!("[DB] Migration {}: Failed to commit: {}", id, e))?;

            println!("[DB] Migration {} complete", id);
            Ok(())
        }
        Err(e) => {
            // Transaction automatically rolls back on drop
            eprintln!("[DB] Migration {} FAILED: {} - rolling back", id, e);
            Err(e)
        }
    }
}

/// Ensure a column exists on a table, adding it if missing.
/// This is a safety net for cases where ALTER TABLE inside a WAL-mode
/// transaction silently fails (e.g., other connections hold read locks).
fn ensure_column_exists(
    conn: &mut rusqlite::Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<(), String> {
    let exists: bool = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name='{}'", table, column),
        [],
        |row| row.get::<_, i32>(0),
    ).map(|c| c > 0).unwrap_or(false);

    if !exists {
        println!("[DB] Safety net: adding missing column {}.{}", table, column);
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type),
            [],
        ).map_err(|e| format!("[DB] Failed to add column {}.{}: {}", table, column, e))?;
    }
    Ok(())
}

/// Run database migrations for schema updates
///
/// GUARANTEES:
/// - Each migration runs in a transaction (atomic - all or nothing)
/// - If any migration fails, changes are rolled back - no partial state
/// - Migrations are tracked in schema_migrations table (idempotent - safe to re-run)
/// - All errors are logged with [DB] prefix and propagated (no silent failures)
fn run_migrations(conn: &mut rusqlite::Connection) -> Result<(), String> {
    // Ensure schema_migrations table exists (bootstrap - must succeed before any migrations)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            id INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("[DB] Failed to create schema_migrations table: {}", e))?;

    // Bootstrap legacy migrations first (for existing databases)
    bootstrap_legacy_migrations(conn)?;

    // Check if messages table exists (needed by some migrations)
    let has_messages_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    // =========================================================================
    // Migration 1: Add wrapper_event_id column to messages table
    // =========================================================================
    if has_messages_table {
        run_atomic_migration(conn,1, "Add wrapper_event_id to messages", |tx| {
            tx.execute(
                "ALTER TABLE messages ADD COLUMN wrapper_event_id TEXT",
                []
            ).map_err(|e| format!("Failed to add column: {}", e))?;

            tx.execute(
                "CREATE INDEX IF NOT EXISTS idx_messages_wrapper ON messages(wrapper_event_id)",
                []
            ).map_err(|e| format!("Failed to create index: {}", e))?;

            Ok(())
        })?;
    }

    // =========================================================================
    // Migration 2: Create miniapps_history table
    // =========================================================================
    run_atomic_migration(conn,2, "Create miniapps_history table", |tx| {
        tx.execute(
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
        ).map_err(|e| format!("Failed to create table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 3: Add installed_version column to miniapps_history
    // =========================================================================
    run_atomic_migration(conn,3, "Add installed_version to miniapps_history", |tx| {
        tx.execute(
            "ALTER TABLE miniapps_history ADD COLUMN installed_version TEXT DEFAULT NULL",
            []
        ).map_err(|e| format!("Failed to add column: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 4: Add cached image columns to profiles table
    // =========================================================================
    run_atomic_migration(conn,4, "Add avatar/banner cache columns to profiles", |tx| {
        tx.execute(
            "ALTER TABLE profiles ADD COLUMN avatar_cached TEXT DEFAULT ''",
            []
        ).map_err(|e| format!("Failed to add avatar_cached: {}", e))?;

        tx.execute(
            "ALTER TABLE profiles ADD COLUMN banner_cached TEXT DEFAULT ''",
            []
        ).map_err(|e| format!("Failed to add banner_cached: {}", e))?;

        Ok(())
    })?;

    // =========================================================================
    // Migration 5: Create miniapp_permissions table
    // =========================================================================
    run_atomic_migration(conn,5, "Create miniapp_permissions table", |tx| {
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS miniapp_permissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_hash TEXT NOT NULL,
                permission TEXT NOT NULL,
                granted INTEGER NOT NULL DEFAULT 0,
                granted_at INTEGER,
                UNIQUE(file_hash, permission)
            )
            "#,
            []
        ).map_err(|e| format!("Failed to create table: {}", e))?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_miniapp_permissions_hash ON miniapp_permissions(file_hash)",
            []
        ).map_err(|e| format!("Failed to create index: {}", e))?;

        Ok(())
    })?;

    // =========================================================================
    // Migration 6: Create events table for flat event-based storage
    // =========================================================================
    run_atomic_migration(conn,6, "Create events table and migrate messages", |tx| {
        // Create the events table
        tx.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                kind INTEGER NOT NULL,
                chat_id INTEGER NOT NULL,
                user_id INTEGER,
                content TEXT NOT NULL DEFAULT '',
                tags TEXT NOT NULL DEFAULT '[]',
                reference_id TEXT,
                created_at INTEGER NOT NULL,
                received_at INTEGER NOT NULL,
                mine INTEGER NOT NULL DEFAULT 0,
                pending INTEGER NOT NULL DEFAULT 0,
                failed INTEGER NOT NULL DEFAULT 0,
                wrapper_event_id TEXT,
                npub TEXT,
                FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE,
                FOREIGN KEY (user_id) REFERENCES profiles(id) ON DELETE SET NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_chat_time ON events(chat_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
            CREATE INDEX IF NOT EXISTS idx_events_reference ON events(reference_id) WHERE reference_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_events_wrapper ON events(wrapper_event_id) WHERE wrapper_event_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_events_user ON events(user_id);
        "#).map_err(|e| format!("Failed to create events table: {}", e))?;

        // Migrate messages within the same transaction
        migrate_messages_to_events_atomic(tx)?;

        Ok(())
    })?;

    // =========================================================================
    // Migration 7: Backfill attachment metadata into event tags
    // =========================================================================
    if has_messages_table {
        run_atomic_migration(conn,7, "Backfill attachment metadata to event tags", |tx| {
            migrate_attachments_to_event_tags_atomic(tx)?;
            Ok(())
        })?;
    } else {
        // If messages table is gone, just mark as complete
        run_atomic_migration(conn,7, "Mark attachment migration complete (no messages table)", |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '3')",
                []
            ).map_err(|e| format!("Failed to update storage version: {}", e))?;
            Ok(())
        })?;
    }

    // =========================================================================
    // Migration 8: Create pivx_promos table
    // =========================================================================
    run_atomic_migration(conn,8, "Create pivx_promos table", |tx| {
        tx.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS pivx_promos (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                gift_code TEXT NOT NULL UNIQUE,
                address TEXT NOT NULL,
                privkey_encrypted TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                claimed_at INTEGER,
                amount_piv REAL,
                status TEXT NOT NULL DEFAULT 'active'
            );
            CREATE INDEX IF NOT EXISTS idx_pivx_promos_code ON pivx_promos(gift_code);
            CREATE INDEX IF NOT EXISTS idx_pivx_promos_address ON pivx_promos(address);
            CREATE INDEX IF NOT EXISTS idx_pivx_promos_status ON pivx_promos(status);
        "#).map_err(|e| format!("Failed to create table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 9: Fix DM chats with empty participants
    // =========================================================================
    run_atomic_migration(conn,9, "Fix DM chats with empty participants", |tx| {
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM chats WHERE chat_type = 0 AND participants = '[]'",
            [],
            |row| row.get(0)
        ).unwrap_or(0);

        if count > 0 {
            tx.execute(
                r#"UPDATE chats
                   SET participants = '["' || chat_identifier || '"]'
                   WHERE chat_type = 0 AND participants = '[]'"#,
                [],
            ).map_err(|e| format!("Failed to fix participants: {}", e))?;
            println!("[DB] Fixed {} DM chats", count);
        }
        Ok(())
    })?;

    // =========================================================================
    // Migration 10: Backfill npub for events from user_id
    // =========================================================================
    run_atomic_migration(conn,10, "Backfill event npub from user_id", |tx| {
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE npub IS NULL AND user_id IS NOT NULL",
            [],
            |row| row.get(0)
        ).unwrap_or(0);

        if count > 0 {
            tx.execute(
                r#"UPDATE events
                   SET npub = (SELECT p.npub FROM profiles p WHERE p.id = events.user_id)
                   WHERE npub IS NULL AND user_id IS NOT NULL"#,
                [],
            ).map_err(|e| format!("Failed to backfill npubs: {}", e))?;
            println!("[DB] Backfilled npub for {} events", count);
        }
        Ok(())
    })?;

    // =========================================================================
    // Migration 11: Create mls_processed_events table for EventTracker
    // This tracks which MLS wrapper events have been processed to prevent
    // re-processing and enable proper deduplication.
    // =========================================================================
    run_atomic_migration(conn,11, "Create mls_processed_events table", |tx| {
        tx.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS mls_processed_events (
                event_id TEXT PRIMARY KEY,
                group_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                processed_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mls_processed_events_group ON mls_processed_events(group_id);
            CREATE INDEX IF NOT EXISTS idx_mls_processed_events_created ON mls_processed_events(created_at);
        "#).map_err(|e| format!("Failed to create mls_processed_events table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 12: v0.3.0 MLS engine upgrade — complete MLS reset
    //
    // The MDK upgrade (7c3157c) changed from a dual-connection OpenMLS
    // architecture to a unified single-connection engine. Additionally,
    // MIP-00/MIP-02 now require an ["encoding", "base64"] tag on all MLS
    // events (keypackages, welcomes, etc.) for security.
    //
    // This migration:
    // 1. Wipes all group chat data (DMs preserved)
    // 2. Recreates mls_keypackages with created_at column for primary device detection
    // 3. Flags keypackage regeneration to publish with new encoding tag
    //
    // Users will need to re-join their groups after upgrading.
    // =========================================================================
    run_atomic_migration(conn,12, "v0.3.0 MLS reset: wipe data, add created_at, force keypackage regen", |tx| {
        // Delete group chats (chat_type=1) — CASCADE deletes their events + messages
        tx.execute("DELETE FROM chats WHERE chat_type = 1", [])
            .map_err(|e| format!("Failed to delete group chats: {}", e))?;

        // Clear all MLS metadata tables
        tx.execute("DELETE FROM mls_groups", [])
            .map_err(|e| format!("Failed to clear mls_groups: {}", e))?;
        tx.execute("DELETE FROM mls_event_cursors", [])
            .map_err(|e| format!("Failed to clear mls_event_cursors: {}", e))?;
        tx.execute("DELETE FROM mls_processed_events", [])
            .map_err(|e| format!("Failed to clear mls_processed_events: {}", e))?;

        // Recreate mls_keypackages with created_at column
        tx.execute("DROP TABLE IF EXISTS mls_keypackages", [])
            .map_err(|e| format!("Failed to drop mls_keypackages: {}", e))?;
        tx.execute(
            "CREATE TABLE mls_keypackages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                owner_pubkey TEXT NOT NULL,
                device_id TEXT NOT NULL,
                keypackage_ref TEXT NOT NULL,
                created_at INTEGER,
                fetched_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            )",
            [],
        ).map_err(|e| format!("Failed to recreate mls_keypackages: {}", e))?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_keypackages_owner ON mls_keypackages(owner_pubkey)",
            [],
        ).map_err(|e| format!("Failed to create keypackages index: {}", e))?;

        // Flag keypackage regeneration (connect() will publish with new encoding tag)
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('mls_force_keypackage_regen', '1')",
            [],
        ).map_err(|e| format!("Failed to set keypackage regen flag: {}", e))?;

        println!("[DB] Migration 12: Complete MLS reset for v0.3.0 (encoding tag support).");
        Ok(())
    })?;

    // Migration 13: Add preview_metadata column to events table for link preview caching
    run_atomic_migration(conn,13, "Add preview_metadata to events table", |tx| {
        // Check if column already exists (may have been added by a prior dev build)
        let col_exists: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name='preview_metadata'",
            [], |row| row.get::<_, i32>(0)
        ).map(|c| c > 0).unwrap_or(false);
        if !col_exists {
            tx.execute(
                "ALTER TABLE events ADD COLUMN preview_metadata TEXT",
                []
            ).map_err(|e| format!("Failed to add preview_metadata column: {}", e))?;
        }
        Ok(())
    })?;

    // Migration 14: Add description column to mls_groups table
    run_atomic_migration(conn, 14, "Add description to mls_groups table", |tx| {
        let col_exists: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mls_groups') WHERE name='description'",
            [], |row| row.get::<_, i32>(0)
        ).map(|c| c > 0).unwrap_or(false);
        if !col_exists {
            tx.execute(
                "ALTER TABLE mls_groups ADD COLUMN description TEXT",
                []
            ).map_err(|e| format!("Failed to add description column: {}", e))?;
        }
        Ok(())
    })?;

    // Migration 15: Add avatar_cached column to mls_groups table
    run_atomic_migration(conn, 15, "Add avatar_cached to mls_groups table", |tx| {
        let col_exists: bool = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mls_groups') WHERE name='avatar_cached'",
            [], |row| row.get::<_, i32>(0)
        ).map(|c| c > 0).unwrap_or(false);
        if !col_exists {
            tx.execute(
                "ALTER TABLE mls_groups ADD COLUMN avatar_cached TEXT",
                []
            ).map_err(|e| format!("Failed to add avatar_cached column: {}", e))?;
        }
        Ok(())
    })?;

    // Safety net: ALTER TABLE inside WAL-mode transactions can silently fail when
    // other connections hold read locks. Verify critical columns exist outside the
    // migration system so they're always present regardless of migration history.
    ensure_column_exists(conn, "mls_groups", "description", "TEXT")?;
    ensure_column_exists(conn, "mls_groups", "avatar_cached", "TEXT")?;

    // =========================================================================
    // Future migrations (16+) follow the same pattern:
    //
    // run_atomic_migration(conn, 16, "Description here", |tx| {
    //     tx.execute("...", [])?;
    //     Ok(())
    // })?;
    // =========================================================================

    Ok(())
}

/// Bootstrap legacy migrations into the new tracking system (ATOMIC)
/// This checks schema state to determine which old migrations have already run,
/// and marks them as applied in schema_migrations table.
/// All tracking records are written in a single transaction.
fn bootstrap_legacy_migrations(conn: &mut rusqlite::Connection) -> Result<(), String> {
    // Only bootstrap once - if migration 1 is tracked, we've already bootstrapped
    if migration_applied(conn, 1) {
        return Ok(());
    }

    println!("[DB] Bootstrapping legacy migration tracking...");

    // Check which migrations have effectively been applied based on schema state
    // (Read operations - don't need transaction)

    // Migration 1: wrapper_event_id column (may not exist if messages table was dropped)
    let messages_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    let m1_applied = !messages_exists || conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='wrapper_event_id'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 2: miniapps_history table
    let m2_applied: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='miniapps_history'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 3: installed_version column
    let m3_applied = !m2_applied || conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('miniapps_history') WHERE name='installed_version'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 4: avatar_cached column
    let m4_applied: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('profiles') WHERE name='avatar_cached'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 5: miniapp_permissions table
    let m5_applied: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='miniapp_permissions'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 6: events table
    // IMPORTANT: Check not just that events table exists, but that data was migrated.
    // This handles partial migration recovery: if events table exists but is empty
    // while messages table has data, we need to re-run the migration.
    let events_table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    let m6_applied = if events_table_exists && messages_exists {
        // Both tables exist - check if migration actually completed
        let events_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap_or(0);
        let messages_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages", [], |row| row.get(0)
        ).unwrap_or(0);

        // Migration is complete if:
        // - events has data, OR
        // - messages is empty (nothing to migrate)
        events_count > 0 || messages_count == 0
    } else {
        // events table exists and messages doesn't = migration complete
        // events table doesn't exist = migration not run
        events_table_exists
    };

    // Migration 7: storage_version >= 3
    let storage_version: i32 = conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'storage_version'",
        [], |row| row.get(0)
    ).unwrap_or(0);
    let m7_applied = storage_version >= 3;

    // Migration 8: pivx_promos table
    let m8_applied: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pivx_promos'",
        [], |row| row.get::<_, i32>(0)
    ).map(|c| c > 0).unwrap_or(false);

    // Migration 9 & 10: Data migrations - assume applied if events table exists
    let m9_applied = m6_applied;
    let m10_applied = m6_applied;

    let migrations_to_record = [
        (1, m1_applied), (2, m2_applied), (3, m3_applied), (4, m4_applied),
        (5, m5_applied), (6, m6_applied), (7, m7_applied), (8, m8_applied),
        (9, m9_applied), (10, m10_applied),
    ];

    let applied_count = migrations_to_record.iter().filter(|(_, a)| *a).count();

    // Record all applied migrations in a single transaction (ATOMIC)
    let tx = conn.transaction()
        .map_err(|e| format!("[DB] Bootstrap: Failed to start transaction: {}", e))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    for (id, applied) in migrations_to_record {
        if applied {
            tx.execute(
                "INSERT OR IGNORE INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
                rusqlite::params![id, now],
            ).map_err(|e| format!("[DB] Bootstrap: Failed to record migration {}: {}", id, e))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("[DB] Bootstrap: Failed to commit: {}", e))?;

    println!("[DB] Bootstrap complete. Tracked {} legacy migrations.", applied_count);

    Ok(())
}

/// Migration 7: Copy attachment metadata from messages table into event tags (ATOMIC)
/// Runs within a transaction - all changes succeed or all are rolled back.
fn migrate_attachments_to_event_tags_atomic(tx: &rusqlite::Transaction) -> Result<(), String> {
    // Find all kind=15 events that don't have attachment tags yet
    let mut stmt = tx.prepare(r#"
        SELECT e.id, e.tags, m.attachments
        FROM events e
        JOIN messages m ON e.id = m.id
        WHERE e.kind = 15
        AND m.attachments IS NOT NULL
        AND m.attachments != '[]'
        AND NOT EXISTS (
            SELECT 1 FROM json_each(e.tags)
            WHERE json_extract(value, '$[0]') = 'attachments'
        )
    "#).map_err(|e| format!("[DB] Failed to prepare attachment query: {}", e))?;

    let events_to_update: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("[DB] Failed to query attachments: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    drop(stmt);

    let mut updated_count = 0;
    for (event_id, existing_tags, attachments_json) in events_to_update {
        // Parse existing tags and add attachments tag
        let mut tags: Vec<Vec<String>> = serde_json::from_str(&existing_tags)
            .unwrap_or_else(|_| Vec::new());

        // Add the attachments tag with the JSON
        tags.push(vec!["attachments".to_string(), attachments_json]);

        let new_tags = serde_json::to_string(&tags)
            .unwrap_or_else(|_| "[]".to_string());

        // Propagate errors - no silent failures
        tx.execute(
            "UPDATE events SET tags = ?1 WHERE id = ?2",
            rusqlite::params![new_tags, event_id]
        ).map_err(|e| format!("[DB] Failed to update event {}: {}", event_id, e))?;

        updated_count += 1;
    }

    // Update storage version to 3
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '3')",
        []
    ).map_err(|e| format!("[DB] Failed to update storage version: {}", e))?;

    println!("[DB] Backfilled {} events with attachment metadata", updated_count);

    Ok(())
}

/// Migrate existing messages from the old nested format to the flat events table (ATOMIC)
/// Runs within a transaction - all changes succeed or all are rolled back.
fn migrate_messages_to_events_atomic(tx: &rusqlite::Transaction) -> Result<(), String> {
    // Count existing messages
    let message_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM messages",
        [],
        |row| row.get(0)
    ).unwrap_or(0);

    if message_count == 0 {
        println!("[DB] No messages to migrate");
        // Still set storage version
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '2')",
            []
        ).map_err(|e| format!("[DB] Failed to update storage version: {}", e))?;
        return Ok(());
    }

    println!("[DB] Migrating {} messages...", message_count);

    // Step 1: Migrate text messages (kind=14) and file attachments (kind=15)
    // Use INSERT OR IGNORE to safely handle partial migration recovery
    tx.execute(r#"
        INSERT OR IGNORE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        )
        SELECT
            m.id,
            CASE
                WHEN m.attachments != '[]' AND m.attachments IS NOT NULL THEN 15
                ELSE 14
            END as kind,
            m.chat_id,
            m.user_id,
            m.content_encrypted,
            CASE
                WHEN m.replied_to != '' THEN json_array(json_array('e', m.replied_to, '', 'reply'))
                ELSE '[]'
            END as tags,
            NULL as reference_id,
            m.at / 1000 as created_at,
            m.at as received_at,
            m.mine,
            0 as pending,
            0 as failed,
            m.wrapper_event_id,
            p.npub
        FROM messages m
        LEFT JOIN profiles p ON p.id = m.user_id
    "#, []).map_err(|e| format!("[DB] Failed to migrate messages: {}", e))?;

    // Step 2: Extract and migrate reactions from the JSON arrays
    let mut reaction_stmt = tx.prepare(
        "SELECT id, chat_id, reactions, at FROM messages WHERE reactions != '[]' AND reactions IS NOT NULL"
    ).map_err(|e| format!("[DB] Failed to prepare reaction query: {}", e))?;

    let reaction_rows: Vec<(String, i64, String, i64)> = reaction_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| format!("[DB] Failed to query reactions: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    drop(reaction_stmt);

    let mut reaction_count = 0;
    for (message_id, chat_id, reactions_json, at) in reaction_rows {
        // Parse the reactions JSON array
        if let Ok(reactions) = serde_json::from_str::<Vec<serde_json::Value>>(&reactions_json) {
            for reaction in reactions {
                if let (Some(reaction_id), Some(emoji), Some(author_id)) = (
                    reaction.get("id").and_then(|v| v.as_str()),
                    reaction.get("emoji").and_then(|v| v.as_str()),
                    reaction.get("author_id").and_then(|v| v.as_str()),
                ) {
                    let tags = serde_json::json!([["e", message_id]]).to_string();

                    // Propagate errors - no silent failures
                    tx.execute(
                        r#"
                        INSERT OR IGNORE INTO events (
                            id, kind, chat_id, user_id, content, tags, reference_id,
                            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
                        ) VALUES (?1, 7, ?2, NULL, ?3, ?4, ?5, ?6, ?7, 0, 0, 0, NULL, ?8)
                        "#,
                        rusqlite::params![
                            reaction_id,
                            chat_id,
                            emoji,
                            tags,
                            message_id,
                            at / 1000,
                            at,
                            author_id
                        ]
                    ).map_err(|e| format!("[DB] Failed to insert reaction {}: {}", reaction_id, e))?;

                    reaction_count += 1;
                }
            }
        }
    }

    println!("[DB] Migrated {} reactions", reaction_count);

    // Step 3: Mark migration as complete
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '2')",
        []
    ).map_err(|e| format!("[DB] Failed to update storage version: {}", e))?;

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
    if let Err(e) = crate::db::preload_id_caches().await {
        eprintln!("[Account Manager] Failed to preload ID caches: {}", e);
    }

    // Update MLS directory
    let mls_dir = get_mls_directory(&handle, &npub)?;
    println!("[Account Manager] MLS directory: {}", mls_dir.display());

    // TODO: Update MLS configuration to use new directory
    // This will be done when we update the MLS module

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact replica of the v0.2.3 SQL_SCHEMA (the last version before the
    /// standardised migration system was introduced in v0.3.0).
    ///
    /// Key differences from v0.3.1 schema:
    /// - profiles: NO avatar_cached, banner_cached columns
    /// - NO events table (messages were stored in the messages table)
    /// - NO miniapps_history table
    /// - NO pivx_promos table
    /// - NO miniapp_permissions table
    /// - NO mls_processed_events table
    /// - NO schema_migrations table
    /// - mls_keypackages: NO created_at column
    const V0_2_3_SCHEMA: &str = r#"
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

        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mls_groups (
            group_id TEXT PRIMARY KEY,
            engine_group_id TEXT NOT NULL DEFAULT '',
            creator_pubkey TEXT NOT NULL,
            name TEXT NOT NULL DEFAULT '',
            description TEXT,
            avatar_ref TEXT,
            avatar_cached TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            evicted INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_mls_groups_evicted_updated ON mls_groups(evicted, updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_mls_groups_creator ON mls_groups(creator_pubkey);

        CREATE TABLE IF NOT EXISTS mls_keypackages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            owner_pubkey TEXT NOT NULL,
            device_id TEXT NOT NULL,
            keypackage_ref TEXT NOT NULL,
            fetched_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_keypackages_owner ON mls_keypackages(owner_pubkey);

        CREATE TABLE IF NOT EXISTS mls_event_cursors (
            group_id TEXT PRIMARY KEY,
            last_seen_event_id TEXT NOT NULL,
            last_seen_at INTEGER NOT NULL
        );
    "#;

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Create a temp database with the v0.2.3 schema and seed data.
    fn create_v0_2_3_db() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let db_path = dir.path().join("vector.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;").unwrap();
        conn.execute_batch(V0_2_3_SCHEMA).unwrap();

        // Seed realistic data
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name, avatar, about) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["npub1testuser", "alice", "Alice", "https://example.com/avatar.png", "Hello world"],
        ).unwrap();
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name) VALUES (?1, ?2, ?3)",
            rusqlite::params!["npub1otheruser", "bob", "Bob"],
        ).unwrap();

        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["chat_dm_abc", 0, "[\"npub1testuser\",\"npub1otheruser\"]", 1700000000],
        ).unwrap();

        conn.execute(
            "INSERT INTO messages (id, chat_id, content_encrypted, at, mine, user_id, wrapper_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["evt_001", 1, "encrypted_hello", 1700000100, 1, 1, "wrap_001"],
        ).unwrap();
        conn.execute(
            "INSERT INTO messages (id, chat_id, content_encrypted, at, mine, user_id, attachments) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["evt_002", 1, "encrypted_reply", 1700000200, 0, 2, "[{\"type\":\"image\",\"url\":\"https://example.com/img.png\"}]"],
        ).unwrap();

        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('pkey', 'encrypted_nsec_data')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('encryption_enabled', 'true')",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO mls_groups (group_id, engine_group_id, creator_pubkey, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["grp_001", "eng_001", "npub1testuser", 1700000000, 1700000000],
        ).unwrap();

        (dir, conn)
    }

    /// Helper: check if a column exists in a table.
    fn has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = ?1", table),
            rusqlite::params![column],
            |row| row.get::<_, i32>(0),
        ).map(|c| c > 0).unwrap_or(false)
    }

    /// Helper: check if a table exists.
    fn has_table(conn: &rusqlite::Connection, table: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            rusqlite::params![table],
            |row| row.get::<_, i32>(0),
        ).map(|c| c > 0).unwrap_or(false)
    }

    /// Helper: get the highest applied migration ID.
    fn max_migration(conn: &rusqlite::Connection) -> u32 {
        conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        ).unwrap_or(0)
    }

    // ── Test Cases ──────────────────────────────────────────────────────

    /// Simulate the v0.2.3 → v0.3.1 upgrade path:
    /// 1. Start with exact v0.2.3 schema + data
    /// 2. Run current SQL_SCHEMA + run_migrations
    /// 3. Verify all new tables/columns exist and data is preserved
    #[test]
    fn upgrade_v0_2_3_to_current() {
        let (_dir, mut conn) = create_v0_2_3_db();

        // --- Pre-upgrade assertions (v0.2.3 state) ---
        assert!(!has_column(&conn, "profiles", "avatar_cached"), "v0.2.3 should NOT have avatar_cached");
        assert!(!has_table(&conn, "events"), "v0.2.3 should NOT have events table");
        assert!(!has_table(&conn, "schema_migrations"), "v0.2.3 should NOT have schema_migrations");
        assert!(!has_table(&conn, "miniapps_history"), "v0.2.3 should NOT have miniapps_history");
        assert!(!has_table(&conn, "pivx_promos"), "v0.2.3 should NOT have pivx_promos");

        // --- Run the upgrade (same as ensure_schema_ready) ---
        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();

        // --- Schema assertions ---
        // Migration 4: avatar/banner cache columns
        assert!(has_column(&conn, "profiles", "avatar_cached"), "Should have avatar_cached after migration");
        assert!(has_column(&conn, "profiles", "banner_cached"), "Should have banner_cached after migration");

        // New tables from SQL_SCHEMA + migrations
        assert!(has_table(&conn, "events"), "Should have events table");
        assert!(has_table(&conn, "miniapps_history"), "Should have miniapps_history table");
        assert!(has_table(&conn, "pivx_promos"), "Should have pivx_promos table");
        assert!(has_table(&conn, "miniapp_permissions"), "Should have miniapp_permissions table");
        assert!(has_table(&conn, "mls_processed_events"), "Should have mls_processed_events table");
        assert!(has_table(&conn, "schema_migrations"), "Should have schema_migrations table");

        // Migration 3: installed_version on miniapps_history
        assert!(has_column(&conn, "miniapps_history", "installed_version"), "Should have installed_version");

        // Migration 13: preview_metadata on events
        assert!(has_column(&conn, "events", "preview_metadata"), "Should have preview_metadata on events");

        // Migration 12: mls_keypackages should have created_at
        assert!(has_column(&conn, "mls_keypackages", "created_at"), "Should have created_at on mls_keypackages");

        // --- Data preservation assertions ---
        // Profiles survived
        let profile_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM profiles", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(profile_count, 2, "Both profiles should survive upgrade");

        // Profile data intact (including new columns defaulting correctly)
        let (name, avatar_cached): (String, String) = conn.query_row(
            "SELECT name, avatar_cached FROM profiles WHERE npub = 'npub1testuser'",
            [], |row| Ok((row.get(0)?, row.get(1)?))
        ).unwrap();
        assert_eq!(name, "alice");
        assert_eq!(avatar_cached, "", "New columns should default to empty string");

        // Chat survived
        let chat_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM chats", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(chat_count, 1, "Chat should survive upgrade");

        // Migration 6: messages migrated to events table
        let events_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count, 2, "Both messages should be migrated to events");

        // Verify event data integrity
        let (content, wrapper): (String, Option<String>) = conn.query_row(
            "SELECT content, wrapper_event_id FROM events WHERE id = 'evt_001'",
            [], |row| Ok((row.get(0)?, row.get(1)?))
        ).unwrap();
        assert_eq!(content, "encrypted_hello");
        assert_eq!(wrapper.as_deref(), Some("wrap_001"));

        // Settings preserved
        let pkey: String = conn.query_row(
            "SELECT value FROM settings WHERE key = 'pkey'",
            [], |row| row.get(0)
        ).unwrap();
        assert_eq!(pkey, "encrypted_nsec_data");

        // MLS groups wiped by migration 12 (v0.3.0 MLS reset)
        let group_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM mls_groups", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(group_count, 0, "MLS groups should be wiped by migration 12 (v0.3.0 reset)");

        // All 13 migrations should be recorded
        assert!(max_migration(&conn) >= 13, "All migrations should be applied");
    }

    /// Fresh install: run schema + migrations on an empty database.
    /// Ensures no migration crashes on missing tables.
    #[test]
    fn fresh_install() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("vector.db");
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;").unwrap();

        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();

        // All tables exist
        for table in &[
            "profiles", "chats", "messages", "settings", "events",
            "mls_groups", "mls_keypackages", "mls_event_cursors",
            "miniapps_history", "pivx_promos", "miniapp_permissions",
            "mls_processed_events", "schema_migrations",
        ] {
            assert!(has_table(&conn, table), "Missing table: {}", table);
        }

        // Current schema columns present
        assert!(has_column(&conn, "profiles", "avatar_cached"));
        assert!(has_column(&conn, "profiles", "banner_cached"));
        assert!(has_column(&conn, "events", "preview_metadata"));
        assert!(has_column(&conn, "mls_keypackages", "created_at"));
        assert!(has_column(&conn, "miniapps_history", "installed_version"));
    }

    /// Idempotency: running schema + migrations twice should not error.
    #[test]
    fn idempotent_double_run() {
        let (_dir, mut conn) = create_v0_2_3_db();

        // First run
        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();
        let first_max = max_migration(&conn);

        // Second run (should be all no-ops)
        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();
        let second_max = max_migration(&conn);

        assert_eq!(first_max, second_max, "Migration max should not change on second run");

        // Data not duplicated
        let profile_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM profiles", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(profile_count, 2);

        let events_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count, 2, "Events should not be duplicated on second run");
    }

    /// The critical query that caused the v0.3.0 hang: SELECT with avatar_cached/banner_cached.
    /// After upgrade, this must succeed (not fail with "no such column").
    #[test]
    fn critical_profile_query_after_upgrade() {
        let (_dir, mut conn) = create_v0_2_3_db();

        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();

        // This is the exact query from db/profiles.rs that caused the v0.3.0 panic
        let mut stmt = conn.prepare(
            "SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, \
             about, website, nip05, status_content, status_url, muted, bot, \
             avatar_cached, banner_cached FROM profiles"
        ).expect("Critical profile query should succeed after upgrade");

        let rows: Vec<String> = stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&"npub1testuser".to_string()));
        assert!(rows.contains(&"npub1otheruser".to_string()));
    }

    // ── v0.3.0 vs v0.3.1 comparison ────────────────────────────────────

    /// Replica of the v0.3.0 migration runner that used MAX(id) as a shortcut.
    /// This is the BUGGY logic that skipped migrations when bootstrap left gaps.
    fn run_migrations_v0_3_0(conn: &mut rusqlite::Connection) -> Result<(), String> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                id INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            )", [],
        ).map_err(|e| format!("Failed to create schema_migrations: {}", e))?;

        bootstrap_legacy_migrations(conn)?;

        // THE BUG: uses MAX(id) to skip all migrations at or below
        let max_applied: u32 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM schema_migrations",
            [], |row| row.get(0),
        ).unwrap_or(0);

        let has_messages_table: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
            [], |row| row.get::<_, i32>(0)
        ).map(|count| count > 0).unwrap_or(false);

        // Run migrations using the old buggy skip logic (id <= max_applied → skip)
        fn run_atomic_migration_v030<F>(
            conn: &mut rusqlite::Connection,
            max_applied: u32,
            id: u32,
            _name: &str,
            migrate: F,
        ) -> Result<(), String>
        where F: FnOnce(&rusqlite::Transaction) -> Result<(), String> {
            if id <= max_applied { return Ok(()); } // THE BUG
            let tx = conn.transaction()
                .map_err(|e| format!("Migration {}: tx failed: {}", id, e))?;
            migrate(&tx)?;
            mark_migration_applied(&tx, id)?;
            tx.commit().map_err(|e| format!("Migration {}: commit failed: {}", id, e))?;
            Ok(())
        }

        if has_messages_table {
            run_atomic_migration_v030(conn, max_applied, 1, "wrapper_event_id", |tx| {
                tx.execute("ALTER TABLE messages ADD COLUMN wrapper_event_id TEXT", [])
                    .map_err(|e| format!("{}", e))?;
                Ok(())
            })?;
        }
        // Migration 4 is the critical one (avatar_cached/banner_cached)
        run_atomic_migration_v030(conn, max_applied, 4, "avatar/banner cache", |tx| {
            tx.execute_batch(
                "ALTER TABLE profiles ADD COLUMN avatar_cached TEXT NOT NULL DEFAULT '';
                 ALTER TABLE profiles ADD COLUMN banner_cached TEXT NOT NULL DEFAULT '';"
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;
        // Migration 6: events table + message migration (SKIPPED by max_applied bug)
        run_atomic_migration_v030(conn, max_applied, 6, "events table", |tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS events (
                    id TEXT PRIMARY KEY, kind INTEGER NOT NULL, chat_id INTEGER NOT NULL,
                    user_id INTEGER, content TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]',
                    reference_id TEXT, created_at INTEGER NOT NULL, received_at INTEGER NOT NULL,
                    mine INTEGER NOT NULL DEFAULT 0, pending INTEGER NOT NULL DEFAULT 0,
                    failed INTEGER NOT NULL DEFAULT 0, wrapper_event_id TEXT, npub TEXT
                );"
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;

        // Migrations 9+ have id > max_applied(8), so they DO run in v0.3.0.
        // But m9 and m10 operate on events data — which is empty because m6 was skipped.
        // They complete as no-ops and get recorded as "applied".
        run_atomic_migration_v030(conn, max_applied, 9, "Fix DM participants", |tx| {
            // No-op: events table is empty
            tx.execute(
                r#"UPDATE chats SET participants = '["' || chat_identifier || '"]'
                   WHERE chat_type = 0 AND participants = '[]'"#, [],
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;
        run_atomic_migration_v030(conn, max_applied, 10, "Backfill npub", |tx| {
            // No-op: events table is empty
            tx.execute(
                r#"UPDATE events SET npub = (SELECT p.npub FROM profiles p WHERE p.id = events.user_id)
                   WHERE npub IS NULL AND user_id IS NOT NULL"#, [],
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;
        run_atomic_migration_v030(conn, max_applied, 11, "mls_processed_events", |tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS mls_processed_events (
                    event_id TEXT PRIMARY KEY, group_id TEXT NOT NULL, created_at INTEGER NOT NULL
                );"
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;
        run_atomic_migration_v030(conn, max_applied, 12, "MLS reset", |tx| {
            tx.execute("DELETE FROM mls_groups", []).ok();
            tx.execute("DROP TABLE IF EXISTS mls_keypackages", []).ok();
            tx.execute_batch(
                "CREATE TABLE mls_keypackages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, owner_pubkey TEXT NOT NULL,
                    device_id TEXT NOT NULL, keypackage_ref TEXT NOT NULL,
                    created_at INTEGER, fetched_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
                );"
            ).map_err(|e| format!("{}", e))?;
            Ok(())
        })?;
        run_atomic_migration_v030(conn, max_applied, 13, "preview_metadata", |tx| {
            // In real v0.3.0, SQL_SCHEMA didn't include preview_metadata in events.
            // Our test uses current SQL_SCHEMA which does, so tolerate duplicate column.
            let _ = tx.execute("ALTER TABLE events ADD COLUMN preview_metadata TEXT", []);
            Ok(())
        })?;

        Ok(())
    }

    /// Prove that v0.3.0's migration logic FAILS on a v0.2.3 database,
    /// while v0.3.1's logic SUCCEEDS on the same schema.
    #[test]
    fn v0_3_0_fails_v0_3_1_succeeds() {
        // ─── v0.3.0 path: buggy MAX(id) logic ───
        let (_dir1, mut conn1) = create_v0_2_3_db();
        conn1.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations_v0_3_0(&mut conn1).unwrap();

        // v0.3.0 FAILS: avatar_cached was never added (migration 4 skipped by max_applied)
        assert!(
            !has_column(&conn1, "profiles", "avatar_cached"),
            "v0.3.0 should FAIL to add avatar_cached (migration 4 skipped by max_applied bug)"
        );

        // v0.3.0 FAILS: the critical query that caused the "Decrypting Database..." hang
        let query_result = conn1.prepare(
            "SELECT npub, avatar_cached, banner_cached FROM profiles"
        );
        assert!(
            query_result.is_err(),
            "v0.3.0 should FAIL the profile query (no such column: avatar_cached)"
        );

        // ─── v0.3.1 path: per-migration checking ───
        let (_dir2, mut conn2) = create_v0_2_3_db();
        conn2.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn2).unwrap();

        // v0.3.1 SUCCEEDS: migration 4 runs correctly
        assert!(
            has_column(&conn2, "profiles", "avatar_cached"),
            "v0.3.1 should successfully add avatar_cached"
        );

        // v0.3.1 SUCCEEDS: the critical query works
        let query_result = conn2.prepare(
            "SELECT npub, avatar_cached, banner_cached FROM profiles"
        );
        assert!(
            query_result.is_ok(),
            "v0.3.1 should handle the profile query after upgrade"
        );

        // v0.3.1 SUCCEEDS: messages migrated to events
        let events_count: i32 = conn2.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count, 2, "v0.3.1 should migrate messages to events");

        // v0.3.0 FAILS: events table exists but is empty (migration 6 was skipped)
        let events_count_old: i32 = conn1.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count_old, 0, "v0.3.0 should have empty events table (migration 6 skipped)");
    }

    /// Simulate a user who hit the v0.3.0 "stuck" bug, then upgrades to v0.3.1.
    ///
    /// Their database state after v0.3.0:
    /// - schema_migrations has: 1, 2, 8, 9, 10, 11, 12, 13 (m3-m7 missing due to max_applied bug)
    /// - profiles: missing avatar_cached/banner_cached
    /// - events: empty (m6 skipped, messages never migrated)
    /// - m9/m10: recorded as "applied" but were no-ops (events was empty)
    ///
    /// v0.3.1 must:
    /// - Run m3, m4, m5, m6, m7 (fill the gaps)
    /// - Detect that m9/m10 ran on empty data and re-run them
    #[test]
    fn stuck_v0_3_0_user_repaired_by_v0_3_1() {
        // Step 1: Create v0.2.3 database
        let (_dir, mut conn) = create_v0_2_3_db();

        // Step 2: Simulate v0.3.0's buggy upgrade
        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations_v0_3_0(&mut conn).unwrap();

        // Verify the broken v0.3.0 state
        assert!(!has_column(&conn, "profiles", "avatar_cached"), "v0.3.0 state: no avatar_cached");
        let events_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count, 0, "v0.3.0 state: events empty (m6 skipped)");
        assert!(migration_applied(&conn, 9), "v0.3.0 state: m9 recorded (but was no-op)");
        assert!(migration_applied(&conn, 10), "v0.3.0 state: m10 recorded (but was no-op)");

        // Step 3: Run v0.3.1 migration system (the repair)
        conn.execute_batch(SQL_SCHEMA).unwrap();
        run_migrations(&mut conn).unwrap();

        // Step 4: Verify the repair
        // Schema gaps filled
        assert!(has_column(&conn, "profiles", "avatar_cached"), "v0.3.1 should add avatar_cached");
        assert!(has_column(&conn, "profiles", "banner_cached"), "v0.3.1 should add banner_cached");
        assert!(has_table(&conn, "miniapp_permissions"), "v0.3.1 should create miniapp_permissions");
        assert!(has_column(&conn, "miniapps_history", "installed_version"), "v0.3.1 should add installed_version");

        // Messages migrated to events (m6 now runs)
        let events_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(events_count, 2, "v0.3.1 should migrate messages to events");

        // Critical profile query works (the exact query that caused the hang)
        conn.prepare(
            "SELECT npub, avatar_cached, banner_cached FROM profiles"
        ).expect("Profile query should work after v0.3.1 repair");

        // m10 data backfill: events should have npub populated
        let null_npub_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE npub IS NULL AND user_id IS NOT NULL",
            [], |row| row.get(0)
        ).unwrap();
        assert_eq!(null_npub_count, 0, "v0.3.1 should backfill npub on events (m10 re-ran or repair)");
    }
}