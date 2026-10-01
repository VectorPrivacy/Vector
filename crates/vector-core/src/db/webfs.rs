//! Account files on the web, where there is no filesystem.
//!
//! The SQLite VFS stores databases by path; the few loose files the account
//! layer keeps (the active-account marker, the account registry) live as rows
//! in one more database beside them. An account "directory" exists when its
//! registry row does.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::{is_valid_npub, ACTIVE_ACCOUNT_FILE};

const STORE: &str = "web-files.db";
const ACCOUNT_MARK: &str = ".account";

fn store(app_data: &Path) -> Result<Connection, String> {
    let conn = Connection::open(app_data.join(STORE)).map_err(|e| format!("web file store: {e}"))?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, data BLOB NOT NULL)")
        .map_err(|e| format!("web file store: {e}"))?;
    Ok(conn)
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn read(app_data: &Path, path: &Path) -> Option<Vec<u8>> {
    store(app_data)
        .ok()?
        .query_row("SELECT data FROM files WHERE path = ?1", [key(path)], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

pub fn write(app_data: &Path, path: &Path, data: &[u8]) -> Result<(), String> {
    store(app_data)?
        .execute("INSERT OR REPLACE INTO files (path, data) VALUES (?1, ?2)", rusqlite::params![key(path), data])
        .map(|_| ())
        .map_err(|e| format!("web file write: {e}"))
}

pub fn remove(app_data: &Path, path: &Path) -> Result<(), String> {
    store(app_data)?
        .execute("DELETE FROM files WHERE path = ?1", [key(path)])
        .map(|_| ())
        .map_err(|e| format!("web file remove: {e}"))
}

/// Remove `dir` and every path beneath it.
pub fn remove_tree(app_data: &Path, dir: &Path) -> Result<(), String> {
    let dir = key(dir);
    let prefix = format!("{}/", dir.trim_end_matches('/'));
    store(app_data)?
        .execute(
            "DELETE FROM files WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
            rusqlite::params![dir, prefix],
        )
        .map(|_| ())
        .map_err(|e| format!("web file remove: {e}"))
}

/// Whether a database exists, without creating it.
pub fn db_exists(path: &Path) -> bool {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).is_ok()
}

pub fn register_account(app_data: &Path, npub: &str) -> Result<(), String> {
    write(app_data, &app_data.join(npub).join(ACCOUNT_MARK), b"")
}

pub fn unregister_account(app_data: &Path, npub: &str) -> Result<(), String> {
    remove(app_data, &app_data.join(npub).join(ACCOUNT_MARK))
}

fn account_registered(app_data: &Path, npub: &str) -> bool {
    read(app_data, &app_data.join(npub).join(ACCOUNT_MARK)).is_some()
}

pub(super) fn read_active_account_file_in(app_data: &Path) -> Result<Option<String>, String> {
    let Some(bytes) = read(app_data, &app_data.join(ACTIVE_ACCOUNT_FILE)) else { return Ok(None) };
    let npub = String::from_utf8_lossy(&bytes).trim().to_string();
    if !is_valid_npub(&npub) || !account_registered(app_data, &npub) {
        return Ok(None);
    }
    Ok(Some(npub))
}

pub(super) fn write_active_account_file_in(app_data: &Path, npub: &str) -> Result<(), String> {
    if !is_valid_npub(npub) {
        return Err(format!("Invalid npub format: {}", npub));
    }
    if !account_registered(app_data, npub) {
        return Err(format!("Account directory missing or invalid: {}", npub));
    }
    write(app_data, &app_data.join(ACTIVE_ACCOUNT_FILE), npub.as_bytes())
}

pub(super) fn clear_active_account_file_in(app_data: &Path) -> Result<(), String> {
    remove(app_data, &app_data.join(ACTIVE_ACCOUNT_FILE))
}

pub(super) fn list_account_npubs_in(app_data: &Path) -> Vec<String> {
    let Ok(conn) = store(app_data) else { return Vec::new() };
    let prefix = key(app_data);
    let Ok(mut stmt) = conn.prepare("SELECT path FROM files") else { return Vec::new() };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else { return Vec::new() };
    rows.flatten()
        .filter_map(|p| {
            let rest = p.strip_prefix(&prefix)?.trim_start_matches('/');
            let npub = rest.strip_suffix(ACCOUNT_MARK)?.trim_end_matches('/');
            is_valid_npub(npub).then(|| npub.to_string())
        })
        .collect()
}
