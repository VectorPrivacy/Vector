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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use web_time::Instant;

use nostr_sdk::prelude::*;

use crate::compact::{secs_from_compact, secs_to_compact};
use crate::profile::Profile;
use crate::state::{nostr_client, my_public_key, STATE};
use crate::traits::emit_event;

// ============================================================================
// SyncPriority
// ============================================================================

/// Priority levels for profile syncing, most urgent first (`Critical < Low`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
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
    /// Matches `ProfileSyncQueue::queued` while this is the npub's live entry; a re-add
    /// supersedes it in place rather than sweeping the lanes for it.
    gen: u64,
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
    /// Each waiting npub's live entry generation and lane. Superseded entries stay in their
    /// lane and are dropped when they reach its front.
    queued: HashMap<String, (u64, SyncPriority)>,
    next_gen: u64,
    processing: HashSet<String>,
    /// Seconds after `epoch` each npub was last fetched; pruned past the longest cache window.
    last_fetched: crate::profile::NpubMap<u32>,
    epoch: Instant,
    done_since_prune: u32,
    is_processing: bool,
}

/// `mark_done` calls between sweeps of `last_fetched`.
const PRUNE_EVERY: u32 = 256;

impl Default for ProfileSyncQueue {
    fn default() -> Self { Self::new() }
}

impl ProfileSyncQueue {
    pub fn new() -> Self {
        Self {
            critical_queue: VecDeque::new(),
            high_queue: VecDeque::new(),
            medium_queue: VecDeque::new(),
            low_queue: VecDeque::new(),
            queued: HashMap::new(),
            next_gen: 0,
            processing: HashSet::new(),
            last_fetched: Default::default(),
            epoch: Instant::now(),
            done_since_prune: 0,
            is_processing: false,
        }
    }

    /// Add a profile to the sync queue.
    pub fn add(&mut self, npub: impl AsRef<str> + Into<String>, priority: SyncPriority, force_refresh: bool) {
        let key = npub.as_ref();
        if self.processing.contains(key) {
            return;
        }
        // An entry already waiting at this priority or higher keeps its place and age: a
        // periodic re-add would otherwise restart the delay and never let a Low entry ripen.
        if self.queued.get(key).is_some_and(|&(_, waiting)| waiting <= priority) {
            return;
        }

        // Check cache window (unless force_refresh)
        if !force_refresh {
            if let Some(&fetched_at) = self.last_fetched.get(key) {
                if u64::from(self.secs().saturating_sub(fetched_at)) < priority.cache_window().as_secs() {
                    return;
                }
            }
        }

        let npub: String = npub.into();
        self.next_gen += 1;
        let gen = self.next_gen;
        self.queued.insert(npub.clone(), (gen, priority));

        let entry = QueueEntry { npub, added_at: Instant::now(), gen };
        // Re-adds while the processor is stalled (offline) would otherwise grow the lanes
        // without bound; one sweep once stale entries outnumber live ones keeps them linear.
        let len = self.critical_queue.len() + self.high_queue.len() + self.medium_queue.len() + self.low_queue.len();
        if len > 2 * self.queued.len() + 64 {
            for lane in [&mut self.critical_queue, &mut self.high_queue, &mut self.medium_queue, &mut self.low_queue] {
                lane.retain(|e| Self::is_live(&self.queued, e));
            }
        }
        match priority {
            SyncPriority::Critical => self.critical_queue.push_back(entry),
            SyncPriority::High => self.high_queue.push_back(entry),
            SyncPriority::Medium => self.medium_queue.push_back(entry),
            SyncPriority::Low => self.low_queue.push_back(entry),
        }
        PROCESSOR_WAKE.notify_one();
    }

    /// How long until [`Self::get_next_batch`] has something to return; `None` when every lane
    /// is empty. Mirrors its lane choice: a waiting higher lane holds the lower ones back.
    fn next_ready_in(&mut self) -> Option<Duration> {
        self.prune_fronts();
        [
            (&self.critical_queue, SyncPriority::Critical),
            (&self.high_queue, SyncPriority::High),
            (&self.medium_queue, SyncPriority::Medium),
            (&self.low_queue, SyncPriority::Low),
        ]
        .into_iter()
        .find_map(|(lane, p)| lane.front().map(|e| p.processing_delay().saturating_sub(e.added_at.elapsed())))
    }

    fn is_live(queued: &HashMap<String, (u64, SyncPriority)>, e: &QueueEntry) -> bool {
        queued.get(&e.npub).is_some_and(|&(gen, _)| gen == e.gen)
    }

    /// Drop superseded entries from the front of every lane, so a non-empty lane has a
    /// live entry first.
    fn prune_fronts(&mut self) {
        for lane in [&mut self.critical_queue, &mut self.high_queue, &mut self.medium_queue, &mut self.low_queue] {
            while lane.front().is_some_and(|e| !Self::is_live(&self.queued, e)) {
                lane.pop_front();
            }
        }
    }

    /// Drop every queued + in-flight entry. Used by `reset_session()` so a
    /// post-reset processor doesn't keep fetching the prior account's contacts.
    pub fn clear(&mut self) {
        self.critical_queue.clear();
        self.high_queue.clear();
        self.medium_queue.clear();
        self.low_queue.clear();
        self.queued.clear();
        self.processing.clear();
        self.last_fetched.clear();
    }

    /// Get the next batch of profiles ready to process (highest priority first).
    pub(crate) fn get_next_batch(&mut self) -> Vec<QueueEntry> {
        let mut batch = Vec::new();
        self.prune_fronts();

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

        while batch.len() < batch_size {
            let Some(entry) = queue.front() else { break };
            if !Self::is_live(&self.queued, entry) {
                queue.pop_front();
                continue;
            }
            if entry.added_at.elapsed() < processing_delay {
                break;
            }
            let entry = queue.pop_front().unwrap();
            self.queued.remove(&entry.npub);
            batch.push(entry);
        }

        batch
    }

    pub fn mark_processing(&mut self, npub: &str) {
        self.processing.insert(npub.to_string());
    }

    pub fn mark_done(&mut self, npub: &str) {
        self.processing.remove(npub);
        let now = self.secs();
        self.last_fetched.insert(npub, now);
        self.done_since_prune += 1;
        if self.done_since_prune >= PRUNE_EVERY {
            self.done_since_prune = 0;
            self.prune_fetched(now);
        }
    }

