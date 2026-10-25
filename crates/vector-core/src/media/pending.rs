//! A send's own bytes, for its bubble while it goes out: the sender's file until it's sealed,
//! then the sealed copy waiting to upload or to retry. Per session, never stored.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
enum Source {
    /// The sender's own file, as they picked it.
    Plain(PathBuf),
    /// Our sealed copy: ciphertext and tag under the message's own key.
    Sealed { path: PathBuf, key: String, nonce: String },
}

#[derive(Default)]
struct Map(Mutex<HashMap<[u8; 32], (Source, String)>>);

struct PendingKey;

fn map() -> Arc<Map> {
    crate::db::current_session().scoped::<PendingKey, Map>()
}

fn lock(map: &Map) -> MutexGuard<'_, HashMap<[u8; 32], (Source, String)>> {
    map.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Built with the session, so a rebind's copy of the session map shares it.
pub(crate) fn attach(session: &Arc<crate::db::Session>) {
    let _ = session.scoped::<PendingKey, Map>();
}

/// The bubble reads the sender's own file.
pub fn plain(hash: [u8; 32], path: PathBuf, mime: &str) {
    lock(&map()).insert(hash, (Source::Plain(path), mime.to_string()));
}

/// The bubble reads our sealed copy, kept for a retry.
pub fn sealed(hash: [u8; 32], path: PathBuf, key: &str, nonce: &str, mime: &str) {
    lock(&map()).insert(hash, (Source::Sealed { path, key: key.to_string(), nonce: nonce.to_string() }, mime.to_string()));
}

pub fn forget(hash: &[u8; 32]) {
    lock(&map()).remove(hash);
}

pub fn holds(hash: &[u8; 32]) -> bool {
    lock(&map()).contains_key(hash)
}

/// The sender's file behind `hash`, while it's still where they picked it.
pub fn plain_path(hash: &[u8; 32]) -> Option<PathBuf> {
    match lock(&map()).get(hash) {
        Some((Source::Plain(path), _)) if path.is_file() => Some(path.clone()),
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open(hash: &[u8; 32]) -> Option<Arc<dyn super::MediaSource>> {
    let (source, mime) = lock(&map()).get(hash)?.clone();
    match source {
        Source::Plain(path) => {
            let file = std::fs::File::open(&path).ok()?;
            let len = file.metadata().ok()?.len();
            Some(Arc::new(PlainFile { file, len, mime }))
        }
        Source::Sealed { path, key, nonce } => {
            super::store::StoredGcm::sealed(&path, &key, &nonce, &mime).map(|s| Arc::new(s) as Arc<dyn super::MediaSource>)
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn open(_hash: &[u8; 32]) -> Option<Arc<dyn super::MediaSource>> {
    None
}

/// The sender's own file, read where it lies.
#[cfg(not(target_arch = "wasm32"))]
struct PlainFile {
    file: std::fs::File,
    len: u64,
    mime: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl super::MediaSource for PlainFile {
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
        super::store::read_exact_at(&self.file, &mut buf[..n], offset)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_shows_its_own_file_then_its_sealed_copy() {
        // The live session's map: no other test may swap it out mid-check.
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        std::fs::write(&file, b"the sender's own bytes").unwrap();
        let hash = [0x71u8; 32];
        plain(hash, file.clone(), "image/png");
        assert!(holds(&hash) && crate::media::resident(&hash));
        assert_eq!(plain_path(&hash), Some(file.clone()));
        let src = open(&hash).unwrap();
        assert_eq!(crate::media::read_all(&*src).unwrap(), b"the sender's own bytes");

        let params = crate::crypto::generate_encryption_params();
        let enc = dir.path().join("photo.enc");
        std::fs::write(&enc, crate::crypto::encrypt_data(b"the sender's own bytes", &params).unwrap()).unwrap();
        sealed(hash, enc, &params.key, &params.nonce, "image/png");
        assert_eq!(plain_path(&hash), None);
        assert_eq!(crate::media::read_all(&*open(&hash).unwrap()).unwrap(), b"the sender's own bytes");
        forget(&hash);
        assert!(!holds(&hash) && open(&hash).is_none());
    }
}
