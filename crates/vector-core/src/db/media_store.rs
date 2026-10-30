//! The media store's ledger: one row per file this account keeps in `media/`. A row holds the
//! key and nonce that open its file, sealed under Local Encryption, so a PIN change or turning
//! encryption off never touches a file.

use crate::simd::hex::{bytes_to_hex_32, hex_to_bytes_32_checked};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Downloaded, kept as the blob arrived.
    Received,
    /// Our own upload, kept as we sent it.
    Sent,
    /// Arrived without encryption, sealed here under fresh parameters.
    Sealed,
    /// A file from before the store, moved in.
    Imported,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Received => "received",
            Origin::Sent => "sent",
            Origin::Sealed => "sealed",
            Origin::Imported => "imported",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "sent" => Origin::Sent,
            "sealed" => Origin::Sealed,
            "imported" => Origin::Imported,
            _ => Origin::Received,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMedia {
    pub hash: [u8; 32],
    /// Its name inside `media/`.
    pub file: String,
    pub key: String,
    pub nonce: String,
    /// The ciphertext's SHA-256: a Blossom address for a received or sent blob.
    pub blob_sha256: String,
    pub pt_len: u64,
    pub mime: String,
    pub origin: Origin,
}

fn now() -> i64 {
    (crate::clock::now_ms() / 1000) as i64
}

/// Record a kept file. False when this hash was kept already, and the caller's file is spare.
pub fn insert(entry: &StoredMedia) -> Result<bool, String> {
    let _ticket = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    crate::crypto::seal_batch(|seal| {
        let (key, nonce) = (seal.seal(&entry.key)?, seal.seal(&entry.nonce)?);
        conn.execute(
            "INSERT INTO media_store (hash, file, format, key, nonce, blob_sha256, pt_len, mime, origin, created_at, used_at)
             VALUES (?1, ?2, 'gcm', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) ON CONFLICT(hash) DO NOTHING",
            rusqlite::params![
                bytes_to_hex_32(&entry.hash), entry.file, key, nonce, entry.blob_sha256,
                entry.pt_len as i64, entry.mime, entry.origin.as_str(), now(),
            ],
        )
        .map(|n| n == 1)
        .map_err(|e| format!("keep media: {e}"))
    })
}

/// Record a kept file in place of whatever was kept for its hash. Returns the file it replaced,
/// for the caller to delete now that the new one is recorded.
pub fn replace(entry: &StoredMedia) -> Result<Option<String>, String> {
    let _ticket = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let old: Option<String> = match conn.query_row("SELECT file FROM media_store WHERE hash = ?1", [bytes_to_hex_32(&entry.hash)], |r| r.get(0)) {
        Ok(file) => Some(file),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(format!("read media: {e}")),
    };
    crate::crypto::seal_batch(|seal| {
        let (key, nonce) = (seal.seal(&entry.key)?, seal.seal(&entry.nonce)?);
        conn.execute(
            "INSERT INTO media_store (hash, file, format, key, nonce, blob_sha256, pt_len, mime, origin, created_at, used_at)
             VALUES (?1, ?2, 'gcm', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(hash) DO UPDATE SET file = excluded.file, format = 'gcm', key = excluded.key,
                nonce = excluded.nonce, blob_sha256 = excluded.blob_sha256, pt_len = excluded.pt_len,
                mime = excluded.mime, origin = excluded.origin, used_at = excluded.used_at,
                missing_at = NULL, chunk_size = NULL, have = NULL",
            rusqlite::params![
                bytes_to_hex_32(&entry.hash), entry.file, key, nonce, entry.blob_sha256,
                entry.pt_len as i64, entry.mime, entry.origin.as_str(), now(),
            ],
        )
        .map(|_| ())
        .map_err(|e| format!("keep media: {e}"))
    })?;
    Ok(old.filter(|file| *file != entry.file))
}

pub fn get(hash: &[u8; 32]) -> Result<Option<StoredMedia>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row = conn.query_row(
        "SELECT file, key, nonce, blob_sha256, pt_len, mime, origin FROM media_store WHERE hash = ?1",
        [bytes_to_hex_32(hash)],
        |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?,
            ))
        },
    );
    let (file, key, nonce, blob_sha256, pt_len, mime, origin) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(format!("read media: {e}")),
    };
    // A key that won't open is no key: the file can't be read either.
    Ok(crate::crypto::open_batch(|at_rest| {
        Some(StoredMedia {
            hash: *hash, file, key: at_rest.open(key).ok()?, nonce: at_rest.open(nonce).ok()?,
            blob_sha256, pt_len: pt_len as u64, mime, origin: Origin::parse(&origin),
        })
    }))
}

