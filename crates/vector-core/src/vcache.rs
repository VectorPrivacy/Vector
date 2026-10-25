//! Vector Cache: the pictures and media Vector keeps for itself (avatars, banners, emoji,
//! icons, previews), sealed on disk under a per-install key so nothing in the app's folders is
//! a plain image to a gallery, an indexer or another app.
//!
//! A file is `VCC1` ‖ key id (4) ‖ nonce (12) ‖ ChaCha20 ciphertext. The stream seeks, so a
//! video plays by range. A file written before sealing has no header and reads as it is.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use zeroize::Zeroizing;

/// Vector's caches under `<app_data>/cache/`. Nothing else there is Vector Cache: on Android the
/// folder is the system's own cache directory, which also holds the webview's files.
pub const KINDS: [&str; 11] = [
    "avatars", "banners", "emojis", "emoji_pack_icons", "emoji_spritesheets", "miniapp_icons",
    "inline_images", "embed_videos", "gif_previews", "svg_renders", "previews",
];

const MAGIC: &[u8; 4] = b"VCC1";
const HEADER: usize = 4 + 4 + 12;
const CHUNK: usize = 256 * 1024;

struct Key {
    bytes: Zeroizing<[u8; 32]>,
    id: [u8; 4],
}

/// The install's key, read or made once. None until the app's folder is known.
fn key() -> Option<&'static Key> {
    static KEY: OnceLock<Key> = OnceLock::new();
    static MAKING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if let Some(key) = KEY.get() {
        return Some(key);
    }
    // One maker at a time: a second reader must never find the file half written.
    let _making = MAKING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = KEY.get() {
        return Some(key);
    }
    let path = key_path().ok()?;
    let bytes = load_or_create(&path).map_err(|e| crate::log_warn!("[VCache] no cache key: {}", e)).ok()?;
    Some(KEY.get_or_init(|| Key { id: key_id(&bytes), bytes }))
}

fn key_path() -> Result<PathBuf, String> {
    Ok(crate::db::get_app_data_dir()?.join("vcache.key"))
}

fn key_id(key: &[u8; 32]) -> [u8; 4] {
    use sha2::Digest;
    let digest = sha2::Sha256::digest([b"vcache key id".as_slice(), key].concat());
    [digest[0], digest[1], digest[2], digest[3]]
}

const WRONG_LENGTH: &str = "cache key has the wrong length";

