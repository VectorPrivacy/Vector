//! Files this account keeps, as the ciphertext they arrived as, under `<account>/media/`. The
//! ledger (`db::media_store`) holds each file's key; nothing here is ever plaintext.

use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

#[cfg(not(target_arch = "wasm32"))]
use zeroize::Zeroizing;

use crate::db::media_store::{self, Origin, StoredMedia};

/// The hashes this account keeps, read once its database opens: residency is asked under the
/// STATE lock, where a query has no business.
#[derive(Default)]
struct Index(RwLock<Option<HashSet<[u8; 32]>>>);

struct IndexKey;

fn index() -> Arc<Index> {
    crate::db::current_session().scoped::<IndexKey, Index>()
}

/// Built with the session, so a rebind's copy of the session map shares it.
pub(crate) fn attach(session: &Arc<crate::db::Session>) {
    let _ = session.scoped::<IndexKey, Index>();
}

/// Read the kept hashes into `session` once its database opens, straight from `conn`: the
/// task opening it may still be bound to the account before.
pub(crate) fn load_index(session: &Arc<crate::db::Session>, conn: &rusqlite::Connection) {
    let hashes = conn.prepare("SELECT hash FROM media_store").ok().map(|mut stmt| {
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| rows.flatten().filter_map(|h| crate::simd::hex::hex_to_bytes_32_checked(&h)).collect::<HashSet<_>>())
            .unwrap_or_default()
    });
    *session.scoped::<IndexKey, Index>().0.write().unwrap_or_else(|e| e.into_inner()) = hashes;
}

pub fn holds(hash: &[u8; 32]) -> bool {
    index().0.read().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|set| set.contains(hash))
}

/// Stop answering for `hash` at once, before its row and file go: a read already under way then
/// finds it gone instead of putting it back in memory.
#[cfg(target_arch = "wasm32")]
pub(crate) fn stop_holding(hash: &[u8; 32]) {
    indexed(*hash, false);
}

fn indexed(hash: [u8; 32], present: bool) {
    if let Some(set) = index().0.write().unwrap_or_else(|e| e.into_inner()).as_mut() {
        if present {
            set.insert(hash);
        } else {
            set.remove(&hash);
        }
    }
}

pub fn dir() -> Result<PathBuf, String> {
    Ok(crate::db::current_account_dir()?.join("media"))
}

pub(crate) fn fresh_name() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    crate::simd::hex::bytes_to_hex_string(&bytes)
}

/// What a kept file is, besides its bytes.
#[derive(Debug, Clone)]
pub struct Keep {
    pub hash: [u8; 32],
    pub key: String,
    pub nonce: String,
    pub blob_sha256: String,
    pub pt_len: u64,
    pub mime: String,
    pub origin: Origin,
}

/// Keep a verified blob: `src` moves into the store, then a row records how to open it.
#[cfg(not(target_arch = "wasm32"))]
pub fn keep_file(src: &Path, keep: Keep) -> Result<(), String> {
    let (dir, name) = place(src)?;
    record(&dir.join(&name), name, keep)
}

/// Move `src` into the store under a fresh name, durable. Returns the store and the name.
#[cfg(not(target_arch = "wasm32"))]
fn place(src: &Path) -> Result<(PathBuf, String), String> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't keep the file: {e}"))?;
    let name = fresh_name();
    let dest = dir.join(&name);
    // A rename can't cross filesystems: copy beside the destination, then rename that.
    if std::fs::rename(src, &dest).is_err() {
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::copy(src, &tmp)
            .and_then(|_| std::fs::rename(&tmp, &dest))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("Couldn't keep the file: {e}")
            })?;
        let _ = std::fs::remove_file(src);
    }
    settle(&dir, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;
    Ok((dir, name))
}

