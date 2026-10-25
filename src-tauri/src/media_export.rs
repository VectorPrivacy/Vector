//! Plaintext copies of held media, made only when the user asks for one. Save writes where they
//! choose; Open, Share and Copy hand another app a copy that the next launch deletes.

use std::path::{Path, PathBuf};

use tauri::Manager;
use vector_core::media::{self, MediaRef};

fn root() -> Result<PathBuf, String> {
    let app = crate::TAURI_APP.get().ok_or("Vector is still starting")?;
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("exports"))
}

/// Clear what earlier launches handed out. Never mid-run: a share may still be reading one.
pub fn purge() {
    if let Ok(dir) = root() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Whether `path` is one of this launch's exports.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn is_export(path: &Path) -> bool {
    let (Ok(root), Ok(path)) = (root().and_then(|r| std::fs::canonicalize(r).map_err(|e| e.to_string())), std::fs::canonicalize(path)) else {
        return false;
    };
    path.starts_with(root)
}

/// A file another app can open for `path_or_ref`: held media and sealed cache files are written
/// out as `name`, and any other path is itself.
pub fn materialize(path_or_ref: &str, name: &str) -> Result<PathBuf, String> {
    match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::File(path) if vector_core::vcache::is_sealed(Path::new(&path)) => {
            let bytes = vector_core::vcache::read(Path::new(&path)).map_err(|e| format!("Couldn't read the file: {e}"))?;
            let ext = Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or_default();
            let hash = vector_core::crypto::sha256::digest(&bytes);
            export(&bytes, file_name(name, &hash, vector_core::crypto::mime_from_extension(ext)))
        }
        MediaRef::File(path) => Ok(PathBuf::from(path)),
        MediaRef::Held(hash) => {
            let (bytes, mime) = held(&hash)?;
            export(&bytes, file_name(name, &hash, &mime))
        }
    }
}

/// `bytes` as `name` in a folder of its own under this launch's exports.
fn export(bytes: &[u8], name: String) -> Result<PathBuf, String> {
    let mut id = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let dir = root()?.join(crate::util::bytes_to_hex_string(&id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't prepare the file: {e}"))?;
    let file = dir.join(name);
    write_private(&file, bytes)?;
    Ok(file)
}

/// The package a Mini App runs from: a file is itself; a kept app is written out once per
/// launch to a path named by its hash, so its window and storage stay the same app.
pub fn package_path(path_or_ref: &str) -> Result<String, String> {
    match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::File(path) => Ok(path),
        MediaRef::Held(hash) => {
            let dir = root()?.join("apps");
            let file = dir.join(format!("{}.xdc", crate::util::bytes_to_hex_string(&hash)));
            let src = media::open(&hash).ok_or("This app isn't on this device any more")?;
            if std::fs::metadata(&file).is_ok_and(|m| m.len() == src.len()) {
                return Ok(file.to_string_lossy().into_owned());
            }
            std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
            let bytes = zeroize::Zeroizing::new(media::read_all(&*src).map_err(|e| format!("Couldn't read the app: {e}"))?);
            let tmp = dir.join(format!(".{}.tmp", crate::util::bytes_to_hex_string(&hash)));
            let _ = std::fs::remove_file(&tmp);
            write_private(&tmp, &bytes)?;
            std::fs::rename(&tmp, &file).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
            Ok(file.to_string_lossy().into_owned())
        }
    }
}

/// A normal copy of held media in the download folder, for an account that asked for one: the
/// same file already there under its name is used as it is. Returns where it is.
pub fn save_to_downloads(hash: &[u8; 32], name: &str) -> Result<PathBuf, String> {
    let (bytes, mime) = held(hash)?;
    let dir = vector_core::db::get_download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't save the file: {e}"))?;
    let name = file_name(name, hash, &mime);
    let candidate = dir.join(&name);
    let hex = crate::util::bytes_to_hex_string(hash);
    if vector_core::crypto::stream::hash_file(&candidate).is_ok_and(|(h, _)| h == hex) {
        return Ok(candidate);
    }
    let dest = vector_core::crypto::resolve_unique_filename(&dir, &name);
    let tmp = dir.join(format!(".{hex}.saving"));
    std::fs::write(&tmp, &*bytes).and_then(|_| std::fs::rename(&tmp, &dest)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Couldn't save the file: {e}")
    })?;
    Ok(dest)
}

/// A held file's bytes, for readers that parse them in memory.
pub fn held_bytes(path_or_ref: &str) -> Option<zeroize::Zeroizing<Vec<u8>>> {
    match MediaRef::parse(path_or_ref)? {
        MediaRef::Held(hash) => held(&hash).ok().map(|(bytes, _)| bytes),
        MediaRef::File(_) => None,
    }
}

/// Write `path_or_ref` to `dest`: beside it first, then into place.
pub fn save_to(path_or_ref: &str, dest: &Path) -> Result<(), String> {
    let bytes = match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::Held(hash) => held(&hash)?.0,
        MediaRef::File(path) => vector_core::vcache::read(Path::new(&path)).map_err(|e| format!("Couldn't read the file: {e}"))?,
    };
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dest.with_file_name(format!(".{name}.saving"));
    std::fs::write(&tmp, &*bytes)
        .and_then(|_| std::fs::rename(&tmp, dest))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Couldn't save the file: {e}")
        })
}

/// The name a file is saved under: the sender's, else its hash and type.
pub fn file_name(name: &str, hash: &[u8; 32], mime: &str) -> String {
    let name = vector_core::crypto::sanitize_filename(name);
    if !name.is_empty() {
        return name;
    }
    let ext = vector_core::crypto::extension_from_mime(mime);
    let stem = &crate::util::bytes_to_hex_string(hash)[..12];
    if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") }
}

fn held(hash: &[u8; 32]) -> Result<(zeroize::Zeroizing<Vec<u8>>, String), String> {
    let src = media::open(hash).ok_or("This file isn't open any more: show it again first")?;
    let bytes = media::read_all(&*src).map_err(|e| format!("Couldn't read the file: {e}"))?;
    Ok((zeroize::Zeroizing::new(bytes), src.mime().to_string()))
}

fn write_private(file: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(file)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| format!("Couldn't prepare the file: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_named_by_its_sender_or_its_hash() {
        let hash = [0xabu8; 32];
        assert_eq!(file_name("holiday.jpg", &hash, "image/jpeg"), "holiday.jpg");
        assert_eq!(file_name("../../etc/passwd", &hash, "image/jpeg"), "passwd");
        assert_eq!(file_name("", &hash, "image/png"), "abababababab.png");
    }

    #[test]
    fn saving_writes_the_held_bytes_and_leaves_no_temp() {
        let _session = crate::commands::transport::tests::TEST_LOCK.blocking_lock();
        let hash = [0x5cu8; 32];
        media::ram::cache().insert(hash, b"held bytes".to_vec(), "image/png").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("saved.png");
        save_to(&MediaRef::held(&hash), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"held bytes");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(save_to(&MediaRef::held(&[0x5du8; 32]), &dir.path().join("gone.png")).is_err());
    }
}
