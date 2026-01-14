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
    // Check if messages table exists (it may have been dropped after full migration to events)
    let has_messages_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    // Migration 1: Add wrapper_event_id column to messages table (only if table exists)
    // This column stores the public giftwrap event ID for fast duplicate detection
    if has_messages_table {
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

    // Migration 4: Add cached image path columns to profiles table
    let has_avatar_cached: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('profiles') WHERE name='avatar_cached'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_avatar_cached {
        println!("[Migration] Adding cached image columns to profiles table...");
        conn.execute(
            "ALTER TABLE profiles ADD COLUMN avatar_cached TEXT DEFAULT ''",
            []
        ).map_err(|e| format!("Failed to add avatar_cached column: {}", e))?;

        conn.execute(
            "ALTER TABLE profiles ADD COLUMN banner_cached TEXT DEFAULT ''",
            []
        ).map_err(|e| format!("Failed to add banner_cached column: {}", e))?;

        println!("[Migration] Cached image columns added successfully");
    }

    // Migration 5: Create miniapp_permissions table for storing granted permissions per-app
    let has_permissions_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='miniapp_permissions'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_permissions_table {
        println!("[Migration] Creating miniapp_permissions table...");
        conn.execute(
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
        ).map_err(|e| format!("Failed to create miniapp_permissions table: {}", e))?;

        // Create index for fast permission lookups
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_miniapp_permissions_hash ON miniapp_permissions(file_hash)",
            []
        ).map_err(|e| format!("Failed to create miniapp_permissions index: {}", e))?;

        println!("[Migration] miniapp_permissions table created successfully");
    }

    // Migration 6: Create events table for flat event-based storage
    // This is the new protocol-aligned storage format where all events (messages, reactions,
    // attachments, etc.) are stored as flat rows rather than nested JSON.
    let has_events_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_events_table {
        println!("[Migration 6] Creating events table for flat event storage...");

        // Create the events table
        conn.execute_batch(r#"
            -- Events table: flat, protocol-aligned storage for all Nostr events
            -- Every event (message, reaction, attachment, etc.) is a separate row
            CREATE TABLE IF NOT EXISTS events (
                -- Identity
                id TEXT PRIMARY KEY,              -- Event ID (hex, 64 chars)
                kind INTEGER NOT NULL,            -- Nostr kind (14=DM, 7=reaction, 15=file, etc.)

                -- Context
                chat_id INTEGER NOT NULL,         -- FK to chats table
                user_id INTEGER,                  -- FK to profiles table (sender)

                -- Content
                content TEXT NOT NULL DEFAULT '', -- Event content (encrypted for messages)
                tags TEXT NOT NULL DEFAULT '[]',  -- JSON array of Nostr tags

                -- Parsed fields for efficient queries (Vector-optimized)
                reference_id TEXT,                -- For reactions/attachments: parent message ID

                -- Timestamps
                created_at INTEGER NOT NULL,      -- Event creation (Unix seconds)
                received_at INTEGER NOT NULL,     -- When we received it (Unix ms)

                -- State flags
                mine INTEGER NOT NULL DEFAULT 0,  -- Is this from current user
                pending INTEGER NOT NULL DEFAULT 0, -- Awaiting send confirmation
                failed INTEGER NOT NULL DEFAULT 0,  -- Send failed

                -- Deduplication
                wrapper_event_id TEXT,            -- Outer giftwrap ID for sync dedup

                -- Sender info (for group chats)
                npub TEXT,                        -- Sender's npub

                -- Foreign keys
                FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE,
                FOREIGN KEY (user_id) REFERENCES profiles(id) ON DELETE SET NULL
            );

            -- Indexes for efficient queries
            CREATE INDEX IF NOT EXISTS idx_events_chat_time ON events(chat_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
            CREATE INDEX IF NOT EXISTS idx_events_reference ON events(reference_id) WHERE reference_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_events_wrapper ON events(wrapper_event_id) WHERE wrapper_event_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_events_user ON events(user_id);
        "#).map_err(|e| format!("Failed to create events table: {}", e))?;

        println!("[Migration 6] Events table created successfully");

        // Migrate existing messages to events table
        migrate_messages_to_events(conn)?;
    }

    // Migration 7: Backfill attachment metadata into event tags
    // Migration 6 didn't copy attachment JSON into tags, so kind=15 events need updating
    // NOTE: This migration requires the messages table to exist - skip if already dropped
    let storage_version: String = conn.query_row(
        "SELECT value FROM settings WHERE key = 'storage_version'",
        [],
        |row| row.get(0)
    ).unwrap_or_else(|_| "0".to_string());

    if storage_version == "2" && has_messages_table {
        // Check if there are any kind=15 events without attachment tags
        let needs_migration: i64 = conn.query_row(
            r#"
            SELECT COUNT(*) FROM events e
            WHERE e.kind = 15
            AND NOT EXISTS (
                SELECT 1 FROM json_each(e.tags)
                WHERE json_extract(value, '$[0]') = 'attachments'
            )
            "#,
            [],
            |row| row.get(0)
        ).unwrap_or(0);

        if needs_migration > 0 {
            println!("[Migration 7] Backfilling {} attachment events with metadata from messages table...", needs_migration);
            migrate_attachments_to_event_tags(conn)?;
        }
    } else if storage_version == "2" && !has_messages_table {
        // Messages table already dropped, just update version
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '3')",
            []
        ).map_err(|e| format!("Failed to update storage version: {}", e))?;
        println!("[Migration 7] Messages table already dropped, skipping attachment backfill. Storage version set to 3.");
    }

    // Migration 8: Create pivx_promos table for addressless PIVX payments
    let has_pivx_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pivx_promos'",
        [],
        |row| row.get::<_, i32>(0)
    ).map(|count| count > 0)
    .unwrap_or(false);

    if !has_pivx_table {
        println!("[Migration 8] Creating pivx_promos table...");
        conn.execute_batch(r#"
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
        "#).map_err(|e| format!("Failed to create pivx_promos table: {}", e))?;

        println!("[Migration 8] pivx_promos table created successfully");
    }

    // Migration 9: Fix DM chats with empty participants
    // DM chats (chat_type = 0) should have the other party's npub in participants
    // A bug in get_or_create_chat_id was creating DM chats with participants = '[]'
    let empty_dm_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chats WHERE chat_type = 0 AND participants = '[]'",
        [],
        |row| row.get(0)
    ).unwrap_or(0);

    if empty_dm_count > 0 {
        println!("[Migration 9] Fixing {} DM chats with empty participants...", empty_dm_count);

        // For DM chats, the chat_identifier IS the other party's npub
        // So we set participants = '["<chat_identifier>"]'
        conn.execute(
            r#"UPDATE chats
               SET participants = '["' || chat_identifier || '"]'
               WHERE chat_type = 0 AND participants = '[]'"#,
            [],
        ).map_err(|e| format!("Failed to fix DM chat participants: {}", e))?;

        println!("[Migration 9] Fixed {} DM chats with empty participants", empty_dm_count);
    }

    Ok(())
}

