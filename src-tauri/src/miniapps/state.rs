//! State management for Mini App instances

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use super::realtime::TopicId;

use super::error::Error;
use super::realtime::RealtimeManager;
use crate::util::bytes_to_hex_string;

/// Metadata from manifest.toml in the Mini App package
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MiniAppManifest {
    /// Display name of the Mini App
    pub name: String,
    /// Optional description
    #[serde(default)]
    pub description: String,
    /// Optional icon path within the package
    #[serde(default)]
    pub icon: String,
    /// Optional version string
    #[serde(default)]
    pub version: String,
    /// Optional source code URL (e.g., GitHub repository)
    #[serde(default)]
    pub source_code_url: Option<String>,
    /// Opt-in to cross-origin isolation (SharedArrayBuffer, threaded WebAssembly).
    /// Desktop then serves the app over loopback HTTP in its own data store.
    #[serde(default)]
    pub cross_origin_isolated: bool,
}

/// A Mini App package (a .xdc ZIP archive) as the app keeps it: what a listing or an open needs.
#[derive(Debug)]
pub struct MiniAppPackage {
    pub path: Box<std::path::Path>,
    pub name: Box<str>,
    pub description: Box<str>,
    pub version: Box<str>,
    pub source_code_url: Option<Box<str>>,
    /// SHA-256 of the .xdc file: its identity for permissions and storage.
    pub file_hash: [u8; 32],
    /// Opted in to cross-origin isolation (see `MiniAppManifest`).
    pub cross_origin_isolated: bool,
    /// Whether the app calls the realtime (Iroh) API.
    pub uses_realtime: bool,
    pub has_icon: bool,
    /// The size and modification time it was read at: a file replaced in place is read again.
    stamp: (u64, i64),
}

/// What one read of a package finds, stored by its content hash.
#[derive(Serialize, Deserialize)]
struct PackageMeta {
    #[serde(default)]
    v: u32,
    /// None when the package ships no manifest.toml: the name then comes from its file name.
    manifest: Option<MiniAppManifest>,
    uses_realtime: bool,
    #[serde(default)]
    has_icon: bool,
}

/// Bumped when a read would find more than the stored metadata holds, so it's read again.
const PACKAGE_META_VERSION: u32 = 2;
const ICON_CAP: u64 = 8 * 1024 * 1024;
/// Longest side of a cached listing icon: the largest tile, at up to 3x density.
const LISTING_ICON_PX: u32 = 256;

/// Where a package's listing icon is cached, by content hash.
pub fn icon_file(icon_dir: &std::path::Path, file_hash: &[u8; 32]) -> PathBuf {
    icon_dir.join(format!("xdc-{}.png", bytes_to_hex_string(file_hash)))
}