    /// Forget fetches older than every cache window: they no longer hold anything back.
    fn prune_fetched(&mut self, now: u32) {
        let window = [SyncPriority::Critical, SyncPriority::High, SyncPriority::Medium, SyncPriority::Low]
            .map(|p| p.cache_window().as_secs())
            .into_iter()
            .max()
            .unwrap_or(0);
        self.last_fetched.retain(|&at| u64::from(now.saturating_sub(at)) < window);
    }

    fn secs(&self) -> u32 {
        u32::try_from(self.epoch.elapsed().as_secs()).unwrap_or(u32::MAX)
    }
}

// ============================================================================
// Global queue
// ============================================================================

/// Queued work for THIS account's contacts. The processor loop is
/// process-lifetime and services whichever queue the live session holds, so a
/// swap leaves the prior account's entries behind rather than fetching them.
struct ProfileSyncQueueKey;

fn profile_sync_queue() -> Arc<Mutex<ProfileSyncQueue>> {
    crate::db::current_session().scoped::<ProfileSyncQueueKey, _>()
}

/// Wakes the idle processor when an entry is queued. Process-wide like the processor itself; a
/// stored permit covers an add that lands between its last look and its wait.
static PROCESSOR_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Wake the idle processor to look at the live session's queue: entries added before that
/// session went live woke it against the previous one.
pub(crate) fn wake_processor() {
    #[cfg(test)]
    SESSION_WAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    PROCESSOR_WAKE.notify_one();
}

/// Wakes sent by [`wake_processor`], apart from the ones queue adds send.
#[cfg(test)]
pub(crate) static SESSION_WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

/// The newest kind-0 the relays hold for `pubkey`, or `None` when they hold none.
async fn fetch_newest_metadata(client: &Client, pubkey: PublicKey) -> Result<Option<Metadata>, String> {
    // `Client::fetch_metadata` is gone: fetch the newest kind-0 and parse it.
    client
        .fetch_events(Filter::new().author(pubkey).kind(Kind::Metadata).limit(1))
        .timeout(crate::relay_request_timeout(Duration::from_secs(15)))
        .await
        .map(|events| {
            events
                .into_iter()
                .max_by_key(|e| e.created_at)
                .and_then(|e| Metadata::from_json(&e.content).ok())
        })
        .map_err(|e| e.to_string())
}

/// Our newest kind-0, `Ok(None)` when the relays hold none, `Err` when none of them answered.
/// For read-modify-writes, which must not mistake a dead relay set for an empty profile.
async fn fetch_own_metadata(client: &Client, pubkey: PublicKey) -> Result<Option<Metadata>, String> {
    let filter = Filter::new().author(pubkey).kind(Kind::Metadata).limit(1);
    let timeout = crate::relay_request_timeout(Duration::from_secs(15));
    let events = crate::fetch_answered(client, filter, timeout).await?;
    Ok(crate::newest_replaceable(events).and_then(|e| Metadata::from_json(&e.content).ok()))
}