/// Whether a row is kept for `hash`, readable or not.
pub fn has_row(hash: &[u8; 32]) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row("SELECT 1 FROM media_store WHERE hash = ?1", [bytes_to_hex_32(hash)], |_| Ok(()))
        .map(|_| true)
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(false) } else { Err(format!("read media: {e}")) })
}

/// Forget a kept file's row, returning its file name. Deleted pages are zeroed, so the key
/// doesn't linger in the database file.
pub fn remove(hash: &[u8; 32]) -> Result<Option<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let file: Option<String> = match conn.query_row("SELECT file FROM media_store WHERE hash = ?1", [bytes_to_hex_32(hash)], |r| r.get(0)) {
        Ok(file) => Some(file),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(format!("read media: {e}")),
    };
    if file.is_some() {
        let before: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0)).unwrap_or(0);
        let _ = conn.execute_batch("PRAGMA secure_delete = ON");
        let removed = conn.execute("DELETE FROM media_store WHERE hash = ?1", [bytes_to_hex_32(hash)]);
        let _ = conn.execute_batch(&format!("PRAGMA secure_delete = {before}"));
        removed.map_err(|e| format!("forget media: {e}"))?;
    }
    Ok(file)
}

/// Every kept hash with its file name, and whether its file was missing at the last check. A row
/// whose hash doesn't read is left out; [`files`] still names its file.
pub fn all() -> Result<Vec<([u8; 32], String, bool)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT hash, file, missing_at IS NOT NULL FROM media_store")
        .map_err(|e| format!("list media: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))
        .map_err(|e| format!("list media: {e}"))?
        .flatten()
        .filter_map(|(hash, file, missing)| hex_to_bytes_32_checked(&hash).map(|h| (h, file, missing)))
        .collect();
    Ok(rows)
}

/// Every file a row names, whether or not the rest of the row reads.
pub fn files() -> Result<std::collections::HashSet<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT file FROM media_store").map_err(|e| format!("list media: {e}"))?;
    let files = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| format!("list media: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("list media: {e}"))?;
    Ok(files)
}

/// Every kept hash with its type.
pub fn totals_by_hash() -> Result<Vec<([u8; 32], String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT hash, mime FROM media_store").map_err(|e| format!("list media: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("list media: {e}"))?
        .flatten()
        .filter_map(|(hash, mime)| hex_to_bytes_32_checked(&hash).map(|h| (h, mime)))
        .collect();
    Ok(rows)
}