/// An icon as a listing shows it: at most `LISTING_ICON_PX` a side, re-encoded as PNG, an SVG
/// drawn to pixels.
fn listing_icon(bytes: &[u8]) -> Option<Vec<u8>> {
    if vector_core::svg::looks_like_svg(bytes) {
        return vector_core::svg::rasterize_png(bytes, LISTING_ICON_PX).ok();
    }
    let img = crate::shared::image::decode_image(bytes, LISTING_ICON_PX).ok()?;
    let img = if img.width().max(img.height()) > LISTING_ICON_PX {
        crate::shared::image::resize_fit(&img, LISTING_ICON_PX, LISTING_ICON_PX, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    crate::shared::image::encode_png(rgba.as_raw(), rgba.width(), rgba.height()).ok()
}

/// Write a cached icon whole or not at all; a concurrent writer of the same hash counts.
fn write_icon(file: &std::path::Path, png: &[u8]) -> bool {
    vector_core::vcache::write(file, png).is_ok() || file.is_file()
}

/// Modification time in nanoseconds since the epoch, 0 when the platform can't say.
fn modified_nanos(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// SHA-256 of a package's content, streamed: a package can be hundreds of MB.
pub(crate) fn hash_file(path: &std::path::Path) -> Result<[u8; 32], Error> {
    use sha2::{Digest, Sha256};
    let mut file = SharedFile::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Whether `needle` occurs in the first `cap` bytes of `reader`, read in chunks.
fn stream_contains(reader: impl Read, needle: &[u8], cap: u64) -> bool {
    let finder = memchr::memmem::Finder::new(needle);
    let mut reader = reader.take(cap);
    let mut buf = vec![0u8; 64 * 1024 + needle.len()];
    let mut kept = 0;
    loop {
        let n = match reader.read(&mut buf[kept..]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        let filled = kept + n;
        if finder.find(&buf[..filled]).is_some() {
            return true;
        }
        // A match can straddle two reads.
        kept = (needle.len() - 1).min(filled);
        buf.copy_within(filled - kept..filled, 0);
    }
}

/// An entry path with empty segments dropped (e.g. "maps//ui.map"), or None when it climbs out.
fn normalize_entry_path(path: &str) -> Option<String> {
    let normalized = path.split('/').filter(|seg| !seg.is_empty()).collect::<Vec<_>>().join("/");
    (!normalized.contains("..")).then_some(normalized)
}

/// Resolve an archive entry name case-insensitively (lenience for authors who mis-case files, e.g. an
/// `Index.html` entry for an `index.html` request — the WebXDC spec wants lowercase, but we'd rather a
/// mis-cased app just work than fail). Exact match wins; otherwise an ASCII case-insensitive scan.
/// Returns the ACTUAL stored name to pass to `by_name`.
fn resolve_entry_ci<R: std::io::Read + std::io::Seek>(
    archive: &zip::ZipArchive<R>,
    target: &str,
) -> Option<String> {
    if archive.file_names().any(|n| n == target) {
        return Some(target.to_string());
    }
    archive
        .file_names()
        .find(|n| n.eq_ignore_ascii_case(target))
        .map(|s| s.to_string())
}

impl MiniAppPackage {
    /// Load a Mini App package from a .xdc file. A known file costs a stat and a lookup; a new
    /// one is hashed and read once, both streamed from disk, and what the read found is stored
    /// by hash for every later load. Its listing icon is cached into `icon_dir`.
    pub fn load(path: PathBuf, icon_dir: Option<&std::path::Path>) -> Result<Self, Error> {
        let stamp = Self::stamp_of(&path)?;
        let path_key = path.to_string_lossy().into_owned();

        let known = vector_core::db::miniapps::miniapp_file_hash(&path_key, stamp.0, stamp.1).ok().flatten();
        if let Some((file_hash, stored)) = known.and_then(|h| Self::stored_meta(&h).map(|m| (h, m))) {
            return Ok(Self::from_meta(path, file_hash, stored, stamp));
        }

        let file_hash = hash_file(&path)?;
        let package_meta = match Self::stored_meta(&file_hash) {
            Some(stored) => stored,
            None => {
                let read = Self::read_package(&path, icon_dir.map(|d| icon_file(d, &file_hash)).as_deref())?;
                if let Ok(data) = serde_json::to_string(&read) {
                    let _ = vector_core::db::miniapps::set_miniapp_meta(&file_hash, &data);
                }
                read
            }
        };
        let _ = vector_core::db::miniapps::set_miniapp_file_hash(&path_key, stamp.0, stamp.1, &file_hash);
        Ok(Self::from_meta(path, file_hash, package_meta, stamp))
    }

    /// Size and modification time, refusing what no package can be.
    fn stamp_of(path: &std::path::Path) -> Result<(u64, i64), Error> {
        // Pre-check: reject 0-byte or missing files without opening them
        // (macOS can hang on open() for corrupted files with 0 logical bytes
        // but non-zero physical blocks)
        let meta = std::fs::metadata(path)?;
        let file_len = meta.len();
        if file_len == 0 {
            return Err(Error::InvalidPackage(format!(
                "File is 0 bytes (corrupted?): {}", path.display()
            )));
        }
        // Sanity: reject files > 500 MB (no legitimate .xdc should be this large)
        if file_len > 500 * 1024 * 1024 {
            return Err(Error::InvalidPackage(format!(
                "File too large ({} MB): {}", file_len / (1024 * 1024), path.display()
            )));
        }
        Ok((file_len, modified_nanos(&meta)))
    }

    /// Whether the file is still the one this was read from.
    pub fn is_current(&self) -> bool {
        Self::stamp_of(&self.path).ok() == Some(self.stamp)
    }

    pub fn hash_hex(&self) -> String {
        bytes_to_hex_string(&self.file_hash)
    }

    fn stored_meta(file_hash: &[u8; 32]) -> Option<PackageMeta> {
        let data = vector_core::db::miniapps::miniapp_meta(file_hash).ok().flatten()?;
        serde_json::from_str::<PackageMeta>(&data).ok().filter(|m| m.v == PACKAGE_META_VERSION)
    }

    fn from_meta(path: PathBuf, file_hash: [u8; 32], meta: PackageMeta, stamp: (u64, i64)) -> Self {
        let manifest = meta.manifest.unwrap_or_else(|| MiniAppManifest {
            name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Mini App").to_string(),
            ..Default::default()
        });
        Self {
            path: path.into_boxed_path(),
            name: manifest.name.into_boxed_str(),
            description: manifest.description.into_boxed_str(),
            version: manifest.version.into_boxed_str(),
            source_code_url: manifest.source_code_url.map(String::into_boxed_str),
            file_hash,
            cross_origin_isolated: manifest.cross_origin_isolated,
            uses_realtime: meta.uses_realtime,
            has_icon: meta.has_icon,
            stamp,
        }
    }

    /// Everything a listing or an open needs from the archive, read from disk without loading
    /// it whole. The listing icon goes to `icon_out` rather than into the record.
    fn read_package(path: &std::path::Path, icon_out: Option<&std::path::Path>) -> Result<PackageMeta, Error> {
        let file = std::io::BufReader::new(SharedFile::open(path)?);
        let mut archive = zip::ZipArchive::new(file)?;
        let manifest = match resolve_entry_ci(&archive, "manifest.toml")
            .and_then(|n| archive.by_name(&n).ok())
        {
            Some(mut file) => {
                let bytes = Self::read_entry_capped(&mut file, 1024 * 1024)?;
                let contents = String::from_utf8(bytes)
                    .map_err(|e| Error::ManifestParse(e.to_string()))?;
                Some(toml::from_str::<MiniAppManifest>(&contents)
                    .map_err(|e| Error::ManifestParse(e.to_string()))?)
            }
            None => None,
        };
        // Entry point required, but accept a mis-cased "Index.html" (served via the same CI lookup).
        if resolve_entry_ci(&archive, "index.html").is_none() {
            return Err(Error::InvalidPackage("Missing index.html".to_string()));
        }
        let icon = Self::icon_from(&mut archive, manifest.as_ref().map_or("", |m| m.icon.as_str()))
            .and_then(|bytes| listing_icon(&bytes));
        if let (Some(png), Some(out)) = (&icon, icon_out) {
            write_icon(out, png);
        }
        let uses_realtime = Self::archive_uses_realtime(&mut archive);
        Ok(PackageMeta { v: PACKAGE_META_VERSION, manifest, uses_realtime, has_icon: icon.is_some() })
    }

    /// The cached listing icon, written again from the archive when the image cache was cleared.
    pub fn icon_path(&self, icon_dir: &std::path::Path) -> Option<PathBuf> {
        if !self.has_icon {
            return None;
        }
        let file = icon_file(icon_dir, &self.file_hash);
        if vector_core::vcache::readable(&file) {
            return Some(file);
        }
        let manifest_icon = Self::stored_meta(&self.file_hash)
            .and_then(|m| m.manifest)
            .map(|m| m.icon)
            .unwrap_or_default();
        let archive = std::io::BufReader::new(SharedFile::open(&self.path).ok()?);
        let bytes = Self::icon_from(&mut zip::ZipArchive::new(archive).ok()?, &manifest_icon)?;
        write_icon(&file, &listing_icon(&bytes)?).then_some(file)
    }

    /// The manifest's icon, else the first of the common icon names.
    fn icon_from<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>, manifest_icon: &str) -> Option<Vec<u8>> {
        let mut read = |name: &str| {
            let name = resolve_entry_ci(archive, &normalize_entry_path(name)?)?;
            let mut file = archive.by_name(&name).ok()?;
            Self::read_entry_capped(&mut file, ICON_CAP).ok()
        };
        if !manifest_icon.is_empty() {
            return read(manifest_icon);
        }
        ["icon.png", "icon.jpg", "icon.svg"].into_iter().find_map(read)
    }

    /// Whether any HTML or JS entry mentions `joinRealtimeChannel`.
    fn archive_uses_realtime<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> bool {
        const NEEDLE: &[u8] = b"joinRealtimeChannel";
        for i in 0..archive.len() {
            let Ok(entry) = archive.by_index(i) else { continue };
            let name = entry.name().to_lowercase();
            if !(name.ends_with(".html") || name.ends_with(".htm")
                || name.ends_with(".js") || name.ends_with(".mjs"))
            {
                continue;
            }
            // Generous: real bundles run to tens of MB. Only a deflate bomb reaches the cap.
            if stream_contains(entry, NEEDLE, 128 * 1024 * 1024) {
                return true;
            }
        }
        false
    }

    /// Read a zip entry with a hard decompressed-size cap. The archive itself
    /// is size-capped, but deflate expands ~1000:1 — entry headers can't be
    /// trusted, so bound the actual bytes read.
    fn read_entry_capped<R: std::io::Read>(entry: &mut R, cap: u64) -> Result<Vec<u8>, Error> {
        let mut buf = Vec::new();
        entry.take(cap + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > cap {
            return Err(Error::InvalidPackage(
                "Zip entry exceeds decompressed size limit".to_string(),
            ));
        }
        Ok(buf)
    }

    /// Load Mini App info from bytes (in-memory, no file needed)
    /// Returns (manifest, icon_bytes)
    /// The opener's gates applied to raw bytes BEFORE a download is cached:
    /// the zip must carry an index.html and fit the same ceiling `load`
    /// enforces — otherwise the cache latches a package that can never open.
    pub fn validate_bytes_openable(bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > 500 * 1024 * 1024 {
            return Err(Error::InvalidPackage(format!(
                "File too large ({} MB)", bytes.len() / (1024 * 1024)
            )));
        }
        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        if resolve_entry_ci(&archive, "index.html").is_none() {
            return Err(Error::InvalidPackage("Missing index.html".to_string()));
        }
        Ok(())
    }

    pub fn load_info_from_bytes(bytes: &[u8], fallback_name: &str) -> Result<(MiniAppManifest, Option<Vec<u8>>), Error> {
        use std::io::Cursor;
        
        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;
        
        // Try to read manifest.toml (case-insensitive lookup — lenient for a mis-cased entry).
        let manifest = match resolve_entry_ci(&archive, "manifest.toml")
            .and_then(|n| archive.by_name(&n).ok())
        {
            Some(mut file) => {
                let bytes = Self::read_entry_capped(&mut file, 1024 * 1024)?;
                let contents = String::from_utf8(bytes)
                    .map_err(|e| Error::ManifestParse(e.to_string()))?;
                toml::from_str(&contents)
                    .map_err(|e| Error::ManifestParse(e.to_string()))?
            }
            None => {
                // No manifest, use fallback name
                MiniAppManifest {
                    name: fallback_name.to_string(),
                    ..Default::default()
                }
            }
        };

        let icon = Self::icon_from(&mut archive, &manifest.icon);

        Ok((manifest, icon))
    }
    
}

/// Generous: real .xdc games ship single packed assets (a Godot .pck, a wasm bundle) past
/// 100 MB. It only stops a small archive deflating to gigabytes; `load` caps the archive.
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// One open package read at explicit offsets, so each clone keeps its own cursor over one
/// handle. A kept app's copy is sealed, and reads open it as they go.
#[derive(Clone)]
struct SharedFile {
    src: Arc<dyn vector_core::media::MediaSource>,
    pos: u64,
}

impl SharedFile {
    fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let src = vector_core::vcache::open(path, "application/zip")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "the package can't be read"))?;
        Ok(Self { src, pos: 0 })
    }
}

impl Read for SharedFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // A megabyte at a time: Windows queues I/O on a synchronous handle, so a big stored asset
        // in one read would hold every other request behind it.
        let len = buf.len().min(1 << 20);
        let n = self.src.read_at(self.pos, &mut buf[..len])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for SharedFile {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            std::io::SeekFrom::Start(p) => Some(p),
            std::io::SeekFrom::End(d) => self.src.len().checked_add_signed(d),
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

/// A running app's archive. Its directory is read once; every read clones it, which shares
/// that directory and the handle but not the cursor, so parallel asset loads don't wait on a lock.
#[derive(Clone)]
pub struct PackageArchive {
    zip: zip::ZipArchive<SharedFile>,
    folded: Arc<std::sync::OnceLock<HashMap<Box<str>, usize>>>,
}

impl std::fmt::Debug for PackageArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageArchive").field("entries", &self.zip.len()).finish()
    }
}

impl PackageArchive {
    pub fn open(path: &std::path::Path) -> Result<Self, Error> {
        let zip = zip::ZipArchive::new(SharedFile::open(path)?)?;
        Ok(Self { zip, folded: Default::default() })
    }

    /// Read one entry. A mis-cased request still finds its file, as `resolve_entry_ci` allows.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        let normalized = normalize_entry_path(path)
            .ok_or_else(|| Error::FileNotFound(format!("Invalid path: {}", path)))?;
        let index = self
            .zip
            .index_for_name(&normalized)
            .or_else(|| self.folded().get(normalized.to_ascii_lowercase().as_str()).copied())
            .ok_or_else(|| Error::FileNotFound(path.to_string()))?;
        let mut zip = self.zip.clone();
        let mut entry = zip.by_index(index).map_err(|_| Error::FileNotFound(path.to_string()))?;
        MiniAppPackage::read_entry_capped(&mut entry, MAX_ENTRY_BYTES)
    }

    /// Lowercased names, built on the first mis-cased request; the first entry of a name wins.
    fn folded(&self) -> &HashMap<Box<str>, usize> {
        self.folded.get_or_init(|| {
            let mut map = HashMap::new();
            for (index, name) in self.zip.file_names().enumerate() {
                map.entry(name.to_ascii_lowercase().into_boxed_str()).or_insert(index);
            }
            map
        })
    }
}

