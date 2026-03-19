//! State management for Mini App instances

use std::collections::HashMap;
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
}

impl MiniAppPackage {
    /// Load a Mini App package from a .xdc file
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
        let file_data = std::fs::read(&path)?;

        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(&file_data);
        let file_hash = bytes_to_hex_string(&hasher.finalize());
        use std::io::Cursor;
        let cursor = Cursor::new(&file_data);
        let mut archive = zip::ZipArchive::new(cursor)?;

        // Try to read manifest.toml
        let manifest = match archive.by_name("manifest.toml") {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;
                toml::from_str(&contents)
                    .map_err(|e| Error::ManifestParseError(e.to_string()))?
            }
            Err(_) => {
                let name = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Mini App")
                    .to_string();
                MiniAppManifest {
                    name,
                    ..Default::default()
                }
            }
        };

        if archive.by_name("index.html").is_err() {
            return Err(Error::InvalidPackage("Missing index.html".to_string()));
        }

        Ok(Self { id, path, manifest, file_hash })
    }
    
    /// Load Mini App info from bytes (in-memory, no file needed)
    /// Returns (manifest, icon_bytes)
    pub fn load_info_from_bytes(bytes: &[u8], fallback_name: &str) -> Result<(MiniAppManifest, Option<Vec<u8>>), Error> {
        use std::io::Cursor;
        
        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;
        
        // Try to read manifest.toml
        let manifest = match archive.by_name("manifest.toml") {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;
                toml::from_str(&contents)
                    .map_err(|e| Error::ManifestParseError(e.to_string()))?
            }
            Err(_) => {
                // No manifest, use fallback name
                MiniAppManifest {
                    name: fallback_name.to_string(),
                    ..Default::default()
                }
            }
        };
        
        // Try to get icon
        let icon = if !manifest.icon.is_empty() {
            // Use icon from manifest
            match archive.by_name(&manifest.icon) {
                Ok(mut file) => {
                    let mut data = Vec::new();
                    file.read_to_end(&mut data).ok();
                    Some(data)
                }
                Err(_) => None,
            }
        } else {
            // Try common icon names
            let mut icon_data = None;
            for icon_name in &["icon.png", "icon.jpg", "icon.svg"] {
                if let Ok(mut file) = archive.by_name(icon_name) {
                    let mut data = Vec::new();
                    if file.read_to_end(&mut data).is_ok() {
                        icon_data = Some(data);
                        break;
                    }
                }
            }
            icon_data
        };
        
        Ok((manifest, icon))
    }
    
    /// Get a file from the package
    pub fn get_file(&self, path: &str) -> Result<Vec<u8>, Error> {
        let file = std::fs::File::open(&self.path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        // Normalize path (remove leading slash) and prevent path traversal
        let normalized_path = path.trim_start_matches('/');

        // Reject paths containing directory traversal sequences
        if normalized_path.contains("..") || normalized_path.contains("\\..") {
            return Err(Error::FileNotFound(format!("Invalid path: {}", path)));
        }
        
        let mut zip_file = archive.by_name(normalized_path)
            .map_err(|_| Error::FileNotFound(path.to_string()))?;
        
        let mut contents = Vec::new();
        zip_file.read_to_end(&mut contents)?;
        Ok(contents)
    }
    
    /// Get the icon as bytes, if available
    pub fn get_icon(&self) -> Option<Vec<u8>> {
        if self.manifest.icon.is_empty() {
            // Try common icon names
            for icon_name in &["icon.png", "icon.jpg", "icon.svg"] {
                if let Ok(data) = self.get_file(icon_name) {
                    return Some(data);
                }
            }
            None
        } else {
            self.get_file(&self.manifest.icon).ok()
        }
    }

    /// Check if an XDC package's source uses the WebXDC realtime channel API.
    /// Scans HTML/JS files for `joinRealtimeChannel`. Designed for `spawn_blocking`.
    pub fn scan_for_realtime_api(path: &std::path::Path) -> bool {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(_) => return false,
        };
        const NEEDLE: &[u8] = b"joinRealtimeChannel";
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else { continue };
            let name = entry.name().to_lowercase();
            if !(name.ends_with(".html") || name.ends_with(".htm")
                || name.ends_with(".js") || name.ends_with(".mjs"))
            {
                continue;
            }
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_err() { continue; }
            if buf.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
                return true;
            }
        }
        false
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
}

/// Realtime channel state for a Mini App instance
#[derive(Debug)]
pub struct RealtimeChannelState {
    /// Topic ID for this channel
    pub topic: TopicId,
    /// Whether the channel is active
    pub active: bool,
}