fn load_or_create(path: &Path) -> Result<Zeroizing<[u8; 32]>, String> {
    let read = |path: &Path| -> Result<Zeroizing<[u8; 32]>, String> {
        let raw = Zeroizing::new(std::fs::read(path).map_err(|e| e.to_string())?);
        let mut key = Zeroizing::new([0u8; 32]);
        if raw.len() != 32 {
            return Err(WRONG_LENGTH.into());
        }
        key.copy_from_slice(&raw);
        Ok(key)
    };
    if path.exists() {
        match read(path) {
            Ok(key) => return Ok(key),
            // A key file that isn't a key is set aside, never left to block every write: what
            // it sealed reads as missing and is fetched again. A read that merely failed is
            // tried again later.
            Err(e) if e == WRONG_LENGTH => {
                crate::log_warn!("[VCache] cache key unusable, starting a new one");
                let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                std::fs::rename(path, path.with_extension(format!("key.unusable-{stamp}"))).map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut key = Zeroizing::new([0u8; 32]);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut *key);
    // Written whole beside it, then linked into place: a key file is complete or absent, and
    // another process that made one first keeps it.
    let tmp = temp_beside(path);
    create_private(&tmp)
        .and_then(|mut f| f.write_all(&*key).and_then(|_| f.sync_all()).map_err(|e| e.to_string()))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    let linked = std::fs::hard_link(&tmp, path);
    let result = match linked {
        Ok(()) => Ok(key),
        Err(_) if path.exists() => read(path),
        // A filesystem without links: the rename still never shows a partial key.
        Err(_) => std::fs::rename(&tmp, path).map(|_| key).map_err(|e| e.to_string()),
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

fn cipher(key: &Key, nonce: &[u8; 12]) -> chacha20::ChaCha20 {
    chacha20::ChaCha20::new((&*key.bytes).into(), nonce.into())
}

fn fresh_header(key: &Key) -> ([u8; HEADER], [u8; 12]) {
    let mut nonce = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let mut header = [0u8; HEADER];
    header[..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&key.id);
    header[8..].copy_from_slice(&nonce);
    (header, nonce)
}

/// What a file's first bytes say about it.
enum Kind {
    /// Ours, under this nonce.
    Sealed([u8; 12]),
    /// Ours, under a key this install no longer has: unreadable, so absent.
    Foreign,
    /// Written before sealing.
    Plain,
}

fn kind(head: &[u8], key: &Key) -> Kind {
    if head.len() < HEADER || &head[..4] != MAGIC {
        return Kind::Plain;
    }
    if head[4..8] != key.id {
        return Kind::Foreign;
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&head[8..HEADER]);
    Kind::Sealed(nonce)
}

/// Sealed `plain`, ready to write.
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    let key = key().ok_or("Vector's cache key isn't available")?;
    let (header, nonce) = fresh_header(key);
    let mut out = Vec::with_capacity(HEADER + plain.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(plain);
    cipher(key, &nonce).apply_keystream(&mut out[HEADER..]);
    Ok(out)
}

/// Write `plain` to `path` sealed: beside it first, then into place.
pub fn write(path: &Path, plain: &[u8]) -> Result<(), String> {
    let sealed = seal(plain)?;
    let tmp = temp_beside(path);
    write_private(&tmp, &sealed).and_then(|_| std::fs::rename(&tmp, path).map_err(|e| e.to_string())).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Write what `src` holds to `path` sealed, a chunk at a time: a large file never sits whole in
/// memory, or plain on disk.
pub fn write_source(src: &dyn crate::media::MediaSource, path: &Path) -> Result<(), String> {
    let key = key().ok_or("Vector's cache key isn't available")?;
    let (header, nonce) = fresh_header(key);
    let mut cipher = cipher(key, &nonce);
    let tmp = temp_beside(path);
    let result = (|| -> Result<(), String> {
        let mut output = std::io::BufWriter::with_capacity(CHUNK, create_private(&tmp)?);
        output.write_all(&header).map_err(|e| e.to_string())?;
        let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
        let mut at = 0u64;
        while at < src.len() {
            let n = src.read_at(at, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("the source ended early".into());
            }
            cipher.apply_keystream(&mut buf[..n]);
            output.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            at += n as u64;
        }
        output.flush().map_err(|e| e.to_string())?;
        output.get_ref().sync_data().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `src` sealed into a temp beside `path`, synced, then renamed over it: `path` is never partial,
/// and `src` may be `path` itself, in which case a write that replaced it meanwhile wins.
fn seal_stream(src: &Path, path: &Path) -> Result<(), String> {
    let key = key().ok_or("Vector's cache key isn't available")?;
    let (header, nonce) = fresh_header(key);
    let mut cipher = cipher(key, &nonce);
    let tmp = temp_beside(path);
    let result = (|| -> Result<(), String> {
        let mut input = std::fs::File::open(src).map_err(|e| e.to_string())?;
        let mut output = std::io::BufWriter::with_capacity(CHUNK, create_private(&tmp)?);
        output.write_all(&header).map_err(|e| e.to_string())?;
        let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
        loop {
            let n = input.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            cipher.apply_keystream(&mut buf[..n]);
            output.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        output.flush().map_err(|e| e.to_string())?;
        output.get_ref().sync_data().map_err(|e| e.to_string())?;
        if src == path && is_sealed(path) {
            return Err("sealed meanwhile".into());
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Where a sealed file's ciphertext starts.
pub const HEADER_LEN: u64 = HEADER as u64;

/// A sealed file written a piece at a time, at any offset: a download's part file, which a
/// resume continues. Once whole it is a cache file as it stands.
pub struct Sealer {
    key: Zeroizing<[u8; 32]>,
    nonce: [u8; 12],
}

impl Sealer {
    /// The part file at `path`, continued under its own header, or begun afresh (emptied, with a
    /// new header) when it has none this install can continue.
    pub fn for_part(path: &Path) -> Result<Sealer, String> {
        let key = key().ok_or("Vector's cache key isn't available")?;
        if let Ok(mut file) = std::fs::File::open(path) {
            let mut head = [0u8; HEADER];
            if read_up_to(&mut file, &mut head) == HEADER {
                if let Kind::Sealed(nonce) = kind(&head, key) {
                    return Ok(Sealer { key: Zeroizing::new(*key.bytes), nonce });
                }
            }
        }
        let (header, nonce) = fresh_header(key);
        let mut file = create_private(path)?;
        file.write_all(&header).map_err(|e| e.to_string())?;
        Ok(Sealer { key: Zeroizing::new(*key.bytes), nonce })
    }

    /// Seal `buf`, the plaintext that sits `offset` bytes into the file's content.
    pub fn seal_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let mut cipher = chacha20::ChaCha20::new((&*self.key).into(), (&self.nonce).into());
        cipher.try_seek(offset).map_err(|_| "past the end of the cipher".to_string())?;
        cipher.apply_keystream(buf);
        Ok(())
    }
}

/// The plaintext of a file Vector may have sealed: a cache file, sealed or from before sealing,
/// and any other file as it is. A file under a lost key reads as missing, so its caller fetches
/// it again and the write replaces it.
pub fn read(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut bytes = Zeroizing::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let Some(key) = key() else {
        return if bytes.starts_with(MAGIC) { Err("Vector's cache key isn't available".into()) } else { Ok(bytes) };
    };
    match kind(&bytes, key) {
        Kind::Plain => Ok(bytes),
        Kind::Foreign => Err("not found".into()),
        Kind::Sealed(nonce) => {
            cipher(key, &nonce).apply_keystream(&mut bytes[HEADER..]);
            bytes.drain(..HEADER);
            Ok(bytes)
        }
    }
}

/// Whether `path` is a sealed cache file: another app reading it would see only ciphertext.
pub fn is_sealed(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 4];
    read_up_to(&mut file, &mut head) == 4 && &head == MAGIC
}

/// Whether `path` holds a cache file this install can read, without reading it.
pub fn readable(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; HEADER];
    let n = read_up_to(&mut file, &mut head);
    // An empty file is a write a crash cut short.
    if n == 0 {
        return false;
    }
    match key() {
        Some(key) => !matches!(kind(&head[..n], key), Kind::Foreign),
        None => !head[..n].starts_with(MAGIC),
    }
}

/// Seal a file written before sealing, in place. False when it was sealed already.
pub fn seal_in_place(path: &Path) -> Result<bool, String> {
    let mut head = [0u8; HEADER];
    let n = read_up_to(&mut std::fs::File::open(path).map_err(|e| e.to_string())?, &mut head);
    if head[..n].starts_with(MAGIC) {
        return Ok(false);
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    seal_stream(path, path)?;
    // Age-based sweeps read the mtime: sealing isn't a use.
    if let (Some(t), Ok(f)) = (modified, std::fs::OpenOptions::new().write(true).open(path)) {
        let _ = f.set_modified(t);
    }
    Ok(true)
}

/// Seal every legacy picture and video under `dir`, recursively; sidecars and markers stay as
/// they are. Returns how many were sealed.
pub fn seal_dir(dir: &Path) -> usize {
    const MEDIA: [&str; 13] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "tif", "tiff", "mp4", "webm", "mov"];
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut sealed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        let media = path.extension().and_then(|e| e.to_str()).is_some_and(|e| MEDIA.contains(&e.to_ascii_lowercase().as_str()));
        if kind.is_dir() {
            sealed += seal_dir(&path);
        } else if path.extension().is_some_and(|e| e == "vtmp") {
            // A write cut short: ciphertext with no file to become. A fresh one may be in flight.
            let age = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if age.is_some_and(|age| age > std::time::Duration::from_secs(600)) {
                let _ = std::fs::remove_file(&path);
            }
        } else if kind.is_file() && media && seal_in_place(&path).unwrap_or(false) {
            sealed += 1;
        }
    }
    sealed
}

/// Seal everything Vector cached before sealing: the shared cache and every account's
/// wallpapers. Idempotent; a sealed file costs one header read.
pub fn seal_legacy() -> usize {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Ok(root) = crate::db::get_app_data_dir() else { return 0 };
    // Files from before sealing don't reappear: once a launch is enough.
    if DONE.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return 0;
    }
    let mut sealed: usize = KINDS.iter().map(|kind| seal_dir(&root.join("cache").join(kind))).sum();
    if let Ok(entries) = std::fs::read_dir(root) {
        for account in entries.flatten() {
            let real_dir = account.file_type().is_ok_and(|t| t.is_dir());
            if real_dir && account.file_name().to_string_lossy().starts_with("npub1") {
                sealed += seal_dir(&account.path().join("wallpapers"));
            }
        }
    }
    sealed
}

/// The cache file a page asks for by its path under the app's folder: `cache/…` or an
/// account's `wallpapers/…`. Anything else, or any path that climbs out, is refused.
pub fn resolve(rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').collect();
    let clean = parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(['\\', ':', '\0']));
    let allowed = match parts.as_slice() {
        ["cache", kind, _, ..] => KINDS.contains(kind),
        [account, "wallpapers", _, ..] => account.starts_with("npub1"),
        _ => false,
    };
    if !clean || !allowed {
        return None;
    }
    let root = crate::db::get_app_data_dir().ok()?;
    let path = parts.iter().fold(root.clone(), |p, part| p.join(part));
    // A link inside the folder could still point out of it.
    let (real, real_root) = (std::fs::canonicalize(&path).ok()?, std::fs::canonicalize(root).ok()?);
    real.starts_with(&real_root).then_some(real)
}

/// A request path's segment text, percent-decoded.
pub fn decode_rel(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A cache file as a page may show it. A cached SVG is a render Vector drew and wrapped, so the
/// page gets the picture inside, never SVG markup; anything else opens as it is.
pub fn open_for_page(path: &Path) -> Option<Arc<dyn crate::media::MediaSource>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "svg" {
        let png = crate::svg::unwrap_raster(&read(path).ok()?)?;
        return Some(Arc::new(InMemory { bytes: png, mime: "image/png" }));
    }
    open(path, crate::crypto::mime_from_extension(&ext))
}

struct InMemory {
    bytes: Vec<u8>,
    mime: &'static str,
}

impl crate::media::MediaSource for InMemory {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn mime(&self) -> &str {
        self.mime
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(self.bytes.len());
        let n = buf.len().min(self.bytes.len() - start);
        buf[..n].copy_from_slice(&self.bytes[start..start + n]);
        Ok(n)
    }
}

/// A cache file read by range: what a player or a large picture asks for.
pub fn open(path: &Path, mime: &str) -> Option<Arc<dyn crate::media::MediaSource>> {
    let mut file = std::fs::File::open(path).ok()?;
    let total = file.metadata().ok()?.len();
    let mut head = [0u8; HEADER];
    let n = read_up_to(&mut file, &mut head);
    let sealed = match key() {
        Some(key) => match kind(&head[..n], key) {
            Kind::Sealed(nonce) => Some((key, nonce)),
            Kind::Foreign => return None,
            Kind::Plain => None,
        },
        None if head[..n].starts_with(MAGIC) => return None,
        None => None,
    };
    Some(Arc::new(match sealed {
        Some((key, nonce)) => CacheFile {
            file,
            offset: HEADER as u64,
            len: total.saturating_sub(HEADER as u64),
            key: Some((Zeroizing::new(*key.bytes), nonce)),
            mime: mime.to_string(),
        },
        None => CacheFile { file, offset: 0, len: total, key: None, mime: mime.to_string() },
    }))
}

struct CacheFile {
    file: std::fs::File,
    offset: u64,
    len: u64,
    key: Option<(Zeroizing<[u8; 32]>, [u8; 12])>,
    mime: String,
}

impl crate::media::MediaSource for CacheFile {
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
        crate::media::store::read_exact_at(&self.file, part, self.offset + offset)?;
        if let Some((key, nonce)) = &self.key {
            let mut cipher = chacha20::ChaCha20::new((&**key).into(), nonce.into());
            cipher.try_seek(offset).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "past the end of the cipher"))?;
            cipher.apply_keystream(part);
        }
        Ok(n)
    }
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    filled
}

fn temp_beside(path: &Path) -> PathBuf {
    let mut id = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    path.with_file_name(format!(".{name}.{}.vtmp", crate::simd::hex::bytes_to_hex_string(&id)))
}

fn create_private(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).map_err(|e| e.to_string())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    create_private(path)?.write_all(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        guard
    }

    #[test]
    fn a_sealed_file_reads_back_and_holds_no_plaintext() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("face.png");
        let plain: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        write(&path, &plain).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(raw.starts_with(MAGIC) && raw.len() == plain.len() + HEADER);
        assert!(!raw.windows(64).any(|w| w == &plain[1000..1064]));
        assert_eq!(&*read(&path).unwrap(), &plain);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_range_reads_any_window() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        let plain: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        write(&path, &plain).unwrap();
        let src = open(&path, "video/mp4").unwrap();
        assert_eq!(src.len(), plain.len() as u64);
        for (start, len) in [(0usize, 10usize), (63, 130), (131_071, 5000), (299_990, 50)] {
            let mut buf = vec![0u8; len];
            let n = src.read_at(start as u64, &mut buf).unwrap();
            assert_eq!(&buf[..n], &plain[start..(start + len).min(plain.len())]);
        }
    }

    #[test]
    fn a_legacy_file_reads_as_it_is_until_sealed_in_place() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.jpg");
        std::fs::write(&path, b"\xFF\xD8\xFF legacy jpeg").unwrap();
        assert_eq!(&*read(&path).unwrap(), b"\xFF\xD8\xFF legacy jpeg");
        assert_eq!(crate::media::read_all(&*open(&path, "image/jpeg").unwrap()).unwrap(), b"\xFF\xD8\xFF legacy jpeg");
        assert!(seal_in_place(&path).unwrap());
        assert!(!seal_in_place(&path).unwrap());
        assert!(std::fs::read(&path).unwrap().starts_with(MAGIC));
        assert_eq!(&*read(&path).unwrap(), b"\xFF\xD8\xFF legacy jpeg");
    }

    #[test]
    fn a_file_under_another_key_is_treated_as_missing() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stranger.png");
        let mut raw = seal(b"under this key").unwrap();
        raw[4] ^= 0xff;
        std::fs::write(&path, &raw).unwrap();
        assert!(!readable(&path));
        assert!(open(&path, "image/png").is_none());
        assert!(read(&path).is_err());
        assert!(path.exists(), "a read never deletes");
    }

    #[test]
    fn a_cached_svg_reaches_a_page_as_the_picture_inside() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="red"/></svg>"#;
        let wrapped = crate::svg::rasterize_wrapped(svg, 64).unwrap();
        let path = dir.path().join("face.svg");
        write(&path, &wrapped).unwrap();
        let src = open_for_page(&path).unwrap();
        assert_eq!(src.mime(), "image/png");
        assert!(crate::media::read_all(&*src).unwrap().starts_with(b"\x89PNG"));
        // Markup Vector didn't draw never reaches a page.
        let raw = dir.path().join("raw.svg");
        write(&raw, svg).unwrap();
        assert!(open_for_page(&raw).is_none());
        let empty = dir.path().join("empty.svg");
        write(&empty, &crate::svg::empty_wrapped()).unwrap();
        assert!(open_for_page(&empty).is_none());
    }

    #[test]
    fn a_part_written_in_pieces_and_resumed_reads_whole() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("v.part");
        let plain: Vec<u8> = (0..200_000u32).map(|i| (i % 239) as u8).collect();
        let first = Sealer::for_part(&part).unwrap();
        let mut a = plain[..70_001].to_vec();
        first.seal_at(0, &mut a).unwrap();
        std::fs::OpenOptions::new().append(true).open(&part).unwrap().write_all(&a).unwrap();
        // A resume reads the header back and continues under it.
        let again = Sealer::for_part(&part).unwrap();
        let mut b = plain[70_001..].to_vec();
        again.seal_at(70_001, &mut b).unwrap();
        std::fs::OpenOptions::new().append(true).open(&part).unwrap().write_all(&b).unwrap();
        assert_eq!(&*read(&part).unwrap(), &plain);
        assert!(!std::fs::read(&part).unwrap().windows(32).any(|w| w == &plain[100..132]));

        // A part from before sealing starts over.
        let old = dir.path().join("old.part");
        std::fs::write(&old, b"plaintext from an older download").unwrap();
        Sealer::for_part(&old).unwrap();
        assert_eq!(std::fs::metadata(&old).unwrap().len(), HEADER_LEN);
    }

    #[test]
    fn a_streamed_seal_matches_and_a_directory_seals_once() {
        let _g = ready();
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("v.part");
        let plain: Vec<u8> = (0..(CHUNK * 2 + 77) as u32).map(|i| (i % 241) as u8).collect();
        std::fs::write(&part, &plain).unwrap();
        let dest = dir.path().join("v.mp4");
        seal_stream(&part, &dest).unwrap();
        assert_eq!(&*read(&dest).unwrap(), &plain);

        let nested = dir.path().join("thumbs");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("a.png"), b"\x89PNG plain").unwrap();
        std::fs::write(dir.path().join("b.gif"), b"GIF89a plain").unwrap();
        std::fs::write(dir.path().join("sheet.json"), b"{}").unwrap();
        assert_eq!(seal_dir(dir.path()), 2);
        assert_eq!(std::fs::read(dir.path().join("sheet.json")).unwrap(), b"{}", "sidecars stay readable");
        assert_eq!(seal_dir(dir.path()), 0);
        assert_eq!(&*read(&nested.join("a.png")).unwrap(), b"\x89PNG plain");
    }
}