/// Represents a running Mini App instance
#[derive(Debug, Clone)]
pub struct MiniAppInstance {
    /// The package this instance is running
    pub package: Arc<MiniAppPackage>,
    /// The chat ID this Mini App is associated with
    pub chat_id: String,
    /// The message ID that contains this Mini App
    pub message_id: String,
    /// Window label for this instance
    pub window_label: String,
    /// Topic ID for realtime channel (from the webxdc-topic tag in the Nostr event)
    pub realtime_topic: Option<TopicId>,
    /// Unique per-open generation (see [`next_instance_id`]). A rapid close→reopen
    /// re-registers the SAME label with a new instance; the old close's async
    /// teardown must only remove the instance it was started for.
    pub instance_id: u64,
    /// The package's archive, opened by the first request and closed with the window.
    pub archive: Arc<std::sync::OnceLock<PackageArchive>>,
}

impl MiniAppInstance {
    pub fn archive(&self) -> Result<PackageArchive, Error> {
        if let Some(open) = self.archive.get() {
            return Ok(open.clone());
        }
        let open = PackageArchive::open(&self.package.path)?;
        Ok(self.archive.get_or_init(|| open).clone())
    }
}

/// Monotonic id for [`MiniAppInstance::instance_id`].
pub fn next_instance_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Realtime channel state for a Mini App instance
#[derive(Debug)]
pub struct RealtimeChannelState {
    /// Topic ID for this channel
    pub topic: TopicId,
    /// Whether the channel is active
    pub active: bool,
    /// The window instance it belongs to: a teardown racing a reopen of the
    /// same label must not remove its successor's channel.
    pub instance_id: u64,
}

