//! Files downloaded before the store, moved into it: checked against their hash, sealed under
//! fresh parameters, then taken out of the download folder. Resumable by construction: a file
//! is either still a download on disk or already the store's.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::db::media_store::Origin;

/// A download still shown from its own file in the download folder.
#[derive(Debug, Clone)]
pub struct OnDisk {
    pub hash: [u8; 32],
    pub path: String,
    pub extension: String,
}

/// What became of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// Kept in Vector and gone from the download folder.
    Moved,
    /// Kept in Vector; the file stays for another account that still shows it.
    Shared,
    /// Not the bytes its message sent: the message stops showing it, and the file is left alone.
    Mismatch,
}

/// This account's downloads that still sit in the download folder, one per file.
pub fn on_disk() -> Result<Vec<OnDisk>, String> {
    let Ok(root) = std::fs::canonicalize(crate::db::get_download_dir()) else {
        return Ok(Vec::new());
    };
    let mut seen = HashSet::new();
    Ok(crate::db::attachments::downloads_on_disk()?
        .into_iter()
        .filter_map(|(hash, path, extension)| {
            let hash = crate::simd::hex::hex_to_bytes_32_checked(&hash)?;
            let real = std::fs::canonicalize(&path).ok()?;
            (real.starts_with(&root) && real.is_file() && seen.insert(path.clone())).then_some(OnDisk { hash, path, extension })
        })
        .collect())
}

/// Every path another account on this device shows a download from. None when one can't be
/// read, so nothing is deleted on a guess.
pub fn paths_elsewhere() -> Option<HashSet<String>> {
    let me = crate::db::current_account_dir().ok()?;
    let dirs = crate::db::get_accounts().ok()?.iter().map(|npub| crate::db::account_dir(npub)).collect::<Result<Vec<_>, _>>().ok()?;
    paths_in(dirs.into_iter().filter(|dir| *dir != me).map(|dir| dir.join("vector.db")))
}

fn paths_in(dbs: impl IntoIterator<Item = PathBuf>) -> Option<HashSet<String>> {
    let mut paths = HashSet::new();
    for db in dbs {
        if let Err(e) = paths_of(&db, &mut paths) {
            crate::log_warn!("[MediaImport] couldn't read another account's downloads, so originals stay: {}", e);
            return None;
        }
    }
    Some(paths)
}

/// One account's download paths. An account not opened since attachments got their own table
/// still names them in the JSON of its events or messages.
fn paths_of(db: &Path, paths: &mut HashSet<String>) -> Result<(), String> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let has = |table: &str| {
        conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [table], |_| Ok(())).is_ok()
    };
    let query = |sql: &str, each: &mut dyn FnMut(String) -> Result<(), String>| -> Result<(), String> {
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        for value in stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
            each(value.map_err(|e| e.to_string())?)?;
        }
        Ok(())
    };
    if has("attachments") {
        return query("SELECT DISTINCT path FROM attachments WHERE path!=''", &mut |path| {
            paths.insert(path);
            Ok(())
        });
    }
    let mut from_json = |text: String| {
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("unreadable attachments: {e}"))?;
        json_paths(&value, paths);
        Ok(())
    };
    if has("events") {
        query("SELECT tags FROM events WHERE tags LIKE '%attachments%'", &mut from_json)?;
    }
    if has("messages") {
        query("SELECT attachments FROM messages WHERE attachments!='[]' AND attachments!=''", &mut from_json)?;
    }
    Ok(())
}

/// Every `"path"` an attachment list names, through any JSON nested inside a string.
fn json_paths(value: &serde_json::Value, paths: &mut HashSet<String>) {
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(|v| json_paths(v, paths)),
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                if !path.is_empty() {
                    paths.insert(path.clone());
                }
            }
            map.values().for_each(|v| json_paths(v, paths));
        }
        serde_json::Value::String(text) if text.starts_with('[') || text.starts_with('{') => {
            if let Ok(inner) = serde_json::from_str(text) {
                json_paths(&inner, paths);
            }
        }
        _ => {}
    }
}