/// Migration 7: Copy attachment metadata from messages table into event tags
/// This completes the migration to fully self-contained events
fn migrate_attachments_to_event_tags(conn: &rusqlite::Connection) -> Result<(), String> {
    // Find all kind=15 events that don't have attachment tags yet
    let mut stmt = conn.prepare(r#"
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
    "#).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;

    let events_to_update: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("Failed to query attachments: {}", e))?
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

        conn.execute(
            "UPDATE events SET tags = ?1 WHERE id = ?2",
            rusqlite::params![new_tags, event_id]
        ).ok();

        updated_count += 1;
    }

    // Update storage version to 3
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '3')",
        []
    ).map_err(|e| format!("Failed to update storage version: {}", e))?;

    println!("[Migration 7] Backfilled {} events with attachment metadata. Storage version set to 3.", updated_count);

    Ok(())
}

/// Migrate existing messages from the old nested format to the flat events table
/// This extracts reactions and attachments as separate event rows
fn migrate_messages_to_events(conn: &rusqlite::Connection) -> Result<(), String> {
    println!("[Migration 6] Migrating messages to events table...");

    // Count existing messages
    let message_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages",
        [],
        |row| row.get(0)
    ).unwrap_or(0);

    if message_count == 0 {
        println!("[Migration 6] No messages to migrate");
        return Ok(());
    }

    println!("[Migration 6] Migrating {} messages...", message_count);

    // Step 1: Migrate text messages (kind=14) and file attachments (kind=15)
    // We need to determine kind based on whether the message has attachments
    conn.execute(r#"
        INSERT INTO events (
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
            NULL as npub
        FROM messages m
    "#, []).map_err(|e| format!("Failed to migrate messages: {}", e))?;

    // Step 2: Extract and migrate reactions from the JSON arrays
    // Get all messages with non-empty reactions
    let mut reaction_stmt = conn.prepare(
        "SELECT id, chat_id, reactions, at FROM messages WHERE reactions != '[]' AND reactions IS NOT NULL"
    ).map_err(|e| format!("Failed to prepare reaction query: {}", e))?;

    let reaction_rows: Vec<(String, i64, String, i64)> = reaction_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| format!("Failed to query reactions: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

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
                    // Insert reaction as separate event
                    let tags = serde_json::json!([["e", message_id]]).to_string();

                    conn.execute(
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
                    ).ok(); // Ignore errors for individual reactions

                    reaction_count += 1;
                }
            }
        }
    }

    println!("[Migration 6] Migrated {} reactions", reaction_count);

    // Step 3: Mark migration as complete by storing version in settings
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_version', '2')",
        []
    ).map_err(|e| format!("Failed to update storage version: {}", e))?;

    println!("[Migration 6] Migration complete. Storage version set to 2.");

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