/// Global state for managing Mini App instances
pub struct MiniAppsState {
    /// Map of window_label -> MiniAppInstance
    instances: RwLock<HashMap<String, MiniAppInstance>>,
    /// Held while any Mini App window is open, so the process is not napped.
    awake: std::sync::Mutex<Option<crate::awake::Hold>>,
    /// Cache of loaded packages (id -> package)
    packages: RwLock<HashMap<String, Arc<MiniAppPackage>>>,
    /// Mini App windows' realtime sessions (vector-core's, over Iroh)
    pub realtime: RealtimeManager,
    /// Map of window_label -> realtime channel state
    pub realtime_channels: RwLock<HashMap<String, RealtimeChannelState>>,
    /// Known session participants (topic -> list of npubs).
    /// Single source of truth for lobby state and avatar display.
    session_peers: RwLock<HashMap<TopicId, Vec<String>>>,
    /// Preconnect completion signals — joinRealtimeChannel awaits these
    /// before attaching the event listener. Sender lives in the preconnect task.
    preconnect_signals: RwLock<HashMap<String, tokio::sync::watch::Receiver<bool>>>,
    /// Window labels whose open is in flight, between the instance check and the window.
    opening: Arc<std::sync::Mutex<HashSet<String>>>,
}

/// Holds a label in the opening set for the length of one `miniapp_open`.
pub struct OpeningGuard {
    set: Arc<std::sync::Mutex<HashSet<String>>>,
    label: String,
}

