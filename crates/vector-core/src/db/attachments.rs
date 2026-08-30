//! Attachments table operations.
//!
//! Attachments are normalized into one row per attachment (see migration 74), keyed to their event
//! (`ON DELETE CASCADE`) and indexed by content hash. This module is the single source of truth for
//! attachment persistence; the legacy `["attachments", …]` tag in `events.tags` is left in place on
//! pre-migration events as an untouched safety net but is never read.

use std::collections::HashMap;

use crate::types::Attachment;

const SELECT_COLS: &str = "event_id, att_index, hash, key, nonce, extension, name, url, \
    path, size, img_meta, downloaded, webxdc_topic, group_id, original_hash, fallback_urls";

/// Rebuild `(event_id, Attachment)` from a row selecting `SELECT_COLS`. `downloading` is transient
/// runtime state and is never persisted (always false on load).
fn row_to_attachment(row: &rusqlite::Row) -> rusqlite::Result<(String, Attachment)> {
    let event_id: String = row.get(0)?;
    let img_meta_json: Option<String> = row.get(10)?;
    let att = Attachment {
        id: row.get(2)?,
        key: row.get(3)?,
        nonce: row.get(4)?,
        extension: row.get(5)?,
        name: row.get(6)?,
        url: row.get(7)?,
        path: row.get(8)?,
        size: row.get::<_, i64>(9)? as u64,
        img_meta: img_meta_json.and_then(|j| serde_json::from_str(&j).ok()),
        downloading: false,
        downloaded: row.get::<_, i64>(11)? != 0,
        webxdc_topic: row.get(12)?,
        group_id: row.get(13)?,
        original_hash: row.get(14)?,
        fallback_urls: row
            .get::<_, String>(15)?
            .split_whitespace()
            .map(|s| s.to_string())
            .collect(),
    };
    Ok((event_id, att))
}

/// Upsert a message's attachment rows onto the given connection or transaction, so `save_message`
/// can commit them ATOMICALLY with the event row (an event + no attachments would render as a broken
/// file message with no fallback). Upserts on `(event_id, att_index)`. The mutable local state is
/// handled so a re-save never regresses a completed download: `downloaded` is MONOTONIC
/// (`MAX(existing, incoming)`), and `hash`/`path` only take the incoming values when the incoming
/// carries a completed download (`downloaded=1`) — the nonce→content-hash rewrite the download path
/// performs. So a relay re-delivery (downloaded=0) preserves the downloaded file, its content-hash
/// key, and its path; a completed download persists all three in one pass. Explicit un-download goes
/// through `clear_attachment_download`, never here.
pub fn insert_attachment_rows(conn: &rusqlite::Connection, event_id: &str, attachments: &[Attachment]) -> Result<(), String> {
    if attachments.is_empty() {
        return Ok(());
    }
    // prepare_cached: the statement survives on the connection across calls (and transactions),
    // so bulk-sync batches don't re-parse the SQL per message.
    let mut stmt = conn.prepare_cached(
        "INSERT INTO attachments (event_id, att_index, hash, key, nonce, extension, name, url, \
         path, size, img_meta, downloaded, webxdc_topic, group_id, original_hash, fallback_urls) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) \
         ON CONFLICT(event_id, att_index) DO UPDATE SET \
            key=excluded.key, nonce=excluded.nonce, extension=excluded.extension, \
            name=excluded.name, url=excluded.url, size=excluded.size, img_meta=excluded.img_meta, \
            webxdc_topic=excluded.webxdc_topic, group_id=excluded.group_id, \
            original_hash=excluded.original_hash, \
            fallback_urls=CASE WHEN excluded.fallback_urls='' THEN fallback_urls ELSE excluded.fallback_urls END, \
            downloaded=MAX(downloaded, excluded.downloaded), \
            hash=CASE WHEN excluded.downloaded=1 THEN excluded.hash ELSE hash END, \
            path=CASE WHEN excluded.downloaded=1 THEN excluded.path ELSE path END",
    ).map_err(|e| format!("prepare insert attachment: {e}"))?;
    for (i, a) in attachments.iter().enumerate() {
        let img_meta_json = a.img_meta.as_ref().and_then(|m| serde_json::to_string(m).ok());
        stmt.execute(
            rusqlite::params![
                event_id, i as i64, a.id, a.key, a.nonce, a.extension, a.name, a.url,
                a.path, a.size as i64, img_meta_json, a.downloaded as i64,
                a.webxdc_topic, a.group_id, a.original_hash,
                a.fallback_urls.join(" "),
            ],
        ).map_err(|e| format!("insert attachment: {e}"))?;
    }
    Ok(())
}

