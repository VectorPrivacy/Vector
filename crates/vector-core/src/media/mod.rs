//! Media Vector shows without writing it down: decrypted bytes held in memory per account
//! (`ram`), read through one interface whatever holds them.

#[cfg(not(target_arch = "wasm32"))]
pub mod import;
pub mod pending;
pub mod ram;
pub mod range;
pub mod store;

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// Bytes a media view reads, wherever they live.
pub trait MediaSource: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn mime(&self) -> &str;
    /// Fill `buf` from `offset`. Fewer bytes only at the end, none past it.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;
}

/// "Keep Files on This Device": every file received is kept in the store too, not only held.
pub fn keep_media() -> bool {
    crate::db::get_sql_setting("keep_media".to_string()).ok().flatten().as_deref() == Some("true")
}

/// "Keep Unencrypted": what is kept is a plain file in the public folder, not sealed in the store.
pub fn keep_unencrypted() -> bool {
    keep_media() && crate::db::get_sql_setting("keep_unencrypted".to_string()).ok().flatten().as_deref() == Some("true")
}

/// The memory key of media fetched by URL rather than by content hash: `kind` keeps one kind's
/// keys apart from another's. The digest is masked so that no file's content hash can be it: a
/// file of exactly the derivation's bytes would otherwise claim the link's slot.
pub fn url_key(kind: &str, url: &str) -> [u8; 32] {
    let mut key = crate::crypto::sha256::digest(format!("vector-url-media\n{kind}\n{url}").as_bytes());
    key[0] ^= 0x80;
    key
}

/// Kinds a chat bubble draws itself: these load for viewing without being kept.
pub fn previewable(extension: &str) -> bool {
    const IMAGES: [&str; 10] = ["png", "jpeg", "jpg", "gif", "webp", "svg", "bmp", "tiff", "tif", "ico"];
    const AUDIO: [&str; 6] = ["wav", "mp3", "flac", "aac", "m4a", "ogg"];
    const VIDEO: [&str; 3] = ["mp4", "webm", "mov"];
    const MODELS: [&str; 5] = ["glb", "gltf", "stl", "obj", "fbx"];
    // Linux's webview plays no inline video.
    let video = !cfg!(all(target_os = "linux", not(target_arch = "wasm32"))) && VIDEO.contains(&extension);
    IMAGES.contains(&extension) || AUDIO.contains(&extension) || MODELS.contains(&extension) || video
}

/// What holds `hash` for this account right now, memory first, if anything.
pub fn open(hash: &[u8; 32]) -> Option<Arc<dyn MediaSource>> {
    match ram::cache().get(hash) {
        Some(entry) => Some(entry as Arc<dyn MediaSource>),
        None => store::open(hash).or_else(|| pending::open(hash)).or_else(|| kept_plain(hash)),
    }
}

/// A file kept unencrypted in one of Vector's folders, for a hash memory has let go of.
#[cfg(not(target_arch = "wasm32"))]
fn kept_plain(hash: &[u8; 32]) -> Option<Arc<dyn MediaSource>> {
    let roots: Vec<std::path::PathBuf> = crate::db::download_roots().iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
    let paths = crate::db::attachments::downloaded_paths_by_hash(&crate::simd::hex::bytes_to_hex_32(hash)).ok()?;
    paths.iter().find_map(|path| {
        let real = std::fs::canonicalize(path).ok().filter(|real| roots.iter().any(|root| real.starts_with(root)))?;
        let ext = real.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        crate::vcache::open(&real, crate::crypto::mime_from_extension(&ext))
    })
}

#[cfg(target_arch = "wasm32")]
fn kept_plain(_: &[u8; 32]) -> Option<Arc<dyn MediaSource>> {
    None
}

/// The plaintext and type of media this account holds, from memory or the store. A kept file
/// read on the web is held in memory after, since its store can't be read a range at a time.
pub async fn held_bytes(hash: &[u8; 32]) -> Option<(Vec<u8>, String)> {
    if let Some(src) = open(hash) {
        return read_all(&*src).ok().map(|bytes| (bytes, src.mime().to_string()));
    }
    #[cfg(target_arch = "wasm32")]
    if let Some((plain, mime)) = store::read_plain(hash).await {
        let _ = ram::cache().insert(*hash, plain.clone(), &mime);
        return Some((plain, mime));
    }
    None
}