/// Keep a sealed file in place of anything kept for its hash, once it has read back whole as
/// those bytes. An old copy goes only after the new one is recorded.
#[cfg(not(target_arch = "wasm32"))]
pub fn keep_verified(src: &Path, keep: Keep) -> Result<(), String> {
    let (dir, name) = place(src)?;
    let dest = dir.join(&name);
    let hex = crate::simd::hex::bytes_to_hex_32(&keep.hash);
    let good = crate::crypto::stream::verify_file(&dest, &keep.key, &keep.nonce).is_ok_and(|seen| seen.output_sha256 == hex);
    if !good {
        let _ = std::fs::remove_file(&dest);
        return Err("the kept copy didn't read back as the file".into());
    }
    let hash = keep.hash;
    let entry = StoredMedia {
        hash, file: name, key: keep.key, nonce: keep.nonce, blob_sha256: keep.blob_sha256,
        pt_len: keep.pt_len, mime: keep.mime, origin: keep.origin,
    };
    match media_store::replace(&entry) {
        Ok(old) => {
            indexed(hash, true);
            if let Some(old) = old {
                let _ = std::fs::remove_file(dir.join(old));
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(e)
        }
    }
}

/// Keep a verified blob held in memory.
#[cfg(not(target_arch = "wasm32"))]
pub fn keep_bytes(blob: &[u8], keep: Keep) -> Result<(), String> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't keep the file: {e}"))?;
    let name = fresh_name();
    let (tmp, dest) = (dir.join(format!(".{name}.tmp")), dir.join(&name));
    std::fs::write(&tmp, blob).and_then(|_| std::fs::rename(&tmp, &dest)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Couldn't keep the file: {e}")
    })?;
    settle(&dir, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;
    record(&dest, name, keep)
}

/// Seal plaintext under fresh parameters and keep it: for bytes that came without a blob of
/// their own to keep (an unencrypted source, a file being imported).
#[cfg(not(target_arch = "wasm32"))]
pub fn keep_sealed(plain: &[u8], hash: [u8; 32], mime: &str, origin: Origin) -> Result<(), String> {
    let params = crate::crypto::generate_encryption_params();
    let blob = crate::crypto::encrypt_data(plain, &params)?;
    keep_bytes(&blob, Keep {
        hash, key: params.key.clone(), nonce: params.nonce.clone(), blob_sha256: crate::crypto::sha256_hex(&blob),
        pt_len: plain.len() as u64, mime: mime.to_string(), origin,
    })
}

/// Durable before its row exists, and dated now: a rename keeps an old download's mtime, which
/// the orphan sweep would read as abandoned.
#[cfg(not(target_arch = "wasm32"))]
fn settle(dir: &Path, file: &Path) -> Result<(), String> {
    let f = std::fs::OpenOptions::new().write(true).open(file).map_err(|e| format!("Couldn't keep the file: {e}"))?;
    let _ = f.set_modified(web_time::SystemTime::now());
    f.sync_all().map_err(|e| format!("Couldn't keep the file: {e}"))?;
    // Windows can't open a directory to sync it; there the rename is already durable.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn record(dest: &Path, file: String, keep: Keep) -> Result<(), String> {
    match record_row(file, keep) {
        Ok(true) => Ok(()),
        // Kept already: this copy is spare.
        Ok(false) => {
            let _ = std::fs::remove_file(dest);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(dest);
            Err(e)
        }
    }
}

/// The row for a file just written; false when the hash was kept already.
fn record_row(file: String, keep: Keep) -> Result<bool, String> {
    let hash = keep.hash;
    let entry = StoredMedia {
        hash, file, key: keep.key, nonce: keep.nonce, blob_sha256: keep.blob_sha256,
        pt_len: keep.pt_len, mime: keep.mime, origin: keep.origin,
    };
    let fresh = media_store::insert(&entry)?;
    indexed(hash, true);
    Ok(fresh)
}