/// Move one file in. Returns what became of it and the events whose rows changed.
pub fn move_in(item: &OnDisk, elsewhere: Option<&HashSet<String>>) -> Result<(Moved, Vec<String>), String> {
    let hex = crate::simd::hex::bytes_to_hex_32(&item.hash);
    let path = Path::new(&item.path);
    let matches = if super::store::holds(&item.hash) {
        crate::crypto::stream::hash_file(path)?.0 == hex
    } else {
        match seal(item, &hex) {
            Ok(()) => true,
            Err(e) if e == crate::crypto::stream::CHANGED => false,
            Err(e) => return Err(e),
        }
    };
    if !matches {
        return Ok((Moved::Mismatch, crate::db::attachments::forget_path(&hex, &item.path)?));
    }
    let events = crate::db::attachments::move_to_store(&hex, &item.path)?;
    if elsewhere.is_none_or(|paths| paths.contains(&item.path)) {
        return Ok((Moved::Shared, events));
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            crate::log_warn!("[MediaImport] kept in Vector, but the original stays: {}", e);
        }
        _ => {}
    }
    Ok((Moved::Moved, events))
}

/// Loaded messages follow a moved file as its rows did: those of its hash show the store's copy,
/// any claiming other bytes there lose the download; for bytes that weren't the message's, only
/// rows of its hash let go. Returns each changed message as (chat id, message).
pub fn reflect(state: &mut crate::state::ChatState, item: &OnDisk, outcome: Moved, events: &[String]) -> Vec<(String, crate::types::Message)> {
    let mut touched = Vec::new();
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        for id in events {
            let Some(msg) = chat.messages.find_by_hex_id_mut(id) else { continue };
            let mut changed = false;
            for att in msg.attachments.iter_mut().filter(|a| *a.path() == *item.path) {
                match (outcome, att.id() == &item.hash) {
                    (Moved::Mismatch, false) => continue,
                    (Moved::Moved | Moved::Shared, true) => att.set_path(String::new()),
                    _ => {
                        att.set_downloaded(false);
                        att.set_path(String::new());
                    }
                }
                changed = true;
            }
            if changed {
                touched.push((chat_idx, id.clone()));
            }
        }
    }
    touched
        .into_iter()
        .filter_map(|(chat_idx, id)| {
            let chat = &state.chats[chat_idx];
            chat.messages.find_by_hex_id(&id).map(|m| (chat.id().clone(), m.to_message(&state.interner)))
        })
        .collect()
}

