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


-- Settings table (key-value pairs)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
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

-- Processed wrappers table (NIP-59 gift wrap dedup + NIP-77 negentropy)
-- Universal outer-event ledger across transports. The `transport` discriminator
-- (0 = nip17 gift-wrap, 1 = concord channel envelope, …) is added by migration 42 so the
-- dedup is shared but NIP-77 negentropy only fingerprints the nip17 (0) subset.
CREATE TABLE IF NOT EXISTS processed_wrappers (
    wrapper_id BLOB PRIMARY KEY,
    wrapper_created_at INTEGER NOT NULL DEFAULT 0
);

-- The nip17_wrap_keys vault is introduced by migration 21. The legacy MLS
-- tables (mls_wrap_keys / mls_pending_events from migrations 22/23) are dropped
-- by migration 41, so on a fresh DB they're created in order and then removed.

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

    // =========================================================================
    // Migration 21: NIP-17 ephemeral wrap-key vault for deletable DMs
    // =========================================================================
    // Stores the ephemeral secp256k1 secret used to sign each kind-1059
    // gift-wrap so the user can later publish a NIP-09 deletion against
    // the wrap event ID — actually removing the message from inbox relays.
    // Encryption-at-rest is handled by Vector's per-account database
    // envelope (ChaCha20 if the account has a password; plaintext otherwise).
    // One row per published wrap; deletion uses (wrap_event_id, secret,
    // relay_urls) to issue an author-signed NIP-09 to the same relay set.
    run_atomic_migration(conn, 21, "Create nip17_wrap_keys table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS nip17_wrap_keys (
                wrap_event_id    TEXT PRIMARY KEY,
                rumor_id         TEXT NOT NULL,
                recipient_pubkey TEXT NOT NULL,
                role             INTEGER NOT NULL,
                secret           BLOB NOT NULL,
                relay_urls       TEXT NOT NULL,
                created_at       INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_nip17_wrap_keys_rumor ON nip17_wrap_keys(rumor_id);"
        ).map_err(|e| format!("Failed to create nip17_wrap_keys table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 22: MLS ephemeral wrap-key vault for deletable group messages
    // =========================================================================
    // Sibling of nip17_wrap_keys: every kind-445 MLS wrapper is signed by an
    // ephemeral keypair that MDK normally discards. With our `create_message_retained`
    // patch the sender retains the secret so a later NIP-09 deletion against
    // the kind-445 event id is valid (NIP-09 requires `event.pubkey ==
    // deletion.pubkey`). One row per published wrapper; retries write new rows.
    run_atomic_migration(conn, 22, "Create mls_wrap_keys table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS mls_wrap_keys (
                wrap_event_id TEXT PRIMARY KEY,
                message_id    TEXT NOT NULL,
                group_id      TEXT NOT NULL,
                secret        BLOB NOT NULL,
                relay_urls    TEXT NOT NULL,
                created_at    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mls_wrap_keys_message ON mls_wrap_keys(message_id);
            CREATE INDEX IF NOT EXISTS idx_mls_wrap_keys_group ON mls_wrap_keys(group_id);"
        ).map_err(|e| format!("Failed to create mls_wrap_keys table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 23: MLS pending event queue for cross-sync retry
    // =========================================================================
    // When MDK can't process an MLS event because its prerequisite commit
    // hasn't arrived, we previously marked it "processed" and advanced the
    // cursor past it — losing it forever. This table persists such events
    // so subsequent syncs can retry once the prerequisite shows up (possibly
    // from a different relay, days or weeks later). Pruned at 90 days.
    run_atomic_migration(conn, 23, "Create mls_pending_events table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS mls_pending_events (
                event_id      TEXT PRIMARY KEY,
                group_id      TEXT NOT NULL,
                event_json    TEXT NOT NULL,
                first_seen_at INTEGER NOT NULL,
                last_retry_at INTEGER NOT NULL,
                retry_count   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_mls_pending_events_group ON mls_pending_events(group_id);
            CREATE INDEX IF NOT EXISTS idx_mls_pending_events_first_seen ON mls_pending_events(first_seen_at);"
        ).map_err(|e| format!("Failed to create mls_pending_events table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 24: Blossom capability cache — drives smart upload routing.
    // =========================================================================
    run_atomic_migration(conn, 24, "Create blossom_server_capabilities table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS blossom_server_capabilities (
                server_url        TEXT    NOT NULL,
                mime_type         TEXT    NOT NULL,
                outcome           INTEGER NOT NULL,
                max_accepted_size INTEGER NOT NULL DEFAULT 0,
                updated_at        INTEGER NOT NULL,
                PRIMARY KEY (server_url, mime_type)
            );"
        ).map_err(|e| format!("Failed to create blossom_server_capabilities table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 25: Add `min_rejected_size` (smallest observed 413).
    // =========================================================================
    run_atomic_migration(conn, 25, "Add min_rejected_size to blossom_server_capabilities", |tx| {
        tx.execute_batch(
            "ALTER TABLE blossom_server_capabilities ADD COLUMN min_rejected_size INTEGER;"
        ).map_err(|e| format!("Failed to add min_rejected_size column: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 26: Split capability rows by encrypted vs plaintext context.
    // Same wire MIME means different things for ciphertext vs real bytes;
    // pre-migration rows didn't track the distinction so they're dropped.
    // =========================================================================
    run_atomic_migration(conn, 26, "Add is_encrypted to capability cache PK", |tx| {
        tx.execute_batch(
            "DROP TABLE IF EXISTS blossom_server_capabilities;
             CREATE TABLE blossom_server_capabilities (
                server_url        TEXT    NOT NULL,
                mime_type         TEXT    NOT NULL,
                is_encrypted      INTEGER NOT NULL DEFAULT 0,
                outcome           INTEGER NOT NULL,
                max_accepted_size INTEGER NOT NULL DEFAULT 0,
                min_rejected_size INTEGER,
                updated_at        INTEGER NOT NULL,
                PRIMARY KEY (server_url, mime_type, is_encrypted)
             );"
        ).map_err(|e| format!("Failed to recreate blossom_server_capabilities: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 27: Mark NIP-46 remote-signer support landed.
    //
    // Settings is a KV — no schema change needed for the three new keys
    // (`signer_type`, `bunker_url`, `bunker_remote_pubkey`). Pre-bunker
    // accounts have no `signer_type` row at all; the loader treats missing
    // as `local`. We backfill an explicit `signer_type='local'` row so every
    // account has a discriminator on disk after this point — makes the
    // discriminator query a clean `=` instead of a NULL-coalesce.
    // =========================================================================
    run_atomic_migration(conn, 27, "Backfill signer_type=local for pre-NIP-46 accounts", |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES ('signer_type', 'local')",
            [],
        ).map_err(|e| format!("Failed to backfill signer_type: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 28: NIP-30 / NIP-51 custom emoji packs
    // =========================================================================
    // `emoji_packs`           — kind 30030 sets (own + subscribed), one row per addr.
    // `emoji_pack_items`      — flattened emoji rows per pack; CASCADE deletes follow.
    // `emoji_pack_subscriptions` — local mirror of kind 10030 `a` tags; fast startup
    //                              read without re-fetching from relays.
    run_atomic_migration(conn, 28, "Create emoji pack tables", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS emoji_packs (
                addr        TEXT PRIMARY KEY,
                pubkey      TEXT NOT NULL,
                identifier  TEXT NOT NULL,
                title       TEXT NOT NULL DEFAULT '',
                image_url   TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                is_own      INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL,
                raw_event   TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_emoji_packs_pubkey ON emoji_packs(pubkey);
            CREATE INDEX IF NOT EXISTS idx_emoji_packs_is_own ON emoji_packs(is_own);

            CREATE TABLE IF NOT EXISTS emoji_pack_items (
                pack_addr  TEXT NOT NULL,
                shortcode  TEXT NOT NULL,
                url        TEXT NOT NULL,
                sha256     TEXT,
                position   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (pack_addr, shortcode),
                FOREIGN KEY (pack_addr) REFERENCES emoji_packs(addr) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_emoji_pack_items_pack ON emoji_pack_items(pack_addr, position);

            CREATE TABLE IF NOT EXISTS emoji_pack_subscriptions (
                addr           TEXT PRIMARY KEY,
                subscribed_at  INTEGER NOT NULL
            );"
        ).map_err(|e| format!("Failed to create emoji pack tables: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 29: Add per-DM wallpaper columns to chats
    // =========================================================================
    // Wallpaper is the local cached file path (decrypted from the Blossom
    // attachment carried by the most recent kind-30078 d=vector-wallpaper rumor
    // for this chat). wallpaper_ts is the rumor created_at that produced it,
    // used for latest-write-wins on concurrent sets.
    run_atomic_migration(conn, 29, "Add wallpaper columns to chats", |tx| {
        tx.execute_batch(
            "ALTER TABLE chats ADD COLUMN wallpaper_path TEXT NOT NULL DEFAULT '';
             ALTER TABLE chats ADD COLUMN wallpaper_ts INTEGER NOT NULL DEFAULT 0;"
        ).map_err(|e| format!("Failed to add wallpaper columns: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 30: Wallpaper customisation knobs (blur + brightness)
    // =========================================================================
    // blur: integer pixels, 0..=30 (0 = no blur).
    // dim:  integer percent, 0..=100 (100 = no darkening, 0 = fully black).
    // Defaults match the values applied when a rumor arrives without the
    // optional tags — keeps older clients interoperable.
    run_atomic_migration(conn, 30, "Add wallpaper blur/dim columns to chats", |tx| {
        tx.execute_batch(
            "ALTER TABLE chats ADD COLUMN wallpaper_blur INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE chats ADD COLUMN wallpaper_dim INTEGER NOT NULL DEFAULT 50;"
        ).map_err(|e| format!("Failed to add wallpaper blur/dim columns: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 31: Track wallpaper Blossom URL + uploader pubkey
    // =========================================================================
    // wallpaper_url is the Blossom blob URL of the current wallpaper.
    // wallpaper_uploader is the npub (bech32) of whoever uploaded it. Together
    // they let us DELETE the previous blob from Blossom when we (or another
    // device of ours) replace the wallpaper — only the original uploader's
    // signature satisfies the server's auth challenge.
    run_atomic_migration(conn, 31, "Add wallpaper url/uploader columns to chats", |tx| {
        tx.execute_batch(
            "ALTER TABLE chats ADD COLUMN wallpaper_url TEXT NOT NULL DEFAULT '';
             ALTER TABLE chats ADD COLUMN wallpaper_uploader TEXT NOT NULL DEFAULT '';"
        ).map_err(|e| format!("Failed to add wallpaper url/uploader columns: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 32: Drop mls_event_cursors — superseded by Total Negentropy
    // =========================================================================
    // MLS sync no longer tracks a per-group cursor. Possession of an event
    // (mls_processed_events ∪ mls_pending_events) is the negentropy fingerprint
    // set, and reconciliation derives the missing set directly. The cursor was
    // a pre-negentropy resume mechanism that could only disagree with it.
    run_atomic_migration(conn, 32, "Drop mls_event_cursors table", |tx| {
        tx.execute_batch("DROP TABLE IF EXISTS mls_event_cursors;")
            .map_err(|e| format!("Failed to drop mls_event_cursors: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // GAP: migration ids 33-39 are PERMANENTLY BURNED — do not reuse.
    // =========================================================================
    // The distributed v0.4.0 "MLS edition" shipped MLS migrations in the 33-39 range that
    // never made it into committed history (its release branch was later squashed to max 32).
    // Migrations are tracked per-id (`schema_migrations`), not by a monotonic counter, so an
    // MLS-edition DB has 33-39 recorded and would SKIP any new migration reusing those ids,
    // silently never creating the table. Community state therefore starts at 40. Never fill
    // the 33-39 gap, even though it looks tidy — those ids are spent forever.
    //
    // Migration 40: Community (Concord) protocol local state
    // =========================================================================
    // Per-account (the DB itself is account-scoped via account_dir(npub)). Holds the
    // owner/member's held secrets (server-root key, epoch-tagged channel keys), the folded
    // control-plane state, and local invite/dedup bookkeeping. Ids are hex. Authority is
    // keyless: real-npub control editions + the owner attestation, never a shared secret.
    run_atomic_migration(conn, 40, "Create community tables", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS communities (
                community_id          TEXT PRIMARY KEY,
                server_root_key       BLOB NOT NULL,
                name                  TEXT NOT NULL,
                relays                TEXT NOT NULL,
                created_at            INTEGER NOT NULL,
                description           TEXT,
                icon                  TEXT,
                banner                TEXT,
                banlist               TEXT NOT NULL DEFAULT '[]',
                banlist_at            INTEGER NOT NULL DEFAULT 0,
                owner_attestation     TEXT,
                roles                 TEXT NOT NULL DEFAULT '{}',
                roles_at              INTEGER NOT NULL DEFAULT 0,
                server_root_epoch     INTEGER NOT NULL DEFAULT 0,
                invite_registry       TEXT NOT NULL DEFAULT '[]',
                read_cut_pending      INTEGER NOT NULL DEFAULT 0,
                read_cut_target_epoch INTEGER NOT NULL DEFAULT 0,
                dissolved             INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS community_channels (
                channel_id              TEXT PRIMARY KEY,
                community_id            TEXT NOT NULL,
                channel_key             BLOB NOT NULL,
                epoch                   INTEGER NOT NULL,
                name                    TEXT NOT NULL,
                created_at              INTEGER NOT NULL,
                rekeyed_at_server_epoch INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_community_channels_community
                ON community_channels(community_id);
            CREATE TABLE IF NOT EXISTS community_message_keys (
                outer_event_id   TEXT PRIMARY KEY,
                ephemeral_secret BLOB NOT NULL,
                relays           TEXT NOT NULL,
                created_at       INTEGER NOT NULL,
                message_id       TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_cmk_message_id
                ON community_message_keys(message_id);
            CREATE TABLE IF NOT EXISTS pending_community_invites (
                community_id TEXT PRIMARY KEY,
                bundle_json  TEXT NOT NULL,
                inviter_npub TEXT NOT NULL,
                received_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS community_public_invites (
                token        TEXT PRIMARY KEY,
                community_id TEXT NOT NULL,
                url          TEXT NOT NULL,
                expires_at   INTEGER,
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_public_invites_community
                ON community_public_invites(community_id);
            CREATE TABLE IF NOT EXISTS community_edition_heads (
                community_id TEXT NOT NULL,
                entity_id    TEXT NOT NULL,
                version      INTEGER NOT NULL,
                self_hash    BLOB NOT NULL,
                inner_id     BLOB,
                epoch        INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (community_id, entity_id)
            );
            CREATE TABLE IF NOT EXISTS community_epoch_keys (
                community_id TEXT NOT NULL,
                scope_id     TEXT NOT NULL,
                epoch        INTEGER NOT NULL,
                key          BLOB NOT NULL,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (community_id, scope_id, epoch)
            );
            CREATE TABLE IF NOT EXISTS community_invite_link_sets (
                community_id TEXT NOT NULL,
                creator      TEXT NOT NULL,
                locators     TEXT NOT NULL DEFAULT '[]',
                version      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (community_id, creator)
            );",
        )
        .map_err(|e| format!("Failed to create community tables: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 41: Purge legacy MLS data (MLS is fully removed)
    // =========================================================================
    // Drop the retired chat_type=1 (MlsGroup) chats + their events, then the MLS-only
    // storage tables. chat_type 2 (Community) is untouched. Runs for accounts upgrading
    // from an MLS build; a no-op on a fresh DB.
    run_atomic_migration(conn, 41, "Purge legacy MLS data", |tx| {
        tx.execute_batch(
            "DELETE FROM events WHERE chat_id IN (SELECT id FROM chats WHERE chat_type = 1);
             DELETE FROM chats WHERE chat_type = 1;
             DROP TABLE IF EXISTS mls_groups;
             DROP TABLE IF EXISTS mls_keypackages;
             DROP TABLE IF EXISTS mls_processed_events;
             DROP TABLE IF EXISTS mls_wrap_keys;
             DROP TABLE IF EXISTS mls_pending_events;",
        )
        .map_err(|e| format!("Failed to purge legacy MLS data: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 42: Make processed_wrappers a cross-transport dedup ledger
    // =========================================================================
    // A `transport` discriminator so every transport (NIP-17 DMs, Concord) shares ONE
    // outer-event dedup store, while NIP-77 negentropy keeps fingerprinting only the 'nip17'
    // subset. Existing rows are gift-wraps, so the default 0 ('nip17') is correct.
    run_atomic_migration(conn, 42, "Add transport discriminator to processed_wrappers", |tx| {
        tx.execute_batch("ALTER TABLE processed_wrappers ADD COLUMN transport INTEGER NOT NULL DEFAULT 0;")
            .map_err(|e| format!("Failed to add transport column: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 43: Persist the optional label on a minted public invite
    // =========================================================================
    // The label set at mint time rides in the relay-published bundle (join attribution) but wasn't
    // stored locally, so the owner's invite-links list had no label to show. Encrypted-at-rest like
    // the sibling columns; NULL when no label was set.
    run_atomic_migration(conn, 43, "Add label to community_public_invites", |tx| {
        tx.execute_batch("ALTER TABLE community_public_invites ADD COLUMN label TEXT;")
            .map_err(|e| format!("Failed to add label column: {}", e))?;
        Ok(())
    })?;

    // Migration 44: Per-account emoji "frecency" (most-used) table.
    // =========================================================================
    // `score` is a time-weighted log-space value: each use adds
    // 2^((t-EPOCH)/half_life), so ranking is a plain `ORDER BY score DESC` (the
    // uniform decay factor cancels) — no per-row decay math at read time. `kind`:
    // 0=unicode, 1=custom. WITHOUT ROWID + (kind,id) PK so a reuse is an in-place
    // upsert (one row per emoji), not an append.
    run_atomic_migration(conn, 44, "Create emoji_usage table", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS emoji_usage (
                kind      INTEGER NOT NULL,
                id        TEXT    NOT NULL,
                url       TEXT,
                score     REAL    NOT NULL,
                last_used INTEGER NOT NULL,
                PRIMARY KEY (kind, id)
            ) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS idx_emoji_usage_score
                ON emoji_usage(score DESC);",
        )
        .map_err(|e| format!("Failed to create emoji_usage table: {}", e))?;
        Ok(())
    })?;

    // Migration 62: Repair — guarantee `label` exists on community_public_invites. Id 43 (which adds it)
    // was burned on DBs created from an older baseline: recorded as applied without the ALTER ever landing,
    // so `label` is silently absent and list_all_public_invites errors. Use a fresh id past every recorded
    // one (DBs already hold up to 61) and add the column only if missing, so it's a no-op where 43 worked.
    run_atomic_migration(conn, 62, "Repair: ensure label column on community_public_invites", |tx| {
        let has_label: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('community_public_invites') WHERE name = 'label'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("Failed to inspect community_public_invites columns: {}", e))?;
        if has_label == 0 {
            tx.execute_batch("ALTER TABLE community_public_invites ADD COLUMN label TEXT;")
                .map_err(|e| format!("Failed to add label column: {}", e))?;
        }
        Ok(())
    })?;

    // =========================================================================
    // Migration 63: Emoji pack health (revocation / durable-absence tracking)
    // =========================================================================
    // `status`: 0 = active, 1 = revoked (a deterministic tombstone was seen: an
    // empty kind-30030 replacement, or an author-signed kind-5 deletion),
    // 2 = missing (absent across enough clean relay sweeps). The miss columns
    // drive the promotion gauntlet in `emoji_packs::apply_pack_health`; a live
    // fetch resets everything back to active.
    run_atomic_migration(conn, 63, "Add health columns to emoji_packs", |tx| {
        tx.execute_batch(
            "ALTER TABLE emoji_packs ADD COLUMN status INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE emoji_packs ADD COLUMN miss_count INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE emoji_packs ADD COLUMN first_missed_at INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE emoji_packs ADD COLUMN last_miss_counted_at INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE emoji_packs ADD COLUMN status_changed_at INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(|e| format!("Failed to add emoji pack health columns: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 64: Drop orphaned pending-id event rows
    // =========================================================================
    // Mid-flight persists could land a row under a message's optimistic
    // "pending-…" id; the finalized message then saved under its REAL id,
    // orphaning the pending-keyed row as a ghost duplicate that renders on
    // reload. Rows still flagged pending/failed are live send-state (the
    // retry UI needs them) and stay.
    run_atomic_migration(conn, 64, "Drop orphaned pending-id event rows", |tx| {
        tx.execute(
            "DELETE FROM events WHERE id LIKE 'pending-%' AND pending = 0 AND failed = 0",
            [],
        )
        .map_err(|e| format!("Failed to drop orphaned pending rows: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 65: Add position to emoji pack subscriptions
    // =========================================================================
    // `subscribed_at` alone can't hold a user-defined order — save_subscriptions
    // rewrites every row with the same `now`, so ties are unordered. `position`
    // is the authoritative display order (cross-device synced via kind 10030).
    // Backfill preserves the current rowid order so existing installs don't
    // reshuffle on first launch.
    run_atomic_migration(conn, 65, "Add position to emoji pack subscriptions", |tx| {
        tx.execute_batch(
            "ALTER TABLE emoji_pack_subscriptions ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
             UPDATE emoji_pack_subscriptions SET position = (
                 SELECT COUNT(*) FROM emoji_pack_subscriptions s2
                 WHERE s2.rowid < emoji_pack_subscriptions.rowid
             );",
        )
        .map_err(|e| format!("Failed to add position column: {}", e))?;
        Ok(())
    })?;

    // Migration 66: Concord v2 dual-stack columns. A community is v1 (the shipped
    // protocol) or v2 (the self-certifying-id CORD stack); the two coexist per
    // account. Existing rows default to v1. v2 stores the owner commitment inputs
    // (owner_pubkey + owner_salt reproduce the community_id) in place of v1's
    // owner_attestation; server_root_key/server_root_epoch carry the v2
    // community_root/root_epoch (same base-key role, reused columns). A channel's
    // `private` flag selects v2 keying: public channels derive from the root (no
    // stored key), private ones carry an independent key.
    run_atomic_migration(conn, 66, "Concord v2 dual-stack columns", |tx| {
        for (table, col, ddl) in [
            ("communities", "protocol", "INTEGER NOT NULL DEFAULT 1"),
            ("communities", "owner_pubkey", "TEXT"),
            ("communities", "owner_salt", "TEXT"),
            ("community_channels", "private", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            // ADD COLUMN is not idempotent; tolerate a re-run (duplicate column).
            let sql = format!("ALTER TABLE {table} ADD COLUMN {col} {ddl}");
            if let Err(e) = tx.execute(&sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(format!("add {table}.{col}: {msg}"));
                }
            }
        }
        Ok(())
    })?;

    // Migration 67: the persisted v2 Guestbook — the RAW membership events (one
    // encrypted JSON blob per community; kick/snapshot validity is judged at fold
    // time against CURRENT authority, so raw events are the correct stored form)
    // plus the newest-seen cursor, so boot catches the plane up incrementally and
    // the memberlist becomes a local read.
    run_atomic_migration(conn, 67, "v2 guestbook store", |tx| {
        tx.execute(
            "CREATE TABLE IF NOT EXISTS community_guestbook (
                community_id TEXT PRIMARY KEY,
                events TEXT NOT NULL,
                cursor_secs INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .map_err(|e| format!("create community_guestbook: {e}"))?;
        Ok(())
    })?;

    // Migration 68: the CORD-02 §6 preservation stash — vsk fields Vector doesn't
    // drive (voice, client `custom`, unknown `extra`) persist beside the entity so
    // our own editions republish the FULL document instead of wiping them.
    run_atomic_migration(conn, 68, "v2 metadata preservation stash", |tx| {
        for (table, col) in [("communities", "meta_extra"), ("community_channels", "meta_extra")] {
            let sql = format!("ALTER TABLE {table} ADD COLUMN {col} TEXT");
            if let Err(e) = tx.execute(&sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(format!("add {table}.{col}: {msg}"));
                }
            }
        }
        Ok(())
    })?;

    // Migration 69: last-known bot manifests (kind 10304) so the `/` command
    // picker serves instantly from boot; a background refetch replaces a row
    // only with a newer edition. Manifests are PUBLIC replaceable events, so
    // rows are plaintext (unlike membership/community state).
    run_atomic_migration(conn, 69, "bot manifest store", |tx| {
        tx.execute(
            "CREATE TABLE IF NOT EXISTS bot_manifests (
                pubkey TEXT PRIMARY KEY,
                manifest TEXT NOT NULL,
                event_created_at INTEGER NOT NULL,
                fetched_at INTEGER NOT NULL
            )",
            [],
        )
        .map_err(|e| format!("create bot_manifests: {e}"))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 70: Retained gift-wrap body for idempotent manual retry
    // =========================================================================
    // A failed (red) DM whose wrap silently landed would double-post on manual
    // Retry, because Retry rebuilt a fresh wrap with a new outer id. Retaining
    // the exact recipient wrap event (+ its rumor, + the local pending id to
    // look it up by) lets Retry republish the byte-identical event: relays
    // no-op the duplicate, so duplication is impossible regardless of client.
    // The body columns are nulled the instant the send is confirmed (a relay
    // OK), so steady-state they are NULL — only unsent messages carry a body.
    run_atomic_migration(conn, 70, "Retained gift-wrap body for idempotent retry", |tx| {
        tx.execute_batch(
            "ALTER TABLE nip17_wrap_keys ADD COLUMN wrap_json  TEXT;
             ALTER TABLE nip17_wrap_keys ADD COLUMN rumor_json TEXT;
             ALTER TABLE nip17_wrap_keys ADD COLUMN pending_id TEXT;
             CREATE INDEX IF NOT EXISTS idx_nip17_wrap_keys_pending ON nip17_wrap_keys(pending_id);"
        ).map_err(|e| format!("Failed to add resend-payload columns: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 71: Covering index for the unread-count query
    // =========================================================================
    // Column order = (chat_id, mine, kind) equality then a created_at range; the four columns
    // cover both the per-chat anchor MAX and the count, so neither touches the table.
    run_atomic_migration(conn, 71, "Covering index for unread counts", |tx| {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_events_unread ON events(chat_id, mine, kind, created_at);"
        ).map_err(|e| format!("Failed to create unread covering index: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 72: Drop the unused events(user_id) index
    // =========================================================================
    // No query filters, joins, or orders by events.user_id, so the index only
    // cost a b-tree write on every event insert. Authors resolve via the
    // denormalized npub column instead.
    run_atomic_migration(conn, 72, "Drop unused events user_id index", |tx| {
        tx.execute_batch("DROP INDEX IF EXISTS idx_events_user;")
            .map_err(|e| format!("Failed to drop idx_events_user: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 73: Drop the legacy `messages` table
    // =========================================================================
    // Superseded by `events` at v0.3.1 (its data + attachment metadata were
    // copied over then). The public app has shipped on `events` since v0.4.0,
    // so no live account writes or reads `messages`. DROP takes its indexes too.
    run_atomic_migration(conn, 73, "Drop legacy messages table", |tx| {
        tx.execute_batch("DROP TABLE IF EXISTS messages;")
            .map_err(|e| format!("Failed to drop legacy messages table: {}", e))?;
        Ok(())
    })?;

    // =========================================================================
    // Migration 74: Dedicated attachments table + backfill from event tags
    // =========================================================================
    // Attachments lived as a `["attachments", <json>]` entry inside events.tags,
    // making dedup a LIKE scan, the integrity check a per-event JSON parse, and
    // every download flip a read-modify-write of the whole tags blob. Normalize
    // into one row per attachment, keyed to its event (cascade on delete) and
    // indexed by content hash. Backfill from the existing tags in this same
    // transaction; the original tag is LEFT IN PLACE as a safety net (the table
    // is authoritative, but no data is destroyed) until a later release strips it.
    // Tags are plaintext at rest (only content is encrypted), so no decrypt here.
    run_atomic_migration(conn, 74, "Attachments table + backfill", |tx| {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS attachments (
                id           INTEGER PRIMARY KEY,
                event_id     TEXT NOT NULL,
                att_index    INTEGER NOT NULL,
                hash         TEXT NOT NULL,
                key          TEXT NOT NULL DEFAULT '',
                nonce        TEXT NOT NULL DEFAULT '',
                extension    TEXT NOT NULL DEFAULT '',
                name         TEXT NOT NULL DEFAULT '',
                url          TEXT NOT NULL DEFAULT '',
                path         TEXT NOT NULL DEFAULT '',
                size         INTEGER NOT NULL DEFAULT 0,
                img_meta     TEXT,
                downloaded   INTEGER NOT NULL DEFAULT 0,
                webxdc_topic TEXT, group_id TEXT, original_hash TEXT, scheme_version TEXT, mls_filename TEXT,
                UNIQUE(event_id, att_index),
                FOREIGN KEY (event_id) REFERENCES events(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_attachments_hash       ON attachments(hash);
            CREATE INDEX IF NOT EXISTS idx_attachments_downloaded ON attachments(downloaded) WHERE downloaded = 1;"
        ).map_err(|e| format!("Failed to create attachments table: {}", e))?;

        // Backfill: parse each event's attachments tag and insert one row per attachment.
        let events: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, tags FROM events WHERE tags LIKE '%attachments%'")
                .map_err(|e| format!("prepare attachment backfill: {}", e))?;
            let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| format!("query attachment backfill: {}", e))?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        for (event_id, tags_json) in events {
            let tags: Vec<Vec<String>> = match serde_json::from_str(&tags_json) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let att_json = tags.iter()
                .find(|t| t.first().map(|s| s.as_str()) == Some("attachments"))
                .and_then(|t| t.get(1));
            let Some(att_json) = att_json else { continue };
            let atts: Vec<crate::types::Attachment> = match serde_json::from_str(att_json) {
                Ok(a) => a,
                Err(_) => continue,
            };
            for (i, a) in atts.iter().enumerate() {
                let img_meta_json = a.img_meta.as_ref().and_then(|m| serde_json::to_string(m).ok());
                tx.execute(
                    "INSERT INTO attachments (event_id, att_index, hash, key, nonce, extension, name, url, \
                     path, size, img_meta, downloaded, webxdc_topic, group_id, original_hash, scheme_version, mls_filename) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                    rusqlite::params![
                        event_id, i as i64, a.id, a.key, a.nonce, a.extension, a.name, a.url,
                        a.path, a.size as i64, img_meta_json, a.downloaded as i64,
                        a.webxdc_topic, a.group_id, a.original_hash, a.scheme_version, a.mls_filename,
                    ],
                ).map_err(|e| format!("insert backfilled attachment: {}", e))?;
            }
        }
        Ok(())
    })?;

    Ok(())
}