impl Drop for OpeningGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.set.lock() {
            set.remove(&self.label);
        }
    }
}

impl MiniAppsState {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            awake: std::sync::Mutex::new(None),
            packages: RwLock::new(HashMap::new()),
            realtime: RealtimeManager::new(),
            realtime_channels: RwLock::new(HashMap::new()),
            session_peers: RwLock::new(HashMap::new()),
            opening: Arc::new(std::sync::Mutex::new(HashSet::new())),
            preconnect_signals: RwLock::new(HashMap::new()),
        }
    }
    
    /// Drop the account-scoped realtime lobby state (session npubs, keyed by game
    /// topic). Called from the session-swap teardown, after the windows close.
    pub async fn clear_account_scoped(&self) {
        self.session_peers.write().await.clear();
    }

    /// Every open Mini App window's label and instance.
    pub async fn open_instances(&self) -> Vec<(String, u64)> {
        self.instances.read().await.iter().map(|(label, i)| (label.clone(), i.instance_id)).collect()
    }

    /// Get or set the realtime channel for an instance
    pub async fn set_realtime_channel(&self, window_label: &str, state: RealtimeChannelState) {
        let mut channels = self.realtime_channels.write().await;
        channels.insert(window_label.to_string(), state);
    }
    
    /// Remove the realtime channel only if it still belongs to `instance_id`.
    pub async fn remove_realtime_channel_if(&self, window_label: &str, instance_id: u64) -> Option<RealtimeChannelState> {
        let mut channels = self.realtime_channels.write().await;
        match channels.get(window_label) {
            Some(c) if c.instance_id == instance_id => channels.remove(window_label),
            _ => None,
        }
    }
    
    /// Check if ANY instance has an active realtime channel for a given topic
    pub async fn has_realtime_channel_for_topic(&self, topic: &TopicId) -> bool {
        let channels = self.realtime_channels.read().await;
        channels.values().any(|s| s.active && &s.topic == topic)
    }

    // ─── Preconnect signals ───────────────────────────────────────────────

    /// Store a preconnect completion signal for a window label
    pub async fn set_preconnect_signal(&self, label: &str, rx: tokio::sync::watch::Receiver<bool>) {
        self.preconnect_signals.write().await.insert(label.to_string(), rx);
    }

    /// Take the preconnect signal for a window label (consumed by joinRealtimeChannel)
    pub async fn take_preconnect_signal(&self, label: &str) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.preconnect_signals.write().await.remove(label)
    }

    // ─── Session peers (persistent participant tracking) ───────────────────

    /// Add a participant npub to the session (idempotent)
    pub async fn add_session_peer(&self, topic: TopicId, npub: String) {
        let mut peers = self.session_peers.write().await;
        let list = peers.entry(topic).or_default();
        if !list.contains(&npub) {
            list.push(npub);
        }
    }

    /// Remove a participant npub from the session
    pub async fn remove_session_peer(&self, topic: &TopicId, npub: &str) {
        let mut peers = self.session_peers.write().await;
        if let Some(list) = peers.get_mut(topic) {
            list.retain(|n| n != npub);
            if list.is_empty() {
                peers.remove(topic);
            }
        }
    }

    /// Get all participant npubs for a session
    pub async fn get_session_peers(&self, topic: &TopicId) -> Vec<String> {
        let peers = self.session_peers.read().await;
        peers.get(topic).cloned().unwrap_or_default()
    }

    /// Clear all session peers for a topic
    #[allow(dead_code)]
    pub async fn clear_session_peers(&self, topic: &TopicId) {
        let mut peers = self.session_peers.write().await;
        peers.remove(topic);
    }

    
    /// Register a new Mini App instance
    pub async fn add_instance(&self, instance: MiniAppInstance) {
        let mut instances = self.instances.write().await;
        instances.insert(instance.window_label.clone(), instance);
        if instances.len() == 1 {
            *self.awake.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::awake::hold(crate::awake::Level::NoNap));
        }
    }
    
    /// Remove an instance by window label
    /// Also cleans up any associated realtime channel state
    /// Remove an instance ONLY if it is still the given generation. A stale
    /// async teardown (close races a rapid reopen of the same label) must not
    /// delete the successor instance out from under its live session.
    pub async fn remove_instance_if(&self, window_label: &str, instance_id: u64) -> Option<MiniAppInstance> {
        let mut instances = self.instances.write().await;
        let removed = if instances.get(window_label).is_some_and(|i| i.instance_id == instance_id) {
            instances.remove(window_label)
        } else {
            None
        };
        if instances.is_empty() {
            self.awake.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        removed
    }
    
    /// Get an instance by window label
    /// Claim a label for an open in progress; `None` when another open holds it.
    pub fn begin_opening(&self, window_label: &str) -> Option<OpeningGuard> {
        let mut set = self.opening.lock().ok()?;
        if !set.insert(window_label.to_string()) {
            return None;
        }
        Some(OpeningGuard { set: Arc::clone(&self.opening), label: window_label.to_string() })
    }

    pub async fn get_instance(&self, window_label: &str) -> Option<MiniAppInstance> {
        let instances = self.instances.read().await;
        instances.get(window_label).cloned()
    }
    
    /// Get an instance by message ID
    pub async fn get_instance_by_message(&self, chat_id: &str, message_id: &str) -> Option<(String, MiniAppInstance)> {
        let instances = self.instances.read().await;
        for (label, instance) in instances.iter() {
            if instance.chat_id == chat_id && instance.message_id == message_id {
                return Some((label.clone(), instance.clone()));
            }
        }
        None
    }
    
    /// The package at `path`, loaded on first use and again only when the file has changed.
    pub async fn get_or_load_package(&self, id: &str, path: PathBuf, icon_dir: Option<PathBuf>) -> Result<Arc<MiniAppPackage>, Error> {
        let cached = self.packages.read().await.get(id).cloned();
        let path_display = path.display().to_string();
        // Blocking: a stat for a known package, the first read for a new one.
        let (package, fresh) = tokio::task::spawn_blocking(move || match cached {
            Some(pkg) if pkg.is_current() => Ok((pkg, false)),
            _ => MiniAppPackage::load(path, icon_dir.as_deref()).map(|p| (Arc::new(p), true)),
        })
        .await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!("Package load task failed for {}: {}", path_display, e)))??;
        if fresh {
            self.packages.write().await.insert(id.to_string(), Arc::clone(&package));
        }
        Ok(package)
    }
}