/// Fetch a profile's metadata and status from relays, update STATE, and
/// notify via EventEmitter + handler callback.
///
/// Returns `true` if the fetch succeeded (even if nothing changed).
pub async fn load_profile(npub: String, handler: &dyn ProfileSyncHandler) -> bool {
    let client = match nostr_client() {
        Some(c) => c,
        None => return false,
    };

    // Session captured for the whole load_profile lifecycle. Relay
    // fetches can sleep multi-second; we re-check before writing back
    // to STATE / DB so a mid-fetch swap doesn't land account A's
    // profile in account B's storage.

    let profile_pubkey = match PublicKey::from_bech32(npub.as_str()) {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    let my_public_key = match my_public_key() {
        Some(pk) => pk,
        None => return false,
    };

    // Grab old status (or create profile if missing)
    let (old_status_title, old_status_purpose, old_status_url): (String, String, String);
    let old_status_emoji_tags: Vec<crate::types::EmojiTag>;
    {
        let mut state = STATE.lock().await;
        match state.get_profile(&npub) {
            Some(p) => {
                old_status_title = p.status_title().to_string();
                old_status_purpose = p.status_purpose().to_string();
                old_status_url = p.status_url().to_string();
                old_status_emoji_tags = p.status_emoji_tags();
            }
            None => {
                state.insert_or_replace_profile(&npub, Profile::new());
                old_status_title = String::new();
                old_status_purpose = String::new();
                old_status_url = String::new();
                old_status_emoji_tags = Vec::new();
            }
        }
    }

    // Fetch status (kind 30315) from relays
    let status_filter = Filter::new()
        .author(profile_pubkey)
        .kind(Kind::from_u16(30315))
        .limit(1);

    let (status_title, status_purpose, status_url, status_emoji_tags) = match client
        .fetch_events(status_filter).timeout(crate::relay_request_timeout(Duration::from_secs(15)))
        .await
    {
        Ok(res) => {
            if !res.is_empty() {
                let status_event = res.first().unwrap();
                (
                    clamp_status(status_event.content.clone()),
                    status_event.tags.first()
                        .and_then(|t| t.content())
                        .unwrap_or_default()
                        .to_string(),
                    String::new(),
                    crate::types::EmojiTag::extract_from_tags(status_event.tags.iter()),
                )
            } else {
                (old_status_title, old_status_purpose, old_status_url, old_status_emoji_tags)
            }
        }
        Err(_) => (old_status_title, old_status_purpose, old_status_url, old_status_emoji_tags),
    };

    let fetch_result = fetch_newest_metadata(&client, profile_pubkey).await;
    if fetch_result.is_ok() {
        let _ = crate::db::profiles::mark_profile_checked(&npub, now_secs());
    }

    match fetch_result {
        Ok(meta) => {
            if let Some(meta) = meta {
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
                        let status_changed = profile.status_title() != status_title.as_str()
                            || profile.status_purpose() != status_purpose.as_str()
                            || profile.status_url() != status_url.as_str()
                            || profile.status_emoji_tags() != status_emoji_tags;
                        profile.set_status(&status_title, &status_purpose, &status_url, &status_emoji_tags);

                        // Update metadata
                        let metadata_changed = profile.from_metadata(meta);

                        // Update timestamp
                        profile.last_updated = secs_to_compact(
                            web_time::SystemTime::now()
                                .duration_since(web_time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                        );

                        (status_changed || metadata_changed,
                         profile.avatar().to_string(),
                         profile.banner().to_string())
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
                        web_time::SystemTime::now()
                            .duration_since(web_time::UNIX_EPOCH)
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
// update_profile — publish metadata to relays
// ============================================================================

/// Update the current user's profile metadata and broadcast to relays.
///
/// Merges the provided fields with the existing profile (empty = keep existing).
/// After successful broadcast, updates STATE and notifies via EventEmitter + handler.
pub async fn update_profile(
    name: String, avatar: String, banner: String, about: String,
    handler: &dyn ProfileSyncHandler,
) -> bool {
    update_profile_inner(name, avatar, banner, about, false, handler).await
}

/// Publish the current user's profile and mark it as a bot (`bot: true` in the metadata). The SDK
/// uses this so every bot it builds is tagged; human clients use [`update_profile`].
pub async fn update_bot_profile(
    name: String, avatar: String, banner: String, about: String,
    handler: &dyn ProfileSyncHandler,
) -> bool {
    update_profile_inner(name, avatar, banner, about, true, handler).await
}

async fn update_profile_inner(
    name: String, avatar: String, banner: String, about: String,
    is_bot: bool,
    handler: &dyn ProfileSyncHandler,
) -> bool {
    let client = match nostr_client() {
        Some(c) => c,
        None => return false,
    };

    let npub = match my_public_key().map(|pk| pk.to_bech32()) {
        Some(Ok(n)) => n,
        _ => return false,
    };

    let local = STATE.lock().await.get_profile(&npub).cloned();
    let meta = match local {
        Some(profile) => own_metadata(&profile, &name, &avatar, &banner, &about, is_bot),
        // An SDK or agent login holds no profile of its own: edit the relay copy, or the publish
        // would drop every field it carries, stream consent included.
        None => {
            let pk = match my_public_key() {
                Some(pk) => pk,
                None => return false,
            };
            match fetch_own_metadata(&client, pk).await {
                Ok(Some(remote)) => edit_metadata(remote, &name, &avatar, &banner, &about, is_bot),
                // Never published (a freshly-created bot): start blank.
                Ok(None) => own_metadata(&Profile::default(), &name, &avatar, &banner, &about, is_bot),
                Err(e) => {
                    crate::log_warn!("[update_profile] couldn't read the current profile: {e}");
                    return false;
                }
            }
        }
    };

    match publish_own_metadata(&client, &npub, meta, handler).await {
        Ok(()) => true,
        Err(e) => {
            crate::log_warn!("[update_profile] {e}");
            false
        }
    }
}

/// Our kind-0 rebuilt from `profile`, with each non-empty argument replacing its field.
///
/// Carries stream consent forward, so editing a name or avatar never revokes it.
pub(crate) fn own_metadata(
    profile: &Profile, name: &str, avatar: &str, banner: &str, about: &str, is_bot: bool,
) -> Metadata {
    let pick = |new: &str, old: &str| if new.is_empty() { old.to_string() } else { new.to_string() };

    let mut meta = Metadata::new().name(pick(name, profile.name()));

    let avatar_url = pick(avatar, profile.avatar());
    if let Ok(url) = Url::parse(&avatar_url) {
        meta = meta.picture(url);
    }
    let banner_url = pick(banner, profile.banner());
    if let Ok(url) = Url::parse(&banner_url) {
        meta = meta.banner(url);
    }

    if !profile.display_name().is_empty() {
        meta = meta.display_name(profile.display_name());
    }
    meta = meta.about(pick(about, profile.about()));

    if let Ok(url) = Url::parse(profile.website()) {
        meta = meta.website(url);
    }
    if !profile.nip05().is_empty() {
        meta = meta.nip05(profile.nip05());
    }
    if !profile.lud06().is_empty() {
        meta = meta.lud06(profile.lud06());
    }
    if !profile.lud16().is_empty() {
        meta = meta.lud16(profile.lud16());
    }

    // SDK-built bots carry `bot: true` so clients can badge them; human clients never set it.
    if is_bot {
        meta = meta.custom_field("bot", true);
    }
    crate::profile::with_stream_consent(meta, profile.flags.is_stream_consent())
}

/// `meta` with each non-empty argument replacing its field; every other key stands.
fn edit_metadata(mut meta: Metadata, name: &str, avatar: &str, banner: &str, about: &str, is_bot: bool) -> Metadata {
    if !name.is_empty() {
        meta.name = Some(name.to_string());
    }
    if let Ok(url) = Url::parse(avatar) {
        meta.picture = Some(url.to_string());
    }
    if let Ok(url) = Url::parse(banner) {
        meta.banner = Some(url.to_string());
    }
    if !about.is_empty() {
        meta.about = Some(about.to_string());
    }
    if is_bot {
        meta.custom.insert("bot".to_string(), serde_json::Value::Bool(true));
    }
    meta
}

/// Sign and broadcast our kind-0, then fold it into STATE, the UI and the DB.
async fn publish_own_metadata(
    client: &Client, npub: &str, meta: Metadata, handler: &dyn ProfileSyncHandler,
) -> Result<(), String> {
    let metadata_json = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
    let metadata_event = EventBuilder::new(Kind::Metadata, metadata_json)
        .tag(Tag::custom("client", vec!["vector"]));
    let event = crate::sign_builder(metadata_event).await?;

    // First-ACK so the UI updates as soon as the fastest relay responds.
    crate::inbox_relays::send_event_pool_first_ok(client, &event)
        .await
        .map_err(|e| format!("relay broadcast failed: {e}"))?;

    apply_own_metadata(npub, meta, handler).await
}

async fn apply_own_metadata(npub: &str, meta: Metadata, handler: &dyn ProfileSyncHandler) -> Result<(), String> {
    let (slim, avatar_url, banner_url) = {
        let mut state = STATE.lock().await;
        // Creates the entry if this identity had none yet (a fresh account is interned here).
        let mut profile = state.get_profile(npub).cloned().unwrap_or_default();
        profile.from_metadata(meta);
        profile.flags.set_mine(true);
        let urls = (profile.avatar().to_string(), profile.banner().to_string());
        state.insert_or_replace_profile(npub, profile);
        let slim = state
            .interner
            .lookup(npub)
            .and_then(|id| state.serialize_profile(id))
            .ok_or("own profile missing after insert")?;
        (slim, urls.0, urls.1)
    };
    emit_event("profile_update", &slim);
    handler.on_profile_fetched(&slim, &avatar_url, &banner_url);
    Ok(())
}

/// What a consent change does, decided from the relay copy (`None` = the relays hold none).
#[derive(Debug, PartialEq)]
pub(crate) enum ConsentPlan {
    /// The relays already say it: adopt their copy, publish nothing.
    Unchanged(Metadata),
    Publish(Metadata),
}

/// A read-modify-write of the relay copy, so every field another client wrote survives. With no
/// relay copy, the local profile is carried, and refused when there is none to carry: a fresh
/// device must never publish a blank profile over the real one.
pub(crate) fn consent_plan(fetched: Option<Metadata>, local: &Profile, on: bool) -> Result<ConsentPlan, String> {
    match fetched {
        Some(remote) if crate::profile::metadata_stream_consent(&remote) == on => Ok(ConsentPlan::Unchanged(remote)),
        Some(remote) => Ok(ConsentPlan::Publish(crate::profile::with_stream_consent(remote, on))),
        None => {
            if local.name().is_empty() && local.display_name().is_empty()
                && local.about().is_empty() && local.avatar().is_empty()
            {
                return Err("Couldn't find your profile on your relays".to_string());
            }
            let meta = own_metadata(local, "", "", "", "", local.flags.is_bot());
            Ok(ConsentPlan::Publish(crate::profile::with_stream_consent(meta, on)))
        }
    }
}

/// Publish our kind-0 with stream consent set to `on`; returns the value now in effect.
///
/// Refused when no relay answers the read, so a fresh device never clobbers the real profile.
pub async fn set_stream_consent(on: bool, handler: &dyn ProfileSyncHandler) -> Result<bool, String> {
    let client = nostr_client().ok_or("Not connected")?;
    let my_public_key = my_public_key().ok_or("No active account")?;
    let npub = my_public_key.to_bech32().map_err(|e| e.to_string())?;

    let fetched = fetch_own_metadata(&client, my_public_key)
        .await
        .map_err(|_| "Couldn't read your profile from your relays".to_string())?;
    let local = STATE.lock().await.get_profile(&npub).cloned().unwrap_or_default();
    let plan = consent_plan(fetched, &local, on)?;

    if !crate::db::session_is_live() {
        return Err("account changed during the operation".to_string());
    }
    match plan {
        ConsentPlan::Unchanged(remote) => apply_own_metadata(&npub, remote, handler).await?,
        ConsentPlan::Publish(meta) => publish_own_metadata(&client, &npub, meta, handler).await?,
    }
    Ok(on)
}

// ============================================================================
// update_status — publish status to relays
// ============================================================================

/// Update the current user's status (kind 30315) and broadcast to relays.
///
/// Status length cap in Unicode scalar characters, enforced on BOTH sides:
/// our own publishes and every stored inbound status. Characters, not bytes —
/// a byte cap would chop emoji-heavy statuses to a third of what text gets.
pub const STATUS_MAX_CHARS: usize = 120;

/// Truncate a status to [`STATUS_MAX_CHARS`] on a character boundary.
fn clamp_status(s: String) -> String {
    if s.chars().count() <= STATUS_MAX_CHARS {
        s
    } else {
        s.chars().take(STATUS_MAX_CHARS).collect()
    }
}

/// Status is ephemeral — updated in STATE + frontend but not persisted to DB.
/// (Re-fetched from relays on next `load_profile` call.)
pub async fn update_status(status: String) -> bool {
    let status = clamp_status(status);
    let client = match nostr_client() {
        Some(c) => c,
        None => return false,
    };

    let my_public_key = match my_public_key() {
        Some(pk) => pk,
        None => return false,
    };

    // Build and sign kind 30315 status event. `:shortcode:`s from the user's
    // equipped packs ride along as NIP-30 tags so other clients render them.
    let emoji_tags = crate::emoji_packs::resolve_outbound_emoji_tags(&status);
    let mut status_builder = EventBuilder::new(Kind::from_u16(30315), status.as_str())
        .tag(Tag::custom("d", vec!["general"]));
    for et in &emoji_tags {
        status_builder = status_builder.tag(Tag::custom("emoji", [et.shortcode.clone(), et.url.clone()]));
    }

    let Ok(event) = crate::sign_builder(status_builder).await else {
        return false;
    };

    match crate::inbox_relays::send_event_pool_first_ok(&client, &event).await {
        Ok(_) => {
            let mut state = STATE.lock().await;
            let npub = match my_public_key.to_bech32() {
                Ok(n) => n,
                Err(_) => return false,
            };
            let id = match state.interner.lookup(&npub) {
                Some(id) => id,
                None => return false,
            };
            {
                let profile = match state.get_profile_mut_by_id(id) {
                    Some(p) => p,
                    None => return false,
                };
                profile.set_status_title(&status);
                profile.set_status_purpose("general");
                profile.set_status_emoji_tags(&emoji_tags);
            }

            let slim = state.serialize_profile(id).unwrap();
            // Persist NOW: without this the new status (and its emoji tags)
            // survives a reboot only if a self-profile sync happens to run
            // before the app closes.
            let _ = crate::db::profiles::set_profile(&slim);
            emit_event("profile_update", &slim);
            true
        }
        Err(_) => false,
    }
}

// ============================================================================
// block / unblock / nickname / blocked list
// ============================================================================

/// Block a user by npub. DM events from blocked users are dropped after decryption.
/// Group messages are stored but filtered in the UI.
///
/// Returns `false` if trying to block yourself or if the profile can't be found.
pub async fn block_user(npub: String, handler: &dyn ProfileSyncHandler) -> bool {
    // Prevent blocking yourself
    if let Some(my_pk) = my_public_key() {
        if my_pk.to_bech32().ok().as_deref() == Some(npub.as_str()) {
            return false;
        }
    }

    let mut state = STATE.lock().await;

    // Create profile if it doesn't exist (can block someone with no prior contact)
    if state.interner.lookup(&npub).is_none() {
        state.insert_or_replace_profile(&npub, Profile::new());
    }

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.flags.set_blocked(true);
        }
        let slim = state.serialize_profile(id).unwrap();
        drop(state);
        emit_event("profile_update", &slim);
        handler.on_profile_fetched(&slim, "", "");
        crate::push::controls_changed();
        true
    } else {
        false
    }
}

/// Unblock a user by npub.
pub async fn unblock_user(npub: String, handler: &dyn ProfileSyncHandler) -> bool {
    let mut state = STATE.lock().await;

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.flags.set_blocked(false);
        }
        let slim = state.serialize_profile(id).unwrap();
        drop(state);
        emit_event("profile_update", &slim);
        handler.on_profile_fetched(&slim, "", "");
        crate::push::controls_changed();
        true
    } else {
        false
    }
}

/// Get all blocked profiles.
pub async fn get_blocked_users() -> Vec<crate::SlimProfile> {
    let state = STATE.lock().await;
    state.profiles.iter()
        .filter(|p| p.flags.is_blocked())
        .filter_map(|p| state.serialize_profile(p.id))
        .collect()
}

/// Set a nickname for a profile.
pub async fn set_nickname(npub: String, nickname: String, handler: &dyn ProfileSyncHandler) -> bool {
    let mut state = STATE.lock().await;

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.set_nickname(&nickname);
        }
        let slim = state.serialize_profile(id).unwrap();
        drop(state);
        emit_event("profile_nick_changed", &serde_json::json!({
            "profile_id": &npub,
            "value": &slim.nickname
        }));
        handler.on_profile_fetched(&slim, "", "");
        true
    } else {
        false
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

                let owner = profile_sync_queue();
                let mut queue = owner.lock().unwrap();
                queue.add(npub, SyncPriority::Low, false);
            }
            last_own_profile_sync = Instant::now();
        }

        // Get next batch (lock scoped)
        let (should_wait, batch) = {
            let owner = profile_sync_queue();
            let mut queue = owner.lock().unwrap();

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
            crate::rt::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        if batch.is_empty() {
            let ready_in = {
                let owner = profile_sync_queue();
                let mut queue = owner.lock().unwrap();
                queue.is_processing = false;
                queue.next_ready_in()
            };
            // Sleep until the next entry ripens or the own-profile check is due, unless an add
            // comes first, so an idle processor costs no wakeups.
            let own_due = own_profile_sync_interval.saturating_sub(last_own_profile_sync.elapsed());
            let wait = ready_in.map_or(own_due, |r| r.min(own_due)).max(Duration::from_millis(50));
            let _ = crate::rt::time::timeout(wait, PROCESSOR_WAKE.notified()).await;
            continue;
        }

        // Session captured per-batch so a swap aborts the loop before
        // account A's queue work lands in account B's DB. The next
        // outer-loop iteration picks up the new session's queue cleanly.

        for entry in &batch {
            load_profile(entry.npub.clone(), handler.as_ref()).await;

            {
                let owner = profile_sync_queue();
                let mut queue = owner.lock().unwrap();
                queue.mark_done(&entry.npub);
            }

            crate::rt::time::sleep(Duration::from_millis(100)).await;
        }

        // Release processing lock
        {
            let owner = profile_sync_queue();
            let mut queue = owner.lock().unwrap();
            queue.is_processing = false;
        }

        crate::rt::time::sleep(Duration::from_millis(500)).await;
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Queue a single profile for syncing.
pub fn queue_profile_sync(npub: String, priority: SyncPriority, force_refresh: bool) {
    let owner = profile_sync_queue();
    let mut queue = owner.lock().unwrap();
    queue.add(npub, priority, force_refresh);
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How long a profile's last check stays good enough for one that is merely on screen.
const ON_SCREEN_FRESH: Duration = Duration::from_secs(6 * 60 * 60);

/// What a profile on screen needs: a fetch now when we know nothing of it, a slow refresh when
/// its last check is older than [`ON_SCREEN_FRESH`], otherwise nothing.
fn on_screen_need(profile: Option<&Profile>, now: u64) -> Option<SyncPriority> {
    let Some(p) = profile else { return Some(SyncPriority::Critical) };
    if p.flags.is_mine() {
        return None;
    }
    let checked = secs_from_compact(p.last_updated);
    if checked == 0 {
        let known = !p.name().is_empty() || !p.display_name().is_empty() || !p.avatar().is_empty();
        return Some(if known { SyncPriority::Medium } else { SyncPriority::Critical });
    }
    (now.saturating_sub(checked) >= ON_SCREEN_FRESH.as_secs()).then_some(SyncPriority::Medium)
}

/// Queue the profiles the UI has on screen (or nearly) whose last check has gone stale. Called
/// on every view, so it costs a lookup per npub and a fetch only for those that need one.
pub async fn queue_visible_profiles(npubs: Vec<String>) {
    const MAX_PER_CALL: usize = 200;
    let now = now_secs();
    let picks: Vec<(String, SyncPriority)> = {
        let state = STATE.lock().await;
        npubs
            .into_iter()
            .take(MAX_PER_CALL)
            .filter(|n| n.starts_with("npub1"))
            .filter_map(|npub| on_screen_need(state.get_profile(&npub), now).map(|p| (npub, p)))
            .collect()
    };
    if picks.is_empty() {
        return;
    }
    let owner = profile_sync_queue();
    let mut queue = owner.lock().unwrap();
    for (npub, priority) in picks {
        queue.add(npub, priority, false);
    }
}

/// Queue all profiles for a chat. A community's members refresh as they come on screen
/// ([`queue_visible_profiles`]), so here a community only fetches the ones we know nothing of.
pub async fn queue_chat_profiles(chat_id: String, is_opening: bool) {
    let state = STATE.lock().await;

    let chat = match state.get_chat(&chat_id) {
        Some(c) => c,
        None => return,
    };

    let is_community = chat.is_community();
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
                let has_data = !p.name().is_empty() || !p.display_name().is_empty() || !p.avatar().is_empty();
                let was_fetched = p.last_updated > 0;
                has_data || was_fetched
            })
            .unwrap_or(false);

        let priority = if !has_metadata {
            SyncPriority::Critical
        } else if is_community {
            continue;
        } else {
            base_priority
        };

        profiles_to_queue.push((member_npub, priority));
    }

    drop(state);

    let owner = profile_sync_queue();
    let mut queue = owner.lock().unwrap();
    for (npub, priority) in profiles_to_queue {
        queue.add(npub, priority, false);
    }
}