/// Global state for managing Mini App instances
pub struct MiniAppsState {
    /// Map of window_label -> MiniAppInstance
    instances: RwLock<HashMap<String, MiniAppInstance>>,
    /// Cache of loaded packages (id -> package)
    packages: RwLock<HashMap<String, Arc<MiniAppPackage>>>,
    /// Realtime channel manager (Iroh P2P)
    pub realtime: RealtimeManager,
    /// Map of window_label -> realtime channel state
    pub realtime_channels: RwLock<HashMap<String, RealtimeChannelState>>,
    /// Cached peer addresses for QUIC connection on join (topic -> addrs).
    /// Populated from peer advertisements, consumed on join.
    peer_addrs: RwLock<HashMap<TopicId, Vec<iroh::EndpointAddr>>>,
    /// Known session participants (topic -> list of npubs).
    /// Single source of truth for lobby state and avatar display.
    session_peers: RwLock<HashMap<TopicId, Vec<String>>>,
    /// Preconnect completion signals — joinRealtimeChannel awaits these
    /// before attaching the event listener. Sender lives in the preconnect task.
    preconnect_signals: RwLock<HashMap<String, tokio::sync::watch::Receiver<bool>>>,
}

impl MiniAppsState {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            packages: RwLock::new(HashMap::new()),
            realtime: RealtimeManager::new(None),
            realtime_channels: RwLock::new(HashMap::new()),
            peer_addrs: RwLock::new(HashMap::new()),
            session_peers: RwLock::new(HashMap::new()),
            preconnect_signals: RwLock::new(HashMap::new()),
        }
    }
    
    /// Get or set the realtime channel for an instance
    pub async fn set_realtime_channel(&self, window_label: &str, state: RealtimeChannelState) {
        let mut channels = self.realtime_channels.write().await;
        channels.insert(window_label.to_string(), state);
    }
    
    /// Get the realtime channel state for an instance
    pub async fn get_realtime_channel(&self, window_label: &str) -> Option<TopicId> {
        let channels = self.realtime_channels.read().await;
        channels.get(window_label).map(|s| s.topic)
    }
    
    /// Remove the realtime channel for an instance
    pub async fn remove_realtime_channel(&self, window_label: &str) -> Option<RealtimeChannelState> {
        let mut channels = self.realtime_channels.write().await;
        channels.remove(window_label)
    }
    
    /// Check if an instance has an active realtime channel
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub async fn has_realtime_channel(&self, window_label: &str) -> bool {
        let channels = self.realtime_channels.read().await;
        channels.get(window_label).map(|s| s.active).unwrap_or(false)
    }

    /// Check if ANY instance has an active realtime channel for a given topic
    pub async fn has_realtime_channel_for_topic(&self, topic: &TopicId) -> bool {
        let channels = self.realtime_channels.read().await;
        channels.values().any(|s| s.active && &s.topic == topic)
    }

    /// Get the chat_id for an instance that has a realtime channel with the given topic
    pub async fn get_chat_id_for_topic(&self, topic: &TopicId) -> Option<String> {
        let channels = self.realtime_channels.read().await;
        let label = channels.iter()
            .find(|(_, s)| s.active && &s.topic == topic)
            .map(|(label, _)| label.clone())?;
        drop(channels);
        self.get_instance(&label).await.map(|i| i.chat_id.clone())
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

    // ─── Peer address cache (for QUIC connection on join) ──────────────────

    /// Cache a peer's address for a topic (from a peer advertisement)
    pub async fn cache_peer_addr(&self, topic: TopicId, addr: iroh::EndpointAddr) {
        let mut addrs = self.peer_addrs.write().await;
        let list = addrs.entry(topic).or_default();
        if !list.iter().any(|a| a.id == addr.id) {
            list.push(addr);
        }
    }

    /// Take all cached peer addresses for a topic (consumed on join)
    pub async fn take_peer_addrs(&self, topic: &TopicId) -> Vec<iroh::EndpointAddr> {
        let mut addrs = self.peer_addrs.write().await;
        addrs.remove(topic).unwrap_or_default()
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
    }
    
    /// Remove an instance by window label
    /// Also cleans up any associated realtime channel state
    pub async fn remove_instance(&self, window_label: &str) -> Option<MiniAppInstance> {
        // Clean up realtime channel state first
        self.remove_realtime_channel(window_label).await;
        
        let mut instances = self.instances.write().await;
        instances.remove(window_label)
    }
    
    /// Get an instance by window label
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
                log_trace!("[MiniApp] Package cache hit for {}", id);
                return Ok(Arc::clone(pkg));
            }
        }

        log_trace!("[MiniApp] Loading package {} from {:?}", id, path);

        // Load on a blocking thread so sync I/O doesn't starve the async runtime
        let id_owned = id.to_string();
        let path_display = path.display().to_string();
        let package = tokio::task::spawn_blocking(move || {
            log_trace!("[MiniApp] spawn_blocking: starting load for {}", id_owned);
            let result = MiniAppPackage::load(id_owned, path);
            log_trace!("[MiniApp] spawn_blocking: load finished, success={}", result.is_ok());
            result
        }).await.map_err(|e| Error::Anyhow(anyhow::anyhow!("Package load task failed for {}: {}", path_display, e)))??;

        log_trace!("[MiniApp] Package loaded: {} ({})", package.manifest.name, id);
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