impl Default for MiniAppsState {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod package_meta_tests {
    use super::*;
    use std::io::Write;

    const NEEDLE: &[u8] = b"joinRealtimeChannel";

    /// A fresh account database, held under the lock every test that moves the session takes.
    fn open_test_db() -> tokio::sync::MutexGuard<'static, ()> {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let guard = crate::commands::transport::tests::TEST_LOCK.blocking_lock();
        use nostr_sdk::prelude::ToBech32;
        let root = ROOT.get_or_init(|| tempfile::tempdir().unwrap()).path();
        let account = nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(root.join(&account)).unwrap();
        vector_core::db::close_database();
        vector_core::db::set_app_data_dir(root.to_path_buf());
        vector_core::db::set_current_account(account.clone()).unwrap();
        vector_core::db::init_database(&account).unwrap();
        guard
    }

    fn xdc(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    fn set_modified(path: &std::path::Path, at: std::time::SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
    }

    #[test]
    fn a_held_archive_serves_parallel_reads_from_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.xdc");
        let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
        let files: Vec<(String, Vec<u8>)> = (0..64).map(|i| (format!("assets/{i}.bin"), vec![i as u8; 4096 + i])).collect();
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("index.html", b"<html></html>"),
            ("Data/Level.JSON", b"{}"),
            ("big.bin", &big),
            ("A.js", b"upper"),
            ("a.js", b"lower"),
        ];
        entries.extend(files.iter().map(|(n, d)| (n.as_str(), d.as_slice())));
        std::fs::write(&path, xdc(&entries)).unwrap();

