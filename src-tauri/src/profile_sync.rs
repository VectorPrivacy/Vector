use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, LazyLock};
use std::time::{Duration, Instant};
use crate::{profile, STATE};

/// Priority levels for profile syncing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SyncPriority {
    Critical,  // No metadata OR user clicked - fetch immediately
    High,      // Active chats - fetch soon
    Medium,    // Recent chats - fetch eventually
    Low,       // Old chats with metadata - passive refresh
}

impl SyncPriority {
    /// Returns the cache window duration for this priority level
    pub fn cache_window(&self) -> Duration {
        match self {
            SyncPriority::Critical => Duration::from_secs(0),      // No cache, always fetch
            SyncPriority::High => Duration::from_secs(5 * 60),     // 5 minutes
            SyncPriority::Medium => Duration::from_secs(30 * 60),  // 30 minutes
            SyncPriority::Low => Duration::from_secs(24 * 60 * 60), // 24 hours
        }
    }

    /// Returns the processing delay before fetching
    pub fn processing_delay(&self) -> Duration {
        match self {
            SyncPriority::Critical => Duration::from_secs(0),      // Immediate
            SyncPriority::High => Duration::from_secs(5),          // 5 seconds
            SyncPriority::Medium => Duration::from_secs(30),       // 30 seconds
            SyncPriority::Low => Duration::from_secs(5 * 60),      // 5 minutes
        }
    }

    /// Returns the maximum batch size for this priority
    pub fn batch_size(&self) -> usize {
        match self {
            SyncPriority::Critical => 10,  // Process critical profiles quickly
            SyncPriority::High => 20,      // Moderate batch for active chats
            SyncPriority::Medium => 30,    // Larger batch for recent chats
            SyncPriority::Low => 50,       // Large batch for passive refresh
        }
    }
}

/// Entry in the sync queue
#[derive(Debug, Clone)]
pub(crate) struct QueueEntry {
    npub: String,
    added_at: Instant,
}

/// Profile sync queue manager
pub struct ProfileSyncQueue {
    // Separate queues for each priority level
    critical_queue: VecDeque<QueueEntry>,
    high_queue: VecDeque<QueueEntry>,
    medium_queue: VecDeque<QueueEntry>,
    low_queue: VecDeque<QueueEntry>,
    
    // Track profiles currently being processed
    processing: HashSet<String>,
    
    // Track when profiles were last fetched
    last_fetched: HashMap<String, Instant>,
    
    // Background processor state
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

    /// Add a profile to the sync queue
    pub fn add(&mut self, npub: String, priority: SyncPriority, force_refresh: bool) {
        // Check if already processing
        if self.processing.contains(&npub) {
            return;
        }

        // Check cache window (unless force_refresh)
        if !force_refresh {
            if let Some(last_fetch) = self.last_fetched.get(&npub) {
                let cache_window = priority.cache_window();
                if last_fetch.elapsed() < cache_window {
                    // Still within cache window, skip
                    return;
                }
            }
        }

        // Remove from all other queues (deduplication)
        self.remove_from_all_queues(&npub);

        // Add to appropriate queue
        let entry = QueueEntry {
            npub,
            added_at: Instant::now(),
        };

        match priority {
            SyncPriority::Critical => self.critical_queue.push_back(entry),
            SyncPriority::High => self.high_queue.push_back(entry),
            SyncPriority::Medium => self.medium_queue.push_back(entry),
            SyncPriority::Low => self.low_queue.push_back(entry),
        }
    }

    /// Remove an npub from all queues (deduplication helper)
    fn remove_from_all_queues(&mut self, npub: &str) {
        self.critical_queue.retain(|e| e.npub != npub);
        self.high_queue.retain(|e| e.npub != npub);
        self.medium_queue.retain(|e| e.npub != npub);
        self.low_queue.retain(|e| e.npub != npub);
    }

    /// Get the next batch of profiles to process
    pub fn get_next_batch(&mut self) -> Vec<QueueEntry> {
        let mut batch = Vec::new();

        // Process in priority order: Critical > High > Medium > Low
        let (queue, priority) = if !self.critical_queue.is_empty() {
            (&mut self.critical_queue, SyncPriority::Critical)
        } else if !self.high_queue.is_empty() {
            (&mut self.high_queue, SyncPriority::High)
        } else if !self.medium_queue.is_empty() {
            (&mut self.medium_queue, SyncPriority::Medium)
        } else if !self.low_queue.is_empty() {
            (&mut self.low_queue, SyncPriority::Low)
        } else {
            return batch; // All queues empty
        };

        let batch_size = priority.batch_size();
        let processing_delay = priority.processing_delay();

        // Get entries that are ready to process
        while batch.len() < batch_size && !queue.is_empty() {
            if let Some(entry) = queue.front() {
                // Check if enough time has passed since adding
                if entry.added_at.elapsed() >= processing_delay {
                    let entry = queue.pop_front().unwrap();
                    batch.push(entry);
                } else {
                    // Not ready yet, stop processing this queue
                    break;
                }
            }
        }

        batch
    }

