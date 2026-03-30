//! Profile sync — priority queue, background processor, and relay fetching.
//!
//! The sync queue batches profile fetches by priority (Critical → High → Medium → Low),
//! with cache windows to avoid hammering relays. The background processor
//! drains the queue and calls `load_profile` for each entry.
//!
//! Platform-specific work (DB persistence, image caching) is handled by the
//! `ProfileSyncHandler` trait — src-tauri provides `TauriProfileSyncHandler`,
//! CLI provides a no-op or logging implementation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, LazyLock};
use std::time::{Duration, Instant};

use nostr_sdk::prelude::*;

use crate::compact::secs_to_compact;
use crate::profile::Profile;
use crate::state::{NOSTR_CLIENT, MY_PUBLIC_KEY, STATE};
use crate::traits::emit_event;

// ============================================================================
// SyncPriority
// ============================================================================

/// Priority levels for profile syncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SyncPriority {
    Critical,  // No metadata OR user clicked — fetch immediately
    High,      // Active chats — fetch soon
    Medium,    // Recent chats — fetch eventually
    Low,       // Old chats with metadata — passive refresh
}

impl SyncPriority {
    /// Cache window duration — how long before a profile can be re-fetched.
    pub fn cache_window(&self) -> Duration {
        match self {
            SyncPriority::Critical => Duration::from_secs(0),
            SyncPriority::High => Duration::from_secs(5 * 60),
            SyncPriority::Medium => Duration::from_secs(30 * 60),
            SyncPriority::Low => Duration::from_secs(24 * 60 * 60),
        }
    }

    /// Processing delay — how long after queuing before fetching.
    pub fn processing_delay(&self) -> Duration {
        match self {
            SyncPriority::Critical => Duration::from_secs(0),
            SyncPriority::High => Duration::from_secs(5),
            SyncPriority::Medium => Duration::from_secs(30),
            SyncPriority::Low => Duration::from_secs(5 * 60),
        }
    }

    /// Maximum batch size for this priority.
    pub fn batch_size(&self) -> usize {
        match self {
            SyncPriority::Critical => 10,
            SyncPriority::High => 20,
            SyncPriority::Medium => 30,
            SyncPriority::Low => 50,
        }
    }
}

// ============================================================================
// QueueEntry
// ============================================================================

#[derive(Debug, Clone)]
pub(crate) struct QueueEntry {
    npub: String,
    added_at: Instant,
}

// ============================================================================
// ProfileSyncQueue
// ============================================================================

/// Profile sync queue manager with four priority lanes.
pub struct ProfileSyncQueue {
    critical_queue: VecDeque<QueueEntry>,
    high_queue: VecDeque<QueueEntry>,
    medium_queue: VecDeque<QueueEntry>,
    low_queue: VecDeque<QueueEntry>,
    processing: HashSet<String>,
    last_fetched: HashMap<String, Instant>,
    is_processing: bool,
}

impl ProfileSyncQueue {
    pub fn new() -> Self {
        Self {
            critical_queue: VecDeque::new(),
            high_queue: VecDeque::new(),
            medium_queue: VecDeque::new(),
            low_queue: VecDeque::new(),
            processing: HashSet::new(),
            last_fetched: HashMap::new(),
            is_processing: false,
        }
    }

    /// Add a profile to the sync queue.
    pub fn add(&mut self, npub: String, priority: SyncPriority, force_refresh: bool) {
        if self.processing.contains(&npub) {
            return;
        }

        // Check cache window (unless force_refresh)
        if !force_refresh {
            if let Some(last_fetch) = self.last_fetched.get(&npub) {
                if last_fetch.elapsed() < priority.cache_window() {
                    return;
                }
            }
        }

        self.remove_from_all_queues(&npub);

        let entry = QueueEntry { npub, added_at: Instant::now() };
        match priority {
            SyncPriority::Critical => self.critical_queue.push_back(entry),
            SyncPriority::High => self.high_queue.push_back(entry),
            SyncPriority::Medium => self.medium_queue.push_back(entry),
            SyncPriority::Low => self.low_queue.push_back(entry),
        }
    }

    fn remove_from_all_queues(&mut self, npub: &str) {
        self.critical_queue.retain(|e| e.npub != npub);
        self.high_queue.retain(|e| e.npub != npub);
        self.medium_queue.retain(|e| e.npub != npub);
        self.low_queue.retain(|e| e.npub != npub);
    }