        let archive = PackageArchive::open(&path).unwrap();
        assert_eq!(archive.read("/index.html").unwrap(), b"<html></html>");
        assert_eq!(archive.read("data/level.json").unwrap(), b"{}");
        assert_eq!(archive.read("a.js").unwrap(), b"lower");
        assert_eq!(archive.read("A.JS").unwrap(), b"upper", "the first entry of a folded name wins");
        assert!(archive.read("missing.js").is_err());
        assert!(archive.read("../index.html").is_err());

        std::thread::scope(|scope| {
            for round in 0..8 {
                let (archive, big, files) = (&archive, &big, &files);
                scope.spawn(move || {
                    for (i, (name, data)) in files.iter().enumerate().skip(round) {
                        assert_eq!(&archive.read(name).unwrap(), data, "{name}");
                        if i % 16 == 0 {
                            assert_eq!(&archive.read("big.bin").unwrap(), big);
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn a_needle_split_across_reads_is_found() {
        /// Hands out a few bytes per read, so every match straddles reads.
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(7).min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut data = vec![b'x'; 64 * 1024 - 5];
        data.extend_from_slice(NEEDLE);
        data.extend_from_slice(&[b'y'; 100]);
        assert!(stream_contains(&data[..], NEEDLE, u64::MAX));
        assert!(stream_contains(Trickle(&data), NEEDLE, u64::MAX));
        assert!(!stream_contains(&data[..], NEEDLE, (data.len() - 110) as u64), "past the cap");
        assert!(!stream_contains(&NEEDLE[1..], NEEDLE, u64::MAX));
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = ::image::RgbaImage::from_fn(w, h, |x, y| ::image::Rgba([(x % 256) as u8, (y % 256) as u8, 9, 255]));
        crate::shared::image::encode_png(img.as_raw(), w, h).unwrap()
    }

    fn png_dims(bytes: &[u8]) -> (u32, u32) {
        let img = ::image::load_from_memory(bytes).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn a_package_is_read_once_then_known_by_its_file() {
        let _db = open_test_db();
        let dir = tempfile::tempdir().unwrap();
        let icons = tempfile::tempdir().unwrap();
        let path = dir.path().join("chess.xdc");
        let mut script = vec![b' '; 200_000];
        script.extend_from_slice(b"webxdc.joinRealtimeChannel()");
        let bytes = xdc(&[
            ("index.html", b"<script src=app.js></script>"),
            ("app.js", &script),
            ("manifest.toml", b"name = \"Chess\"\nicon = \"art//logo.png\"\n"),
            ("art/logo.png", &png(1024, 512)),
        ]);
        std::fs::write(&path, &bytes).unwrap();

        let first = MiniAppPackage::load(path.clone(), Some(icons.path())).unwrap();
        assert_eq!(&*first.name, "Chess");
        assert!(first.uses_realtime);
        assert_eq!(first.file_hash, vector_core::crypto::sha256::digest(&bytes));
        // The icon lives on disk, downscaled for a listing, never in the record.
        let icon = first.icon_path(icons.path()).unwrap();
        assert_eq!(png_dims(&vector_core::vcache::read(&icon).unwrap()), (256, 128));

        // Same size and time over other bytes: the second load trusts what it stored.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, vec![0u8; bytes.len()]).unwrap();
        set_modified(&path, modified);
        assert!(first.is_current());
        let again = MiniAppPackage::load(path.clone(), Some(icons.path())).unwrap();
        assert_eq!((again.file_hash, &*again.name, again.uses_realtime), (first.file_hash, "Chess", true));

        // A file that changed is read again: this one is no package any more.
        set_modified(&path, modified + std::time::Duration::from_secs(1));
        assert!(!first.is_current());
        assert!(MiniAppPackage::load(path, Some(icons.path())).is_err());
    }

    #[test]
    fn a_cleared_icon_is_written_again_from_the_archive() {
        let _db = open_test_db();
        let dir = tempfile::tempdir().unwrap();
        let icons = tempfile::tempdir().unwrap();
        let path = dir.path().join("draw.xdc");
        std::fs::write(&path, xdc(&[("index.html", b"<p>hi</p>"), ("ICON.PNG", &png(64, 64))])).unwrap();

        let pkg = MiniAppPackage::load(path, Some(icons.path())).unwrap();
        let icon = pkg.icon_path(icons.path()).unwrap();
        std::fs::remove_file(&icon).unwrap();
        assert_eq!(pkg.icon_path(icons.path()), Some(icon.clone()));
        assert_eq!(png_dims(&vector_core::vcache::read(&icon).unwrap()), (64, 64));
    }

    #[test]
    fn a_package_without_a_manifest_is_named_by_each_file() {
        let _db = open_test_db();
        let dir = tempfile::tempdir().unwrap();
        let bytes = xdc(&[("Index.html", b"<p>hi</p>")]);
        let (one, two) = (dir.path().join("puzzle.xdc"), dir.path().join("riddle.xdc"));
        std::fs::write(&one, &bytes).unwrap();
        std::fs::write(&two, &bytes).unwrap();
        let a = MiniAppPackage::load(one, None).unwrap();
        let b = MiniAppPackage::load(two, None).unwrap();
        assert_eq!(a.file_hash, b.file_hash);
        assert_eq!((&*a.name, &*b.name), ("puzzle", "riddle"));
        assert!(!a.uses_realtime && !a.has_icon);
    }
}