/// Keep plaintext on the web, sealed under fresh parameters into the origin's private files.
#[cfg(target_arch = "wasm32")]
pub async fn keep_sealed(plain: &[u8], hash: [u8; 32], mime: &str, origin: Origin) -> Result<(), String> {
    let params = crate::crypto::generate_encryption_params();
    let blob = crate::crypto::encrypt_data(plain, &params)?;
    let name = fresh_name();
    let path = dir()?.join(&name);
    crate::webfiles::write(&path, &blob).await?;
    let keep = Keep {
        hash, key: params.key.clone(), nonce: params.nonce.clone(), blob_sha256: crate::crypto::sha256_hex(&blob),
        pt_len: plain.len() as u64, mime: mime.to_string(), origin,
    };
    match record_row(name, keep) {
        Ok(true) => Ok(()),
        other => {
            crate::webfiles::remove(&path).await;
            other.map(|_| ())
        }
    }
}

/// A kept file's plaintext and type on the web, read whole: the origin's files are async.
#[cfg(target_arch = "wasm32")]
pub async fn read_plain(hash: &[u8; 32]) -> Option<(Vec<u8>, String)> {
    if !holds(hash) {
        return None;
    }
    let entry = media_store::get(hash).ok().flatten()?;
    let blob = crate::webfiles::read(&dir().ok()?.join(&entry.file)).await.ok()?;
    let plain = crate::crypto::decrypt_data_owned(blob, &entry.key, &entry.nonce).ok()?;
    Some((plain, entry.mime))
}

/// Stop keeping `hash` on the web: the row first, then its file.
#[cfg(target_arch = "wasm32")]
pub async fn forget(hash: &[u8; 32]) {
    if let Ok(Some(file)) = media_store::remove(hash) {
        if let Ok(dir) = dir() {
            crate::webfiles::remove(&dir.join(file)).await;
        }
        let _ = crate::db::attachments::clear_kept(&crate::simd::hex::bytes_to_hex_32(hash));
    }
    indexed(*hash, false);
}

/// Stop keeping `hash`: the row goes first, then its file, and the attachments that had no
/// other copy are no longer downloaded.
#[cfg(not(target_arch = "wasm32"))]
pub fn forget(hash: &[u8; 32]) {
    if let Ok(Some(file)) = media_store::remove(hash) {
        if let Ok(dir) = dir() {
            let _ = std::fs::remove_file(dir.join(file));
        }
        let _ = crate::db::attachments::clear_kept(&crate::simd::hex::bytes_to_hex_32(hash));
    }
    indexed(*hash, false);
}

/// The files kept here, and their size in bytes.
pub fn kept_files() -> (u64, u64) {
    media_store::totals()
        .unwrap_or_default()
        .into_iter()
        .fold((0, 0), |(files, bytes), (_, n, size)| (files + n, bytes + size))
}

/// Forget every kept file whose type `matches`; returns the hashes forgotten.
#[cfg(not(target_arch = "wasm32"))]
pub fn forget_where(matches: impl Fn(&str) -> bool) -> Vec<[u8; 32]> {
    let rows = media_store::totals_by_hash().unwrap_or_default();
    let gone: Vec<[u8; 32]> = rows.into_iter().filter(|(_, mime)| matches(mime)).map(|(h, _)| h).collect();
    for hash in &gone {
        forget(hash);
    }
    gone
}

/// A kept file, read through its own keystream.
#[cfg(not(target_arch = "wasm32"))]
pub fn open(hash: &[u8; 32]) -> Option<Arc<dyn super::MediaSource>> {
    if !holds(hash) {
        return None;
    }
    let entry = media_store::get(hash).ok().flatten()?;
    let (key, nonce) = crate::crypto::attachment_params(&entry.key, &entry.nonce, "Invalid key").ok()?;
    let file = std::fs::File::open(dir().ok()?.join(&entry.file)).ok()?;
    // The blob is the plaintext and its 16-byte tag.
    if file.metadata().ok()?.len() != entry.pt_len + 16 {
        return None;
    }
    Some(Arc::new(StoredGcm { file, key: Zeroizing::new(key), nonce, len: entry.pt_len, mime: entry.mime }))
}

