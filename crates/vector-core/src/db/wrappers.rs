//! Wrapper tracking — NIP-59 gift wrap dedup + NIP-77 negentropy.

use nostr_sdk::prelude::{EventId, Timestamp};

/// Transport carriers for the shared outer-event ledger — stored as a small INTEGER discriminator
/// (cheaper than a per-row string, and the ledger can grow large). Never renumber an existing value.
pub const TRANSPORT_NIP17: i64 = 0;
pub const TRANSPORT_CONCORD: i64 = 1;

/// Persist an outer-event id for cross-session dedup (INSERT OR IGNORE), tagged by `transport`
/// so the ledger is shared across transports while negentropy stays NIP-17-scoped.
pub fn save_processed_wrapper(wrapper_id_bytes: &[u8; 32], wrapper_created_at: u64, transport: i64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR IGNORE INTO processed_wrappers (wrapper_id, wrapper_created_at, transport) VALUES (?1, ?2, ?3)",
        rusqlite::params![&wrapper_id_bytes[..], wrapper_created_at as i64, transport],
    ).map_err(|e| format!("Failed to save processed wrapper: {}", e))?;
    Ok(())
}

/// Sync existence check against the ledger (any transport) — the DB half of the outer-event dedup
/// for callers that can't reach the async `WRAPPER_ID_CACHE` (e.g. the synchronous Concord ingest).
/// Returns false on a missing/closed DB so a dedup failure never drops a genuinely-new event.
pub fn processed_wrapper_exists(wrapper_id_bytes: &[u8; 32]) -> bool {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM processed_wrappers WHERE wrapper_id = ?1)",
        rusqlite::params![&wrapper_id_bytes[..]],
        |row| row.get(0),
    ).unwrap_or(false)
}

/// Backfill a wrapper timestamp onto an EXISTING ledger row (pre-migration-17 rows hold 0).
/// UPDATE-only by design: the ledger is the negentropy fingerprint set, and message wrappers
/// may be deferred (batch-buffered) — an INSERT here could ledger a message before its row
/// lands, marking it "have" forever. Inserting belongs to the save paths, never this backfill.
pub fn update_wrapper_timestamp(wrapper_id_bytes: &[u8; 32], wrapper_created_at: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE processed_wrappers SET wrapper_created_at = ?2 \
         WHERE wrapper_id = ?1 AND wrapper_created_at = 0",
        rusqlite::params![&wrapper_id_bytes[..], wrapper_created_at as i64],
    ).map_err(|e| format!("Failed to backfill wrapper timestamp: {}", e))?;
    Ok(())
}

/// Load all processed wrapper IDs as raw bytes for the dedup cache.
pub fn load_processed_wrappers() -> Result<Vec<[u8; 32]>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };
    // NIP-17 only: this feeds the WRAPPER_ID_CACHE, the DM gift-wrap dedup. Concord uses the
    // synchronous ledger check (processed_wrapper_exists), not this in-memory cache.
    let mut stmt = conn.prepare("SELECT wrapper_id FROM processed_wrappers WHERE transport = 0")
        .map_err(|e| format!("Failed to prepare processed_wrappers query: {}", e))?;
    let rows = stmt.query_map([], |row| {
        let blob: Vec<u8> = row.get(0)?;
        if blob.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&blob);
            Ok(arr)
        } else {
            Err(rusqlite::Error::InvalidParameterCount(blob.len(), 32))
        }
    }).map_err(|e| format!("Failed to query processed_wrappers: {}", e))?;

    Ok(rows.flatten().collect())
}

/// [`load_processed_wrappers`] bounded to `wrapper_created_at >= since_secs` —
/// the dedup cache only needs the window the planned reconciles can touch;
/// anything older that still arrives dedups through the DB fallback.
pub fn load_processed_wrappers_since(since_secs: u64) -> Result<Vec<[u8; 32]>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare(
        "SELECT wrapper_id FROM processed_wrappers WHERE transport = 0 AND wrapper_created_at >= ?1",
    ).map_err(|e| format!("Failed to prepare processed_wrappers query: {}", e))?;
    let rows = stmt.query_map(rusqlite::params![since_secs as i64], |row| {
        let blob: Vec<u8> = row.get(0)?;
        if blob.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&blob);
            Ok(arr)
        } else {
            Err(rusqlite::Error::InvalidParameterCount(blob.len(), 32))
        }
    }).map_err(|e| format!("Failed to query processed_wrappers: {}", e))?;

    Ok(rows.flatten().collect())
}