/// Attachments for a set of events, `event_id → Vec<Attachment>` ordered by `att_index`. Batched
/// (one `IN (…)` query) for a message window, mirroring the reactions/edits loaders.
pub fn get_attachments_for_events(event_ids: &[String]) -> Result<HashMap<String, Vec<Attachment>>, String> {
    let mut out: HashMap<String, Vec<Attachment>> = HashMap::new();
    if event_ids.is_empty() {
        return Ok(out);
    }
    let conn = super::get_db_connection_guard_static()?;
    let placeholders = event_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {SELECT_COLS} FROM attachments WHERE event_id IN ({placeholders}) ORDER BY event_id, att_index"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare get_attachments: {e}"))?;
    let params = rusqlite::params_from_iter(event_ids.iter());
    let rows = stmt.query_map(params, row_to_attachment)
        .map_err(|e| format!("query get_attachments: {e}"))?;
    for r in rows.flatten() {
        out.entry(r.0).or_default().push(r.1);
    }
    Ok(out)
}

/// Attachments for a single event, ordered by `att_index`.
pub fn get_attachments_for_event(event_id: &str) -> Result<Vec<Attachment>, String> {
    let map = get_attachments_for_events(std::slice::from_ref(&event_id.to_string()))?;
    Ok(map.into_values().next().unwrap_or_default())
}

/// Flip one attachment's downloaded state — a single-row UPDATE keyed by (event_id, content hash),
/// replacing the old read-modify-write of the whole tags blob.
pub fn set_attachment_downloaded(event_id: &str, hash: &str, downloaded: bool, path: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE attachments SET downloaded=?1, path=?2 WHERE event_id=?3 AND hash=?4",
        rusqlite::params![downloaded as i64, path, event_id, hash],
    ).map_err(|e| format!("set_attachment_downloaded: {e}"))?;
    Ok(())
}

/// Mark every OTHER attachment sharing this content hash as downloaded to the same path — the
/// download-sharing dedup, now an indexed `WHERE hash = ?` instead of a `LIKE '%hash%'` table scan.
/// Returns the affected event ids so the caller can reconcile in-memory STATE.
pub fn backfill_downloaded_by_hash(hash: &str, path: &str, exclude_event_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let affected: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT event_id FROM attachments WHERE hash=?1 AND event_id!=?2 AND downloaded=0"
        ).map_err(|e| format!("prepare backfill_by_hash: {e}"))?;
        let rows = stmt.query_map(rusqlite::params![hash, exclude_event_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("query backfill_by_hash: {e}"))?;
        rows.flatten().collect()
    };
    conn.execute(
        "UPDATE attachments SET downloaded=1, path=?1 WHERE hash=?2 AND event_id!=?3 AND downloaded=0",
        rusqlite::params![path, hash, exclude_event_id],
    ).map_err(|e| format!("backfill_by_hash update: {e}"))?;
    Ok(affected)
}

/// A downloaded attachment's on-disk path, for the integrity sweep. Returns (event_id, hash, path)
/// for every attachment claiming `downloaded=1` with a non-empty path — an indexed read, no per-event
/// JSON parse.
pub fn downloaded_attachment_paths() -> Result<Vec<(String, String, String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare(
        "SELECT event_id, hash, path FROM attachments WHERE downloaded=1 AND path!=''"
    ).map_err(|e| format!("prepare downloaded_paths: {e}"))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| format!("query downloaded_paths: {e}"))?;
    Ok(rows.flatten().collect())
}

/// Mark an attachment not-downloaded (its file went missing). Clears the path.
pub fn clear_attachment_download(event_id: &str, hash: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE attachments SET downloaded=0, path='' WHERE event_id=?1 AND hash=?2",
        rusqlite::params![event_id, hash],
    ).map_err(|e| format!("clear_attachment_download: {e}"))?;
    Ok(())
}

