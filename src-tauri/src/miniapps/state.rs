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

/// Represents a Mini App package (a .xdc file which is a ZIP archive)
#[derive(Debug, Clone)]
pub struct MiniAppPackage {
    /// Unique identifier (typically the message ID or file hash)
    pub id: String,
    /// Path to the .xdc file
    pub path: PathBuf,
    /// Cached manifest data
    pub manifest: MiniAppManifest,
    /// SHA-256 hash of the .xdc file (used for permission identification)
    pub file_hash: String,
    /// Whether the app calls the realtime (Iroh) API.
    pub uses_realtime: bool,
}

/// What one read of a package finds, stored by its content hash.
#[derive(Serialize, Deserialize)]
struct PackageMeta {
    #[serde(default)]
    v: u32,
    /// None when the package ships no manifest.toml: the name then comes from its file name.
    manifest: Option<MiniAppManifest>,
    uses_realtime: bool,
}

/// Bumped when a read would find more than the stored metadata holds, so it's read again.
const PACKAGE_META_VERSION: u32 = 1;
const ICON_CAP: u64 = 8 * 1024 * 1024;

/// Modification time in nanoseconds since the epoch, 0 when the platform can't say.
fn modified_nanos(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// SHA-256 of a file, streamed: a package can be hundreds of MB.
pub(crate) fn hash_file(path: &std::path::Path) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(bytes_to_hex_string(&hasher.finalize()))
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
    /// by hash for every later load.
    pub fn load(id: String, path: PathBuf) -> Result<Self, Error> {
        // Pre-check: reject 0-byte or missing files without opening them
        // (macOS can hang on open() for corrupted files with 0 logical bytes
        // but non-zero physical blocks)
        let meta = std::fs::metadata(&path)?;
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
        let modified = modified_nanos(&meta);
        let path_key = path.to_string_lossy().into_owned();

        let known = vector_core::db::miniapps::miniapp_file_hash(&path_key, file_len, modified).ok().flatten();
        if let Some((file_hash, stored)) = known.and_then(|h| Self::stored_meta(&h).map(|m| (h, m))) {
            return Ok(Self::from_meta(id, path, file_hash, stored));
        }

        let file_hash = hash_file(&path)?;
        let package_meta = match Self::stored_meta(&file_hash) {
            Some(stored) => stored,
            None => {
                let (read, icon) = Self::read_package(&path)?;
                if let Ok(data) = serde_json::to_string(&read) {
                    let _ = vector_core::db::miniapps::set_miniapp_meta(&file_hash, &data, icon.as_deref());
                }
                read
            }
        };
        let _ = vector_core::db::miniapps::set_miniapp_file_hash(&path_key, file_len, modified, &file_hash);
        Ok(Self::from_meta(id, path, file_hash, package_meta))
    }

    fn stored_meta(file_hash: &str) -> Option<PackageMeta> {
        let data = vector_core::db::miniapps::miniapp_meta(file_hash).ok().flatten()?;
        serde_json::from_str::<PackageMeta>(&data).ok().filter(|m| m.v == PACKAGE_META_VERSION)
    }

    fn from_meta(id: String, path: PathBuf, file_hash: String, meta: PackageMeta) -> Self {
        let manifest = meta.manifest.unwrap_or_else(|| MiniAppManifest {
            name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Mini App").to_string(),
            ..Default::default()
        });
        Self { id, path, manifest, file_hash, uses_realtime: meta.uses_realtime }
    }

    /// Everything a listing or an open needs from the archive, read from disk without loading
    /// it whole. The icon comes back as the pixels a listing shows: an SVG is rendered.
    fn read_package(path: &std::path::Path) -> Result<(PackageMeta, Option<Vec<u8>>), Error> {
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
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
            .map(|bytes| {
                if vector_core::svg::looks_like_svg(&bytes) {
                    vector_core::svg::rasterize_png(&bytes, 256).unwrap_or_default()
                } else {
                    bytes
                }
            })
            .filter(|bytes| !bytes.is_empty());
        let uses_realtime = Self::archive_uses_realtime(&mut archive);
        Ok((PackageMeta { v: PACKAGE_META_VERSION, manifest, uses_realtime }, icon))
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
    
    /// Get a file from the package
    pub fn get_file(&self, path: &str) -> Result<Vec<u8>, Error> {
        let file = std::fs::File::open(&self.path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let normalized = normalize_entry_path(path)
            .ok_or_else(|| Error::FileNotFound(format!("Invalid path: {}", path)))?;

        // Case-insensitive resolve so a mis-cased entry (e.g. "Index.html") still serves.
        let actual_name = resolve_entry_ci(&archive, &normalized)
            .ok_or_else(|| Error::FileNotFound(path.to_string()))?;
        let mut zip_file = archive.by_name(&actual_name)
            .map_err(|_| Error::FileNotFound(path.to_string()))?;

        // Generous: real .xdc games ship single packed assets (e.g. a Godot
        // .pck / wasm bundle) well past 100 MB. The cap only exists to stop
        // a small archive deflating to multi-GB, not to police app size —
        // the 500 MB whole-archive cap handles that.
        Self::read_entry_capped(&mut zip_file, 512 * 1024 * 1024)
    }
    
    /// The icon a listing shows: the stored copy, else read from the archive.
    pub fn icon(&self) -> Option<Vec<u8>> {
        match vector_core::db::miniapps::miniapp_icon(&self.file_hash) {
            Ok(Some(stored)) => stored,
            _ => {
                let file = std::io::BufReader::new(std::fs::File::open(&self.path).ok()?);
                Self::icon_from(&mut zip::ZipArchive::new(file).ok()?, &self.manifest.icon)
            }
        }
    }
}

/// Represents a running Mini App instance
#[derive(Debug, Clone)]
pub struct MiniAppInstance {
    /// The package this instance is running
    pub package: MiniAppPackage,
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
    
    /// Load or get cached package
    pub async fn get_or_load_package(&self, id: &str, path: PathBuf) -> Result<Arc<MiniAppPackage>, Error> {
        // Check cache first
        {
            let packages = self.packages.read().await;
            if let Some(pkg) = packages.get(id) {
                return Ok(Arc::clone(pkg));
            }
        }

        // Load on a blocking thread so sync I/O doesn't starve the async runtime.
        // No per-step trace logs — boot already preloads every installed app,
        // which spams 4× lines per app for no actionable signal.
        let id_owned = id.to_string();
        let path_display = path.display().to_string();
        let package = tokio::task::spawn_blocking(move || {
            MiniAppPackage::load(id_owned, path)
        }).await.map_err(|e| Error::Anyhow(anyhow::anyhow!("Package load task failed for {}: {}", path_display, e)))??;

        let package = Arc::new(package);

        {
            let mut packages = self.packages.write().await;
            packages.insert(id.to_string(), Arc::clone(&package));
        }

        Ok(package)
    }
    
    /// Clear package cache
    #[allow(dead_code)]
    pub async fn clear_package_cache(&self, id: &str) {
        let mut packages = self.packages.write().await;
        packages.remove(id);
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

    #[test]
    fn a_package_is_read_once_then_known_by_its_file() {
        let _db = open_test_db();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chess.xdc");
        let icon = b"\x89PNG\r\n\x1a\nnot really".to_vec();
        let mut script = vec![b' '; 200_000];
        script.extend_from_slice(b"webxdc.joinRealtimeChannel()");
        let bytes = xdc(&[
            ("index.html", b"<script src=app.js></script>"),
            ("app.js", &script),
            ("manifest.toml", b"name = \"Chess\"\nicon = \"art//logo.png\"\n"),
            ("art/logo.png", &icon),
        ]);
        std::fs::write(&path, &bytes).unwrap();

        let first = MiniAppPackage::load("a".into(), path.clone()).unwrap();
        assert_eq!(first.manifest.name, "Chess");
        assert!(first.uses_realtime);
        assert_eq!(first.file_hash, bytes_to_hex_string(&vector_core::crypto::sha256::digest(&bytes)));
        assert_eq!(first.icon(), Some(icon));

        // Same size and time over other bytes: the second load trusts what it stored.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, vec![0u8; bytes.len()]).unwrap();
        set_modified(&path, modified);
        let again = MiniAppPackage::load("b".into(), path.clone()).unwrap();
        assert_eq!((&again.file_hash, &again.manifest.name, again.uses_realtime), (&first.file_hash, &first.manifest.name, true));

        // A file that changed is read again: this one is no package any more.
        set_modified(&path, modified + std::time::Duration::from_secs(1));
        assert!(MiniAppPackage::load("c".into(), path).is_err());
    }

    #[test]
    fn a_package_without_a_manifest_is_named_by_each_file() {
        let _db = open_test_db();
        let dir = tempfile::tempdir().unwrap();
        let bytes = xdc(&[("Index.html", b"<p>hi</p>"), ("icon.png", b"\x89PNG\r\n\x1a\n")]);
        let (one, two) = (dir.path().join("puzzle.xdc"), dir.path().join("riddle.xdc"));
        std::fs::write(&one, &bytes).unwrap();
        std::fs::write(&two, &bytes).unwrap();
        let a = MiniAppPackage::load("a".into(), one).unwrap();
        let b = MiniAppPackage::load("b".into(), two).unwrap();
        assert_eq!(a.file_hash, b.file_hash);
        assert_eq!((a.manifest.name.as_str(), b.manifest.name.as_str()), ("puzzle", "riddle"));
        assert!(!a.uses_realtime);
        assert!(a.icon().is_some());
    }
}
