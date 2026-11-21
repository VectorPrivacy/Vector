//! A pasted picture has no file of its own: it's held in memory under a path that is never
//! written, so the preview and the send read it without a plain copy touching disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Pasted;
type Held = Mutex<HashMap<PathBuf, Arc<Vec<u8>>>>;

fn held() -> Arc<Held> {
    vector_core::db::current_session().scoped::<Pasted, _>()
}

/// `bytes` held under a fresh `pasted_image.<ext>` path.
pub fn hold(bytes: Vec<u8>, ext: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = super::archive::work_dir().join(format!("pasted-{n}")).join(format!("pasted_image.{ext}"));
    held().lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone(), Arc::new(bytes));
    path
}

pub fn get(path: &Path) -> Option<Arc<Vec<u8>>> {
    held().lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
}

pub fn forget(path: &Path) {
    held().lock().unwrap_or_else(|e| e.into_inner()).remove(path);
}

/// What a preview or send reads: the held bytes, else the file (sealed or plain).
pub fn read(path: &Path) -> std::io::Result<Arc<Vec<u8>>> {
    if let Some(b) = get(path) {
        return Ok(b);
    }
    vector_core::vcache::read(path).map(|mut b| Arc::new(std::mem::take(&mut *b))).map_err(std::io::Error::other)
}

pub fn len(path: &Path) -> Option<u64> {
    get(path).map(|b| b.len() as u64).or_else(|| std::fs::metadata(path).ok().map(|m| m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_picture_is_read_from_memory_and_never_written() {
        let path = hold(vec![7; 12], "png");
        assert!(!path.exists() && !path.parent().unwrap().exists());
        assert_eq!(len(&path), Some(12));
        assert_eq!(*read(&path).unwrap(), vec![7; 12]);
        super::super::archive::forget(&path);
        assert!(get(&path).is_none() && read(&path).is_err());
    }
}