/// Repoint downloaded-file paths from old download directories to a new one (Android download-dir
/// migration). For each downloaded attachment whose path starts with an old prefix: move it to
/// `new_dir/<filename>` if that exists, else mark it not-downloaded. Returns the affected event ids.
pub fn rewrite_downloaded_paths(old_prefixes: &[String], new_dir: &std::path::Path) -> Result<Vec<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, event_id, path FROM attachments WHERE downloaded=1 AND path!=''")
            .map_err(|e| format!("prepare rewrite_paths: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("query rewrite_paths: {e}"))?;
        mapped.flatten().collect()
    };
    let mut affected = Vec::new();
    for (rowid, event_id, path) in rows {
        if !old_prefixes.iter().any(|p| path.starts_with(p.as_str())) {
            continue;
        }
        let Some(name) = std::path::Path::new(&path).file_name() else { continue };
        let new_path = new_dir.join(name);
        let res = if new_path.exists() {
            conn.execute("UPDATE attachments SET path=?1 WHERE id=?2",
                rusqlite::params![new_path.to_string_lossy().to_string(), rowid])
        } else {
            conn.execute("UPDATE attachments SET downloaded=0, path='' WHERE id=?1", rusqlite::params![rowid])
        };
        if res.is_ok() {
            affected.push(event_id);
        }
    }
    Ok(affected)
}

/// A prior upload of this exact plaintext, reusable by reference: the blob is
/// already on a Blossom server and `key`/`nonce` decrypt it.
#[derive(Debug, Clone)]
pub struct ReusableUpload {
    pub url: String,
    pub key: String,
    pub nonce: String,
    pub size: u64,
    /// Whether the event that carried it was ours — a foreign blob's lifetime
    /// belongs to its uploader, so a reuse of it wants a background mirror.
    pub mine: bool,
}

/// The smart-forward ledger read: has this exact plaintext (by SHA-256) ever
/// crossed this account with a decryptable blob behind it?
///
/// `downloaded = 1` is the trust gate: the download path hash-verifies the
/// decrypted bytes against `hash` before setting it, so a row that merely
/// CLAIMS this hash (a hostile sender's fabricated imeta) never qualifies —
/// our own sends set it by construction. Own uploads win the ordering:
/// their blobs live on our servers under our delete authority.
pub fn find_reusable_by_hash(hash: &str) -> Result<Option<ReusableUpload>, String> {
    if hash.is_empty() {
        return Ok(None);
    }
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT a.url, a.key, a.nonce, a.size, COALESCE(e.mine, 0)
             FROM attachments a JOIN events e ON e.id = a.event_id
             WHERE a.hash = ?1 AND a.url <> '' AND a.key <> '' AND a.nonce <> '' AND a.downloaded = 1
             ORDER BY COALESCE(e.mine, 0) DESC, a.id DESC LIMIT 1",
        )
        .map_err(|e| format!("find_reusable_by_hash prepare: {e}"))?;
    let row = stmt
        .query_row(rusqlite::params![hash], |r| {
            Ok(ReusableUpload {
                url: r.get(0)?,
                key: r.get(1)?,
                nonce: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mine: r.get::<_, i64>(4)? != 0,
            })
        })
        .ok();
    Ok(row)
}

/// Of `urls`, the ones NO other event's attachment still points at — the only
/// ones a delete may scrub from Blossom. One blob can back many messages
/// (smart-forward), and the shared copy must outlive every send but the last.
/// DB-backed on purpose: the in-memory STATE windows to recent messages, so a
/// reference in an older message is invisible there.
pub fn urls_unreferenced_elsewhere(urls: &[String], excluding_event_id: &str) -> Result<Vec<String>, String> {
    if urls.is_empty() {
        return Ok(Vec::new());
    }
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT 1 FROM attachments WHERE url = ?1 AND event_id <> ?2 LIMIT 1")
        .map_err(|e| format!("urls_unreferenced prepare: {e}"))?;
    let mut out = Vec::with_capacity(urls.len());
    for url in urls {
        let referenced = stmt.exists(rusqlite::params![url, excluding_event_id])
            .map_err(|e| format!("urls_unreferenced query: {e}"))?;
        if !referenced {
            out.push(url.clone());
        }
    }
    Ok(out)
}

