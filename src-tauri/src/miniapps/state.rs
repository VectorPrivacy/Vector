//! State management for Mini App instances

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use iroh_gossip::proto::TopicId;
use tauri::ipc::Channel;

use super::error::Error;
use super::realtime::{RealtimeEvent, RealtimeManager};

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
}

impl MiniAppPackage {
    /// Load a Mini App package from a .xdc file
    pub fn load(id: String, path: PathBuf) -> Result<Self, Error> {
        let file = std::fs::File::open(&path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        
        // Try to read manifest.toml
        let manifest = match archive.by_name("manifest.toml") {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;
                toml::from_str(&contents)
                    .map_err(|e| Error::ManifestParseError(e.to_string()))?
            }
            Err(_) => {
                // No manifest, try to get name from index.html title or use filename
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
        
        // Verify index.html exists
        if archive.by_name("index.html").is_err() {
            return Err(Error::InvalidPackage("Missing index.html".to_string()));
        }
        
        Ok(Self { id, path, manifest })
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
        
        // Normalize path (remove leading slash)
        let normalized_path = path.trim_start_matches('/');
        
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
pub struct RealtimeChannelState {
    /// Topic ID for this channel
    pub topic: TopicId,
    /// Event channel for sending data to frontend
    pub event_channel: Option<Channel<RealtimeEvent>>,
    /// Whether the channel is active
    pub active: bool,
}

impl std::fmt::Debug for RealtimeChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeChannelState")
            .field("topic", &self.topic)
            .field("event_channel", &self.event_channel.is_some())
            .field("active", &self.active)
            .finish()
    }
}

/// Global state for managing Mini App instances
/// A pending peer advertisement (received before we joined the channel)
#[derive(Clone, Debug)]
pub struct PendingPeer {
    pub node_addr: iroh::NodeAddr,
    pub received_at: std::time::Instant,
}

pub struct MiniAppsState {
    /// Map of window_label -> MiniAppInstance
    instances: RwLock<HashMap<String, MiniAppInstance>>,
    /// Cache of loaded packages (id -> package)
    packages: RwLock<HashMap<String, Arc<MiniAppPackage>>>,
    /// Realtime channel manager (Iroh P2P)
    pub realtime: RealtimeManager,
    /// Map of window_label -> realtime channel state
    pub realtime_channels: RwLock<HashMap<String, RealtimeChannelState>>,
    /// Pending peer advertisements (topic -> list of peers)
    /// These are peers that advertised before we joined the channel
    pub pending_peers: RwLock<HashMap<TopicId, Vec<PendingPeer>>>,
}

impl MiniAppsState {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            packages: RwLock::new(HashMap::new()),
            realtime: RealtimeManager::new(None),
            realtime_channels: RwLock::new(HashMap::new()),
            pending_peers: RwLock::new(HashMap::new()),
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
    pub async fn has_realtime_channel(&self, window_label: &str) -> bool {
        let channels = self.realtime_channels.read().await;
        channels.get(window_label).map(|s| s.active).unwrap_or(false)
    }
    
    /// Add a pending peer for a topic (received before we joined)
    pub async fn add_pending_peer(&self, topic: TopicId, node_addr: iroh::NodeAddr) {
        let topic_encoded = crate::miniapps::realtime::encode_topic_id(&topic);
        println!("[WEBXDC] add_pending_peer: Adding peer {} for topic {}", node_addr.node_id, topic_encoded);
        
        let mut pending = self.pending_peers.write().await;
        let peers = pending.entry(topic).or_insert_with(Vec::new);
        
        // Don't add duplicates
        if !peers.iter().any(|p| p.node_addr.node_id == node_addr.node_id) {
            peers.push(PendingPeer {
                node_addr: node_addr.clone(),
                received_at: std::time::Instant::now(),
            });
            println!("[WEBXDC] add_pending_peer: Stored pending peer {} for topic {}", node_addr.node_id, topic_encoded);
        } else {
            println!("[WEBXDC] add_pending_peer: Peer {} already exists for topic {}", node_addr.node_id, topic_encoded);
        }
    }
    
    /// Get and clear pending peers for a topic
    pub async fn take_pending_peers(&self, topic: &TopicId) -> Vec<PendingPeer> {
        let topic_encoded = crate::miniapps::realtime::encode_topic_id(topic);
        println!("[WEBXDC] take_pending_peers: Looking for peers for topic {}", topic_encoded);
        
        let mut pending = self.pending_peers.write().await;
        
        // Debug: print all pending topics
        println!("[WEBXDC] take_pending_peers: Currently have {} topics with pending peers", pending.len());
        for (t, peers) in pending.iter() {
            let t_encoded = crate::miniapps::realtime::encode_topic_id(t);
            println!("[WEBXDC] take_pending_peers:   Topic {} has {} pending peers", t_encoded, peers.len());
        }
        
        let peers = pending.remove(topic).unwrap_or_default();
        println!("[WEBXDC] take_pending_peers: Found {} peers for topic {}", peers.len(), topic_encoded);
        
        // Filter out peers that are too old (more than 5 minutes)
        let now = std::time::Instant::now();
        let filtered: Vec<PendingPeer> = peers.into_iter()
            .filter(|p| now.duration_since(p.received_at).as_secs() < 300)
            .collect();
        
        println!("[WEBXDC] take_pending_peers: After filtering, {} peers remain", filtered.len());
        filtered
    }
    
    /// Get the count of pending peers for a topic (without removing them)
    pub async fn get_pending_peer_count(&self, topic: &TopicId) -> usize {
        let pending = self.pending_peers.read().await;
        pending.get(topic).map(|peers| peers.len()).unwrap_or(0)
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
                return Ok(Arc::clone(pkg));
            }
        }
        
        // Load and cache
        let package = Arc::new(MiniAppPackage::load(id.to_string(), path)?);
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