/// Load recent wrapper IDs from events table (last N days) as raw bytes.
pub fn load_recent_wrapper_ids(days: u64) -> Result<Vec<[u8; 32]>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };

    let cutoff_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap()
        .as_secs()
        .saturating_sub(days * 24 * 60 * 60);

    // DM cache only: exclude Community chats (chat_type 2). Concord stamps its OUTER id on
    // `events.wrapper_event_id` too (atomic message dedup), so without this join those ids would warm
    // the DM gift-wrap cache — harmless (they'd never match a gift-wrap lookup) but wasteful. Concord
    // dedup uses the synchronous `processed_wrapper_exists` ledger, not this cache.
    let mut stmt = conn.prepare(
        "SELECT e.wrapper_event_id FROM events e \
         JOIN chats c ON e.chat_id = c.id \
         WHERE e.wrapper_event_id IS NOT NULL AND e.wrapper_event_id != '' \
         AND e.created_at >= ?1 AND c.chat_type != 2"
    ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

    // Decoded straight off the borrowed column text. Collecting owned `String`s
    // first would allocate once per row and hold the whole hex set resident
    // alongside the decoded one, for no gain — each id is read exactly once.
    let mut rows = stmt
        .query(rusqlite::params![cutoff_secs as i64])
        .map_err(|e| format!("Failed to query wrapper_ids: {}", e))?;

    let mut result: Vec<[u8; 32]> = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to read wrapper_id row: {}", e))?
    {
        let Ok(hex) = row.get_ref(0).and_then(|v| v.as_str().map_err(Into::into)) else {
            continue;
        };
        if hex.len() == 64 {
            result.push(crate::simd::hex::hex_to_bytes_32(hex));
        }
    }
    Ok(result)
}

/// Load all processed wrappers as (EventId, Timestamp) pairs for negentropy (NIP-77).
pub fn load_negentropy_items() -> Result<Vec<(EventId, Timestamp)>, String> {
    load_negentropy_items_inner(None)
}

/// Fingerprint items no older than `since_secs`.
///
/// Reconnect and quick reconciles cover a window of days, not all of history —
/// on an established account that is a few dozen items out of six figures, so
/// the bound belongs in SQL rather than in a filter over a fully materialised
/// set.
pub fn load_negentropy_items_since(
    since_secs: u64,
) -> Result<Vec<(EventId, Timestamp)>, String> {
    load_negentropy_items_inner(Some(since_secs))
}

fn load_negentropy_items_inner(
    since_secs: Option<u64>,
) -> Result<Vec<(EventId, Timestamp)>, String> {
    let conn = super::get_db_connection_guard_static()
        .map_err(|_| "No DB connection".to_string())?;

    // NIP-77 reconciles gift-wraps for our pubkey, so fingerprint ONLY the 'nip17' carrier.
    // Concord outer events share the ledger for dedup but must never enter DM negentropy.
    //
    // Ordered so negentropy's `seal()` sorts an already-sorted set rather than a
    // scan-ordered one. This is only cheap because `idx_processed_wrappers_neg`
    // (migration 87) carries all three columns: against an index missing
    // `wrapper_id` the same ORDER BY costs a temp B-tree and a row lookup per
    // hit, which is far more than the sort it saves.
    let sql = if since_secs.is_some() {
        "SELECT wrapper_id, wrapper_created_at FROM processed_wrappers \
         WHERE transport = 0 AND wrapper_created_at >= ?1 \
         ORDER BY wrapper_created_at, wrapper_id"
    } else {
        "SELECT wrapper_id, wrapper_created_at FROM processed_wrappers \
         WHERE transport = 0 \
         ORDER BY wrapper_created_at, wrapper_id"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare negentropy query: {}", e))?;

    // Ids are read as borrowed blobs: `get::<Vec<u8>>` would heap-allocate once
    // per row, which on a six-figure set is the bulk of this function's cost.
    let mut rows = match since_secs {
        Some(since) => stmt.query(rusqlite::params![since as i64]),
        None => stmt.query([]),
    }
    .map_err(|e| format!("Failed to query processed_wrappers: {}", e))?;

    let mut items: Vec<(EventId, Timestamp)> = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to read processed_wrappers row: {}", e))?
    {
        let Ok(blob) = row.get_ref(0).and_then(|v| v.as_blob().map_err(Into::into)) else {
            continue;
        };
        if blob.len() != 32 {
            continue;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(blob);
        // A malformed row is skipped, never fatal: callers fall back to an empty
        // set on `Err`, and an empty fingerprint set tells negentropy we hold
        // nothing, which re-downloads the entire history.
        let Ok(created_at) = row.get::<_, i64>(1) else {
            continue;
        };
        items.push((
            EventId::from_byte_array(arr),
            Timestamp::from_secs(created_at as u64),
        ));
    }

    Ok(items)
}