/// Kept bytes and file count by type, for the storage breakdown: `(mime, files, bytes)`.
pub fn totals() -> Result<Vec<(String, u64, u64)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT mime, COUNT(*), SUM(pt_len) FROM media_store GROUP BY mime")
        .map_err(|e| format!("count media: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64)))
        .map_err(|e| format!("count media: {e}"))?
        .flatten()
        .collect();
    Ok(rows)
}

/// Mark the rows whose file was not found and clear the ones found again, in one write.
/// A second miss in a row is when a row goes, since some rows hold the only key there is.
pub fn set_missing(lost: &[[u8; 32]], found: &[[u8; 32]]) -> Result<(), String> {
    if lost.is_empty() && found.is_empty() {
        return Ok(());
    }
    let mut conn = super::get_write_connection_guard_static()?;
    let tx = conn.transaction().map_err(|e| format!("mark media: {e}"))?;
    {
        let mut mark = tx
            .prepare("UPDATE media_store SET missing_at = COALESCE(missing_at, ?2) WHERE hash = ?1")
            .map_err(|e| format!("mark media: {e}"))?;
        let at = now();
        for hash in lost {
            mark.execute(rusqlite::params![bytes_to_hex_32(hash), at]).map_err(|e| format!("mark media: {e}"))?;
        }
        let mut clear = tx
            .prepare("UPDATE media_store SET missing_at = NULL WHERE hash = ?1")
            .map_err(|e| format!("mark media: {e}"))?;
        for hash in found {
            clear.execute([bytes_to_hex_32(hash)]).map_err(|e| format!("mark media: {e}"))?;
        }
    }
    tx.commit().map_err(|e| format!("mark media: {e}"))
}

pub fn touch(hash: &[u8; 32]) {
    if let Ok(conn) = super::get_write_connection_guard_static() {
        let _ = conn.execute("UPDATE media_store SET used_at = ?2 WHERE hash = ?1", rusqlite::params![bytes_to_hex_32(hash), now()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(94000);

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

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let account = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    pub(crate) fn sample(hash: [u8; 32]) -> StoredMedia {
        StoredMedia {
            hash, file: "f00d".into(), key: "c3".repeat(32), nonce: "d4".repeat(16), blob_sha256: "ee".repeat(32),
            pt_len: 1234, mime: "image/png".into(), origin: Origin::Received,
        }
    }

    #[test]
    fn a_kept_file_reads_back_and_is_kept_once() {
        let (_tmp, _guard) = init_test_db();
        let entry = sample([7; 32]);
        assert!(insert(&entry).unwrap());
        assert!(!insert(&StoredMedia { file: "other".into(), ..entry.clone() }).unwrap(), "the first copy stands");
        assert_eq!(get(&[7; 32]).unwrap(), Some(entry.clone()));
        assert_eq!(all().unwrap(), vec![([7; 32], "f00d".to_string(), false)]);
        assert_eq!(files().unwrap(), std::collections::HashSet::from(["f00d".to_string()]));
        assert_eq!(totals().unwrap(), vec![("image/png".to_string(), 1, 1234)]);
        assert_eq!(remove(&[7; 32]).unwrap().as_deref(), Some("f00d"));
        assert_eq!(get(&[7; 32]).unwrap(), None);
        assert_eq!(remove(&[7; 32]).unwrap(), None);
    }

    #[test]
    fn a_missing_file_is_forgiven_once() {
        let (_tmp, _guard) = init_test_db();
        insert(&sample([8; 32])).unwrap();
        insert(&sample([9; 32])).unwrap();
        let missing = |h: [u8; 32]| all().unwrap().into_iter().find(|r| r.0 == h).unwrap().2;
        assert!(!missing([8; 32]));
        set_missing(&[[8; 32]], &[]).unwrap();
        assert!(missing([8; 32]) && !missing([9; 32]), "a miss marks only its own row");
        set_missing(&[[8; 32]], &[]).unwrap();
        assert!(missing([8; 32]), "a second miss keeps the mark");
        set_missing(&[], &[[8; 32]]).unwrap();
        assert!(!missing([8; 32]), "found again, so the count starts over");
    }

    #[test]
    fn its_key_and_nonce_are_sealed_on_disk() {
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        crate::state::ENCRYPTION_KEY.set([0x44; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let entry = sample([9; 32]);
        insert(&entry).unwrap();
        let (key, nonce): (String, String) = crate::db::get_db_connection_guard_static().unwrap()
            .query_row("SELECT key, nonce FROM media_store", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(key != entry.key && nonce != entry.nonce);
        assert_eq!(get(&[9; 32]).unwrap(), Some(entry));
        crate::state::ENCRYPTION_KEY.set([0x45; 32], &[]);
        assert_eq!(get(&[9; 32]).unwrap(), None, "under the wrong key it can't open");
    }
}
