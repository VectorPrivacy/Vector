//! Files downloaded before the store, moved into it: checked against their hash, sealed under
//! fresh parameters, then taken out of the download folder. Resumable by construction: a file
//! is either still a download on disk or already the store's. An original goes only once its
//! kept copy has been read back whole and the database that points at it is on disk.

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
    /// Kept in Vector; the original may go.
    Moved,
    /// Kept in Vector; the original stays for another account that still shows it.
    Shared,
    /// Kept in Vector already; the file holds other bytes now and is left alone.
    Changed,
    /// Not the bytes its message sent: the message stops showing it, and the file is left alone.
    Mismatch,
}

/// What another account on this device still shows: by path, and by content, since a path can
/// be stale (an account that hasn't logged in since its download folder moved).
#[derive(Debug, Default)]
pub struct Elsewhere {
    paths: HashSet<String>,
    hashes: HashSet<[u8; 32]>,
}

impl Elsewhere {
    fn shows(&self, item: &OnDisk) -> bool {
        self.paths.contains(&item.path) || self.hashes.contains(&item.hash)
    }
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

/// What every other account on this device shows. None when one can't be read, so nothing is
/// deleted on a guess.
pub fn elsewhere() -> Option<Elsewhere> {
    let me = crate::db::current_account_dir().ok()?;
    let dirs = crate::db::get_accounts().ok()?.iter().map(|npub| crate::db::account_dir(npub)).collect::<Result<Vec<_>, _>>().ok()?;
    shown_in(dirs.into_iter().filter(|dir| *dir != me).map(|dir| dir.join("vector.db")))
}

fn shown_in(dbs: impl IntoIterator<Item = PathBuf>) -> Option<Elsewhere> {
    let mut shown = Elsewhere::default();
    for db in dbs {
        if let Err(e) = shown_by(&db, &mut shown) {
            crate::log_warn!("[MediaImport] couldn't read another account's downloads, so originals stay: {}", e);
            return None;
        }
    }
    Some(shown)
}

/// One account's downloads. An account not opened since attachments got their own table still
/// names them in the JSON of its events or messages.
fn shown_by(db: &Path, shown: &mut Elsewhere) -> Result<(), String> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let has = |table: &str| {
        conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [table], |_| Ok(())).is_ok()
    };
    if has("attachments") {
        let mut stmt = conn
            .prepare("SELECT DISTINCT hash, path FROM attachments WHERE downloaded=1 OR path!=''")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
        for row in rows {
            let (hash, path) = row.map_err(|e| e.to_string())?;
            shown.hashes.extend(crate::simd::hex::hex_to_bytes_32_checked(&hash));
            if !path.is_empty() {
                shown.paths.insert(path);
            }
        }
        return Ok(());
    }
    let mut json = |sql: &str| -> Result<(), String> {
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        for text in stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
            let text = text.map_err(|e| e.to_string())?;
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("unreadable attachments: {e}"))?;
            json_shown(&value, shown);
        }
        Ok(())
    };
    if has("events") {
        json("SELECT tags FROM events WHERE tags LIKE '%attachments%'")?;
    }
    if has("messages") {
        json("SELECT attachments FROM messages WHERE attachments!='[]' AND attachments!=''")?;
    }
    Ok(())
}

/// Every attachment an attachment list shows from a file, through any JSON nested inside a
/// string: its path, and its content hash (`id`).
fn json_shown(value: &serde_json::Value, shown: &mut Elsewhere) {
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(|v| json_shown(v, shown)),
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                if !path.is_empty() {
                    shown.paths.insert(path.clone());
                    if let Some(serde_json::Value::String(id)) = map.get("id") {
                        shown.hashes.extend(crate::simd::hex::hex_to_bytes_32_checked(id));
                    }
                }
            }
            map.values().for_each(|v| json_shown(v, shown));
        }
        serde_json::Value::String(text) if text.starts_with('[') || text.starts_with('{') => {
            if let Ok(inner) = serde_json::from_str(text) {
                json_shown(&inner, shown);
            }
        }
        _ => {}
    }
}

