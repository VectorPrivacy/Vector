//! Per-scope notification preferences: one row per community, channel or DM
//! that deviates from the defaults. Absence of a row means "inherit
//! everything", so an untouched account stores nothing.

use std::collections::HashMap;

use crate::notify::ScopePrefs;

fn row_to_prefs(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, ScopePrefs)> {
    let id: String = row.get(0)?;
    let level: Option<u8> = row.get::<_, Option<i64>>(1)?.map(|v| v as u8);
    let mute_until: i64 = row.get(2)?;
    let suppress: Option<bool> = row.get::<_, Option<i64>>(3)?.map(|v| v != 0);
    Ok((id, ScopePrefs {
        level: level.and_then(crate::notify::NotifyLevel::from_u8),
        mute_until,
        suppress_everyone: suppress,
    }))
}

/// Every stored scope. The table holds only customised scopes, so this stays
/// small enough to keep in memory rather than querying per lookup.
pub fn load_all() -> Result<HashMap<String, ScopePrefs>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT scope_id, level, mute_until, suppress_everyone FROM notify_prefs")
        .map_err(|e| format!("prepare notify_prefs: {e}"))?;
    let rows = stmt
        .query_map([], row_to_prefs)
        .map_err(|e| format!("query notify_prefs: {e}"))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, prefs) = row.map_err(|e| format!("read notify_prefs: {e}"))?;
        out.insert(id, prefs);
    }
    Ok(out)
}

/// Write one scope. A scope back at every default is deleted rather than
/// stored, so the table never accumulates rows that say nothing.
pub fn save(scope_id: &str, prefs: &ScopePrefs) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    if prefs.is_default() {
        conn.execute("DELETE FROM notify_prefs WHERE scope_id = ?1", rusqlite::params![scope_id])
            .map_err(|e| format!("clear notify_prefs: {e}"))?;
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO notify_prefs (scope_id, level, mute_until, suppress_everyone) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            scope_id,
            prefs.level.map(|l| l.as_u8() as i64),
            prefs.mute_until,
            prefs.suppress_everyone.map(|s| s as i64),
        ],
    )
    .map_err(|e| format!("save notify_prefs: {e}"))?;
    Ok(())
}

/// Clear the mute on scopes whose timer has run out, in one statement.
pub fn clear_expired_mutes(now_ms: i64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE notify_prefs SET mute_until = 0 WHERE mute_until > 0 AND mute_until <= ?1",
        rusqlite::params![now_ms],
    )
    .map_err(|e| format!("clear expired mutes: {e}"))?;
    conn.execute(
        "DELETE FROM notify_prefs WHERE level IS NULL AND mute_until = 0 AND suppress_everyone IS NULL",
        [],
    )
    .map_err(|e| format!("prune notify_prefs: {e}"))?;
    Ok(())
}

/// Replace the whole table in one transaction, for a list arriving from
/// another device. Whole-list newest-wins is the merge these projections use,
/// so a partial apply would be a third state neither device holds.
pub fn replace_all(scopes: &HashMap<String, ScopePrefs>) -> Result<(), String> {
    let mut conn = super::get_write_connection_guard_static()?;
    let tx = conn.transaction().map_err(|e| format!("begin notify tx: {e}"))?;
    tx.execute("DELETE FROM notify_prefs", [])
        .map_err(|e| format!("clear notify_prefs: {e}"))?;
    for (scope_id, prefs) in scopes.iter().filter(|(_, p)| !p.is_default()) {
        tx.execute(
            "INSERT OR REPLACE INTO notify_prefs (scope_id, level, mute_until, suppress_everyone) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                scope_id,
                prefs.level.map(|l| l.as_u8() as i64),
                prefs.mute_until,
                prefs.suppress_everyone.map(|s| s as i64),
            ],
        )
        .map_err(|e| format!("write notify_prefs: {e}"))?;
    }
    tx.commit().map_err(|e| format!("commit notify tx: {e}"))?;
    Ok(())
}

/// Every channel id its community still lists. A tombstoned channel's row is
/// pruned here while its chat row and history stay on disk, so this is the
/// difference between "belongs to a community" and "can still be opened".
pub fn live_channel_ids() -> Result<std::collections::HashSet<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT channel_id FROM community_channels")
        .map_err(|e| format!("prepare live channels: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("query live channels: {e}"))?;
    let mut out = std::collections::HashSet::new();
    for row in rows {
        out.insert(row.map_err(|e| format!("read live channels: {e}"))?);
    }
    Ok(out)
}