/// Force immediate refresh of a profile (for user clicks).
pub fn refresh_profile_now(npub: String) {
    let owner = profile_sync_queue();
    let mut queue = owner.lock().unwrap();
    queue.add(npub, SyncPriority::Critical, true);
}

/// Queue every profile that has never been fetched. Known profiles refresh when their chat
/// opens or they come on screen ([`queue_visible_profiles`]).
pub async fn sync_all_profiles() {
    let state = STATE.lock().await;
    let owner = profile_sync_queue();
    let mut queue = owner.lock().unwrap();

    for profile in &state.profiles {
        let has_metadata = !profile.name().is_empty() || !profile.display_name().is_empty() || !profile.avatar().is_empty();
        if has_metadata || profile.last_updated > 0 {
            continue;
        }
        if let Some(npub) = state.interner.resolve(profile.id) {
            queue.add(npub, SyncPriority::Critical, false);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod status_clamp_tests {
    use super::*;

    #[test]
    fn a_status_clamps_at_120_characters_not_bytes() {
        assert_eq!(clamp_status("hi".to_string()), "hi");
        let exact: String = "a".repeat(STATUS_MAX_CHARS);
        assert_eq!(clamp_status(exact.clone()), exact, "at the cap is untouched");
        let long = "b".repeat(10_000);
        assert_eq!(clamp_status(long).chars().count(), STATUS_MAX_CHARS);
        // Characters, not bytes: 120 four-byte emoji survive whole.
        let emoji: String = "\u{1F980}".repeat(STATUS_MAX_CHARS);
        let clamped = clamp_status(format!("{emoji}overflow"));
        assert_eq!(clamped.chars().count(), STATUS_MAX_CHARS);
        assert_eq!(clamped, emoji, "truncation lands on a character boundary");
    }
}

#[cfg(test)]
mod on_screen_tests {
    use super::*;

    #[test]
    fn an_on_screen_profile_is_fetched_only_when_unknown_or_stale() {
        let now = 1_900_000_000;
        assert_eq!(on_screen_need(None, now), Some(SyncPriority::Critical));
        let mut p = Profile::new();
        assert_eq!(on_screen_need(Some(&p), now), Some(SyncPriority::Critical));
        p.set_name("kitty");
        assert_eq!(on_screen_need(Some(&p), now), Some(SyncPriority::Medium), "named but never checked");
        p.last_updated = secs_to_compact(now - 60);
        assert_eq!(on_screen_need(Some(&p), now), None, "checked a minute ago");
        p.last_updated = secs_to_compact(now - ON_SCREEN_FRESH.as_secs());
        assert_eq!(on_screen_need(Some(&p), now), Some(SyncPriority::Medium), "stale");
        p.flags.set_mine(true);
        assert_eq!(on_screen_need(Some(&p), now), None, "our own syncs on its own clock");
    }
}

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

    impl ProfileSyncQueue {
        fn push_raw(&mut self, priority: SyncPriority, npub: &str, added_at: Instant) {
            self.next_gen += 1;
            self.queued.insert(npub.to_string(), (self.next_gen, priority));
            let entry = QueueEntry { npub: npub.to_string(), added_at, gen: self.next_gen };
            match priority {
                SyncPriority::Critical => self.critical_queue.push_back(entry),
                SyncPriority::High => self.high_queue.push_back(entry),
                SyncPriority::Medium => self.medium_queue.push_back(entry),
                SyncPriority::Low => self.low_queue.push_back(entry),
            }
        }

        fn live(&self, lane: &VecDeque<QueueEntry>) -> Vec<String> {
            lane.iter().filter(|e| Self::is_live(&self.queued, e)).map(|e| e.npub.clone()).collect()
        }
    }

    #[test]
    fn next_ready_in_follows_the_lane_the_batch_would_take() {
        let mut queue = ProfileSyncQueue::new();
        assert_eq!(queue.next_ready_in(), None, "empty: nothing to wake for");
        let now = Instant::now();
        queue.push_raw(SyncPriority::Low, "npub1low", now - Duration::from_secs(10 * 60));
        assert_eq!(queue.next_ready_in(), Some(Duration::ZERO), "a ripe low entry is due now");
        queue.push_raw(SyncPriority::High, "npub1high", now);
        let wait = queue.next_ready_in().unwrap();
        assert!(wait > Duration::from_secs(4) && wait <= Duration::from_secs(5), "a waiting high entry holds the low lane back: {wait:?}");
        assert!(queue.get_next_batch().is_empty());
        queue.add("npub1high", SyncPriority::Critical, true);
        assert_eq!(queue.next_ready_in(), Some(Duration::ZERO), "the superseded high entry no longer counts");
    }

    #[test]
    fn re_adds_while_stalled_stay_bounded() {
        let mut queue = ProfileSyncQueue::new();
        for _ in 0..100 {
            for i in 0..50 {
                queue.add(format!("npub1{i}"), SyncPriority::Low, true);
            }
        }
        assert_eq!(queue.live(&queue.low_queue).len(), 50);
        assert!(queue.low_queue.len() <= 2 * 50 + 64 + 1, "{} entries for 50 profiles", queue.low_queue.len());
    }

    #[test]
    fn queue_add_and_dedup() {
        let mut queue = ProfileSyncQueue::new();

        queue.add("npub1alice", SyncPriority::Low, false);
        queue.add("npub1alice", SyncPriority::High, false);

        // Should be in High queue only (deduped from Low)
        assert!(queue.live(&queue.low_queue).is_empty());
        assert_eq!(queue.live(&queue.high_queue), ["npub1alice"]);
    }

    #[test]
    fn queue_skips_processing() {
        let mut queue = ProfileSyncQueue::new();
        queue.mark_processing("npub1bob");

        queue.add("npub1bob", SyncPriority::Critical, false);
        assert!(queue.critical_queue.is_empty(), "should skip profiles being processed");
    }

    #[test]
    fn queue_cache_window_skips() {
        let mut queue = ProfileSyncQueue::new();

        // Mark as recently fetched
        queue.mark_done("npub1carol");

        // Try to add with Low priority (24h cache window) — should skip
        queue.add("npub1carol", SyncPriority::Low, false);
        assert!(queue.low_queue.is_empty(), "should skip within cache window");

        // Force refresh should bypass cache
        queue.add("npub1carol", SyncPriority::Low, true);
        assert_eq!(queue.low_queue.len(), 1, "force_refresh should bypass cache");
    }

    #[test]
    fn queue_critical_skips_cache() {
        let mut queue = ProfileSyncQueue::new();

        // Critical has 0s cache window — always fetches
        queue.mark_done("npub1dave");
        queue.add("npub1dave", SyncPriority::Critical, false);
        assert_eq!(queue.critical_queue.len(), 1, "Critical should always fetch");
    }

    #[test]
    fn re_adding_moves_an_entry_between_lanes() {
        let mut queue = ProfileSyncQueue::new();
        queue.add("npub1a", SyncPriority::Low, true);
        queue.add("npub1b", SyncPriority::Low, true);
        queue.add("npub1a", SyncPriority::Critical, true);
        assert_eq!(queue.live(&queue.low_queue), ["npub1b"]);
        assert_eq!(queue.live(&queue.critical_queue), ["npub1a"]);
        let batch = queue.get_next_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].npub, "npub1a");
        // Its superseded Low entry never surfaces, even once the Low lane's delay passes.
        queue.critical_queue.clear();
        queue.add("npub1c", SyncPriority::Low, true);
        assert_eq!(queue.live(&queue.low_queue), ["npub1b", "npub1c"]);
    }

    #[test]
    fn a_periodic_re_add_keeps_the_waiting_entry() {
        let mut queue = ProfileSyncQueue::new();
        queue.push_raw(SyncPriority::Low, "npub1low", Instant::now() - Duration::from_secs(10 * 60));
        queue.push_raw(SyncPriority::High, "npub1high", Instant::now() - Duration::from_secs(10));
        queue.add("npub1low", SyncPriority::Low, false);
        queue.add("npub1high", SyncPriority::Low, false);
        assert_eq!(queue.live(&queue.high_queue), ["npub1high"], "a lower re-add never demotes");
        assert_eq!(queue.get_next_batch().len(), 1);
        let batch = queue.get_next_batch();
        assert_eq!(batch.len(), 1, "the ripe Low entry keeps its age");
        assert_eq!(batch[0].npub, "npub1low");
    }

    #[test]
    fn get_next_batch_priority_order() {
        let mut queue = ProfileSyncQueue::new();

        // Add to Low and Critical queues
        queue.push_raw(SyncPriority::Low, "npub1low", Instant::now() - Duration::from_secs(600));
        queue.push_raw(SyncPriority::Critical, "npub1critical", Instant::now());

        let batch = queue.get_next_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].npub, "npub1critical", "Critical should process before Low");
    }

    #[test]
    fn get_next_batch_respects_delay() {
        let mut queue = ProfileSyncQueue::new();

        // Add a High priority entry just now (5s delay required)
        queue.push_raw(SyncPriority::High, "npub1new", Instant::now());

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
        assert!(queue.last_fetched.contains("npub1eve"));
    }

    #[test]
    fn fetches_older_than_every_window_are_forgotten() {
        let mut queue = ProfileSyncQueue::new();
        let day = 24 * 60 * 60;
        let now = 3 * day;
        queue.last_fetched.insert("npub1stale", now - day);
        queue.last_fetched.insert("npub1edge", now - day + 1);
        queue.last_fetched.insert("npub1fresh", now - 10);
        queue.prune_fetched(now);
        assert!(!queue.last_fetched.contains("npub1stale"), "a day old gates nothing");
        assert!(queue.last_fetched.contains("npub1edge"), "inside the Low window it still gates a Low re-add");
        assert!(queue.last_fetched.contains("npub1fresh"));
    }

    #[test]
    fn mark_done_sweeps_every_so_often() {
        let mut queue = ProfileSyncQueue::new();
        for i in 0..PRUNE_EVERY - 1 {
            queue.mark_done(&format!("npub1done{i}"));
        }
        assert_eq!(queue.done_since_prune, PRUNE_EVERY - 1);
        queue.last_fetched.insert("npub1future", u32::MAX);
        queue.mark_done("npub1last");
        assert_eq!(queue.done_since_prune, 0, "the sweep ran");
        assert_eq!(queue.last_fetched.len(), PRUNE_EVERY as usize + 1, "nothing recent was dropped");
    }

    #[test]
    fn the_cache_window_reads_whole_seconds_since_the_fetch() {
        use nostr_sdk::prelude::{Keys, ToBech32};
        let npub = Keys::generate().public_key().to_bech32().unwrap();
        let mut queue = ProfileSyncQueue::new();
        let now = queue.secs();
        queue.last_fetched.insert(&npub, now);
        queue.add(npub.clone(), SyncPriority::High, false);
        assert!(queue.high_queue.is_empty(), "a canonical npub is found by its key bytes");

        let mut queue = ProfileSyncQueue::new();
        queue.epoch = queue.epoch.checked_sub(Duration::from_secs(400)).unwrap_or(queue.epoch);
        if queue.secs() >= 400 {
            queue.last_fetched.insert(&npub, queue.secs() - 301);
            queue.add(npub.clone(), SyncPriority::Medium, false);
            assert!(queue.medium_queue.is_empty(), "301 s is inside Medium's 30 min");
            queue.add(npub.clone(), SyncPriority::High, false);
            assert_eq!(queue.live(&queue.high_queue), [npub.as_str()], "and past High's 5 min");
        }
    }

    fn consenting_profile() -> Profile {
        let mut p = Profile::new();
        p.from_metadata(
            Metadata::from_json(r#"{"name":"old","about":"bio","picture":"https://x/a.png","lud16":"a@b.c","stream_consent":true}"#).unwrap(),
        );
        p
    }

    #[test]
    fn an_own_profile_edit_carries_stream_consent_forward() {
        let p = consenting_profile();
        assert!(p.flags.is_stream_consent());

        let meta = own_metadata(&p, "new", "", "", "", false);
        assert!(crate::profile::metadata_stream_consent(&meta), "a rename keeps consent");
        assert_eq!(meta.name.as_deref(), Some("new"));
        assert_eq!(meta.about.as_deref(), Some("bio"));
        assert_eq!(meta.picture.as_deref(), Some("https://x/a.png"));
        assert_eq!(meta.lud16.as_deref(), Some("a@b.c"));
        assert!(!meta.custom.contains_key("bot"));

        let mut republished = p.clone();
        assert!(!republished.from_metadata(own_metadata(&p, "", "", "", "", false)), "a no-op edit changes nothing");
        assert!(republished.flags.is_stream_consent());
    }

    #[test]
    fn an_own_profile_without_consent_publishes_no_key() {
        let mut p = consenting_profile();
        p.flags.set_stream_consent(false);
        let meta = own_metadata(&p, "", "", "", "about", true);
        assert!(!meta.custom.contains_key(crate::profile::STREAM_CONSENT_KEY));
        assert_eq!(meta.custom.get("bot"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn a_fresh_device_with_no_relay_copy_never_publishes_a_blank_profile() {
        assert!(consent_plan(None, &Profile::default(), true).is_err());
        // With a profile of its own to carry, it publishes that.
        let mut p = consenting_profile();
        p.flags.set_stream_consent(false);
        let Ok(ConsentPlan::Publish(meta)) = consent_plan(None, &p, true) else { panic!("publishes the local copy") };
        assert!(crate::profile::metadata_stream_consent(&meta));
        assert_eq!(meta.name.as_deref(), Some("old"));
    }

    #[test]
    fn a_consent_change_keeps_every_key_the_relay_copy_holds() {
        let remote = Metadata::from_json(r#"{"name":"r","pronouns":"they","bot":true}"#).unwrap();
        let Ok(ConsentPlan::Publish(meta)) = consent_plan(Some(remote), &Profile::default(), true) else {
            panic!("a change is published")
        };
        assert!(crate::profile::metadata_stream_consent(&meta));
        assert_eq!(meta.custom.get("pronouns"), Some(&serde_json::json!("they")));
        assert_eq!(meta.custom.get("bot"), Some(&serde_json::json!(true)));
        assert_eq!(meta.name.as_deref(), Some("r"), "the relay copy wins over the local one");

        let withdrawn = consent_plan(Some(meta), &consenting_profile(), false).unwrap();
        let ConsentPlan::Publish(off) = withdrawn else { panic!("withdrawing is published") };
        assert!(!off.custom.contains_key(crate::profile::STREAM_CONSENT_KEY), "off is the absent key");
        assert_eq!(off.custom.get("pronouns"), Some(&serde_json::json!("they")));
    }

    #[test]
    fn a_relay_copy_that_already_says_it_publishes_nothing() {
        let remote = Metadata::from_json(r#"{"name":"r","stream_consent":true}"#).unwrap();
        assert!(matches!(consent_plan(Some(remote.clone()), &Profile::default(), true), Ok(ConsentPlan::Unchanged(_))));
        let bare = Metadata::from_json(r#"{"name":"r"}"#).unwrap();
        assert!(matches!(consent_plan(Some(bare), &consenting_profile(), false), Ok(ConsentPlan::Unchanged(_))));
    }

    #[test]
    fn an_sdk_edit_of_the_relay_copy_keeps_its_other_fields() {
        let remote = Metadata::from_json(
            r#"{"name":"old","display_name":"Old","nip05":"a@b.c","lud16":"z@b.c","stream_consent":true,"pronouns":"they"}"#,
        )
        .unwrap();
        let meta = edit_metadata(remote, "new", "https://x/a.png", "", "", true);
        assert_eq!(meta.name.as_deref(), Some("new"));
        assert_eq!(meta.picture.as_deref(), Some("https://x/a.png"));
        assert_eq!(meta.display_name.as_deref(), Some("Old"));
        assert_eq!(meta.nip05.as_deref(), Some("a@b.c"));
        assert_eq!(meta.lud16.as_deref(), Some("z@b.c"));
        assert!(crate::profile::metadata_stream_consent(&meta), "consent is not revoked by an edit");
        assert_eq!(meta.custom.get("pronouns"), Some(&serde_json::json!("they")));
        assert_eq!(meta.custom.get("bot"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn noop_handler_compiles() {
        let handler = NoOpProfileSyncHandler;
        let slim = crate::SlimProfile::default();
        handler.on_profile_fetched(&slim, "", "");
    }
}

#[cfg(test)]
mod processor_bench {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Thread wakeups while the queue sits empty, and how long a user-clicked refresh waits
    /// before the processor picks it up. `#[ignore]`d: a measurement, run deliberately:
    ///   cargo test --release -p vector-core --lib bench_profile_processor -- --ignored --nocapture
    #[test]
    #[ignore = "benchmark, not an assertion"]
    fn bench_profile_processor() {
        static UNPARKS: AtomicUsize = AtomicUsize::new(0);
        const IDLE: Duration = Duration::from_secs(20);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .on_thread_unpark(|| {
                UNPARKS.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::spawn(start_profile_sync_processor(Arc::new(NoOpProfileSyncHandler)));
            tokio::time::sleep(Duration::from_millis(50)).await;
            let before = UNPARKS.load(Ordering::Relaxed);
            tokio::time::sleep(IDLE).await;
            let woke = UNPARKS.load(Ordering::Relaxed) - before;
            println!("idle wakeups: {woke} in {IDLE:?} ({:.2}/min)", woke as f64 * 60.0 / IDLE.as_secs_f64());

            let mut waits = Vec::new();
            for i in 0..20u64 {
                tokio::time::sleep(Duration::from_millis(700 + i * 37)).await;
                let npub = format!("npub1bench{i}");
                let t = Instant::now();
                refresh_profile_now(npub.clone());
                while !profile_sync_queue().lock().unwrap().last_fetched.contains(&npub) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                waits.push(t.elapsed().as_secs_f64() * 1e3);
            }
            waits.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "critical refresh pickup: median {:.1} ms, max {:.1} ms",
                waits[waits.len() / 2],
                waits[waits.len() - 1]
            );
        });
    }
}
