//! Database layer — SQLite with per-account databases.
//!
//! Architecture:
//! - Read pool: multiple connections for parallel reads (WAL mode)
//! - Write pool: single Mutex-protected connection (serialized writes)
//! - RAII guards: auto-return connections to pools on drop
//!
//! All connection functions use static `DATA_DIR` — no Tauri AppHandle required.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, LazyLock, RwLock};
use std::ops::{Deref, DerefMut};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod settings;
pub mod schema;
pub mod profiles;

pub use settings::{get_sql_setting, set_sql_setting, get_pkey, set_pkey, get_seed, set_seed, remove_setting};

// ============================================================================
// App Data Directory
// ============================================================================

static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_app_data_dir(path: PathBuf) {
    let _ = APP_DATA_DIR.set(path);
}

pub fn get_app_data_dir() -> Result<&'static PathBuf, String> {
    APP_DATA_DIR.get().ok_or_else(|| "App data directory not initialized".to_string())
}

/// Get the platform-appropriate download directory for file attachments.
/// Returns `{Downloads}/vector/` on desktop, `{data_dir}/vector_downloads/` on mobile/fallback.
pub fn get_download_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Downloads/vector");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Downloads/vector");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Downloads").join("vector");
        }
    }
    // Mobile / fallback: use data dir
    if let Ok(data_dir) = get_app_data_dir() {
        return data_dir.join("vector_downloads");
    }
    PathBuf::from("/tmp/vector_downloads")
}

// ============================================================================
// Current Account
// ============================================================================

static CURRENT_ACCOUNT: LazyLock<Arc<RwLock<Option<String>>>> = LazyLock::new(|| Arc::new(RwLock::new(None)));
static PENDING_ACCOUNT: LazyLock<Arc<RwLock<Option<String>>>> = LazyLock::new(|| Arc::new(RwLock::new(None)));

pub fn get_current_account() -> Result<String, String> {
    CURRENT_ACCOUNT.read().unwrap()
        .as_ref().cloned()
        .ok_or_else(|| "No active account".to_string())
}

pub fn set_current_account(npub: String) -> Result<(), String> {
    *CURRENT_ACCOUNT.write().unwrap() = Some(npub);
    Ok(())
}

pub fn get_pending_account() -> Result<Option<String>, String> {
    Ok(PENDING_ACCOUNT.read().unwrap().clone())
}

pub fn set_pending_account(npub: String) -> Result<(), String> {
    *PENDING_ACCOUNT.write().unwrap() = Some(npub);
    Ok(())
}

pub fn clear_pending_account() -> Result<(), String> {
    *PENDING_ACCOUNT.write().unwrap() = None;
    Ok(())
}

// ============================================================================
// Connection Pools
// ============================================================================

static DB_READ_POOL: LazyLock<Arc<Mutex<Vec<rusqlite::Connection>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

static DB_WRITE_CONN: LazyLock<Arc<Mutex<Option<rusqlite::Connection>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

/// RAII guard for READ connections — auto-returns to pool on drop.
pub struct ConnectionGuard {
    conn: Option<rusqlite::Connection>,
}

impl ConnectionGuard {
    fn new(conn: rusqlite::Connection) -> Self { Self { conn: Some(conn) } }
}

impl Deref for ConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target { self.conn.as_ref().expect("Connection already taken") }
}

impl DerefMut for ConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { self.conn.as_mut().expect("Connection already taken") }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut pool) = DB_READ_POOL.lock() {
                pool.push(conn);
            }
        }
    }
}

/// RAII guard for the WRITE connection — auto-returns on drop.
pub struct WriteConnectionGuard {
    conn: Option<rusqlite::Connection>,
}

impl WriteConnectionGuard {
    fn new(conn: rusqlite::Connection) -> Self { Self { conn: Some(conn) } }
}

impl Deref for WriteConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target { self.conn.as_ref().expect("Write connection already taken") }
}

impl DerefMut for WriteConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { self.conn.as_mut().expect("Write connection already taken") }
}

impl Drop for WriteConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            *DB_WRITE_CONN.lock().unwrap() = Some(conn);
        }
    }
}

// ============================================================================
// Connection Factory
// ============================================================================

fn get_current_db_path() -> Result<PathBuf, String> {
    let app_data = get_app_data_dir()?;
    let npub = get_current_account()?;
    Ok(app_data.join(&npub).join("vector.db"))
}

fn create_connection(path: &PathBuf) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // WAL mode for concurrent reads, busy_timeout for lock contention
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA cache_size=-1000;")
        .map_err(|e| format!("Failed to set pragmas: {}", e))?;

    Ok(conn)
}

/// Get a READ connection (headless-safe — no AppHandle).
pub fn get_db_connection_guard_static() -> Result<ConnectionGuard, String> {
    // Try to get from pool first
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        if let Some(conn) = pool.pop() {
            return Ok(ConnectionGuard::new(conn));
        }
    }
    // Create new connection
    let path = get_current_db_path()?;
    let conn = create_connection(&path)?;
    Ok(ConnectionGuard::new(conn))
}