/// Seal the file straight into the store, refusing bytes that aren't `hex`.
fn seal(item: &OnDisk, hex: &str) -> Result<(), String> {
    let dir = super::store::dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't keep the file: {e}"))?;
    let params = crate::crypto::generate_encryption_params();
    let tmp = dir.join(format!(".import-{}.tmp", &hex[..16]));
    let (blob_sha256, blob_len) =
        crate::crypto::stream::encrypt_file_matching(Path::new(&item.path), &tmp, &params.key, &params.nonce, hex, &mut |_| true)?;
    super::store::keep_file(&tmp, super::store::Keep {
        hash: item.hash,
        key: params.key.clone(),
        nonce: params.nonce.clone(),
        blob_sha256,
        pt_len: blob_len.saturating_sub(16),
        mime: crate::crypto::mime_from_extension(&item.extension).to_string(),
        origin: Origin::Imported,
    })
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(97000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", String::from_utf8(payload).unwrap())
    }

    fn init_test_db() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let account = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        guard
    }

    /// A download of `bytes` at `path`, claimed as `hash`, on a message of its own.
    fn download(event: &str, hash: &str, path: &Path) {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute("INSERT OR IGNORE INTO chats (chat_identifier, chat_type, participants, created_at) VALUES ('npub1chat', 0, '[]', 1)", []).unwrap();
        let chat: i64 = conn.query_row("SELECT id FROM chats WHERE chat_identifier='npub1chat'", [], |r| r.get(0)).unwrap();
        conn.execute("INSERT INTO events (id, kind, chat_id, content, created_at, received_at) VALUES (?1, 14, ?2, '', 1, 1)", rusqlite::params![event, chat]).unwrap();
        conn.execute(
            "INSERT INTO attachments (event_id, att_index, hash, extension, path, downloaded) VALUES (?1, 0, ?2, 'png', ?3, 1)",
            rusqlite::params![event, hash, path.to_string_lossy()],
        )
        .unwrap();
    }

    fn row(event: &str) -> (String, bool) {
        let conn = crate::db::get_db_connection_guard_static().unwrap();
        conn.query_row("SELECT path, downloaded FROM attachments WHERE event_id=?1", [event], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1))).unwrap()
    }

    fn item(bytes: &[u8], path: &Path) -> OnDisk {
        OnDisk {
            hash: crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(bytes)),
            path: path.to_string_lossy().into_owned(),
            extension: "png".to_string(),
        }
    }

    #[test]
    fn a_download_moves_in_and_its_messages_show_the_store() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        std::fs::write(&file, b"a photo from before the store").unwrap();
        let moving = item(b"a photo from before the store", &file);
        let hex = crate::simd::hex::bytes_to_hex_32(&moving.hash);
        download("ev-ours", &hex, &file);
        download("ev-other", &"ab".repeat(32), &file);

        let (outcome, mut events) = move_in(&moving, Some(&HashSet::new())).unwrap();
        events.sort();
        assert_eq!(outcome, Moved::Moved);
        assert_eq!(events, ["ev-other", "ev-ours"]);
        assert!(!file.exists());
        assert_eq!(row("ev-ours"), (String::new(), true));
        assert_eq!(row("ev-other"), (String::new(), false));
        let src = crate::media::open(&moving.hash).unwrap();
        assert_eq!(crate::media::read_all(&*src).unwrap(), b"a photo from before the store");
        assert_eq!(src.mime(), "image/png");
        // Nothing in the store is the plaintext.
        for entry in std::fs::read_dir(crate::media::store::dir().unwrap()).unwrap().flatten() {
            assert_ne!(std::fs::read(entry.path()).unwrap(), b"a photo from before the store");
        }
    }

    #[test]
    fn a_file_another_account_shows_stays_where_it_is() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.png");
        std::fs::write(&file, b"two accounts saw this").unwrap();
        let moving = item(b"two accounts saw this", &file);
        download("ev-shared", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);

        let elsewhere = HashSet::from([moving.path.clone()]);
        assert_eq!(move_in(&moving, Some(&elsewhere)).unwrap().0, Moved::Shared);
        assert!(file.exists() && crate::media::store::holds(&moving.hash));
        assert_eq!(row("ev-shared"), (String::new(), true));
        // Unknown elsewhere: nothing is deleted on a guess.
        let again = dir.path().join("unknown.png");
        std::fs::write(&again, b"no one can say who else saw this").unwrap();
        let unknown = item(b"no one can say who else saw this", &again);
        download("ev-unknown", &crate::simd::hex::bytes_to_hex_32(&unknown.hash), &again);
        assert_eq!(move_in(&unknown, None).unwrap().0, Moved::Shared);
        assert!(again.exists());
    }

    #[test]
    fn bytes_that_are_not_the_message_are_left_alone() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("edited.png");
        std::fs::write(&file, b"edited since it arrived").unwrap();
        let claimed = item(b"what the message sent", &file);
        download("ev-edited", &crate::simd::hex::bytes_to_hex_32(&claimed.hash), &file);

        assert_eq!(move_in(&claimed, Some(&HashSet::new())).unwrap().0, Moved::Mismatch);
        assert_eq!(std::fs::read(&file).unwrap(), b"edited since it arrived");
        assert!(!crate::media::store::holds(&claimed.hash));
        assert_eq!(row("ev-edited"), (String::new(), false));
        assert_eq!(std::fs::read_dir(crate::media::store::dir().unwrap()).map(|d| d.count()).unwrap_or(0), 0);
    }

    #[test]
    fn an_already_kept_file_is_checked_before_its_original_goes() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.png"), dir.path().join("a-1.png"));
        std::fs::write(&first, b"downloaded twice").unwrap();
        std::fs::write(&second, b"downloaded twice, then changed").unwrap();
        let hex = crate::crypto::sha256_hex(b"downloaded twice");
        download("ev-first", &hex, &first);
        download("ev-second", &hex, &second);

        assert_eq!(move_in(&item(b"downloaded twice", &first), Some(&HashSet::new())).unwrap().0, Moved::Moved);
        assert_eq!(move_in(&item(b"downloaded twice", &second), Some(&HashSet::new())).unwrap().0, Moved::Mismatch);
        assert!(second.exists());
    }

    #[test]
    fn loaded_messages_follow_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png").to_string_lossy().into_owned();
        let moving = OnDisk { hash: [0x11; 32], path: path.clone(), extension: "png".into() };
        let att = |hash: [u8; 32], path: &str| crate::types::Attachment {
            id: crate::simd::hex::bytes_to_hex_32(&hash),
            extension: "png".into(),
            path: path.to_string(),
            downloaded: true,
            ..Default::default()
        };
        let message = |seed: u8, attachments| crate::types::Message { id: format!("{seed:02x}").repeat(32), attachments, at: 1, ..Default::default() };
        let build = || {
            let mut state = crate::state::ChatState::new();
            state.create_dm_chat("npub1peer");
            state.add_message_to_chat("npub1peer", &message(1, vec![att([0x11; 32], &path)]));
            state.add_message_to_chat("npub1peer", &message(2, vec![att([0x22; 32], &path)]));
            state.add_message_to_chat("npub1peer", &message(3, vec![att([0x11; 32], "/elsewhere.png")]));
            state
        };
        let events: Vec<String> = [1u8, 2, 3].iter().map(|s| format!("{s:02x}").repeat(32)).collect();
        let shown = |out: &[(String, crate::types::Message)], seed: u8| {
            out.iter().find(|(_, m)| m.id == format!("{seed:02x}").repeat(32)).map(|(_, m)| (m.attachments[0].path.clone(), m.attachments[0].downloaded))
        };

        let mut state = build();
        let out = reflect(&mut state, &moving, Moved::Moved, &events);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(chat, _)| chat == "npub1peer"));
        assert_eq!(shown(&out, 1), Some((String::new(), true)));
        assert_eq!(shown(&out, 2), Some((String::new(), false)));
        assert_eq!(shown(&out, 3), None);

        let mut state = build();
        let out = reflect(&mut state, &moving, Moved::Mismatch, &events);
        assert_eq!(out.len(), 1);
        assert_eq!(shown(&out, 1), Some((String::new(), false)));
    }

    #[test]
    fn another_accounts_paths_are_read_and_an_unreadable_one_stops_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vector.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE attachments (path TEXT NOT NULL DEFAULT ''); INSERT INTO attachments VALUES ('/d/a.png'), ('');").unwrap();
        drop(conn);
        assert_eq!(paths_in([db.clone()]), Some(HashSet::from(["/d/a.png".to_string()])));

        // Older schemas name paths in their events' tags and their messages' attachment lists.
        let old = dir.path().join("old.db");
        let tag = serde_json::json!([["attachments", serde_json::json!([{ "id": "x", "path": "/d/b.png" }]).to_string()]]).to_string();
        let conn = rusqlite::Connection::open(&old).unwrap();
        conn.execute_batch("CREATE TABLE events (tags TEXT); CREATE TABLE messages (attachments TEXT NOT NULL DEFAULT '[]');").unwrap();
        conn.execute("INSERT INTO events VALUES (?1)", [&tag]).unwrap();
        conn.execute("INSERT INTO messages VALUES ('[{\"path\":\"/d/c.png\"},{\"path\":\"\"}]'), ('[]')", []).unwrap();
        drop(conn);
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        assert_eq!(paths_in([db.clone(), old.clone()]), Some(names(&["/d/a.png", "/d/b.png", "/d/c.png"])));

        let garbled = dir.path().join("garbled.db");
        rusqlite::Connection::open(&garbled).unwrap().execute_batch("CREATE TABLE messages (attachments TEXT); INSERT INTO messages VALUES ('sealed:abc');").unwrap();
        assert_eq!(paths_in([db, garbled]), None);
        assert_eq!(paths_in([dir.path().join("missing.db")]), None);
    }
}