/// Keep one file and point its messages at the kept copy. Returns what became of it and the
/// events whose rows changed; on `Moved` the original may go, once [`remove_originals`] has made
/// those rows durable. Nothing here deletes a file the user had.
pub fn move_in(item: &OnDisk, elsewhere: Option<&Elsewhere>) -> Result<(Moved, Vec<String>), String> {
    let hex = crate::simd::hex::bytes_to_hex_32(&item.hash);
    let path = Path::new(&item.path);
    // A row whose copy is gone or won't open is no copy: it goes, and this file is kept anew.
    if super::store::holds(&item.hash) && !super::store::verify(&item.hash) {
        super::store::forget(&item.hash);
    }
    if super::store::holds(&item.hash) {
        if crate::crypto::stream::hash_file(path)?.0 != hex {
            return Ok((Moved::Changed, crate::db::attachments::keep_at_store(&hex, &item.path)?));
        }
    } else {
        match seal(item, &hex) {
            Ok(()) => {}
            Err(e) if e == crate::crypto::stream::CHANGED => {
                return Ok((Moved::Mismatch, crate::db::attachments::forget_path(&hex, &item.path)?));
            }
            Err(e) => return Err(e),
        }
        if !super::store::verify(&item.hash) {
            return Err("the kept copy didn't read back as the file".into());
        }
    }
    let events = crate::db::attachments::move_to_store(&hex, &item.path)?;
    let shared = elsewhere.is_none_or(|shown| shown.shows(item));
    Ok((if shared { Moved::Shared } else { Moved::Moved }, events))
}