    /// Mark a profile as currently being processed
    pub fn mark_processing(&mut self, npub: &str) {
        self.processing.insert(npub.to_string());
    }

    /// Mark a profile as done processing
    pub fn mark_done(&mut self, npub: &str) {
        self.processing.remove(npub);
        self.last_fetched.insert(npub.to_string(), Instant::now());
    }

}

// Global profile sync queue
static PROFILE_SYNC_QUEUE: LazyLock<Arc<Mutex<ProfileSyncQueue>>> =
    LazyLock::new(|| Arc::new(Mutex::new(ProfileSyncQueue::new())));

/// Background processor that continuously processes the profile sync queue
pub async fn start_profile_sync_processor() {
    let mut last_own_profile_sync = std::time::Instant::now();
    let own_profile_sync_interval = Duration::from_secs(5 * 60); // Sync our own profile every 5 minutes
    
    loop {
        let _cycle_start = std::time::Instant::now();
        
        // Periodically queue our own profile to detect changes from other Nostr apps
        if last_own_profile_sync.elapsed() >= own_profile_sync_interval {
            let state = STATE.lock().await;
            if let Some(own_profile) = state.profiles.iter().find(|p| p.flags.is_mine()) {
                let npub = state.interner.resolve(own_profile.id).unwrap_or("").to_string();
                drop(state);
                
                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.add(npub.clone(), SyncPriority::Low, false);
                drop(queue);
            }
            last_own_profile_sync = std::time::Instant::now();
        }
        
        // Check if we should process (lock scoped to avoid holding across await)
        let (should_wait, batch) = {
            let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();

            // Prevent multiple processors
            if queue.is_processing {
                (true, vec![])
            } else {
                queue.is_processing = true;
                let batch = queue.get_next_batch();

                // Mark all as processing
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
            // No work to do, release lock and sleep
            {
                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.is_processing = false;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        // Process the batch
        for entry in &batch {
            // Fetch the profile
            profile::load_profile(entry.npub.clone()).await;

            // Mark as done
            {
                let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
                queue.mark_done(&entry.npub);
            }

            // Small delay between profiles to avoid overwhelming relays
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Release processing lock
        {
            let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
            queue.is_processing = false;
        }

        // Small delay before next batch
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Queue a single profile for syncing
pub fn queue_profile_sync(npub: String, priority: SyncPriority, force_refresh: bool) {
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    queue.add(npub, priority, force_refresh);
}

/// Queue all profiles for a chat
pub async fn queue_chat_profiles(chat_id: String, is_opening: bool) {
    let state = STATE.lock().await;
    
    // Find the chat
    let chat = match state.get_chat(&chat_id) {
        Some(c) => c,
        None => return,
    };

    // Determine priority based on chat activity
    let base_priority = if is_opening {
        SyncPriority::High
    } else {
        SyncPriority::Medium
    };

    let mut profiles_to_queue = Vec::new();

    // Queue profiles for all participants
    // Note: chat.participants should be kept up-to-date by the MLS event handlers
    for &handle in chat.participants() {
        let member_npub = match state.interner.resolve(handle) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Check if profile exists and has ANY metadata (name, display_name, or avatar)
        // Also check last_updated to see if it was ever fetched from relays
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

    drop(state); // Release state lock before queuing

    // Queue all profiles
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    
    for (npub, priority) in profiles_to_queue {
        queue.add(npub.to_string(), priority, false);
    }
}

/// Force immediate refresh of a profile (for user clicks)
pub fn refresh_profile_now(npub: String) {
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    queue.add(npub, SyncPriority::Critical, true);
}

/// Sync all profiles in the system (replaces old fetchProfiles)
pub async fn sync_all_profiles() {
    let state = STATE.lock().await;
    
    let mut profiles_to_queue = Vec::new();
    
    // Queue all profiles with appropriate priority
    for profile in &state.profiles {
        let npub = match state.interner.resolve(profile.id) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Check if profile has ANY metadata or was ever fetched
        let has_metadata = !profile.name.is_empty() || !profile.display_name.is_empty() || !profile.avatar.is_empty();
        let was_fetched = profile.last_updated > 0;

        let priority = if !has_metadata && !was_fetched {
            SyncPriority::Critical
        } else {
            SyncPriority::Low // Passive refresh for existing profiles
        };

        profiles_to_queue.push((npub, priority));
    }
    
    drop(state); // Release state lock
    
    // Queue all profiles
    let mut queue = PROFILE_SYNC_QUEUE.lock().unwrap();
    
    for (npub, priority) in profiles_to_queue {
        queue.add(npub, priority, false);
    }
}