    /// Get the next batch of profiles ready to process (highest priority first).
    pub(crate) fn get_next_batch(&mut self) -> Vec<QueueEntry> {
        let mut batch = Vec::new();

        let (queue, priority) = if !self.critical_queue.is_empty() {
            (&mut self.critical_queue, SyncPriority::Critical)
        } else if !self.high_queue.is_empty() {
            (&mut self.high_queue, SyncPriority::High)
        } else if !self.medium_queue.is_empty() {
            (&mut self.medium_queue, SyncPriority::Medium)
        } else if !self.low_queue.is_empty() {
            (&mut self.low_queue, SyncPriority::Low)
        } else {
            return batch;
        };

        let batch_size = priority.batch_size();
        let processing_delay = priority.processing_delay();

        while batch.len() < batch_size && !queue.is_empty() {
            if let Some(entry) = queue.front() {
                if entry.added_at.elapsed() >= processing_delay {
                    let entry = queue.pop_front().unwrap();
                    batch.push(entry);
                } else {
                    break;
                }
            }
        }

        batch
    }

    pub fn mark_processing(&mut self, npub: &str) {
        self.processing.insert(npub.to_string());
    }

    pub fn mark_done(&mut self, npub: &str) {
        self.processing.remove(npub);
        self.last_fetched.insert(npub.to_string(), Instant::now());
    }
}

// ============================================================================
// Global queue
// ============================================================================

static PROFILE_SYNC_QUEUE: LazyLock<Arc<Mutex<ProfileSyncQueue>>> =
    LazyLock::new(|| Arc::new(Mutex::new(ProfileSyncQueue::new())));

// ============================================================================
// ProfileSyncHandler — platform-specific callbacks
// ============================================================================

/// Callback trait for platform-specific profile sync work.
///
/// The core `load_profile` handles relay fetching, STATE updates, and
/// EventEmitter notifications. This trait covers what differs per platform:
/// - **DB persistence** (SQLite upsert)
/// - **Image caching** (avatar/banner download + disk cache)
pub trait ProfileSyncHandler: Send + Sync {
    /// Called after a profile is fetched from relays and updated in STATE.
    /// `slim` is ready for DB persistence. `avatar_url`/`banner_url` are
    /// for image caching (may be empty).
    fn on_profile_fetched(&self, _slim: &crate::SlimProfile, _avatar_url: &str, _banner_url: &str) {}
}

/// No-op handler for CLI/tests.
pub struct NoOpProfileSyncHandler;
impl ProfileSyncHandler for NoOpProfileSyncHandler {}

// ============================================================================
// load_profile — core relay fetch + STATE update
// ============================================================================