/// What reading a kept copy back whole found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// It opens under its key, tag and all, and is those bytes.
    Good,
    /// Provably not those bytes: gone, the wrong size, or other bytes under a key that opened.
    Bad,
    /// It couldn't be checked: a key that won't open yet, or a read that failed.
    Unknown,
}

/// Read the kept copy of `hash` back whole: for when it is about to become the only one.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_back(hash: &[u8; 32]) -> Check {
    let Ok(dir) = dir() else { return Check::Unknown };
    let entry = match media_store::get(hash) {
        Ok(Some(entry)) => entry,
        // No row is no copy; a row whose key won't open can't be judged.
        Ok(None) if media_store::has_row(hash) == Ok(false) => return Check::Bad,
        _ => return Check::Unknown,
    };
    let path = dir.join(&entry.file);
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Check::Bad,
        Err(_) => return Check::Unknown,
        Ok(meta) if meta.len() != entry.pt_len + 16 => return Check::Bad,
        Ok(_) => {}
    }
    match crate::crypto::stream::verify_file(&path, &entry.key, &entry.nonce) {
        Ok(seen) if seen.output_sha256 == crate::simd::hex::bytes_to_hex_32(hash) => Check::Good,
        Ok(_) => Check::Bad,
        Err(e) if e == crate::crypto::stream::TAG_MISMATCH => Check::Bad,
        Err(_) => Check::Unknown,
    }
}


#[cfg(not(target_arch = "wasm32"))]
impl StoredGcm {
    /// A blob of our own sealing, read the same way: `path` holds ciphertext and tag.
    pub(super) fn sealed(path: &Path, key_hex: &str, nonce_hex: &str, mime: &str) -> Option<Self> {
        let (key, nonce) = crate::crypto::attachment_params(key_hex, nonce_hex, "Invalid key").ok()?;
        let file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len().checked_sub(16)?;
        Some(StoredGcm { file, key: Zeroizing::new(key), nonce, len, mime: mime.to_string() })
    }
}

#[cfg(target_arch = "wasm32")]
pub fn open(_hash: &[u8; 32]) -> Option<Arc<dyn super::MediaSource>> {
    None
}

/// A kept blob, decrypted a range at a time. Its tag was checked when it was kept; anyone who
/// could change the file since could change the database too.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct StoredGcm {
    file: std::fs::File,
    key: Zeroizing<[u8; 32]>,
    nonce: [u8; 16],
    len: u64,
    mime: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl super::MediaSource for StoredGcm {
    fn len(&self) -> u64 {
        self.len
    }

    fn mime(&self) -> &str {
        &self.mime
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let n = buf.len().min(usize::try_from(self.len - offset).unwrap_or(usize::MAX));
        let part = &mut buf[..n];
        read_exact_at(&self.file, part, offset)?;
        crate::crypto::gcm::Keystream::new(&self.key, &self.nonce)
            .apply_at(offset, part)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "past the end of the cipher"))?;
        Ok(n)
    }
}

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &std::fs::File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buf, offset)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

