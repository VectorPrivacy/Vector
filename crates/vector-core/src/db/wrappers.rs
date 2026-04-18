//! Wrapper tracking — NIP-59 gift wrap dedup + NIP-77 negentropy.

use nostr_sdk::prelude::{EventId, Timestamp};

/// Persist a wrapper_event_id for cross-session dedup (INSERT OR IGNORE).
pub fn save_processed_wrapper(wrapper_id_bytes: &[u8; 32], wrapper_created_at: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR IGNORE INTO processed_wrappers (wrapper_id, wrapper_created_at) VALUES (?1, ?2)",
        rusqlite::params![&wrapper_id_bytes[..], wrapper_created_at as i64],
    ).map_err(|e| format!("Failed to save processed wrapper: {}", e))?;
    Ok(())
}

/// Upsert a wrapper timestamp (backfill for pre-migration-17 wrappers).
pub fn update_wrapper_timestamp(wrapper_id_bytes: &[u8; 32], wrapper_created_at: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT INTO processed_wrappers (wrapper_id, wrapper_created_at) VALUES (?1, ?2) \
         ON CONFLICT(wrapper_id) DO UPDATE SET wrapper_created_at = ?2 WHERE wrapper_created_at = 0",
        rusqlite::params![&wrapper_id_bytes[..], wrapper_created_at as i64],
    ).map_err(|e| format!("Failed to upsert wrapper timestamp: {}", e))?;
    Ok(())
}

/// Load all processed wrapper IDs as raw bytes for the dedup cache.
pub fn load_processed_wrappers() -> Result<Vec<[u8; 32]>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };
    let mut stmt = conn.prepare("SELECT wrapper_id FROM processed_wrappers")
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

    let mut stmt = conn.prepare(
        "SELECT wrapper_event_id FROM events \
         WHERE wrapper_event_id IS NOT NULL AND wrapper_event_id != '' \
         AND created_at >= ?1"
    ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

    let hex_ids: Vec<String> = stmt.query_map(rusqlite::params![cutoff_secs as i64], |row| {
        row.get::<_, String>(0)
    }).map_err(|e| format!("Failed to query wrapper_ids: {}", e))?
    .flatten().collect();

    let mut result = Vec::with_capacity(hex_ids.len());
    for hex in hex_ids {
        if hex.len() == 64 {
            result.push(crate::simd::hex::hex_to_bytes_32(&hex));
        }
    }
    Ok(result)
}

/// Load all processed wrappers as (EventId, Timestamp) pairs for negentropy (NIP-77).
pub fn load_negentropy_items() -> Result<Vec<(EventId, Timestamp)>, String> {
    let conn = super::get_db_connection_guard_static()
        .map_err(|_| "No DB connection".to_string())?;

    let mut stmt = conn.prepare(
        "SELECT wrapper_id, wrapper_created_at FROM processed_wrappers"
    ).map_err(|e| format!("Failed to prepare negentropy query: {}", e))?;

    let items: Vec<_> = stmt.query_map([], |row| {
        let blob: Vec<u8> = row.get(0)?;
        let created_at: i64 = row.get(1)?;
        Ok((blob, created_at))
    }).map_err(|e| format!("Failed to query processed_wrappers: {}", e))?
    .flatten()
    .filter_map(|(blob, ts)| {
        if blob.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&blob);
            Some((
                EventId::from_byte_array(arr),
                Timestamp::from_secs(ts as u64),
            ))
        } else {
            None
        }
    })
    .collect();

    Ok(items)
}