/// Delete originals whose rows now point at kept copies: the database is synced first, so a
/// crash can never leave a row pointing at a file that's gone. Returns the ones that stayed.
pub fn remove_originals(paths: &[String]) -> Result<Vec<String>, String> {
    crate::db::sync_wal()?;
    Ok(paths
        .iter()
        .filter(|path| match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                crate::log_warn!("[MediaImport] kept in Vector, but the original stays: {}", e);
                true
            }
            _ => false,
        })
        .cloned()
        .collect())
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
                    (Moved::Mismatch | Moved::Changed, false) => continue,
                    (Moved::Moved | Moved::Shared | Moved::Changed, true) => att.set_path(String::new()),
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

    fn nobody_else() -> Elsewhere {
        Elsewhere::default()
    }

    #[test]
    fn a_download_moves_in_and_its_original_goes_only_when_removed() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        std::fs::write(&file, b"a photo from before the store").unwrap();
        let moving = item(b"a photo from before the store", &file);
        let hex = crate::simd::hex::bytes_to_hex_32(&moving.hash);
        download("ev-ours", &hex, &file);
        download("ev-other", &"ab".repeat(32), &file);

        let (outcome, mut events) = move_in(&moving, Some(&nobody_else())).unwrap();
        events.sort();
        assert_eq!(outcome, Moved::Moved);
        assert_eq!(events, ["ev-other", "ev-ours"]);
        assert!(file.exists(), "nothing is deleted until the rows are durable");
        assert_eq!(row("ev-ours"), (String::new(), true));
        assert_eq!(row("ev-other"), (String::new(), false));
        assert!(crate::media::store::verify(&moving.hash));
        let src = crate::media::open(&moving.hash).unwrap();
        assert_eq!(crate::media::read_all(&*src).unwrap(), b"a photo from before the store");
        assert_eq!(src.mime(), "image/png");
        // Nothing in the store is the plaintext.
        for entry in std::fs::read_dir(crate::media::store::dir().unwrap()).unwrap().flatten() {
            assert_ne!(std::fs::read(entry.path()).unwrap(), b"a photo from before the store");
        }
        assert!(remove_originals(&[moving.path.clone()]).unwrap().is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn a_file_another_account_shows_stays_where_it_is() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.png");
        std::fs::write(&file, b"two accounts saw this").unwrap();
        let moving = item(b"two accounts saw this", &file);
        download("ev-shared", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);

        let by_path = Elsewhere { paths: HashSet::from([moving.path.clone()]), ..Default::default() };
        assert_eq!(move_in(&moving, Some(&by_path)).unwrap().0, Moved::Shared);
        assert!(crate::media::store::holds(&moving.hash));
        assert_eq!(row("ev-shared"), (String::new(), true));

        // The other account names the file under a path that has since moved: its bytes still say.
        let again = dir.path().join("moved.png");
        std::fs::write(&again, b"named under an old folder").unwrap();
        let stale = item(b"named under an old folder", &again);
        download("ev-stale", &crate::simd::hex::bytes_to_hex_32(&stale.hash), &again);
        let by_hash = Elsewhere { paths: HashSet::from(["/old/folder/moved.png".to_string()]), hashes: HashSet::from([stale.hash]) };
        assert_eq!(move_in(&stale, Some(&by_hash)).unwrap().0, Moved::Shared);

        // Unknown elsewhere: nothing is deleted on a guess.
        let unknown_file = dir.path().join("unknown.png");
        std::fs::write(&unknown_file, b"no one can say who else saw this").unwrap();
        let unknown = item(b"no one can say who else saw this", &unknown_file);
        download("ev-unknown", &crate::simd::hex::bytes_to_hex_32(&unknown.hash), &unknown_file);
        assert_eq!(move_in(&unknown, None).unwrap().0, Moved::Shared);
        assert!(file.exists() && again.exists() && unknown_file.exists());
    }

    #[test]
    fn bytes_that_are_not_the_message_are_left_alone() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("edited.png");
        std::fs::write(&file, b"edited since it arrived").unwrap();
        let claimed = item(b"what the message sent", &file);
        download("ev-edited", &crate::simd::hex::bytes_to_hex_32(&claimed.hash), &file);

        assert_eq!(move_in(&claimed, Some(&nobody_else())).unwrap().0, Moved::Mismatch);
        assert_eq!(std::fs::read(&file).unwrap(), b"edited since it arrived");
        assert!(!crate::media::store::holds(&claimed.hash));
        assert_eq!(row("ev-edited"), (String::new(), false));
        assert_eq!(std::fs::read_dir(crate::media::store::dir().unwrap()).map(|d| d.count()).unwrap_or(0), 0);
    }

    #[test]
    fn a_kept_copy_is_trusted_only_once_it_reads_back() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.png"), dir.path().join("a-1.png"));
        std::fs::write(&first, b"downloaded twice").unwrap();
        std::fs::write(&second, b"downloaded twice, then changed").unwrap();
        let hex = crate::crypto::sha256_hex(b"downloaded twice");
        download("ev-first", &hex, &first);
        download("ev-second", &hex, &second);
        assert_eq!(move_in(&item(b"downloaded twice", &first), Some(&nobody_else())).unwrap().0, Moved::Moved);

        // The store keeps these bytes, and this file holds others: it stays, its row shows the copy.
        assert_eq!(move_in(&item(b"downloaded twice", &second), Some(&nobody_else())).unwrap().0, Moved::Changed);
        assert!(second.exists());
        assert_eq!(row("ev-second"), (String::new(), true));

        // A row whose blob vanished is no copy: the next file of those bytes is kept anew.
        let hash = crate::simd::hex::hex_to_bytes_32(&hex);
        let entry = crate::db::media_store::get(&hash).unwrap().unwrap();
        std::fs::remove_file(crate::media::store::dir().unwrap().join(&entry.file)).unwrap();
        assert!(!crate::media::store::verify(&hash));
        let third = dir.path().join("a-2.png");
        std::fs::write(&third, b"downloaded twice").unwrap();
        download("ev-third", &hex, &third);
        assert_eq!(move_in(&item(b"downloaded twice", &third), Some(&nobody_else())).unwrap().0, Moved::Moved);
        assert!(crate::media::store::verify(&hash));
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
    fn another_accounts_downloads_are_read_and_an_unreadable_one_stops_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vector.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE attachments (hash TEXT NOT NULL, path TEXT NOT NULL DEFAULT '', downloaded INTEGER NOT NULL DEFAULT 0);").unwrap();
        conn.execute("INSERT INTO attachments VALUES (?1, '/d/a.png', 1), (?2, '', 1), (?3, '', 0)", ["11".repeat(32), "22".repeat(32), "33".repeat(32)]).unwrap();
        drop(conn);
        let shown = shown_in([db.clone()]).unwrap();
        assert_eq!(shown.paths, HashSet::from(["/d/a.png".to_string()]));
        assert_eq!(shown.hashes, HashSet::from([[0x11; 32], [0x22; 32]]));

        // Older schemas name downloads in their events' tags and their messages' attachment lists.
        let old = dir.path().join("old.db");
        let tag = serde_json::json!([["attachments", serde_json::json!([{ "id": "44".repeat(32), "path": "/d/b.png" }]).to_string()]]).to_string();
        let conn = rusqlite::Connection::open(&old).unwrap();
        conn.execute_batch("CREATE TABLE events (tags TEXT); CREATE TABLE messages (attachments TEXT NOT NULL DEFAULT '[]');").unwrap();
        conn.execute("INSERT INTO events VALUES (?1)", [&tag]).unwrap();
        conn.execute("INSERT INTO messages VALUES ('[{\"path\":\"/d/c.png\"},{\"path\":\"\"}]'), ('[]')", []).unwrap();
        drop(conn);
        let both = shown_in([db.clone(), old.clone()]).unwrap();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        assert_eq!(both.paths, names(&["/d/a.png", "/d/b.png", "/d/c.png"]));
        assert!(both.hashes.contains(&[0x44; 32]));

        let garbled = dir.path().join("garbled.db");
        rusqlite::Connection::open(&garbled).unwrap().execute_batch("CREATE TABLE messages (attachments TEXT); INSERT INTO messages VALUES ('sealed:abc');").unwrap();
        assert!(shown_in([db, garbled]).is_none());
        assert!(shown_in([dir.path().join("missing.db")]).is_none());
    }
}
