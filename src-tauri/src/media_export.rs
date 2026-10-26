//! Plaintext copies of held media, made only when the user asks for one. Save writes where they
//! choose; Open, Share and Copy hand another app a copy that the next launch deletes.

use std::path::{Path, PathBuf};

use tauri::Manager;
use vector_core::media::{self, MediaRef};

fn root() -> Result<PathBuf, String> {
    root_in(crate::TAURI_APP.get().ok_or("Vector is still starting")?)
}

fn root_in<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("exports"))
}

/// Clear what earlier launches handed out. Never mid-run: a share may still be reading one.
pub fn purge<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Ok(dir) = root_in(app) {
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
            let path = Path::new(&path);
            let name = if name.is_empty() { path.file_name().and_then(|n| n.to_str()).unwrap_or("file") } else { name };
            let src = vector_core::vcache::open(path, "application/octet-stream").ok_or("Couldn't read the file")?;
            export(&*src, vector_core::crypto::sanitize_filename(name))
        }
        MediaRef::File(path) => Ok(PathBuf::from(path)),
        MediaRef::Held(hash) => {
            let src = media::open(&hash).ok_or("This file isn't open any more: show it again first")?;
            export(&*src, file_name(name, &hash, src.mime()))
        }
    }
}

/// What `src` holds, as `name` in a folder of its own under this launch's exports.
fn export(src: &dyn media::MediaSource, name: String) -> Result<PathBuf, String> {
    let mut id = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let dir = root()?.join(crate::util::bytes_to_hex_string(&id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't prepare the file: {e}"))?;
    let name = vector_core::crypto::sanitize_filename(&name);
    let file = dir.join(if name.is_empty() { "file" } else { &name });
    copy_out(src, &file)?;
    Ok(file)
}

/// Write what `src` holds to `dest` a chunk at a time, beside it first, then into place: a
/// large file is never whole in memory.
fn copy_out(src: &dyn media::MediaSource, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dest.with_file_name(format!(".{name}.{:08x}.saving", rand::random::<u32>()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut out = std::io::BufWriter::with_capacity(1 << 20, options.open(&tmp)?);
        write_out(src, &mut out)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, dest)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Couldn't save the file: {e}")
    })
}

fn write_out(src: &dyn media::MediaSource, out: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut buf = zeroize::Zeroizing::new(vec![0u8; 1 << 20]);
    let mut at = 0u64;
    while at < src.len() {
        let n = src.read_at(at, &mut buf)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        out.write_all(&buf[..n])?;
        at += n as u64;
    }
    Ok(())
}

/// The package a Mini App runs from: a file is itself; a kept app is written out sealed once
/// per launch to a path named by its hash, so its window and storage stay the same app.
pub fn package_path(path_or_ref: &str) -> Result<String, String> {
    match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::File(path) => Ok(path),
        MediaRef::Held(hash) => {
            let dir = root()?.join("apps");
            let file = dir.join(format!("{}.xdc", crate::util::bytes_to_hex_string(&hash)));
            let src = media::open(&hash).ok_or("This app isn't on this device any more")?;
            if vector_core::vcache::open(&file, "application/zip").is_some_and(|f| f.len() == src.len()) {
                return Ok(file.to_string_lossy().into_owned());
            }
            std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
            // Sealed like any cache file: the app reads it through vcache, never in the clear.
            vector_core::vcache::write_source(&*src, &file).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
            Ok(file.to_string_lossy().into_owned())
        }
    }
}

type Source = (std::sync::Arc<dyn media::MediaSource>, Option<[u8; 32]>);

/// What `path_or_ref` names, and its content hash when the ref carries one.
fn source(path_or_ref: &str) -> Result<Source, String> {
    match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::Held(hash) => Ok((media::open(&hash).ok_or("This file isn't open any more: show it again first")?, Some(hash))),
        // A cache file opens sealed; any other file reads as it is.
        MediaRef::File(path) => Ok((vector_core::vcache::open(Path::new(&path), "application/octet-stream").ok_or("Couldn't read the file")?, None)),
    }
}

fn digest(src: &dyn media::MediaSource) -> Result<[u8; 32], String> {
    let mut hasher = vector_core::crypto::sha256::Sha256::new();
    let mut buf = zeroize::Zeroizing::new(vec![0u8; 1 << 20]);
    let mut at = 0u64;
    while at < src.len() {
        let n = src.read_at(at, &mut buf).map_err(|e| format!("Couldn't read the file: {e}"))?;
        if n == 0 {
            return Err("Couldn't read the file: it ended early".into());
        }
        hasher.update(&buf[..n]);
        at += n as u64;
    }
    Ok(hasher.finalize())
}

/// A plain copy of `path_or_ref` in `dir` named `name`; a file there already holding these bytes
/// is used as it is. Returns where it is, and whether this wrote it.
pub fn place_plain(path_or_ref: &str, name: &str, dir: &Path) -> Result<(PathBuf, bool), String> {
    let (src, hash) = source(path_or_ref)?;
    let hash = match hash {
        Some(hash) => hash,
        None => digest(&*src)?,
    };
    let hex = crate::util::bytes_to_hex_string(&hash);
    let holds = |path: &Path| vector_core::crypto::stream::hash_file(path).is_ok_and(|(h, _)| h == hex);
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't save the file: {e}"))?;
    let name = file_name(name, &hash, src.mime());
    let candidate = dir.join(&name);
    if holds(&candidate) {
        return Ok((candidate, false));
    }
    let dest = vector_core::crypto::resolve_unique_filename(dir, &name);
    copy_out(&*src, &dest)?;
    // A kept file reads through its keystream, which checks no tag: the copy proves its bytes.
    if !holds(&dest) {
        let _ = std::fs::remove_file(&dest);
        return Err("Couldn't save the file: it didn't read back as sent".into());
    }
    Ok((dest, true))
}

/// A kept file made plain in the public folder: its messages show that copy, then the store lets
/// go of its own. Returns where it is and the events whose rows moved.
pub fn unseal(hash: &[u8; 32], name: &str) -> Result<(PathBuf, Vec<String>), String> {
    let (path, wrote) = place_plain(&MediaRef::held(hash), name, &vector_core::db::get_public_dir())?;
    let spelled = path.to_string_lossy().into_owned();
    if wrote {
        vector_core::db::saved::unmark(&spelled)?;
    }
    let events = vector_core::db::attachments::out_of_store(&crate::util::bytes_to_hex_string(hash), &spelled)?;
    vector_core::db::sync_wal()?;
    vector_core::media::store::forget(hash);
    #[cfg(target_os = "android")]
    crate::android::storage::scan_file(&spelled);
    Ok((path, events))
}

/// Write `path_or_ref` into a file the caller opened, from its first byte.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn save_into(path_or_ref: &str, file: std::fs::File) -> Result<(), String> {
    let (src, _) = source(path_or_ref)?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    write_out(&*src, &mut out).and_then(|()| out.into_inner().map_err(|e| e.into_error())?.sync_all()).map_err(|e| format!("Couldn't save the file: {e}"))
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
    let (src, _) = source(path_or_ref)?;
    copy_out(&*src, dest)
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
