//! Database schema and migrations.

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

-- Events table: flat, protocol-aligned storage for all Nostr events
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

-- PIVX Promos table
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

-- Mini Apps history table
CREATE TABLE IF NOT EXISTS miniapps_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    src_url TEXT NOT NULL,
    attachment_ref TEXT,
    open_count INTEGER DEFAULT 1,
    last_opened_at INTEGER NOT NULL,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    categories TEXT NOT NULL DEFAULT '',
    marketplace_id TEXT DEFAULT NULL,
    installed_version TEXT DEFAULT NULL
);

-- Mini App permissions table
CREATE TABLE IF NOT EXISTS miniapp_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_hash TEXT NOT NULL,
    permission TEXT NOT NULL,
    granted INTEGER NOT NULL DEFAULT 0,
    granted_at INTEGER,
    UNIQUE(file_hash, permission)
);
CREATE INDEX IF NOT EXISTS idx_miniapp_permissions_hash ON miniapp_permissions(file_hash);

-- MLS processed events table (tracks which MLS wrapper events have been processed)
CREATE TABLE IF NOT EXISTS mls_processed_events (
    event_id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    processed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mls_processed_events_group ON mls_processed_events(group_id);
CREATE INDEX IF NOT EXISTS idx_mls_processed_events_created ON mls_processed_events(created_at);

-- Processed wrappers table (NIP-59 gift wrap dedup + NIP-77 negentropy)
CREATE TABLE IF NOT EXISTS processed_wrappers (
    wrapper_id BLOB PRIMARY KEY,
    wrapper_created_at INTEGER NOT NULL DEFAULT 0
);

-- Schema migrations tracking table
CREATE TABLE IF NOT EXISTS schema_migrations (
    id INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
"#;

/// Check if a specific migration has already been applied
pub fn migration_applied(conn: &rusqlite::Connection, migration_id: u32) -> bool {
    conn.query_row(
        "SELECT 1 FROM schema_migrations WHERE id = ?1",
        rusqlite::params![migration_id],
        |_| Ok(())
    ).is_ok()
}

/// Mark a migration as applied (within a transaction)
pub fn mark_migration_applied(tx: &rusqlite::Transaction, migration_id: u32) -> Result<(), String> {
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
#[allow(dead_code)]
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
pub fn run_migrations(conn: &mut rusqlite::Connection) -> Result<(), String> {
    // Ensure schema_migrations table exists (bootstrap - must succeed before any migrations)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            id INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("[DB] Failed to create schema_migrations table: {}", e))?;

    // =========================================================================
    // Migration 19: Create marketplace_cache table for persistent Nexus cache
    // =========================================================================
    // Caches marketplace app listings in SQLite so they survive restarts.
    // On login, the cache is loaded into MARKETPLACE_STATE immediately (so
    // permission checks work before the user visits the Nexus tab), then a
    // background network fetch refreshes the data.
    run_atomic_migration(conn, 19, "Create marketplace_cache table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS marketplace_cache (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            );"
        ).map_err(|e| format!("Failed to create marketplace_cache table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 20: Add is_blocked column to profiles table
    // =========================================================================
    // Supports user blocking: blocked profiles have DM events dropped after
    // decrypt (wrapper kept for negentropy), group messages filtered in UI.
    run_atomic_migration(conn, 20, "Add is_blocked column to profiles", |tx| {
        tx.execute_batch(
            "ALTER TABLE profiles ADD COLUMN is_blocked INTEGER NOT NULL DEFAULT 0;"
        ).map_err(|e| format!("Failed to add is_blocked column: {}", e))?;
        Ok(())
    })?;

    Ok(())
}