/// The bytes behind a path or a `vmedia:` ref, for readers that want the whole file.
pub async fn read(path_or_ref: &str) -> Result<Vec<u8>, String> {
    match MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        MediaRef::File(path) => crate::files::read(std::path::Path::new(&path)).await,
        MediaRef::Held(hash) => held_bytes(&hash).await.map(|(bytes, _)| bytes).ok_or_else(|| "This file isn't on this device any more".to_string()),
    }
}

/// Every byte of `src`, for a reader that wants the whole file.
pub fn read_all(src: &dyn MediaSource) -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; usize::try_from(src.len()).map_err(|_| std::io::ErrorKind::OutOfMemory)?];
    let mut filled = 0;
    while filled < bytes.len() {
        match src.read_at(filled as u64, &mut bytes[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

/// Whether this account can show `hash` without fetching it.
pub fn resident(hash: &[u8; 32]) -> bool {
    ram::cache().contains(hash) || store::holds(hash) || pending::holds(hash)
}

/// Whether [`open`] would find `hash`: held, or a plain copy in a download folder.
pub fn openable(hash: &[u8; 32]) -> bool {
    resident(hash) || open(hash).is_some()
}

/// How a command names an attachment's bytes: a hash this account holds, or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRef {
    Held([u8; 32]),
    File(String),
}

impl MediaRef {
    pub const SCHEME: &'static str = "vmedia:";

    /// None for a `vmedia:` ref that names no hash.
    pub fn parse(s: &str) -> Option<Self> {
        match s.strip_prefix(Self::SCHEME) {
            Some(hex) => crate::simd::hex::hex_to_bytes_32_checked(hex).map(MediaRef::Held),
            None => Some(MediaRef::File(s.to_string())),
        }
    }

    pub fn held(hash: &[u8; 32]) -> String {
        format!("{}{}", Self::SCHEME, crate::simd::hex::bytes_to_hex_32(hash))
    }
}

/// A cursor over a source, for readers that want `Read + Seek`.
pub struct Reader {
    src: Arc<dyn MediaSource>,
    pos: u64,
}

pub fn reader(src: Arc<dyn MediaSource>) -> Reader {
    Reader { src, pos: 0 }
}

impl Reader {
    pub fn len(&self) -> u64 {
        self.src.len()
    }

    pub fn is_empty(&self) -> bool {
        self.src.is_empty()
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.src.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(d) => i128::from(self.src.len()) + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        if next < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = u64::try_from(next).unwrap_or(u64::MAX);
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_can_claim_a_links_memory_slot() {
        let url = "https://example.com/a.png";
        let derivation = format!("vector-url-media\ninline-image\n{url}");
        assert_ne!(url_key("inline-image", url), crate::crypto::sha256::digest(derivation.as_bytes()), "a file of the derivation's bytes hashes elsewhere");
        assert_ne!(url_key("inline-image", url), url_key("embed-poster", url));
    }

    #[test]
    fn a_ref_is_a_held_hash_or_a_file() {
        let hash = [0xabu8; 32];
        let held = MediaRef::held(&hash);
        assert_eq!(MediaRef::parse(&held), Some(MediaRef::Held(hash)));
        assert_eq!(MediaRef::parse("/dl/photo.jpg"), Some(MediaRef::File("/dl/photo.jpg".into())));
        assert_eq!(MediaRef::parse("vmedia:nothex"), None);
        assert_eq!(MediaRef::parse("vmedia:"), None);
    }

    #[test]
    fn the_reader_reads_and_seeks_like_a_file() {
        let ram = ram::MediaRam::default();
        let data: Vec<u8> = (0..=255u8).collect();
        let mut r = reader(ram.insert([9; 32], data.clone(), "audio/wav").unwrap());
        let mut head = [0u8; 4];
        r.read_exact(&mut head).unwrap();
        assert_eq!(head, [0, 1, 2, 3]);
        assert_eq!(r.seek(SeekFrom::End(-2)).unwrap(), 254);
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, vec![254, 255]);
        assert_eq!(r.seek(SeekFrom::Current(-100)).unwrap(), 156);
        assert!(r.seek(SeekFrom::Current(-1000)).is_err());
        assert_eq!(r.seek(SeekFrom::Start(9999)).unwrap(), 9999);
        assert_eq!(r.read(&mut head).unwrap(), 0, "past the end reads nothing");
    }
}