/// Fetch a profile's metadata and status from relays, update STATE, and
/// notify via EventEmitter + handler callback.
///
/// Returns `true` if the fetch succeeded (even if nothing changed).
pub async fn load_profile(npub: String, handler: &dyn ProfileSyncHandler) -> bool {
    let client = match NOSTR_CLIENT.get() {
        Some(c) => c,
        None => return false,
    };

    let profile_pubkey = match PublicKey::from_bech32(npub.as_str()) {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    let my_public_key = match MY_PUBLIC_KEY.get() {
        Some(&pk) => pk,
        None => return false,
    };

    // Grab old status (or create profile if missing)
    let (old_status_title, old_status_purpose, old_status_url): (String, String, String);
    {
        let mut state = STATE.lock().await;
        match state.get_profile(&npub) {
            Some(p) => {
                old_status_title = p.status_title.to_string();
                old_status_purpose = p.status_purpose.to_string();
                old_status_url = p.status_url.to_string();
            }
            None => {
                state.insert_or_replace_profile(&npub, Profile::new());
                old_status_title = String::new();
                old_status_purpose = String::new();
                old_status_url = String::new();
            }
        }
    }

    // Fetch status (kind 30315) from relays
    let status_filter = Filter::new()
        .author(profile_pubkey)
        .kind(Kind::from_u16(30315))
        .limit(1);

    let (status_title, status_purpose, status_url) = match client
        .fetch_events(status_filter, Duration::from_secs(15))
        .await
    {
        Ok(res) => {
            if !res.is_empty() {
                let status_event = res.first().unwrap();
                (
                    status_event.content.clone(),
                    status_event.tags.first()
                        .and_then(|t| t.content())
                        .unwrap_or_default()
                        .to_string(),
                    String::new(),
                )
            } else {
                (old_status_title, old_status_purpose, old_status_url)
            }
        }
        Err(_) => (old_status_title, old_status_purpose, old_status_url),
    };

    // Fetch metadata from relays
    let fetch_result = client
        .fetch_metadata(profile_pubkey, Duration::from_secs(15))
        .await;

    match fetch_result {
        Ok(meta) => {
            if meta.is_some() {
                let save_data = {
                    let mut state = STATE.lock().await;
                    let id = match state.interner.lookup(&npub) {
                        Some(id) => id,
                        None => return false,
                    };
                    let (changed, avatar_url, banner_url) = {
                        let profile = match state.get_profile_mut_by_id(id) {
                            Some(p) => p,
                            None => return false,
                        };
                        profile.flags.set_mine(my_public_key == profile_pubkey);

                        // Update status
                        let status_changed = *profile.status_title != *status_title
                            || *profile.status_purpose != *status_purpose
                            || *profile.status_url != *status_url;
                        profile.status_title = status_title.into_boxed_str();
                        profile.status_purpose = status_purpose.into_boxed_str();
                        profile.status_url = status_url.into_boxed_str();

                        // Update metadata
                        let metadata_changed = profile.from_metadata(meta.unwrap());

                        // Update timestamp
                        profile.last_updated = secs_to_compact(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                        );

                        (status_changed || metadata_changed,
                         profile.avatar.to_string(),
                         profile.banner.to_string())
                    };

                    if changed {
                        let slim = state.serialize_profile(id).unwrap();
                        Some((slim, avatar_url, banner_url))
                    } else {
                        None
                    }
                };

                if let Some((slim, avatar_url, banner_url)) = save_data {
                    // Notify UI via EventEmitter
                    emit_event("profile_update", &slim);
                    // Platform-specific: DB persist + image caching
                    handler.on_profile_fetched(&slim, &avatar_url, &banner_url);
                }
                true
            } else {
                // No metadata on relays — update timestamp so we don't keep retrying
                let mut state = STATE.lock().await;
                if let Some(profile) = state.get_profile_mut(&npub) {
                    profile.last_updated = secs_to_compact(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                    );
                }
                true
            }
        }
        Err(_) => false,
    }
}

// ============================================================================
// Background processor
// ============================================================================

/// Background processor that continuously drains the profile sync queue.
///
/// Spawned once at startup. Processes batches in priority order, calling
/// `load_profile` for each entry with the provided handler.
pub async fn start_profile_sync_processor(handler: Arc<dyn ProfileSyncHandler>) {
    let mut last_own_profile_sync = Instant::now();
    let own_profile_sync_interval = Duration::from_secs(5 * 60);

    loop {
        // Periodically queue our own profile to detect changes from other Nostr apps
        if last_own_profile_sync.elapsed() >= own_profile_sync_interval {
            let state = STATE.lock().await;
            if let Some(own_profile) = state.profiles.iter().find(|p| p.flags.is_mine()) {
                let npub = state.interner.resolve(own_profile.id).unwrap_or("").to_string();
                drop(state);

                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.add(npub, SyncPriority::Low, false);
            }
            last_own_profile_sync = Instant::now();
        }

        // Get next batch (lock scoped)
        let (should_wait, batch) = {
            let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();

            if queue.is_processing {
                (true, vec![])
            } else {
                queue.is_processing = true;
                let batch = queue.get_next_batch();
                for entry in &batch {
                    queue.mark_processing(&entry.npub);
                }
                (false, batch)
            }
        };

        if should_wait {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        if batch.is_empty() {
            {
                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.is_processing = false;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        // Process the batch
        for entry in &batch {
            load_profile(entry.npub.clone(), handler.as_ref()).await;

            {
                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.mark_done(&entry.npub);
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Release processing lock
        {
            let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
            queue.is_processing = false;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Queue a single profile for syncing.
pub fn queue_profile_sync(npub: String, priority: SyncPriority, force_refresh: bool) {
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    queue.add(npub, priority, force_refresh);
}

/// Queue all profiles for a chat.
pub async fn queue_chat_profiles(chat_id: String, is_opening: bool) {
    let state = STATE.lock().await;

    let chat = match state.get_chat(&chat_id) {
        Some(c) => c,
        None => return,
    };

    let base_priority = if is_opening {
        SyncPriority::High
    } else {
        SyncPriority::Medium
    };

    let mut profiles_to_queue = Vec::new();

    for &handle in chat.participants() {
        let member_npub = match state.interner.resolve(handle) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let has_metadata = state.get_profile_by_id(handle)
            .map(|p| {
                let has_data = !p.name.is_empty() || !p.display_name.is_empty() || !p.avatar.is_empty();
                let was_fetched = p.last_updated > 0;
                has_data || was_fetched
            })
            .unwrap_or(false);

        let priority = if !has_metadata {
            SyncPriority::Critical
        } else {
            base_priority
        };

        profiles_to_queue.push((member_npub, priority));
    }

    drop(state);

    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    for (npub, priority) in profiles_to_queue {
        queue.add(npub, priority, false);
    }
}

/// Force immediate refresh of a profile (for user clicks).
pub fn refresh_profile_now(npub: String) {
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    queue.add(npub, SyncPriority::Critical, true);
}

/// Sync all profiles in the system.
pub async fn sync_all_profiles() {
    let state = STATE.lock().await;

    let mut profiles_to_queue = Vec::new();

    for profile in &state.profiles {
        let npub = match state.interner.resolve(profile.id) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let has_metadata = !profile.name.is_empty() || !profile.display_name.is_empty() || !profile.avatar.is_empty();
        let was_fetched = profile.last_updated > 0;

        let priority = if !has_metadata && !was_fetched {
            SyncPriority::Critical
        } else {
            SyncPriority::Low
        };

        profiles_to_queue.push((npub, priority));
    }

    drop(state);

    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    for (npub, priority) in profiles_to_queue {
        queue.add(npub, priority, false);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_priority_cache_windows() {
        assert_eq!(SyncPriority::Critical.cache_window(), Duration::from_secs(0));
        assert_eq!(SyncPriority::High.cache_window(), Duration::from_secs(300));
        assert_eq!(SyncPriority::Medium.cache_window(), Duration::from_secs(1800));
        assert_eq!(SyncPriority::Low.cache_window(), Duration::from_secs(86400));
    }

    #[test]
    fn sync_priority_batch_sizes() {
        assert_eq!(SyncPriority::Critical.batch_size(), 10);
        assert_eq!(SyncPriority::High.batch_size(), 20);
        assert_eq!(SyncPriority::Medium.batch_size(), 30);
        assert_eq!(SyncPriority::Low.batch_size(), 50);
    }

    #[test]
    fn queue_add_and_dedup() {
        let mut queue = ProfileSyncQueue::new();

        queue.add("npub1alice".to_string(), SyncPriority::Low, false);
        queue.add("npub1alice".to_string(), SyncPriority::High, false);

        // Should be in High queue only (deduped from Low)
        assert!(queue.low_queue.is_empty());
        assert_eq!(queue.high_queue.len(), 1);
        assert_eq!(queue.high_queue[0].npub, "npub1alice");
    }

    #[test]
    fn queue_skips_processing() {
        let mut queue = ProfileSyncQueue::new();
        queue.mark_processing("npub1bob");

        queue.add("npub1bob".to_string(), SyncPriority::Critical, false);
        assert!(queue.critical_queue.is_empty(), "should skip profiles being processed");
    }

    #[test]
    fn queue_cache_window_skips() {
        let mut queue = ProfileSyncQueue::new();

        // Mark as recently fetched
        queue.mark_done("npub1carol");

        // Try to add with Low priority (24h cache window) — should skip
        queue.add("npub1carol".to_string(), SyncPriority::Low, false);
        assert!(queue.low_queue.is_empty(), "should skip within cache window");

        // Force refresh should bypass cache
        queue.add("npub1carol".to_string(), SyncPriority::Low, true);
        assert_eq!(queue.low_queue.len(), 1, "force_refresh should bypass cache");
    }

    #[test]
    fn queue_critical_skips_cache() {
        let mut queue = ProfileSyncQueue::new();

        // Critical has 0s cache window — always fetches
        queue.mark_done("npub1dave");
        queue.add("npub1dave".to_string(), SyncPriority::Critical, false);
        assert_eq!(queue.critical_queue.len(), 1, "Critical should always fetch");
    }

    #[test]
    fn get_next_batch_priority_order() {
        let mut queue = ProfileSyncQueue::new();

        // Add to Low and Critical queues
        queue.low_queue.push_back(QueueEntry {
            npub: "npub1low".to_string(),
            added_at: Instant::now() - Duration::from_secs(600),
        });
        queue.critical_queue.push_back(QueueEntry {
            npub: "npub1critical".to_string(),
            added_at: Instant::now(),
        });

        let batch = queue.get_next_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].npub, "npub1critical", "Critical should process before Low");
    }

    #[test]
    fn get_next_batch_respects_delay() {
        let mut queue = ProfileSyncQueue::new();

        // Add a High priority entry just now (5s delay required)
        queue.high_queue.push_back(QueueEntry {
            npub: "npub1new".to_string(),
            added_at: Instant::now(),
        });

        let batch = queue.get_next_batch();
        assert!(batch.is_empty(), "should not process before delay elapses");
    }

    #[test]
    fn mark_done_updates_last_fetched() {
        let mut queue = ProfileSyncQueue::new();
        queue.mark_processing("npub1eve");
        assert!(queue.processing.contains("npub1eve"));

        queue.mark_done("npub1eve");
        assert!(!queue.processing.contains("npub1eve"));
        assert!(queue.last_fetched.contains_key("npub1eve"));
    }

    #[test]
    fn noop_handler_compiles() {
        let handler = NoOpProfileSyncHandler;
        let slim = crate::SlimProfile::default();
        handler.on_profile_fetched(&slim, "", "");
    }
}