/// Check every row against its file. A file missing once is marked; missing twice, its row
/// goes, since some rows hold the only key there is. Returns the hashes no longer kept.
#[cfg(not(target_arch = "wasm32"))]
pub fn check() -> Vec<[u8; 32]> {
    let (Ok(dir), Ok(rows), Ok(named)) = (dir(), media_store::all(), media_store::files()) else {
        crate::log_warn!("[Media] the kept files couldn't be listed; the launch check is skipped");
        return Vec::new();
    };
    if rows.len() < named.len() {
        crate::log_warn!("[Media] {} kept rows don't read; their files are left alone", named.len() - rows.len());
    }
    let (mut lost, mut found, mut gone) = (Vec::new(), Vec::new(), Vec::new());
    for (hash, file, missing) in rows {
        if dir.join(&file).is_file() {
            if missing {
                found.push(hash);
            }
        } else if missing {
            forget(&hash);
            gone.push(hash);
        } else {
            lost.push(hash);
        }
    }
    let _ = media_store::set_missing(&lost, &found);
    // Files no row names, from a crash between a file and its row, once they're an hour old.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let hour = std::time::Duration::from_secs(3600);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > hour);
            if old && !named.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(95000);

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

    fn sealed(len: usize) -> (Vec<u8>, Vec<u8>, crate::crypto::EncryptionParams) {
        let plain: Vec<u8> = (0..len).map(|i| (i * 17 % 251) as u8).collect();
        let params = crate::crypto::generate_encryption_params();
        let blob = crate::crypto::encrypt_data(&plain, &params).unwrap();
        (plain, blob, params)
    }

    fn keep_of(plain: &[u8], blob: &[u8], params: &crate::crypto::EncryptionParams) -> Keep {
        Keep {
            hash: crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(plain)),
            key: params.key.clone(), nonce: params.nonce.clone(), blob_sha256: crate::crypto::sha256_hex(blob),
            pt_len: plain.len() as u64, mime: "video/mp4".into(), origin: Origin::Received,
        }
    }

    #[test]
    fn a_kept_blob_reads_back_any_range_and_never_as_plaintext_on_disk() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(300_001);
        let keep = keep_of(&plain, &blob, &params);
        keep_bytes(&blob, keep.clone()).unwrap();
        assert!(holds(&keep.hash) && crate::media::resident(&keep.hash));

        let src = crate::media::open(&keep.hash).expect("kept");
        assert_eq!((src.len(), src.mime()), (plain.len() as u64, "video/mp4"));
        for (at, n) in [(0usize, 16usize), (15, 2), (4095, 70_000), (300_000, 1), (299_990, 50)] {
            let mut buf = vec![0u8; n];
            let got = src.read_at(at as u64, &mut buf).unwrap();
            assert_eq!(&buf[..got], &plain[at..(at + n).min(plain.len())], "at {at}");
        }
        assert_eq!(crate::media::read_all(&*src).unwrap(), plain);

        let on_disk: Vec<Vec<u8>> = std::fs::read_dir(dir().unwrap()).unwrap().flatten().map(|e| std::fs::read(e.path()).unwrap()).collect();
        assert_eq!(on_disk, vec![blob], "the store holds the blob, nothing else");
    }

    #[test]
    fn a_file_is_kept_once_and_forgotten_row_first() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(64);
        let keep = keep_of(&plain, &blob, &params);
        let part = dir().unwrap().with_file_name("x.part");
        std::fs::write(&part, &blob).unwrap();
        keep_file(&part, keep.clone()).unwrap();
        assert!(!part.exists(), "the download moved in");
        keep_bytes(&blob, keep.clone()).unwrap();
        assert_eq!(std::fs::read_dir(dir().unwrap()).unwrap().count(), 1, "a second copy of the same hash isn't kept");

        forget(&keep.hash);
        assert!(!holds(&keep.hash) && crate::media::open(&keep.hash).is_none());
        assert_eq!(std::fs::read_dir(dir().unwrap()).unwrap().count(), 0);
        assert_eq!(media_store::get(&keep.hash).unwrap(), None);
    }

    #[test]
    fn a_missing_file_goes_on_its_second_miss_and_an_orphan_once_it_is_old() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(32);
        let keep = keep_of(&plain, &blob, &params);
        keep_bytes(&blob, keep.clone()).unwrap();
        let file = media_store::get(&keep.hash).unwrap().unwrap().file;
        std::fs::remove_file(dir().unwrap().join(&file)).unwrap();
        assert!(check().is_empty(), "one miss only marks it");
        assert!(holds(&keep.hash));
        assert_eq!(check(), vec![keep.hash]);
        assert!(!holds(&keep.hash));

        let orphan = dir().unwrap().join("feedface");
        std::fs::write(&orphan, b"x").unwrap();
        check();
        assert!(orphan.exists(), "a new file may be a keep in flight");
        let f = std::fs::OpenOptions::new().write(true).open(&orphan).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        check();
        assert!(!orphan.exists());
    }

    #[test]
    fn a_kept_file_hours_old_is_never_taken_for_an_orphan() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(48);
        let keep = keep_of(&plain, &blob, &params);
        keep_bytes(&blob, keep.clone()).unwrap();
        let path = dir().unwrap().join(media_store::get(&keep.hash).unwrap().unwrap().file);
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        assert!(check().is_empty());
        assert!(path.exists() && holds(&keep.hash), "a row names it, however old");
    }

    #[test]
    fn a_file_found_again_starts_its_misses_over() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(56);
        let keep = keep_of(&plain, &blob, &params);
        keep_bytes(&blob, keep.clone()).unwrap();
        let path = dir().unwrap().join(media_store::get(&keep.hash).unwrap().unwrap().file);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(check().is_empty(), "one miss only marks it");
        std::fs::write(&path, &bytes).unwrap();
        assert!(check().is_empty(), "found again");
        std::fs::remove_file(&path).unwrap();
        assert!(check().is_empty(), "a miss after it was found is a first miss");
        assert!(holds(&keep.hash));
    }

    #[test]
    fn a_file_only_an_unreadable_row_names_is_never_swept() {
        let (_tmp, _guard) = init_test_db();
        let (plain, blob, params) = sealed(64);
        keep_bytes(&blob, keep_of(&plain, &blob, &params)).unwrap();
        crate::db::get_write_connection_guard_static()
            .unwrap()
            .execute(
                "INSERT INTO media_store (hash, file, format, key, nonce, blob_sha256, pt_len, mime, origin, created_at, used_at)
                 SELECT 'nothex', 'unnamed', format, key, nonce, blob_sha256, pt_len, mime, origin, created_at, used_at FROM media_store LIMIT 1",
                [],
            )
            .unwrap();
        let file = dir().unwrap().join("unnamed");
        std::fs::write(&file, b"x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        let stray = dir().unwrap().join("stray");
        std::fs::write(&stray, b"x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&stray).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        check();
        assert!(file.exists(), "a file only an unreadable row names is not an orphan");
        assert!(!stray.exists(), "the check still runs: a file no row names is swept");
    }

    #[test]
    fn every_kept_file_counts_and_goes_whatever_its_type() {
        let (_dir, _guard) = init_test_db();
        let (picture, archive) = (b"a kept picture".to_vec(), b"a kept archive".to_vec());
        let (p, a) = (
            crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(&picture)),
            crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(&archive)),
        );
        keep_sealed(&picture, p, "image/png", Origin::Sealed).unwrap();
        keep_sealed(&archive, a, "application/zip", Origin::Sealed).unwrap();
        assert_eq!(kept_files(), (2, (picture.len() + archive.len()) as u64));

        let mut gone = forget_where(|_| true);
        gone.sort();
        let mut both = vec![p, a];
        both.sort();
        assert_eq!(gone, both);
        assert!(!holds(&p) && !holds(&a));
        assert_eq!(kept_files(), (0, 0));
    }

    #[test]
    fn plaintext_with_no_blob_is_sealed_before_it_is_kept() {
        let (_tmp, _guard) = init_test_db();
        let plain = b"an unencrypted picture".to_vec();
        let hash = crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(&plain));
        keep_sealed(&plain, hash, "image/png", Origin::Sealed).unwrap();
        let on_disk = std::fs::read(std::fs::read_dir(dir().unwrap()).unwrap().next().unwrap().unwrap().path()).unwrap();
        assert!(on_disk.windows(plain.len()).all(|w| w != plain.as_slice()));
        assert_eq!(crate::media::read_all(&*crate::media::open(&hash).unwrap()).unwrap(), plain);
    }
}