/// Get the WRITE connection (headless-safe — no AppHandle).
pub fn get_write_connection_guard_static() -> Result<WriteConnectionGuard, String> {
    let mut write_slot = DB_WRITE_CONN.lock().unwrap();
    if let Some(conn) = write_slot.take() {
        return Ok(WriteConnectionGuard::new(conn));
    }
    drop(write_slot);

    let path = get_current_db_path()?;
    let conn = create_connection(&path)?;
    Ok(WriteConnectionGuard::new(conn))
}

// ============================================================================
// Database Initialization
// ============================================================================

/// Initialize the database for a given account (creates tables if needed).
pub fn init_database(npub: &str) -> Result<(), String> {
    let app_data = get_app_data_dir()?;
    let profile_dir = app_data.join(npub);

    if !profile_dir.exists() {
        std::fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    }

    let db_path = profile_dir.join("vector.db");
    let mut conn = create_connection(&db_path)?;
    conn.execute_batch(schema::SQL_SCHEMA)
        .map_err(|e| format!("Failed to create schema: {}", e))?;

    // Run migrations
    schema::run_migrations(&mut conn)?;

    // Pre-warm read pool
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        pool.clear();
        for _ in 0..4 {
            if let Ok(c) = create_connection(&db_path) {
                pool.push(c);
            }
        }
    }

    // Set write connection
    let write_conn = create_connection(&db_path)?;
    *DB_WRITE_CONN.lock().unwrap() = Some(write_conn);

    Ok(())
}

/// Close all database connections (for logout/account switch).
pub fn close_database() {
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        pool.clear();
    }
    *DB_WRITE_CONN.lock().unwrap() = None;
}

/// Get all available accounts (npub directories in app data).
pub fn get_accounts() -> Result<Vec<String>, String> {
    let app_data = get_app_data_dir()?;
    let mut accounts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("npub1") {
                    // Check if vector.db exists
                    if entry.path().join("vector.db").exists() {
                        accounts.push(name);
                    }
                }
            }
        }
    }

    Ok(accounts)
}

/// Get the profile directory path for a given npub.
pub fn get_profile_directory(npub: &str) -> Result<PathBuf, String> {
    let app_data = get_app_data_dir()?;
    if !npub.starts_with("npub1") {
        return Err(format!("Invalid npub format: {}", npub));
    }
    let dir = app_data.join(npub);
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    }
    Ok(dir)
}

/// Get database path for a given npub.
pub fn get_database_path(npub: &str) -> Result<PathBuf, String> {
    Ok(get_profile_directory(npub)?.join("vector.db"))
}

// ============================================================================
// ID Caches
// ============================================================================

static CHAT_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

static USER_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
}

/// Get or create a chat_id integer for a chat identifier string.
pub fn get_or_create_chat_id(conn: &rusqlite::Connection, identifier: &str, chat_type: i32) -> Result<i64, String> {
    // Check cache first
    if let Some(&id) = CHAT_ID_CACHE.read().unwrap().get(identifier) {
        return Ok(id);
    }

    // Try to find existing
    let result: Option<i64> = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![identifier],
        |row| row.get(0),
    ).ok();

    if let Some(id) = result {
        CHAT_ID_CACHE.write().unwrap().insert(identifier.to_string(), id);
        return Ok(id);
    }

    // Create new
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    conn.execute(
        "INSERT INTO chats (chat_identifier, chat_type, participants, created_at) VALUES (?1, ?2, '', ?3)",
        rusqlite::params![identifier, chat_type, now],
    ).map_err(|e| format!("Failed to create chat: {}", e))?;

    let id = conn.last_insert_rowid();
    CHAT_ID_CACHE.write().unwrap().insert(identifier.to_string(), id);
    Ok(id)
}

/// Get or create a user_id integer for an npub.
pub fn get_or_create_user_id(conn: &rusqlite::Connection, npub: &str) -> Result<i64, String> {
    if let Some(&id) = USER_ID_CACHE.read().unwrap().get(npub) {
        return Ok(id);
    }

    let result: Option<i64> = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0),
    ).ok();

    if let Some(id) = result {
        USER_ID_CACHE.write().unwrap().insert(npub.to_string(), id);
        return Ok(id);
    }

    conn.execute(
        "INSERT OR IGNORE INTO profiles (npub) VALUES (?1)",
        rusqlite::params![npub],
    ).map_err(|e| format!("Failed to create user: {}", e))?;

    let id = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to get user id: {}", e))?;

    USER_ID_CACHE.write().unwrap().insert(npub.to_string(), id);
    Ok(id)
}

// ============================================================================
// System Event Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SystemEventType {
    MemberLeft = 0,
    MemberJoined = 1,
    MemberRemoved = 2,
}

impl SystemEventType {
    pub fn display_message(&self, display_name: &str) -> String {
        match self {
            SystemEventType::MemberLeft => format!("{} has left", display_name),
            SystemEventType::MemberJoined => format!("{} has joined", display_name),
            SystemEventType::MemberRemoved => format!("{} was removed", display_name),
        }
    }

    pub fn as_u8(&self) -> u8 { *self as u8 }
}
