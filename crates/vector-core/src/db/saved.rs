//! Files the user saved into one of Vector's folders. They are the user's: nothing Vector keeps,
//! releases or clears there deletes one.

/// The spelling a saved file is recorded under: its real path when it resolves.
fn spelling(path: &str) -> String {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

pub fn mark(path: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("INSERT OR IGNORE INTO saved_files (path) VALUES (?1)", [spelling(path)])
        .map_err(|e| format!("mark saved: {e}"))?;
    Ok(())
}

/// `path` holds a new file now, not the one that was saved there.
pub fn unmark(path: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM saved_files WHERE path=?1 OR path=?2", [path.to_string(), spelling(path)])
        .map_err(|e| format!("unmark saved: {e}"))?;
    Ok(())
}

pub fn is_saved(path: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM saved_files WHERE path=?1 OR path=?2)",
        [path.to_string(), spelling(path)],
        |r| r.get(0),
    )
    .map_err(|e| format!("is_saved: {e}"))
}

pub fn all() -> Result<Vec<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT path FROM saved_files").map_err(|e| format!("saved files: {e}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| format!("saved files: {e}"))?;
    Ok(rows.flatten().collect())
}