/// Whether any OTHER event's attachment shares this plaintext hash — the local
/// file half of the same refcount: the decrypted file on disk serves every
/// message that names its hash, windowed out of memory or not.
pub fn hash_referenced_elsewhere(hash: &str, excluding_event_id: &str) -> Result<bool, String> {
    if hash.is_empty() {
        return Ok(false);
    }
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT 1 FROM attachments WHERE hash = ?1 AND event_id <> ?2 LIMIT 1")
        .map_err(|e| format!("hash_referenced prepare: {e}"))?;
    stmt.exists(rusqlite::params![hash, excluding_event_id])
        .map_err(|e| format!("hash_referenced query: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Attachment, Message};

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(74000);

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
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn att(hash: &str, url: &str, keyed: bool, downloaded: bool) -> Attachment {
        Attachment {
            id: hash.to_string(),
            key: if keyed { "k".into() } else { String::new() },
            nonce: if keyed { "n".into() } else { String::new() },
            url: url.to_string(),
            size: 42,
            downloaded,
            ..Default::default()
        }
    }

    async fn save(chat: &str, event_id: &str, mine: bool, a: Attachment) {
        let msg = Message {
            id: event_id.to_string(),
            content: String::new(),
            mine,
            at: 1_000_000,
            attachments: vec![a],
            ..Default::default()
        };
        crate::db::events::save_message(chat, &msg).await.unwrap();
    }

    /// The smart-forward ledger: a verified prior send/download of the same
    /// plaintext is reusable, our own upload wins over a foreign one, and a
    /// row that never proved itself (undownloaded, or keyless) never qualifies.
    #[tokio::test]
    async fn the_ledger_finds_a_prior_upload_and_prefers_our_own() {
        let (_tmp, _guard) = init_test_db();
        save("chat_a", "evt_foreign", false, att("H1", "https://b.example/foreign", true, true)).await;
        save("chat_b", "evt_mine", true, att("H1", "https://b.example/mine", true, true)).await;
        save("chat_c", "evt_unverified", false, att("H2", "https://b.example/x", true, false)).await;
        save("chat_d", "evt_keyless", true, att("H3", "https://b.example/y", false, true)).await;

        let hit = find_reusable_by_hash("H1").unwrap().expect("H1 is reusable");
        assert_eq!(hit.url, "https://b.example/mine", "own upload preferred");
        assert!(hit.mine);
        assert_eq!(hit.key, "k");
        assert_eq!(hit.size, 42);

        assert!(find_reusable_by_hash("H2").unwrap().is_none(), "unverified rows never qualify");
        assert!(find_reusable_by_hash("H3").unwrap().is_none(), "keyless rows never qualify");
        assert!(find_reusable_by_hash("H9").unwrap().is_none());
    }

    /// One blob, many messages: the shared copy must outlive every send but
    /// the last. The gate answers from the DB, not memory — an old message
    /// windowed out of STATE still holds its reference here.
    #[tokio::test]
    async fn a_shared_blob_survives_every_delete_but_the_last() {
        let (_tmp, _guard) = init_test_db();
        let url = "https://b.example/shared".to_string();
        save("chat_a", "evt_first", true, att("HH", &url, true, true)).await;
        save("chat_b", "evt_second", true, att("HH", &url, true, true)).await;

        // Deleting either message alone: the other still rides the blob.
        assert!(urls_unreferenced_elsewhere(&[url.clone()], "evt_first").unwrap().is_empty());
        assert!(urls_unreferenced_elsewhere(&[url.clone()], "evt_second").unwrap().is_empty());
        assert!(hash_referenced_elsewhere("HH", "evt_first").unwrap());

        // The sibling's row goes (its event deleted) — NOW the last delete may scrub.
        crate::db::events::delete_event("evt_second").await.unwrap();
        assert_eq!(urls_unreferenced_elsewhere(&[url.clone()], "evt_first").unwrap(), vec![url]);
        assert!(!hash_referenced_elsewhere("HH", "evt_first").unwrap());
    }
}
