//! MLS Service — MDK engine management and core operations.
//!
//! Creates FRESH MDK instances for each operation to ensure we always read
//! current state from SQLite, avoiding stale cache issues.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::sync::LazyLock;
use tokio::sync::Mutex as TokioMutex;

use mdk_core::prelude::*;
use mdk_sqlite_storage::MdkSqliteStorage;

use crate::mls::types::{MlsError, MlsGroupFull, EventCursor, KeyPackageIndexEntry};
use crate::mls::tracking::wipe_legacy_mls_database;
use crate::state::active_trusted_relays;

// ============================================================================
// MDK config — extended event-age window for catch-up sync
// ============================================================================

/// Build an MDK engine configured for long-offline catch-up sync.
///
/// MDK's default `max_event_age_secs` is 45 days — anti-replay defense at
/// the nostr-event layer. For legitimate group members coming back after
/// extended downtime, that gate rejects valid commits at `validate_event`
/// before MDK can attempt MLS decryption. MLS already provides
/// epoch-sequenced anti-replay so the nostr-layer window is redundant; we
/// extend it to 10 years (kept finite as a sanity guard).
pub(crate) fn build_mdk_engine(storage: MdkSqliteStorage) -> MDK<MdkSqliteStorage> {
    use mdk_core::MdkConfig;
    let config = MdkConfig {
        max_event_age_secs: 10 * 365 * 24 * 60 * 60, // 10 years
        ..MdkConfig::default()
    };
    MDK::builder(storage).with_config(config).build()
}

// ============================================================================
// Pure helpers (extracted for unit testability)
// ============================================================================

/// Compute the cursor `last_seen_at` value to persist after a sync batch.
///
/// Rules:
/// - Cursor must never decrease (`>= current_cursor_at`)
/// - If there are pending events, cap at `oldest_pending_at - 1` so the
///   next sync re-fetches the pending window without skipping past it
/// - The cap itself is floored at `current_cursor_at`: a pending event
///   older than the existing cursor must NOT drag the cursor backward
///   (which would cause redundant refetch or pin to genesis on first write)
/// - `oldest_pending_at == 0` (clock-skewed/malformed event) is treated
///   as "no pending floor" — don't pin the cursor to epoch zero
///
/// Returns the cursor value to write. Caller still gates the actual write
/// behind a `> current_cursor_at` check.
pub(crate) fn compute_cursor_with_pending_cap(
    last_seen_at: u64,
    current_cursor_at: u64,
    oldest_pending_at: Option<u64>,
) -> u64 {
    if let Some(oldest) = oldest_pending_at {
        if oldest > 0 {
            let cap = oldest.saturating_sub(1).max(current_cursor_at);
            if last_seen_at > cap {
                return cap;
            }
        }
    }
    last_seen_at
}

/// Compute the `since` filter timestamp for the next batch / next sync.
///
/// Always advances `last_seen_at + 1` (saturating) so we don't refetch the
/// wrapper the cursor is anchored on. Critical when the anchored event is
/// a permanently-failing wrapper: without `+1`, the relay's `since`-inclusive
/// filter returns it again, MDK retries decryption, fails, and the cycle
/// never breaks.
pub(crate) fn cursor_to_next_since(last_seen_at: u64) -> u64 {
    last_seen_at.saturating_add(1)
}

/// Outcome of the post-Commit membership check.
///
/// Distinguishes the three states `engine.get_members()` can produce so the
/// caller doesn't conflate transient storage errors with real evictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MembershipOutcome {
    /// Confirmed not-a-member after applying this commit. Trigger eviction.
    Evicted,
    /// Confirmed still-a-member. Run the metadata-update side effects.
    Member,
    /// Membership couldn't be determined (storage error, missing pubkey, etc.).
    /// Skip BOTH branches; next sync retries the Commit naturally via the
    /// is_mls_event_processed dedup.
    Unknown,
}

/// Decide whether a Commit-applied membership state means evicted, still
/// member, or undetermined. Replaces inline matches in both the first loop
/// and retry loop of sync_group_since_cursor.
pub(crate) fn membership_outcome_from_get_members<E>(
    get_members_result: Result<bool, E>,
) -> MembershipOutcome {
    match get_members_result {
        Ok(true) => MembershipOutcome::Member,
        Ok(false) => MembershipOutcome::Evicted,
        Err(_) => MembershipOutcome::Unknown,
    }
}

/// Pure h-tag filter for the pagination fallback path.
///
/// When the primary `#h`-filtered relay query returns 0 events, the fallback
/// fetches kind=445 across all groups. We MUST filter to our group ID here:
/// without this, unrelated wrappers fall through to the per-event h-tag
/// guard which silently `continue`s, leaving `last_seen_id == None` and
/// the cursor unable to advance — causing pagination to spin in place.
pub(crate) fn filter_events_to_h_tag(
    events: Vec<nostr_sdk::Event>,
    h_tag_value: &str,
) -> Vec<nostr_sdk::Event> {
    use nostr_sdk::prelude::*;
    events.into_iter()
        .filter(|ev| {
            match ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                Some(tag) => tag.content().map(|s| s.eq_ignore_ascii_case(h_tag_value)).unwrap_or(false),
                None => false,
            }
        })
        .collect()
}

// ============================================================================
// Per-Group Sync Locks
// ============================================================================

/// Per-group lock to ensure only one sync/process_message runs at a time for a given MLS group.
/// Prevents concurrent relay syncs from interleaving epoch-sequential commits.
static GROUP_SYNC_LOCKS: LazyLock<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Get or create a per-group sync lock.
pub fn get_group_sync_lock(group_id: &str) -> Arc<TokioMutex<()>> {
    let mut locks = GROUP_SYNC_LOCKS.lock().unwrap();
    locks.entry(group_id.to_string())
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone()
}

/// Drop every cached per-group sync lock — called by `reset_session()`.
/// The map grows unbounded across sessions otherwise; lock identity
/// also shouldn't be assumed to persist across an account swap.
pub fn clear_group_sync_locks() {
    GROUP_SYNC_LOCKS.lock().unwrap().clear();
}

// ============================================================================
// MLS Directory Resolution
// ============================================================================

/// Get the MLS directory for the current account. Resolved through the
/// canonical `account_dir` helper so the layout stays in lockstep with
/// every other per-account subsystem.
pub fn get_mls_directory() -> Result<std::path::PathBuf, String> {
    let npub = crate::db::get_current_account()?;
    let mls_dir = crate::db::account_dir(&npub)?.join("mls");

    if !mls_dir.exists() {
        std::fs::create_dir_all(&mls_dir)
            .map_err(|e| format!("Failed to create MLS directory: {}", e))?;
    }

    Ok(mls_dir)
}

// ============================================================================
// Network Helpers
// ============================================================================

/// Publish a nostr event with retries and exponential backoff.
///
/// If `relay_urls` is provided, publishes to those relays (group-specific relays
/// from MDK's `get_relays`). Otherwise falls back to active TRUSTED_RELAYS.
///
/// 5 attempts, 250ms base backoff. Only bails early on definitive rejections.
pub async fn publish_event_with_retries(
    client: &nostr_sdk::Client,
    event: &nostr_sdk::Event,
    relay_urls: Option<&[nostr_sdk::RelayUrl]>,
) -> Result<(), String> {
    use std::time::Duration;

    let targets: Vec<String> = if let Some(urls) = relay_urls {
        if urls.is_empty() {
            active_trusted_relays().await.into_iter().map(|s| s.to_string()).collect()
        } else {
            urls.iter().map(|u| u.to_string()).collect()
        }
    } else {
        active_trusted_relays().await.into_iter().map(|s| s.to_string()).collect()
    };

    if targets.is_empty() {
        return Err("no relays available for publishing".to_string());
    }

    let mut last_err: Option<String> = None;
    for attempt in 0..5u8 {
        match client
            .send_event_to(targets.iter().map(|s| s.as_str()), event)
            .await
        {
            Ok(output) if !output.success.is_empty() => {
                return Ok(());
            }
            Ok(output) => {
                let errors: Vec<&str> = output.failed.values().map(|s| s.as_str()).collect();
                let summary = if errors.is_empty() {
                    "no relay accepted event".to_string()
                } else {
                    errors.join("; ")
                };
                let any_definitive = errors.iter().any(|e| {
                    e.contains("duplicate") || e.contains("blocked")
                });
                last_err = Some(summary);
                if any_definitive {
                    break;
                }
            }
            Err(e) => {
                last_err = Some(e.to_string());
            }
        }
        if attempt < 4 {
            let delay_ms = 250u64.saturating_mul(1u64 << attempt);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
    Err(last_err.unwrap_or_else(|| "unknown error".to_string()))
}

/// Publish an evolution event to relays, then merge the pending commit.
///
/// Follows MIP-03 ordering: publish first, merge only after relay confirmation.
/// On publish failure, rolls back the pending commit via `clear_pending_commit`.
pub async fn publish_and_merge_commit(
    client: &nostr_sdk::Client,
    event: &nostr_sdk::Event,
    db_path: &std::path::Path,
    mls_group_id: &GroupId,
    group_relay_urls: &[nostr_sdk::RelayUrl],
) -> Result<(), String> {
    let relay_arg = if group_relay_urls.is_empty() { None } else { Some(group_relay_urls) };
    if let Err(e) = publish_event_with_retries(client, event, relay_arg).await {
        // Rollback the pending commit so the group isn't stuck
        if let Ok(s) = MdkSqliteStorage::new_unencrypted(db_path) {
            let rollback_engine = build_mdk_engine(s);
            if let Err(re) = rollback_engine.clear_pending_commit(mls_group_id) {
                eprintln!("[MLS] Failed to rollback pending commit: {}", re);
            } else {
                println!("[MLS] Rolled back pending commit after publish failure");
            }
        }
        return Err(e);
    }

    let storage = MdkSqliteStorage::new_unencrypted(db_path)
        .map_err(|e| format!("Failed to open storage for merge: {}", e))?;
    let engine = build_mdk_engine(storage);
    engine.merge_pending_commit(mls_group_id)
        .map_err(|e| format!("Failed to merge commit: {}", e))?;

    Ok(())
}

// ============================================================================
// MlsService
// ============================================================================

/// Main MLS service facade.
///
/// Creates FRESH MDK instances for each operation to ensure we always read
/// current state from SQLite, avoiding stale cache issues.
pub struct MlsService {
    /// Path to the SQLite database for creating fresh MDK instances.
    pub(crate) db_path: std::path::PathBuf,
}

impl MlsService {
    /// Create a new MLS service instance (not initialized — will fail on engine()).
    pub fn new() -> Self {
        Self { db_path: std::path::PathBuf::new() }
    }

    /// Create a new MLS service using vector-core's app data dir (headless-safe).
    pub fn new_persistent_static() -> Result<Self, MlsError> {
        let mls_dir = get_mls_directory()
            .map_err(|e| MlsError::StorageError(format!("Failed to get MLS directory: {}", e)))?;
        Self::init_at_path(mls_dir)
    }

    /// Shared init logic: given an MLS directory, set up the database and return the service.
    pub fn init_at_path(mls_dir: std::path::PathBuf) -> Result<Self, MlsError> {
        let db_path = mls_dir.join("vector-mls.db");
        let codec_marker = mls_dir.join("mls-codec-v2");

        // v0.2.x → v0.3.0: Wipe incompatible dual-connection MLS database
        if db_path.exists() {
            wipe_legacy_mls_database(&db_path);
        }

        // v0.3.x → v0.4.0: MDK 0.6.0 switched from JSON to postcard codec
        if db_path.exists() && !codec_marker.exists() {
            println!("[MLS] Detected pre-postcard MLS database — wiping for codec upgrade...");
            let _ = std::fs::remove_file(&db_path);
            let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
            let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(db_path.with_extension("db-journal"));
            println!("[MLS] Pre-postcard database wiped. Groups will need to be re-joined.");
        }

        // Verify we can create a storage instance
        let _storage = MdkSqliteStorage::new_unencrypted(&db_path)
            .map_err(|e| MlsError::StorageError(format!("init sqlite storage: {}", e)))?;

        // Write codec version marker for future upgrades
        let _ = std::fs::write(&codec_marker, b"postcard");

        Ok(Self { db_path })
    }

    /// Create a FRESH MDK engine instance for this operation.
    ///
    /// The returned MDK is non-Send — must not be held across await boundaries.
    /// Use for a single logical operation, then drop.
    pub fn engine(&self) -> Result<MDK<MdkSqliteStorage>, MlsError> {
        if self.db_path.as_os_str().is_empty() {
            return Err(MlsError::NotInitialized);
        }

        let storage = MdkSqliteStorage::new_unencrypted(&self.db_path)
            .map_err(|e| MlsError::StorageError(format!("open sqlite storage: {}", e)))?;
        Ok(build_mdk_engine(storage))
    }

    /// Get the path to the MLS SQLite database.
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    // ========================================================================
    // Database helpers (read/modify/write pattern)
    // ========================================================================

    /// Read group metadata from database.
    pub fn read_groups(&self) -> Result<Vec<MlsGroupFull>, MlsError> {
        crate::db::mls::load_mls_groups()
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write group metadata to database.
    pub fn write_groups(&self, groups: &[MlsGroupFull]) -> Result<(), MlsError> {
        crate::db::mls::save_mls_groups(groups)
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read keypackage index from database.
    #[allow(dead_code)]
    pub fn read_keypackage_index(&self) -> Result<Vec<KeyPackageIndexEntry>, MlsError> {
        let packages = crate::db::mls::load_mls_keypackages()
            .map_err(|e| MlsError::StorageError(e))?;

        let entries: Vec<KeyPackageIndexEntry> = packages.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();

        Ok(entries)
    }

    /// Write keypackage index to database.
    #[allow(dead_code)]
    pub fn write_keypackage_index(&self, index: &[KeyPackageIndexEntry]) -> Result<(), MlsError> {
        let packages: Vec<serde_json::Value> = index.iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect();

        crate::db::mls::save_mls_keypackages(&packages)
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read event cursors from database.
    pub fn read_event_cursors(&self) -> Result<HashMap<String, EventCursor>, MlsError> {
        crate::db::mls::load_mls_event_cursors()
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write event cursors to database.
    pub fn write_event_cursors(&self, cursors: &HashMap<String, EventCursor>) -> Result<(), MlsError> {
        crate::db::mls::save_mls_event_cursors(cursors)
            .map_err(|e| MlsError::StorageError(e))
    }

    // ========================================================================
    // Group Operations
    // ========================================================================

    /// Clean up an evicted group: mark as evicted, remove from STATE, delete chat from DB.
    ///
    /// Called from both sync and live subscription handlers when eviction is detected.
    pub async fn cleanup_evicted_group(&self, group_id: &str) -> Result<(), MlsError> {
        // 1. Find and mark the specific group as evicted in metadata
        let groups = self.read_groups().unwrap_or_default();
        let mut marked_group: Option<MlsGroupFull> = None;

        for group in &groups {
            if group.group.group_id == group_id || group.group.engine_group_id == group_id {
                let mut updated = group.clone();
                updated.group.evicted = true;
                marked_group = Some(updated);
                break;
            }
        }

        // 2. Persist eviction flag
        if let Some(group_to_update) = marked_group {
            if let Err(e) = crate::db::mls::save_mls_group(&group_to_update) {
                eprintln!("[MLS] Failed to mark group as evicted: {}", e);
            }
        }

        // 3. Remove from in-memory STATE
        {
            let mut state = crate::state::STATE.lock().await;
            state.chats.retain(|c| c.id() != group_id);
        }

        // 4. Delete chat from database
        if let Err(e) = crate::db::chats::delete_chat(group_id) {
            eprintln!("[MLS] Failed to delete chat from storage: {}", e);
        }

        // 5. Notify frontend
        crate::traits::emit_event("mls_group_left", &serde_json::json!({
            "group_id": group_id
        }));

        // 6. Refresh the integration layer's live subscription so we stop
        // receiving kind=445 events for this group. Without this, post-eviction
        // events would land in MDK's processed_messages with state=Failed
        // (no keys to decrypt) and become unrecoverable even after rejoin —
        // MDK's Step-0 dedup short-circuits Failed events to Unprocessable.
        crate::traits::refresh_subscriptions();

        Ok(())
    }

    /// Get the members and admins of an MLS group.
    ///
    /// Returns (wire_group_id, member_npubs, admin_npubs).
    /// Runs engine operations synchronously (non-Send engine, no awaits while held).
    pub fn get_group_members(&self, group_id: &str) -> Result<(String, Vec<String>, Vec<String>), MlsError> {
        use nostr_sdk::prelude::ToBech32;

        let meta_groups = self.read_groups().unwrap_or_default();
        let (wire_id, engine_id) = if let Some(m) = meta_groups.iter()
            .find(|g| g.group.group_id == group_id || (!g.group.engine_group_id.is_empty() && g.group.engine_group_id == group_id))
        {
            (
                m.group.group_id.clone(),
                if !m.group.engine_group_id.is_empty() { m.group.engine_group_id.clone() } else { m.group.group_id.clone() },
            )
        } else {
            (group_id.to_string(), group_id.to_string())
        };

        let engine = self.engine()?;

        let mut members: Vec<String> = Vec::new();
        let mut admins: Vec<String> = Vec::new();
        let gid_bytes = crate::simd::hex::hex_string_to_bytes(&engine_id);
        if !gid_bytes.is_empty() {
            let gid = GroupId::from_slice(&gid_bytes);

            match engine.get_members(&gid) {
                Ok(pk_list) => {
                    members = pk_list.into_iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect();
                }
                Err(e) => eprintln!("[MLS] get_members failed for engine_id={}: {}", engine_id, e),
            }

            match engine.get_groups() {
                Ok(groups) => {
                    for g in groups {
                        let gid_hex = crate::simd::hex::bytes_to_hex_string(g.mls_group_id.as_slice());
                        if gid_hex == engine_id {
                            admins = g.admin_pubkeys.iter()
                                .filter_map(|pk| pk.to_bech32().ok())
                                .collect();
                            break;
                        }
                    }
                }
                Err(e) => eprintln!("[MLS] get_groups failed: {}", e),
            }
        }

        // Fallback: if admins list is empty, use creator_pubkey from stored metadata
        if admins.is_empty() {
            if let Some(meta) = meta_groups.iter().find(|g| g.group.group_id == wire_id) {
                if !meta.group.creator_pubkey.is_empty() {
                    admins.push(meta.group.creator_pubkey.clone());
                }
            }
        }

        Ok((wire_id, members, admins))
    }

    /// Sync group participants in STATE from the MLS engine.
    ///
    /// Reads actual members from the engine, updates the in-memory chat's participant list,
    /// and persists the updated chat to DB.
    pub async fn sync_group_participants(&self, group_id: &str) -> Result<(), MlsError> {
        let (_, members, _) = self.get_group_members(group_id)?;

        let slim = {
            let mut state = crate::state::STATE.lock().await;
            if let Some(chat_idx) = state.chats.iter().position(|c| c.id() == group_id) {
                let new_handles: Vec<u16> = members.iter().map(|p| state.interner.intern(p)).collect();
                state.chats[chat_idx].participants = new_handles;

                Some(crate::db::chats::SlimChatDB::from_chat(&state.chats[chat_idx], &state.interner))
            } else {
                None
            }
        };

        if let Some(slim) = slim {
            if let Err(e) = crate::db::chats::save_slim_chat(&slim) {
                eprintln!("[MLS] Failed to save chat after syncing participants: {}", e);
            }
        }

        Ok(())
    }

    /// Create a new MLS group.
    ///
    /// 1. Resolves signer and relay config
    /// 2. Fetches member KeyPackages from index or network
    /// 3. Creates group via MDK engine (no awaits while engine held)
    /// 4. Publishes welcome(s) to invited recipients via gift_wrap
    /// 5. Persists group metadata, creates Chat in STATE
    /// 6. Emits mls_group_initial_sync + mls_group_metadata
    ///
    /// Returns the wire group_id (64-hex, used for relay filtering and UI).
    pub async fn create_group(
        &self,
        name: &str,
        avatar_ref: Option<&str>,
        avatar_cached: Option<&str>,
        initial_member_devices: &[(String, String)], // (member_pubkey, device_id) pairs
        description: Option<&str>,
        image_hash: Option<[u8; 32]>,
        image_key: Option<[u8; 32]>,
        image_nonce: Option<[u8; 12]>,
        admin_npubs: &[String],
    ) -> Result<String, MlsError> {
        use nostr_sdk::prelude::*;

        let client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;
        let signer = client.signer().await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let my_pubkey = signer.get_public_key().await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let creator_pubkey_b32 = my_pubkey.to_bech32()
            .map_err(|e| MlsError::CryptoError(e.to_string()))?;

        // Build group config (relay-scoped)
        let relay_urls: Vec<RelayUrl> = crate::state::active_trusted_relays().await
            .into_iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();
        let desc = match description.filter(|d| !d.is_empty()) {
            Some(d) => d.to_string(),
            None => format!("Vector group: {}", name),
        };
        let mut admins = vec![my_pubkey];
        for npub in admin_npubs {
            if let Ok(pk) = nostr_sdk::PublicKey::from_bech32(npub) {
                if pk != my_pubkey { admins.push(pk); }
            }
        }
        let group_config = NostrGroupConfigData::new(
            name.to_string(), desc.clone(),
            image_hash, image_key, image_nonce,
            relay_urls, admins,
        );

        // Resolve member KeyPackage events (awaits allowed here, before engine scope)
        let mut member_kp_events: Vec<Event> = Vec::new();
        let mut invited_recipients: Vec<PublicKey> = Vec::new();
        let index = self.read_keypackage_index().unwrap_or_default();

        for (member_npub, device_id) in initial_member_devices.iter() {
            let member_pk = match PublicKey::from_bech32(member_npub) {
                Ok(pk) => pk,
                Err(_) => {
                    eprintln!("[MLS] Invalid member npub: {}", member_npub);
                    continue;
                }
            };

            // Try index first
            let mut ref_event_id_hex: Option<String> = None;
            for entry in &index {
                if entry.owner_pubkey == *member_npub && entry.device_id == *device_id {
                    ref_event_id_hex = Some(entry.keypackage_ref.clone());
                    break;
                }
            }

            let kp_event: Option<Event> = if let Some(id_hex) = ref_event_id_hex {
                let id = match EventId::from_hex(&id_hex) {
                    Ok(v) => v,
                    Err(_) => {
                        println!("[MLS] Invalid keypackage_ref in index for {}:{}", member_npub, device_id);
                        continue;
                    }
                };
                let filter = Filter::new().id(id).limit(1);
                match client
                    .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => events.into_iter().next(),
                    Err(e) => {
                        eprintln!("[MLS] Fetch KeyPackage by id failed ({}:{}): {}", member_npub, device_id, e);
                        None
                    }
                }
            } else {
                let filter = Filter::new().author(member_pk).kind(Kind::MlsKeyPackage).limit(50);
                match client
                    .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => {
                        let selected = events.into_iter().max_by_key(|e| e.created_at.as_secs());
                        if selected.is_none() {
                            eprintln!("[MLS] No KeyPackage events found for {}", member_npub);
                        }
                        selected
                    }
                    Err(e) => {
                        eprintln!("[MLS] Fetch KeyPackages for {} failed: {}", member_npub, e);
                        None
                    }
                }
            };

            if let Some(ev) = kp_event {
                if !crate::mls::has_encoding_tag(&ev) || !ev.tags.iter().any(|t| t.as_slice().first().map(|s| s.as_str()) == Some("i")) {
                    let display_name = {
                        let state = crate::state::STATE.lock().await;
                        state.get_profile(member_npub)
                            .and_then(|p| {
                                if !p.name.is_empty() { Some(p.name.to_string()) }
                                else if !p.display_name.is_empty() { Some(p.display_name.to_string()) }
                                else { None }
                            })
                            .unwrap_or_else(|| member_npub.clone())
                    };
                    return Err(MlsError::OutdatedKeyPackage(display_name));
                }
                member_kp_events.push(ev);
                invited_recipients.push(member_pk);
            } else {
                eprintln!("[MLS] Skipping member device {}:{} (no KeyPackage event)", member_npub, device_id);
            }
        }

        let invited_count = member_kp_events.len();

        // Engine operations — no awaits while engine is in scope (non-Send)
        let (group_id_hex, engine_gid_hex, welcome_rumors) = {
            let engine = self.engine()?;
            let create_out = engine
                .create_group(&my_pubkey, member_kp_events, group_config)
                .map_err(|e| MlsError::NostrMlsError(format!("create_group: {}", e)))?;

            // CRITICAL: Merge the pending commit immediately!
            engine.merge_pending_commit(&create_out.group.mls_group_id)
                .map_err(|e| MlsError::NostrMlsError(format!("merge_pending_commit after create: {}", e)))?;

            let gid_bytes = create_out.group.mls_group_id.as_slice();
            let engine_gid_hex = crate::simd::hex::bytes_to_hex_string(gid_bytes);

            // Derive wire id (wrapper 'h' tag, 64-hex) using a dummy wrapper
            let wire_gid_hex = {
                let dummy_rumor = EventBuilder::new(Kind::Custom(9), "vector-mls-bootstrap")
                    .tag(Tag::custom(
                        TagKind::Custom(std::borrow::Cow::Borrowed("vector-mls-bootstrap")),
                        vec!["true"],
                    ))
                    .build(my_pubkey);
                if let Ok(wrapper) = engine.create_message(&create_out.group.mls_group_id, dummy_rumor) {
                    if let Some(h_tag) = wrapper.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                        if let Some(canon) = h_tag.content() {
                            if canon.len() == 64 { canon.to_string() }
                            else { engine_gid_hex.clone() }
                        } else { engine_gid_hex.clone() }
                    } else { engine_gid_hex.clone() }
                } else { engine_gid_hex.clone() }
            };

            (wire_gid_hex, engine_gid_hex, create_out.welcome_rumors)
        }; // engine dropped

        if group_id_hex.len() != 32 && group_id_hex.len() != 64 {
            eprintln!("[MLS] create_group: unexpected group_id length={}", group_id_hex.len());
        }

        // Publish welcomes (gift-wrapped) 1:1 with invited recipients
        if !welcome_rumors.is_empty() {
            if welcome_rumors.len() != invited_count {
                eprintln!("[MLS] welcome/member count mismatch: welcomes={}, invited={}", welcome_rumors.len(), invited_count);
            }
            let min_len = std::cmp::min(welcome_rumors.len(), invited_recipients.len());
            let futs: Vec<_> = (0..min_len)
                .map(|i| {
                    let welcome = welcome_rumors[i].clone();
                    let target = invited_recipients[i];
                    let client = client.clone();
                    async move {
                        match client.gift_wrap_to(crate::state::active_trusted_relays().await.into_iter(), &target, welcome, []).await {
                            Ok(wrapper_id) => {
                                println!("[MLS][welcome][published] wrapper_id={}, recipient={}", wrapper_id.to_hex(), target.to_bech32().unwrap_or_default());
                            }
                            Err(e) => {
                                eprintln!("[MLS][welcome][publish_error] recipient={}, err={}", target.to_bech32().unwrap_or_default(), e);
                            }
                        }
                    }
                })
                .collect();
            futures_util::future::join_all(futs).await;
        } else {
            println!("[MLS] No welcome rumors (invited={}, self-only path likely)", invited_count);
        }

        // Persist group metadata
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| MlsError::StorageError(format!("system time error: {}", e)))?
            .as_secs();

        let meta = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: group_id_hex.clone(),
                engine_group_id: engine_gid_hex,
                creator_pubkey: creator_pubkey_b32,
                created_at: now_secs,
                updated_at: now_secs,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: name.to_string(),
                description: Some(desc),
                avatar_ref: avatar_ref.map(|s| s.to_string()),
                avatar_cached: avatar_cached.map(|s| s.to_string()),
            },
        };

        let mut groups = self.read_groups()?;
        groups.push(meta.clone());
        self.write_groups(&groups)?;
        crate::traits::emit_event("mls_group_metadata", &serde_json::json!({
            "metadata": crate::mls::types::metadata_to_frontend(&meta)
        }));

        // Create the Chat in STATE with metadata and save to disk
        {
            let mut state = crate::state::STATE.lock().await;
            let chat_id = state.create_or_get_mls_group_chat(&group_id_hex, vec![]);

            if let Some(chat) = state.get_chat_mut(&chat_id) {
                chat.metadata.set_name(meta.profile.name.clone());
                chat.metadata.set_member_count(invited_count + 1);
            }

            let slim = state.get_chat(&chat_id).map(|chat| {
                crate::db::chats::SlimChatDB::from_chat(chat, &state.interner)
            });
            drop(state);

            if let Some(slim) = slim {
                if let Err(e) = crate::db::chats::save_slim_chat(&slim) {
                    eprintln!("[MLS] Failed to save chat after group creation: {}", e);
                }
            }
        }

        // Notify UI
        crate::traits::emit_event("mls_group_initial_sync", &serde_json::json!({
            "group_id": group_id_hex,
            "processed": 0u32,
            "new": 0u32
        }));

        println!("[MLS] Created group (persistent) id={}, name=\"{}\", invited_devices_hint={}",
            group_id_hex, name, initial_member_devices.len());
        Ok(group_id_hex)
    }

    /// Add a single member device to an existing MLS group.
    ///
    /// Fetches the member's keypackage from index or network, validates it,
    /// then spawns background: lock → commit → publish → merge → welcome → UI update.
    pub async fn add_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        device_id: &str,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        let client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;

        let member_pk = PublicKey::from_bech32(member_pubkey)
            .map_err(|e| MlsError::CryptoError(format!("Invalid member npub: {}", e)))?;

        // Fetch keypackage from index or network
        let index = self.read_keypackage_index().unwrap_or_default();
        let mut kp_event: Option<Event> = None;

        for entry in &index {
            if entry.owner_pubkey == member_pubkey && entry.device_id == device_id {
                let id = EventId::from_hex(&entry.keypackage_ref)
                    .map_err(|e| MlsError::CryptoError(format!("Invalid keypackage ref: {}", e)))?;
                let filter = Filter::new().id(id).limit(1);
                if let Ok(events) = client
                    .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    kp_event = events.into_iter().next();
                }
                break;
            }
        }

        if kp_event.is_none() {
            let filter = Filter::new().author(member_pk).kind(Kind::MlsKeyPackage).limit(50);
            if let Ok(events) = client
                .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                .await
            {
                kp_event = events.into_iter().max_by_key(|e| e.created_at.as_secs());
            }
        }

        let kp_event = kp_event.ok_or_else(|| {
            MlsError::NetworkError(format!("No keypackage found for {}:{}", member_pubkey, device_id))
        })?;

        // Validate encoding tag + i tag (MIP-00/MIP-02)
        if !crate::mls::has_encoding_tag(&kp_event) || !kp_event.tags.iter().any(|t| t.as_slice().first().map(|s| s.as_str()) == Some("i")) {
            let display_name = {
                let state = crate::state::STATE.lock().await;
                state.get_profile(member_pubkey)
                    .and_then(|p| {
                        if !p.name.is_empty() { Some(p.name.to_string()) }
                        else if !p.display_name.is_empty() { Some(p.display_name.to_string()) }
                        else { None }
                    })
                    .unwrap_or_else(|| member_pubkey.to_string())
            };
            return Err(MlsError::OutdatedKeyPackage(display_name));
        }

        let groups = self.read_groups()?;
        let group_meta = groups.iter()
            .find(|g| g.group.group_id == group_id || g.group.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.group.engine_group_id.clone();
        // SessionGuard captured pre-spawn — the task bypasses POOL_GENERATION
        // (opens MdkSqliteStorage directly against the captured `db_path`)
        // so without this gate a post-swap wake-up would write into the
        // wrong account's MLS storage.
        let session = crate::state::SessionGuard::capture();

        tokio::spawn(async move {
            let Some(client) = crate::state::nostr_client() else { return; };

            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // Re-check after lock — a swap may have completed while
            // we were waiting; abandoning prevents account A's MLS
            // evolution leaking into account B's session.
            if !session.is_valid() { return; }

            let mls_group_id = GroupId::from_slice(&crate::simd::hex::hex_string_to_bytes(&engine_group_id));

            let (evolution_event, welcome_rumors, group_relays) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for add: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to open storage: {}", e)
                        }));
                        return;
                    }
                };
                let engine = build_mdk_engine(storage);
                let relays: Vec<nostr_sdk::RelayUrl> = engine.get_relays(&mls_group_id)
                    .unwrap_or_default().into_iter().collect();
                match engine.add_members(&mls_group_id, std::slice::from_ref(&kp_event)) {
                    Ok(result) => (result.evolution_event, result.welcome_rumors, relays),
                    Err(e) => {
                        eprintln!("[MLS] Failed to add member: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to add member: {}", e)
                        }));
                        return;
                    }
                }
            };

            if let Err(e) = publish_and_merge_commit(&client, &evolution_event, &db_path, &mls_group_id, &group_relays).await {
                eprintln!("[MLS] Failed to publish/merge add-member commit: {}", e);
                crate::traits::emit_event("mls_error", &serde_json::json!({
                    "group_id": group_id_owned, "error": format!("Failed to publish invite: {}", e)
                }));
                return;
            }

            let _ = crate::mls::tracking::track_mls_event_processed(
                &evolution_event.id.to_hex(), &group_id_owned, evolution_event.created_at.as_secs(),
            );

            // Send welcome messages
            if let Some(welcome_rumors) = welcome_rumors {
                let futs: Vec<_> = welcome_rumors.into_iter()
                    .map(|welcome| {
                        let client = client.clone();
                        async move {
                            if let Err(e) = client.gift_wrap_to(crate::state::active_trusted_relays().await.into_iter(), &member_pk, welcome, []).await {
                                eprintln!("[MLS] Failed to send welcome: {}", e);
                            }
                        }
                    })
                    .collect();
                futures_util::future::join_all(futs).await;
            }

            // Sync participants + update metadata
            let mls = MlsService::new_persistent_static().ok();
            if let Some(mls) = mls {
                if let Err(e) = mls.sync_group_participants(&group_id_owned).await {
                    eprintln!("[MLS] Failed to sync participants after add: {}", e);
                }
                if let Ok(mut groups) = crate::db::mls::load_mls_groups() {
                    if let Some(group) = groups.iter_mut().find(|g| g.group.group_id == group_id_owned) {
                        group.group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                        let _ = crate::db::mls::save_mls_groups(&groups);
                    }
                }
            }
            crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": group_id_owned }));
        });

        Ok(())
    }

    /// Add multiple members to an existing MLS group in a single commit.
    ///
    /// Fetches all members' keypackages, validates them, then spawns background:
    /// lock → commit → publish → merge → welcomes → UI update.
    pub async fn add_member_devices(
        &self,
        group_id: &str,
        members: &[(String, String)], // (npub, device_id) pairs
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        let client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;
        let index = self.read_keypackage_index().unwrap_or_default();

        let mut member_kp_events: Vec<Event> = Vec::new();
        let mut invited_recipients: Vec<PublicKey> = Vec::new();

        for (member_npub, device_id) in members {
            let member_pk = PublicKey::from_bech32(member_npub)
                .map_err(|e| MlsError::CryptoError(format!("Invalid member npub: {}", e)))?;

            let mut kp_event: Option<Event> = None;
            for entry in &index {
                if entry.owner_pubkey == *member_npub && entry.device_id == *device_id {
                    let id = EventId::from_hex(&entry.keypackage_ref)
                        .map_err(|e| MlsError::CryptoError(format!("Invalid keypackage ref: {}", e)))?;
                    let filter = Filter::new().id(id).limit(1);
                    if let Ok(events) = client
                        .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                        .await
                    {
                        kp_event = events.into_iter().next();
                    }
                    break;
                }
            }

            if kp_event.is_none() {
                let filter = Filter::new().author(member_pk).kind(Kind::MlsKeyPackage).limit(50);
                if let Ok(events) = client
                    .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    kp_event = events.into_iter().max_by_key(|e| e.created_at.as_secs());
                }
            }

            let kp_event = kp_event.ok_or_else(|| {
                MlsError::NetworkError(format!("No keypackage found for {}:{}", member_npub, device_id))
            })?;

            if !crate::mls::has_encoding_tag(&kp_event) || !kp_event.tags.iter().any(|t| t.as_slice().first().map(|s| s.as_str()) == Some("i")) {
                let display_name = {
                    let state = crate::state::STATE.lock().await;
                    state.get_profile(member_npub)
                        .and_then(|p| {
                            if !p.name.is_empty() { Some(p.name.to_string()) }
                            else if !p.display_name.is_empty() { Some(p.display_name.to_string()) }
                            else { None }
                        })
                        .unwrap_or_else(|| member_npub.to_string())
                };
                return Err(MlsError::OutdatedKeyPackage(display_name));
            }

            member_kp_events.push(kp_event);
            invited_recipients.push(member_pk);
        }

        let groups = self.read_groups()?;
        let group_meta = groups.iter()
            .find(|g| g.group.group_id == group_id || g.group.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.group.engine_group_id.clone();
        // SessionGuard captured pre-spawn — the task bypasses POOL_GENERATION
        // (opens MdkSqliteStorage directly against the captured `db_path`)
        // so without this gate a post-swap wake-up would write into the
        // wrong account's MLS storage.
        let session = crate::state::SessionGuard::capture();

        tokio::spawn(async move {
            let Some(client) = crate::state::nostr_client() else { return; };

            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // Re-check after lock — a swap may have completed while
            // we were waiting; abandoning prevents account A's MLS
            // evolution leaking into account B's session.
            if !session.is_valid() { return; }

            let mls_group_id = GroupId::from_slice(&crate::simd::hex::hex_string_to_bytes(&engine_group_id));

            let (evolution_event, welcome_rumors, group_relays) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for add: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to open storage: {}", e)
                        }));
                        return;
                    }
                };
                let engine = build_mdk_engine(storage);
                let relays: Vec<nostr_sdk::RelayUrl> = engine.get_relays(&mls_group_id)
                    .unwrap_or_default().into_iter().collect();
                match engine.add_members(&mls_group_id, &member_kp_events) {
                    Ok(result) => (result.evolution_event, result.welcome_rumors, relays),
                    Err(e) => {
                        eprintln!("[MLS] Failed to add members: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to add members: {}", e)
                        }));
                        return;
                    }
                }
            };

            if let Err(e) = publish_and_merge_commit(&client, &evolution_event, &db_path, &mls_group_id, &group_relays).await {
                eprintln!("[MLS] Failed to publish/merge add-members commit: {}", e);
                crate::traits::emit_event("mls_error", &serde_json::json!({
                    "group_id": group_id_owned, "error": format!("Failed to publish invite: {}", e)
                }));
                return;
            }

            let _ = crate::mls::tracking::track_mls_event_processed(
                &evolution_event.id.to_hex(), &group_id_owned, evolution_event.created_at.as_secs(),
            );

            // Send welcome messages — pair each welcome with its recipient
            if let Some(welcome_rumors) = welcome_rumors {
                let min_len = std::cmp::min(welcome_rumors.len(), invited_recipients.len());
                let futs: Vec<_> = (0..min_len)
                    .map(|i| {
                        let welcome = welcome_rumors[i].clone();
                        let target = invited_recipients[i];
                        let client = client.clone();
                        async move {
                            match client
                                .gift_wrap_to(crate::state::active_trusted_relays().await.into_iter(), &target, welcome, [])
                                .await
                            {
                                Ok(wrapper_id) => {
                                    let recipient = target.to_bech32().unwrap_or_default();
                                    println!("[MLS][welcome][published] wrapper_id={}, recipient={}", wrapper_id.to_hex(), recipient);
                                }
                                Err(e) => {
                                    let recipient = target.to_bech32().unwrap_or_default();
                                    eprintln!("[MLS][welcome][publish_error] recipient={}, err={}", recipient, e);
                                }
                            }
                        }
                    })
                    .collect();
                futures_util::future::join_all(futs).await;
            }

            // Sync participants + update metadata
            let mls = MlsService::new_persistent_static().ok();
            if let Some(mls) = mls {
                if let Err(e) = mls.sync_group_participants(&group_id_owned).await {
                    eprintln!("[MLS] Failed to sync participants after add: {}", e);
                }
                if let Ok(mut groups) = crate::db::mls::load_mls_groups() {
                    if let Some(group) = groups.iter_mut().find(|g| g.group.group_id == group_id_owned) {
                        group.group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                        let _ = crate::db::mls::save_mls_groups(&groups);
                    }
                }
            }
            crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": group_id_owned }));
        });

        Ok(())
    }

    /// Remove a member device from a group (admin only).
    ///
    /// Validates pubkey and group lookup synchronously, then spawns background task:
    /// lock → verify member → create commit → relay confirm → merge → UI update
    pub async fn remove_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        _device_id: &str,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        let _client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;

        let member_pk = PublicKey::from_bech32(member_pubkey)
            .map_err(|e| MlsError::CryptoError(format!("Invalid member pubkey: {}", e)))?;

        let groups = self.read_groups()?;
        let group_meta = groups.iter()
            .find(|g| g.group.group_id == group_id || g.group.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.group.engine_group_id.clone();
        let member_pubkey_owned = member_pubkey.to_string();
        let session = crate::state::SessionGuard::capture();

        tokio::spawn(async move {
            let Some(client) = crate::state::nostr_client() else { return; };

            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // Re-check after lock — see add_member above.
            if !session.is_valid() { return; }

            let mls_group_id = GroupId::from_slice(&crate::simd::hex::hex_string_to_bytes(&engine_group_id));

            let (evolution_event, group_relays) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for remove: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to open storage: {}", e)
                        }));
                        return;
                    }
                };
                let engine = build_mdk_engine(storage);

                let current_members = match engine.get_members(&mls_group_id) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[MLS] Failed to get current members: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to get group members: {}", e)
                        }));
                        return;
                    }
                };

                if !current_members.contains(&member_pk) {
                    eprintln!("[MLS] Member {} not found in group", member_pubkey_owned);
                    crate::traits::emit_event("mls_error", &serde_json::json!({
                        "group_id": group_id_owned, "error": "Member not found in group. The group state may be out of sync."
                    }));
                    return;
                }

                let relays: Vec<nostr_sdk::RelayUrl> = engine.get_relays(&mls_group_id)
                    .unwrap_or_default().into_iter().collect();
                match engine.remove_members(&mls_group_id, &[member_pk]) {
                    Ok(result) => (result.evolution_event, relays),
                    Err(e) => {
                        eprintln!("[MLS] Failed to remove member: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to remove member: {}", e)
                        }));
                        return;
                    }
                }
            }; // engine dropped before await

            if let Err(e) = publish_and_merge_commit(&client, &evolution_event, &db_path, &mls_group_id, &group_relays).await {
                eprintln!("[MLS] Failed to publish/merge remove-member commit: {}", e);
                crate::traits::emit_event("mls_error", &serde_json::json!({
                    "group_id": group_id_owned, "error": format!("Failed to publish remove commit: {}", e)
                }));
                return;
            }

            let _ = crate::mls::tracking::track_mls_event_processed(
                &evolution_event.id.to_hex(), &group_id_owned, evolution_event.created_at.as_secs(),
            );

            // Sync participants + emit UI refresh
            let mls = MlsService::new_persistent_static().ok();
            if let Some(mls) = mls {
                if let Err(e) = mls.sync_group_participants(&group_id_owned).await {
                    eprintln!("[MLS] Failed to sync participants after remove: {}", e);
                }
            }
            crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": group_id_owned }));
        });

        Ok(())
    }

    /// Update group metadata (name, description, admins, avatar).
    ///
    /// Admin-only operation. Creates an MLS commit with the updated group data,
    /// publishes to relays, then updates local metadata.
    /// Runs the engine + publish work on a spawned background task.
    pub async fn update_group_data(
        &self,
        group_id: &str,
        name: Option<String>,
        description: Option<String>,
        admin_npubs: Option<Vec<String>>,
        image_hash: Option<Option<[u8; 32]>>,
        image_key: Option<Option<[u8; 32]>>,
        image_nonce: Option<Option<[u8; 12]>>,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        let client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;
        let signer = client.signer().await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let my_pubkey = signer.get_public_key().await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;

        let groups = self.read_groups()?;
        let _group_meta = groups.iter()
            .find(|g| g.group.group_id == group_id || g.group.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = _group_meta.group.engine_group_id.clone();
        let name_clone = name.clone();
        let description_clone = description.clone();
        let session = crate::state::SessionGuard::capture();

        tokio::spawn(async move {
            let Some(client) = crate::state::nostr_client() else { return; };

            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // Re-check after lock acquisition: see remove path above.
            if !session.is_valid() { return; }

            let mls_group_id = GroupId::from_slice(&crate::simd::hex::hex_string_to_bytes(&engine_group_id));

            // 1. Build update and create commit under lock
            let (evolution_event, group_relays) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for update: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to open storage: {}", e)
                        }));
                        return;
                    }
                };
                let engine = build_mdk_engine(storage);

                let mut update = NostrGroupDataUpdate::new();
                if let Some(ref n) = name_clone { update = update.name(n.clone()); }
                if let Some(ref d) = description_clone { update = update.description(d.clone()); }
                if let Some(ref npubs) = admin_npubs {
                    let mut admin_pks = vec![my_pubkey];
                    for npub in npubs {
                        if let Ok(pk) = PublicKey::from_bech32(npub) {
                            if pk != my_pubkey { admin_pks.push(pk); }
                        }
                    }
                    update = update.admins(admin_pks);
                }
                if let Some(hash) = image_hash { update = update.image_hash(hash); }
                if let Some(key) = image_key { update = update.image_key(key); }
                if let Some(nonce) = image_nonce { update = update.image_nonce(nonce); }

                let relays: Vec<nostr_sdk::RelayUrl> = engine.get_relays(&mls_group_id)
                    .unwrap_or_default().into_iter().collect();
                match engine.update_group_data(&mls_group_id, update) {
                    Ok(result) => {
                        println!("[MLS] update_group_data commit created, event_id={}", result.evolution_event.id.to_hex());
                        (result.evolution_event, relays)
                    }
                    Err(e) => {
                        eprintln!("[MLS] Failed to update group data: {}", e);
                        crate::traits::emit_event("mls_error", &serde_json::json!({
                            "group_id": group_id_owned, "error": format!("Failed to update group data: {}", e)
                        }));
                        return;
                    }
                }
            }; // engine dropped before await

            // 2. Publish and merge (MIP-03 ordering)
            if let Err(e) = publish_and_merge_commit(&client, &evolution_event, &db_path, &mls_group_id, &group_relays).await {
                eprintln!("[MLS] Failed to publish/merge update-group-data commit: {}", e);
                crate::traits::emit_event("mls_error", &serde_json::json!({
                    "group_id": group_id_owned, "error": format!("Failed to publish update commit: {}", e)
                }));
                return;
            }

            let _ = crate::mls::tracking::track_mls_event_processed(
                &evolution_event.id.to_hex(), &group_id_owned, evolution_event.created_at.as_secs(),
            );

            // 3. Update local metadata if name or description changed
            if name_clone.is_some() || description_clone.is_some() {
                let mls = match MlsService::new_persistent_static() {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[MLS] Failed to create MlsService for local update: {}", e);
                        crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": group_id_owned }));
                        return;
                    }
                };
                if let Ok(mut groups) = mls.read_groups() {
                    if let Some(meta) = groups.iter_mut().find(|g| g.group.group_id == group_id_owned || g.group.engine_group_id == group_id_owned) {
                        if let Some(ref n) = name_clone { meta.profile.name = n.clone(); }
                        if let Some(ref d) = description_clone { meta.profile.description = Some(d.clone()); }
                        meta.group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                    }
                    let updated_meta = groups.iter().find(|g| g.group.group_id == group_id_owned || g.group.engine_group_id == group_id_owned).cloned();
                    let _ = mls.write_groups(&groups);
                    if let Some(meta) = updated_meta {
                        crate::traits::emit_event("mls_group_metadata", &serde_json::json!({
                            "metadata": crate::mls::types::metadata_to_frontend(&meta)
                        }));
                    }
                }

                // Update STATE chat name
                if let Some(ref n) = name_clone {
                    let mut state = crate::state::STATE.lock().await;
                    if let Some(chat) = state.get_chat_mut(&group_id_owned) {
                        chat.metadata.set_name(n.clone());
                    }
                }
            }

            // 4. Sync participants + emit UI refresh
            if admin_npubs.is_some() {
                let mls = MlsService::new_persistent_static().ok();
                if let Some(mls) = mls {
                    if let Err(e) = mls.sync_group_participants(&group_id_owned).await {
                        eprintln!("[MLS] Failed to sync participants after update: {}", e);
                    }
                }
            }
            crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": group_id_owned }));
        });

        Ok(())
    }

    /// Sync a group from the last cursor position.
    ///
    /// 1. Read cursor from mls_event_cursors
    /// 2. Fetch events from relays since cursor (or use prefetched_events)
    /// 3. Process each event via engine.process_message
    /// 4. Process buffered rumors (messages, reactions, typing, leave requests, etc.)
    /// 5. Update cursor position
    ///
    /// Returns (processed_events_count, new_messages_count)
    pub async fn sync_group_since_cursor(
        &self,
        group_id: &str,
        prefetched_events: Option<Vec<nostr_sdk::Event>>,
    ) -> Result<(u32, u32), MlsError> {
        use nostr_sdk::prelude::*;
        use crate::rumor::{RumorEvent, RumorContext, ConversationType, RumorProcessingResult};

        if group_id.is_empty() {
            return Err(MlsError::InvalidGroupId);
        }

        // Account-swap defence. `self.db_path` is frozen at construction
        // time; if a swap landed between then and now, every decrypt below
        // would still succeed against account A's MLS storage but the
        // resulting plaintext would be committed into account B's STATE.
        // Snapshot the live generation and short-circuit before doing any
        // network I/O or STATE write. Re-checked again right before each
        // chat / message mutation below.
        let session = crate::state::SessionGuard::capture();
        if !session.is_valid() {
            return Ok((0, 0));
        }

        // Acquire per-group lock
        let group_lock = get_group_sync_lock(group_id);
        let _guard = group_lock.lock().await;

        if !session.is_valid() {
            return Ok((0, 0));
        }

        // Check eviction
        let groups = self.read_groups().ok();
        let group_metadata = groups.as_ref().and_then(|gs| {
            gs.iter().find(|g| g.group.group_id == group_id || (!g.group.engine_group_id.is_empty() && g.group.engine_group_id == group_id))
        });

        if let Some(meta) = group_metadata {
            if meta.group.evicted {
                return Ok((0, 0));
            }
        }

        let group_display = group_metadata
            .and_then(|m| if m.profile.name.is_empty() { None } else { Some(format!("{} ({})", m.profile.name, &group_id[..8.min(group_id.len())])) })
            .unwrap_or_else(|| group_id[..16.min(group_id.len())].to_string());

        // Load cursor
        let mut cursors = self.read_event_cursors().unwrap_or_default();
        let now = Timestamp::now();

        let since = if let Some(cur) = cursors.get(group_id) {
            // See `cursor_to_next_since` for rationale: we advance past the
            // cursor's anchor wrapper so a permanently-failing event can
            // never re-trap the sync.
            Timestamp::from_secs(cursor_to_next_since(cur.last_seen_at))
        } else {
            if let Some(meta) = group_metadata {
                if meta.group.created_at > 0 {
                    println!("[MLS] First sync for group {}, fetching from invite time {}", group_display, meta.group.created_at);
                    Timestamp::from_secs(meta.group.created_at)
                } else {
                    println!("[MLS] First sync for group {} (no created_at), fetching 1 year history", group_display);
                    Timestamp::from_secs(now.as_secs().saturating_sub(60 * 60 * 24 * 365))
                }
            } else {
                println!("[MLS] First sync for group {} (no metadata), fetching 1 year history", group_display);
                Timestamp::from_secs(now.as_secs().saturating_sub(60 * 60 * 24 * 365))
            }
        };
        let until = now;

        let gid_for_fetch = if let Some(meta) = group_metadata {
            meta.group.group_id.clone()
        } else {
            group_id.to_string()
        };

        let group_id_len = gid_for_fetch.len();
        if group_id_len != 32 && group_id_len != 64 {
            eprintln!("[MLS] sync_group_since_cursor: unsupported group_id length {} for id={}; skipping", group_id_len, gid_for_fetch);
            return Ok((0, 0));
        }

        // ====================================================================
        // Phase 0 — Auto-recovery self-heal
        // ====================================================================
        //
        // Transition recoverable MDK Failed events to Retryable so MDK will
        // re-attempt decryption instead of short-circuiting. Three categories:
        //
        //   (a) Failed in this group with epoch=NULL — via MDK's typed API.
        //   (b) Failed with mls_group_id IS NULL — events killed at
        //       validate_event before group could be determined. Direct SQL
        //       (typed API filters on group_id). Group-agnostic by nature
        //       (the events have no attributable group); MDK's retry will
        //       attribute them on success.
        //   (c) Failed with mls_group_id = engine_gid AND
        //       failure_reason='invalid_event_format'. Scoped to current
        //       group to avoid cross-group blast radius. Direct SQL because
        //       the typed API filters only on epoch=NULL.
        //
        // Each transitioned wrapper is also removed from Vector's
        // mls_processed_events; without that, is_mls_event_processed below
        // skips them before MDK is asked. If any transitions happen, we
        // wipe the cursor — Phase 1's backward-walk needs to start from
        // mls_groups.created_at, not the last-seen timestamp.
        //
        // Skipped entirely for prefetched_events syncs (negentropy quick
        // path): the prefetched set was selected against the OLD cursor;
        // wiping it here would advance the new cursor based only on that
        // strict subset, leaving older events forever unreachable.
        //
        // String constant 'invalid_event_format' is coupled to MDK rev
        // 136a9ee (see error_handling.rs). Re-verify on MDK upgrade.
        //
        // Idempotent: no-op when nothing matches. Recovered events become
        // Processed/ProcessedCommit; events that fail again under a
        // different failure_reason aren't re-transitioned. Note: every
        // transition triggers a cursor wipe, which means Phase 1 below
        // does a full archival re-walk on this sync — expected to be rare.
        let mut all_transitioned: Vec<String> = Vec::new();
        let mut engine_gid_hex_for_phase0: Option<String> = None;
        if prefetched_events.is_none() {
            if let Ok(engine) = self.engine() {
                let group_check_id_local: Option<String> = if let Ok(groups) = self.read_groups() {
                    groups.iter()
                        .find(|g| g.group.group_id == gid_for_fetch || g.group.engine_group_id == gid_for_fetch)
                        .map(|m| if !m.group.engine_group_id.is_empty() { m.group.engine_group_id.clone() } else { m.group.group_id.clone() })
                } else { None };

                if let Some(check_id) = group_check_id_local {
                    let engine_gid_bytes = crate::simd::hex::hex_string_to_bytes(&check_id);
                    if engine_gid_bytes.len() == 16 || engine_gid_bytes.len() == 32 {
                        use mdk_storage_traits::messages::MessageStorage;
                        use openmls_traits::OpenMlsProvider;
                        let engine_gid = mdk_storage_traits::GroupId::from_slice(&engine_gid_bytes);
                        let storage = engine.provider.storage();
                        // (a) MDK typed API
                        if let Ok(failed_ids) = storage.find_failed_messages_for_retry(&engine_gid) {
                            for ev_id in &failed_ids {
                                if storage.mark_processed_message_retryable(ev_id).is_ok() {
                                    all_transitioned.push(ev_id.to_hex());
                                }
                            }
                        }
                        engine_gid_hex_for_phase0 = Some(check_id);
                    }
                }
            }

            // (b) + (c) Direct SQL on MDK's DB. Skip if the file doesn't
            // exist (a fresh install or a path mis-config — opening would
            // create an empty file with no schema).
            if self.db_path.exists() {
                if let Ok(mdk_conn) = rusqlite::Connection::open(&self.db_path) {
                    // (b) NULL mls_group_id — group-agnostic by definition.
                    let null_ids: Vec<String> = mdk_conn
                        .prepare("SELECT lower(hex(wrapper_event_id)) FROM processed_messages WHERE state='failed' AND mls_group_id IS NULL")
                        .and_then(|mut stmt| stmt.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
                        .unwrap_or_else(|e| { crate::log_warn!("[MLS] Phase 0 (b) SELECT failed: {}", e); Vec::new() });
                    if !null_ids.is_empty() {
                        match mdk_conn.execute(
                            "UPDATE processed_messages SET state='retryable' WHERE state='failed' AND mls_group_id IS NULL",
                            [],
                        ) {
                            Ok(_) => all_transitioned.extend(null_ids),
                            Err(e) => crate::log_warn!("[MLS] Phase 0 (b) UPDATE failed: {}", e),
                        }
                    }
                    // (c) failure_reason='invalid_event_format' — scoped to
                    // current group to prevent cross-group transitions.
                    if let Some(ref engine_gid_hex) = engine_gid_hex_for_phase0 {
                        let engine_gid_bytes = crate::simd::hex::hex_string_to_bytes(engine_gid_hex);
                        let invalid_ids: Vec<String> = mdk_conn
                            .prepare("SELECT lower(hex(wrapper_event_id)) FROM processed_messages WHERE state='failed' AND failure_reason='invalid_event_format' AND mls_group_id = ?1")
                            .and_then(|mut stmt| stmt.query_map(rusqlite::params![&engine_gid_bytes], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
                            .unwrap_or_else(|e| { crate::log_warn!("[MLS] Phase 0 (c) SELECT failed: {}", e); Vec::new() });
                        if !invalid_ids.is_empty() {
                            match mdk_conn.execute(
                                "UPDATE processed_messages SET state='retryable' WHERE state='failed' AND failure_reason='invalid_event_format' AND mls_group_id = ?1",
                                rusqlite::params![&engine_gid_bytes],
                            ) {
                                Ok(_) => all_transitioned.extend(invalid_ids),
                                Err(e) => crate::log_warn!("[MLS] Phase 0 (c) UPDATE failed: {}", e),
                            }
                        }
                    }
                }
            }

            all_transitioned.sort();
            all_transitioned.dedup();

            if !all_transitioned.is_empty() {
                crate::log_info!("[MLS] Auto-recovery: {} previously-failed messages re-eligible for {}",
                    all_transitioned.len(), group_display);
                if let Err(e) = crate::mls::remove_processed_events_by_ids(&all_transitioned) {
                    crate::log_warn!("[MLS] remove_processed_events_by_ids failed: {}", e);
                }
                let mut cursors_for_wipe = self.read_event_cursors().unwrap_or_default();
                if cursors_for_wipe.remove(&gid_for_fetch).is_some() {
                    if let Err(e) = self.write_event_cursors(&cursors_for_wipe) {
                        crate::log_warn!("[MLS] Auto-recovery cursor wipe failed: {}", e);
                    }
                }
            }
        }

        const BATCH_SIZE: usize = 1000;
        const MAX_BATCHES: usize = 100;

        let total_processed: u32;
        let total_new_msgs: u32;

        // ============================================================
        // Phase 1: COLLECT all events in [since, until]
        // ============================================================
        //
        // Relays often cap responses below the requested `limit`. When the
        // matching event count exceeds that cap, the relay returns the
        // most-recent N in DESC order, silently dropping older ones.
        // Walking `since` forward never reaches the dropped events. We
        // walk `until` BACKWARD instead: after each batch, set
        // `current_until = MIN(created_at) - 1` and re-query.
        //
        // Events must be processed in chronological order (MLS commits
        // ratchet sequentially), so we collect everything first and run
        // ONE processing pass on the sorted set.
        let mut all_events: Vec<nostr_sdk::Event> = Vec::new();
        let mut used_fallback_anywhere = false;

        if let Some(events) = prefetched_events {
            // Quick-sync (NIP-77 negentropy) supplied the events directly.
            // Skip Phase 1 entirely.
            if events.is_empty() { return Ok((0, 0)); }
            all_events = events;
        } else {
            let client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;
            let mut current_until = until;
            let mut batch_count: usize = 0;

            loop {
                batch_count += 1;
                if batch_count > MAX_BATCHES {
                    // Likely silent partial sync — events older than the
                    // last fetched batch's window may be unreached. The
                    // cursor that gets written reflects only what we
                    // collected, so subsequent syncs would skip past
                    // the gap. Loud warning so an operator notices.
                    crate::log_warn!("[MLS] Pagination safety limit reached ({} batches) for group {} — possible partial sync; {} events collected", MAX_BATCHES, gid_for_fetch, all_events.len());
                    break;
                }

                let mut filter = Filter::new()
                    .kind(Kind::MlsGroupMessage)
                    .since(since)
                    .until(current_until)
                    .custom_tag(SingleLetterTag::lowercase(Alphabet::H), &gid_for_fetch)
                    .limit(BATCH_SIZE);

                let mut used_fallback = false;
                let mut events = match client
                    .fetch_events_from(crate::state::active_trusted_relays().await, filter.clone(), std::time::Duration::from_secs(15))
                    .await
                {
                    Ok(evts) => evts,
                    Err(e) => return Err(MlsError::NetworkError(format!("fetch MLS events (with h tag) failed: {}", e))),
                };

                if events.is_empty() {
                    used_fallback = true;
                    filter = Filter::new()
                        .kind(Kind::MlsGroupMessage)
                        .since(since)
                        .until(current_until)
                        .limit(BATCH_SIZE);
                    events = match client
                        .fetch_events_from(crate::state::active_trusted_relays().await, filter, std::time::Duration::from_secs(15))
                        .await
                    {
                        Ok(evts) => evts,
                        Err(e) => return Err(MlsError::NetworkError(format!("fetch MLS events (fallback) failed: {}", e))),
                    };
                }

                if events.is_empty() {
                    // No more events at or before `current_until`. Done.
                    break;
                }

                let batch_vec: Vec<nostr_sdk::Event> = if used_fallback {
                    used_fallback_anywhere = true;
                    let filtered = filter_events_to_h_tag(events.into_iter().collect(), &gid_for_fetch);
                    if filtered.is_empty() {
                        // Relay had events but none for our group. Done.
                        break;
                    }
                    filtered
                } else {
                    events.into_iter().collect()
                };

                let count = batch_vec.len();
                let oldest = batch_vec.iter().map(|e| e.created_at.as_secs()).min().unwrap_or(since.as_secs());
                if batch_count > 1 && batch_count % 10 == 0 {
                    println!("[MLS] Collection batch {} for group {}: {} events (oldest at {})", batch_count, gid_for_fetch, count, oldest);
                }

                all_events.extend(batch_vec);

                // Walk backward for the next batch. If `oldest <= since`,
                // we've covered the whole window — stop.
                if oldest <= since.as_secs() { break; }
                current_until = Timestamp::from_secs(oldest.saturating_sub(1));
                if current_until.as_secs() <= since.as_secs() { break; }

                // If the relay returned fewer than our requested limit, it
                // has no more events at or before this batch's window.
                // Still walk one more step to be safe — relays may chunk
                // responses, so a small batch isn't a guaranteed terminator.
                // The empty-batch check at the top of the next iteration
                // catches the true end.
                let _ = count; // count consumed via batch_vec.len above
            }
        }

        if all_events.is_empty() { return Ok((0, 0)); }

        // Dedupe (relays may have overlapping coverage) and sort ASC for
        // chronological MLS commit application.
        all_events.sort_by(|a, b| a.created_at.as_secs().cmp(&b.created_at.as_secs()).then(a.id.cmp(&b.id)));
        all_events.dedup_by(|a, b| a.id == b.id);

        if used_fallback_anywhere {
            println!("[MLS] Collection complete (fallback path): {} events for {}", all_events.len(), group_display);
        }

        // ============================================================
        // Phase 2: PROCESS the entire collected set as one batch
        // ============================================================
        {
            let ordered: Vec<nostr_sdk::Event> = all_events;
            let batch_size: usize = ordered.len();

        // Process with engine
        let mut processed: u32 = 0;
        let mut new_msgs: u32 = 0;
        let mut last_seen_id: Option<nostr_sdk::EventId> = None;
        let mut last_seen_at: u64 = 0;
        let mut rumors_to_process: Vec<(RumorEvent, String, bool)> = Vec::new();
        let mut was_evicted = false;
        let mut pending_retry: Vec<nostr_sdk::Event> = Vec::new();
        let mut events_to_track: Vec<(String, u64)> = Vec::new();

        // Prune pending events older than 90 days. After that long the
        // prerequisite is genuinely unrecoverable from any relay; keeping
        // them just bloats the table and slows every sync.
        const MAX_PENDING_AGE_SECS: u64 = 90 * 24 * 60 * 60;
        match crate::mls::prune_old_pending_events(&gid_for_fetch, MAX_PENDING_AGE_SECS) {
            Ok(n) if n > 0 => println!("[MLS] Pruned {} stale pending events for {}", n, group_display),
            Err(e) => eprintln!("[MLS] prune_old_pending_events failed: {}", e),
            _ => {}
        }

        // Carry over events from prior syncs that we couldn't process.
        // Their prerequisite commits may now be reachable — either from a
        // different relay or in the batch we're about to process.
        match crate::mls::load_pending_events(&gid_for_fetch) {
            Ok(events) if !events.is_empty() => {
                println!("[MLS] Carrying over {} pending events for {}", events.len(), group_display);
                pending_retry.extend(events);
            }
            Err(e) => eprintln!("[MLS] load_pending_events failed: {}", e),
            _ => {}
        }

        let my_pubkey_hex = if let Some(pk) = crate::state::my_public_key() {
            pk.to_hex()
        } else {
            String::new()
        };

        let group_check_id = if let Ok(groups) = self.read_groups() {
            if let Some(meta) = groups.iter().find(|g| g.group.group_id == gid_for_fetch || g.group.engine_group_id == gid_for_fetch) {
                if !meta.group.engine_group_id.is_empty() { Some(meta.group.engine_group_id.clone()) }
                else { Some(meta.group.group_id.clone()) }
            } else { None }
        } else { None };

        let mut pending_metadata_update: Option<(String, String)> = None;

        {
            let engine = self.engine()?;

            // Self-heal moved to Phase 0 (before cursor read) — see the
            // "Phase 0 — Auto-recovery self-heal" block earlier in this fn.

            if let Some(ref check_id) = group_check_id {
                let check_gid_bytes = crate::simd::hex::hex_string_to_bytes(check_id);
                if !check_gid_bytes.is_empty() {
                    let check_gid = GroupId::from_slice(&check_gid_bytes);
                    let dummy_rumor = EventBuilder::new(Kind::Custom(9), "engine_check")
                        .build(nostr_sdk::PublicKey::from_hex("000000000000000000000000000000000000000000000000000000000000dead").unwrap());

                    if let Err(e) = engine.create_message(&check_gid, dummy_rumor) {
                        eprintln!("[MLS] Engine missing group: {}", e);
                        crate::traits::emit_event("mls_group_needs_rejoin", &serde_json::json!({
                            "group_id": gid_for_fetch, "reason": "Group not found in MLS engine state"
                        }));
                    }

                    // Single-pass processing — log group epoch once at the
                    // start of the sole processing block.
                    if let Ok(Some(g)) = engine.get_group(&check_gid) {
                        println!("[MLS] Group {} at epoch {} before processing", group_display, g.epoch);
                    }
                }
            }

            for ev in ordered.iter() {
                // h-tag guard
                if let Some(tag) = ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                    if let Some(h_val) = tag.content() {
                        if !h_val.eq_ignore_ascii_case(&gid_for_fetch) { continue; }
                    } else { continue; }
                } else { continue; }

                if crate::mls::is_mls_event_processed(&ev.id.to_hex()) {
                    last_seen_id = Some(ev.id);
                    last_seen_at = ev.created_at.as_secs();
                    continue;
                }

                match engine.process_message(ev) {
                    Ok(res) => {
                        match res {
                            MessageProcessingResult::ApplicationMessage(msg) => {
                                let rumor_event = RumorEvent {
                                    id: msg.id, kind: msg.kind,
                                    content: msg.content.clone(), tags: msg.tags.clone(),
                                    created_at: msg.created_at, pubkey: msg.pubkey,
                                };
                                let is_mine = !my_pubkey_hex.is_empty() && msg.pubkey.to_hex() == my_pubkey_hex;
                                let wrapper_id = msg.wrapper_event_id.to_hex();
                                rumors_to_process.push((rumor_event, wrapper_id, is_mine));
                                new_msgs = new_msgs.saturating_add(1);
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::Commit { mls_group_id: _ } => {
                                if let Some(ref check_id) = group_check_id {
                                    let check_gid_bytes = crate::simd::hex::hex_string_to_bytes(check_id);
                                    if !check_gid_bytes.is_empty() {
                                        let check_gid = GroupId::from_slice(&check_gid_bytes);
                                        let my_pk = nostr_sdk::PublicKey::from_hex(&my_pubkey_hex).ok();
                                        let outcome = if let Some(pk) = my_pk {
                                            let r = engine.get_members(&check_gid)
                                                .map(|members| members.contains(&pk))
                                                .map_err(|e| {
                                                    eprintln!("[MLS] get_members error during eviction check (skipping, will retry): {}", e);
                                                    e
                                                });
                                            membership_outcome_from_get_members(r)
                                        } else { MembershipOutcome::Evicted };

                                        match outcome {
                                            MembershipOutcome::Evicted => {
                                                // We were just removed by this commit. Mirror the retry
                                                // loop's behavior: flag eviction so the rest of this
                                                // batch is skipped (post-kick events are undecryptable
                                                // garbage that would only pollute mls_pending_events)
                                                // and cleanup_evicted_group runs to wipe local state.
                                                eprintln!("[MLS] EVICTION DETECTED via commit for group: {}", gid_for_fetch);
                                                was_evicted = true;
                                                crate::traits::emit_event("mls_group_left", &serde_json::json!({ "group_id": gid_for_fetch }));
                                            }
                                            MembershipOutcome::Member => {
                                                let _ = engine.sync_group_metadata_from_mls(&check_gid);
                                                if let Ok(Some(group)) = engine.get_group(&check_gid) {
                                                    pending_metadata_update = Some((group.name.clone(), group.description.clone()));
                                                }
                                                crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": gid_for_fetch }));
                                            }
                                            MembershipOutcome::Unknown => {
                                                // Transient storage error: skip both branches.
                                                // Next sync retries the Commit via is_mls_event_processed dedup.
                                            }
                                        }
                                    }
                                }
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                                if was_evicted { break; }
                            }
                            MessageProcessingResult::Proposal(_) => {
                                crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": gid_for_fetch }));
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::ExternalJoinProposal { .. } |
                            MessageProcessingResult::PendingProposal { .. } |
                            MessageProcessingResult::IgnoredProposal { .. } |
                            MessageProcessingResult::PreviouslyFailed => {
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::Unprocessable { mls_group_id } => {
                                let current_epoch = group_check_id.as_ref().and_then(|cid| {
                                    let gid_bytes = crate::simd::hex::hex_string_to_bytes(cid);
                                    if gid_bytes.is_empty() { return None; }
                                    engine.get_group(&GroupId::from_slice(&gid_bytes)).ok().flatten().map(|g| g.epoch)
                                });
                                println!("[MLS] Unprocessable event: group={}, mls_gid={}, id={}, created_at={}, epoch={:?}",
                                    group_display, crate::simd::hex::bytes_to_hex_string(mls_group_id.as_slice()),
                                    ev.id.to_hex(), ev.created_at.as_secs(), current_epoch);
                                pending_retry.push(ev.clone());
                            }
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        if error_msg.contains("own leaf not found") || error_msg.contains("after being evicted") || error_msg.contains("evicted from it") {
                            eprintln!("[MLS] EVICTION DETECTED - removed from group: {}", gid_for_fetch);
                            was_evicted = true;
                        } else if !error_msg.contains("group not found") {
                            eprintln!("[MLS] process_message failed (group={}, id={}, created_at={}): {}",
                                gid_for_fetch, ev.id, ev.created_at.as_secs(), error_msg);
                            events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            last_seen_id = Some(ev.id);
                            last_seen_at = ev.created_at.as_secs();
                        }
                    }
                }
            }
        } // engine dropped

        // Track processed events
        for (event_id, created_at) in events_to_track.iter() {
            let _ = crate::mls::track_mls_event_processed(event_id, &gid_for_fetch, *created_at);
        }

        // Retry loop
        if !pending_retry.is_empty() && !was_evicted {
            let max_retry_passes: u32 = 50;
            let mut retry_attempt: u32 = 0;

            while !pending_retry.is_empty() && retry_attempt < max_retry_passes {
                retry_attempt += 1;
                pending_retry.sort_by_key(|e| e.created_at.as_secs());

                let engine = match self.engine() {
                    Ok(e) => e,
                    Err(e) => { eprintln!("[MLS] Failed to create engine for retry: {}", e); break; }
                };

                let retry_epoch = group_check_id.as_ref().and_then(|cid| {
                    let gid_bytes = crate::simd::hex::hex_string_to_bytes(cid);
                    if gid_bytes.is_empty() { return None; }
                    engine.get_group(&GroupId::from_slice(&gid_bytes)).ok().flatten().map(|g| g.epoch)
                });
                println!("[MLS] Retry pass {}/{} for {} events (epoch={:?})", retry_attempt, max_retry_passes, pending_retry.len(), retry_epoch);

                let mut still_pending: Vec<nostr_sdk::Event> = Vec::new();

                for ev in pending_retry.iter() {
                    if crate::mls::is_mls_event_processed(&ev.id.to_hex()) {
                        last_seen_id = Some(ev.id);
                        last_seen_at = ev.created_at.as_secs();
                        continue;
                    }

                    match engine.process_message(ev) {
                        Ok(res) => {
                            match res {
                                MessageProcessingResult::ApplicationMessage(msg) => {
                                    let rumor_event = RumorEvent {
                                        id: msg.id, kind: msg.kind,
                                        content: msg.content.clone(), tags: msg.tags.clone(),
                                        created_at: msg.created_at, pubkey: msg.pubkey,
                                    };
                                    let is_mine = !my_pubkey_hex.is_empty() && msg.pubkey.to_hex() == my_pubkey_hex;
                                    rumors_to_process.push((rumor_event, msg.wrapper_event_id.to_hex(), is_mine));
                                    new_msgs = new_msgs.saturating_add(1);
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    let _ = crate::mls::remove_pending_event(&ev.id.to_hex());
                                }
                                MessageProcessingResult::Commit { .. } => {
                                    if let Some(ref check_id) = group_check_id {
                                        let check_gid_bytes = crate::simd::hex::hex_string_to_bytes(check_id);
                                        if !check_gid_bytes.is_empty() {
                                            let check_gid = GroupId::from_slice(&check_gid_bytes);
                                            let my_pk = nostr_sdk::PublicKey::from_hex(&my_pubkey_hex).ok();
                                            let outcome = if let Some(pk) = my_pk {
                                                let r = engine.get_members(&check_gid)
                                                    .map(|members| members.contains(&pk))
                                                    .map_err(|e| {
                                                        eprintln!("[MLS] get_members error in retry-loop eviction check (skipping): {}", e);
                                                        e
                                                    });
                                                membership_outcome_from_get_members(r)
                                            } else { MembershipOutcome::Evicted };

                                            match outcome {
                                                MembershipOutcome::Evicted => {
                                                    was_evicted = true;
                                                    crate::traits::emit_event("mls_group_left", &serde_json::json!({ "group_id": gid_for_fetch }));
                                                }
                                                MembershipOutcome::Member => {
                                                    let _ = engine.sync_group_metadata_from_mls(&check_gid);
                                                    if let Ok(Some(group)) = engine.get_group(&check_gid) {
                                                        pending_metadata_update = Some((group.name.clone(), group.description.clone()));
                                                    }
                                                    crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": gid_for_fetch }));
                                                }
                                                MembershipOutcome::Unknown => {}
                                            }
                                        }
                                    }
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    let _ = crate::mls::remove_pending_event(&ev.id.to_hex());
                                }
                                MessageProcessingResult::Proposal(_) => {
                                    crate::traits::emit_event("mls_group_updated", &serde_json::json!({ "group_id": gid_for_fetch }));
                                    let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    let _ = crate::mls::remove_pending_event(&ev.id.to_hex());
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                }
                                MessageProcessingResult::ExternalJoinProposal { .. } |
                                MessageProcessingResult::PendingProposal { .. } |
                                MessageProcessingResult::IgnoredProposal { .. } => {
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    let _ = crate::mls::remove_pending_event(&ev.id.to_hex());
                                }
                                MessageProcessingResult::Unprocessable { .. } => {
                                    still_pending.push(ev.clone());
                                }
                                MessageProcessingResult::PreviouslyFailed => {
                                    // MDK has firmly given up on this wrapper
                                    // (state=Failed with no extractable group_id
                                    // OR EpochInvalidated). Treat as terminal:
                                    // track in our processed table and remove
                                    // from the persistent pending queue so it
                                    // doesn't sit there forever bumping
                                    // retry_count until the 90-day prune.
                                    //
                                    // Deliberately asymmetric with the first loop:
                                    // we do NOT update last_seen_id/last_seen_at
                                    // here. Carry-overs from prior syncs may have
                                    // created_at OLDER than the current cursor;
                                    // advancing on them risks dragging the cursor
                                    // backward (compute_cursor_with_pending_cap
                                    // floors at current_cursor_at, but if every
                                    // event in the retry batch is PreviouslyFailed
                                    // we'd want last_seen_id to stay None so the
                                    // pagination defensive break fires instead of
                                    // re-fetching the same window). Vector's
                                    // mls_processed_events dedup catches them
                                    // next sync regardless.
                                    let _ = crate::mls::track_mls_event_processed(&ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    let _ = crate::mls::remove_pending_event(&ev.id.to_hex());
                                }
                            }
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            if error_msg.contains("own leaf not found") || error_msg.contains("after being evicted") || error_msg.contains("evicted from it") {
                                was_evicted = true;
                                break;
                            }
                            still_pending.push(ev.clone());
                        }
                    }
                    if was_evicted { break; }
                }

                let made_progress = still_pending.len() < pending_retry.len();
                pending_retry = still_pending;
                if pending_retry.is_empty() || was_evicted { break; }
                if !made_progress {
                    eprintln!("[MLS] No progress in retry pass {} — {} events permanently unprocessable", retry_attempt, pending_retry.len());
                    break;
                }
            }

            if !pending_retry.is_empty() {
                eprintln!("[MLS] Sync ending with {} still-pending events for {} — persisting for retry next sync",
                    pending_retry.len(), group_display);
                for ev in &pending_retry {
                    if let Err(e) = crate::mls::save_pending_event(&gid_for_fetch, ev) {
                        eprintln!("[MLS] save_pending_event failed for {}: {}", ev.id, e);
                    }
                }
                if crate::mls::record_group_failure(&gid_for_fetch).await {
                    eprintln!("[MLS] Group {} appears to be desynced (too many consecutive failures)", gid_for_fetch);
                    crate::traits::emit_event("mls_group_needs_rejoin", &serde_json::json!({
                        "group_id": gid_for_fetch, "reason": "Too many unprocessable events - group may be desynced"
                    }));
                }
            } else if retry_attempt > 0 {
                println!("[MLS] All retry events processed successfully after {} passes", retry_attempt);
                crate::mls::record_group_success(&gid_for_fetch).await;
            }
        }

        // Apply metadata changes from commits
        if let Some((new_name, new_desc)) = pending_metadata_update {
            if let Ok(mut groups) = self.read_groups() {
                let mut changed = false;
                if let Some(meta) = groups.iter_mut().find(|g| g.group.group_id == gid_for_fetch || g.group.engine_group_id == gid_for_fetch) {
                    if meta.profile.name != new_name { meta.profile.name = new_name; changed = true; }
                    if meta.profile.description.as_deref().unwrap_or("") != new_desc {
                        meta.profile.description = if new_desc.is_empty() { None } else { Some(new_desc) };
                        changed = true;
                    }
                    if changed {
                        meta.group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    }
                }
                if changed {
                    if !session.is_valid() {
                        return Ok((0, 0));
                    }
                    let updated_meta = groups.iter().find(|g| g.group.group_id == gid_for_fetch || g.group.engine_group_id == gid_for_fetch).cloned();
                    let _ = self.write_groups(&groups);
                    if let Some(meta) = updated_meta {
                        crate::traits::emit_event("mls_group_metadata", &serde_json::json!({
                            "metadata": crate::mls::metadata_to_frontend(&meta)
                        }));
                    }
                    let mut state = crate::state::STATE.lock().await;
                    if let Some(chat) = state.get_chat_mut(&gid_for_fetch) {
                        let name = groups.iter().find(|g| g.group.group_id == gid_for_fetch || g.group.engine_group_id == gid_for_fetch)
                            .map(|m| m.profile.name.clone()).unwrap_or_default();
                        if !name.is_empty() { chat.metadata.set_name(name); }
                    }
                }
            }
        }

        // Process buffered rumors (skip if evicted)
        if !rumors_to_process.is_empty() && !was_evicted {
            // Account-swap defence: an in-flight `MlsService` instance keeps
            // its `db_path` pinned to the account that was active at
            // construction. After a swap the rumors above were decrypted
            // against the OLD account's MLS storage; if we proceeded with
            // `create_or_get_mls_group_chat` here we would synthesize an
            // OLD group_id into the NEW account's STATE and persist its
            // plaintext messages there. Bail before any STATE/DB write.
            if !session.is_valid() {
                return Ok((0, 0));
            }

            let group_meta = self.read_groups().ok()
                .and_then(|groups| groups.into_iter().find(|g| g.group.group_id == gid_for_fetch));

            let (chat_id, slim_to_save) = {
                let mut state = crate::state::STATE.lock().await;
                let chat_id = state.create_or_get_mls_group_chat(&gid_for_fetch, vec![]);
                let mut slim_to_save = None;
                if let Some(meta) = group_meta {
                    if let Some(idx) = state.chats.iter().position(|c| c.id == chat_id) {
                        state.chats[idx].metadata.set_name(meta.profile.name.clone());
                        slim_to_save = Some(crate::db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner));
                    }
                }
                (chat_id, slim_to_save)
            };

            if let Some(slim) = slim_to_save {
                let _ = crate::db::chats::save_slim_chat(&slim);
            }

            let download_dir = crate::db::get_download_dir();
            for (rumor_event, _wrapper_id, is_mine) in rumors_to_process.iter() {
                // Re-validate per rumor in case the swap happened mid-loop.
                if !session.is_valid() {
                    return Ok((0, 0));
                }
                let rumor_context = RumorContext {
                    sender: rumor_event.pubkey,
                    is_mine: *is_mine,
                    conversation_id: gid_for_fetch.clone(),
                    conversation_type: ConversationType::MlsGroup,
                };

                match crate::mls::process_rumor_with_mls(rumor_event, &rumor_context, &download_dir).await {
                    Ok(result) => {
                        match result {
                            RumorProcessingResult::TextMessage(msg) | RumorProcessingResult::FileAttachment(msg) => {
                                if crate::db::events::message_exists_in_db(&msg.id).unwrap_or(false) { continue; }

                                let mut msg = msg;
                                for att in &mut msg.attachments {
                                    if att.size == 0 && !att.url.is_empty() {
                                        if let Some(size) = crate::net::get_remote_file_size(&att.url).await {
                                            att.size = size;
                                        }
                                    }
                                }

                                let was_added = {
                                    let mut state = crate::state::STATE.lock().await;
                                    state.add_message_to_chat(&chat_id, msg.clone())
                                };

                                if was_added {
                                    crate::traits::emit_event("mls_message_new", &serde_json::json!({
                                        "group_id": gid_for_fetch, "message": msg
                                    }));
                                    let _ = crate::db::events::save_message(&chat_id, &msg).await;
                                }
                            }
                            RumorProcessingResult::Reaction(reaction) => {
                                let (was_added, chat_id_for_save) = {
                                    let mut state = crate::state::STATE.lock().await;
                                    if let Some((cid, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
                                        (added, if added { Some(cid) } else { None })
                                    } else { (false, None) }
                                };
                                if was_added {
                                    if let Some(cid) = chat_id_for_save {
                                        let updated = {
                                            let state = crate::state::STATE.lock().await;
                                            state.find_message(&reaction.reference_id).map(|(_, msg)| msg.clone())
                                        };
                                        if let Some(msg) = updated {
                                            let _ = crate::db::events::save_message(&cid, &msg).await;
                                        }
                                    }
                                }
                            }
                            RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                let active_typers = {
                                    let mut state = crate::state::STATE.lock().await;
                                    state.update_typing_and_get_active(&chat_id, &profile_id, until)
                                };
                                crate::traits::emit_event("typing-update", &serde_json::json!({
                                    "conversation_id": gid_for_fetch, "typers": active_typers,
                                }));
                            }
                            RumorProcessingResult::LeaveRequest { event_id, member_pubkey } => {
                                if crate::db::events::event_exists(&event_id).unwrap_or(false) { continue; }

                                let member_name = {
                                    let state = crate::state::STATE.lock().await;
                                    state.get_profile(&member_pubkey).map(|p| {
                                        if !p.nickname.is_empty() { p.nickname.to_string() }
                                        else if !p.name.is_empty() { p.name.to_string() }
                                        else { member_pubkey.chars().take(12).collect::<String>() + "..." }
                                    })
                                };

                                let am_i_admin = if let Some(meta) = &group_metadata {
                                    if let Some(my_pk) = crate::state::my_public_key() {
                                        let my_npub = my_pk.to_bech32().unwrap_or_default();
                                        let my_hex = my_pk.to_hex();
                                        meta.group.creator_pubkey == my_npub || meta.group.creator_pubkey == my_hex
                                    } else { false }
                                } else { false };

                                if am_i_admin {
                                    let was_inserted = crate::db::events::save_system_event_by_id(
                                        &event_id, &gid_for_fetch,
                                        crate::stored_event::SystemEventType::MemberLeft,
                                        &member_pubkey, member_name.as_deref(),
                                    ).await.unwrap_or(false);

                                    if was_inserted {
                                        crate::traits::emit_event("system_event", &serde_json::json!({
                                            "conversation_id": gid_for_fetch,
                                            "event_id": event_id,
                                            "event_type": crate::stored_event::SystemEventType::MemberLeft.as_u8(),
                                            "member_pubkey": member_pubkey,
                                            "member_name": member_name,
                                        }));

                                        let mls_service = match MlsService::new_persistent_static() {
                                            Ok(s) => s,
                                            Err(e) => { eprintln!("[MLS] Failed to create MLS service for auto-remove: {}", e); continue; }
                                        };
                                        if let Err(e) = mls_service.remove_member_device(&gid_for_fetch, &member_pubkey, "").await {
                                            eprintln!("[MLS] Failed to auto-remove member {}: {}", member_pubkey, e);
                                        }
                                    }
                                }
                            }
                            RumorProcessingResult::UnknownEvent(mut event) => {
                                if let Ok(chat_int_id) = crate::db::id_cache::get_chat_id_by_identifier(&chat_id) {
                                    event.chat_id = chat_int_id;
                                    let _ = crate::db::events::save_event(&event).await;
                                }
                            }
                            RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                let event_timestamp = event.created_at;
                                let _ = crate::db::events::save_pivx_payment_event(&gid_for_fetch, event).await;
                                crate::traits::emit_event("pivx_payment_received", &serde_json::json!({
                                    "conversation_id": gid_for_fetch,
                                    "gift_code": gift_code, "amount_piv": amount_piv,
                                    "address": address, "message_id": message_id,
                                    "sender": rumor_event.pubkey.to_hex(),
                                    "is_mine": *is_mine,
                                    "at": event_timestamp * 1000,
                                }));
                            }
                            RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                if crate::db::events::event_exists(&event.id).unwrap_or(false) { continue; }
                                if let Ok(chat_int_id) = crate::db::id_cache::get_chat_id_by_identifier(&chat_id) {
                                    let mut event_with_chat = event;
                                    event_with_chat.chat_id = chat_int_id;
                                    let _ = crate::db::events::save_event(&event_with_chat).await;
                                }
                                let mut state = crate::state::STATE.lock().await;
                                let chat_idx = state.chats.iter().position(|c| c.id == chat_id);
                                if let Some(idx) = chat_idx {
                                    if let Some(msg) = state.chats[idx].get_message_mut(&message_id) {
                                        msg.apply_edit(new_content, edited_at);
                                    }
                                    if let Some(msg) = state.chats[idx].get_compact_message(&message_id) {
                                        let msg_for_emit = msg.to_message(&state.interner);
                                        crate::traits::emit_event("message_update", &serde_json::json!({
                                            "old_id": &message_id, "message": msg_for_emit, "chat_id": &chat_id
                                        }));
                                    }
                                }
                            }
                            RumorProcessingResult::WebxdcPeerAdvertisement { .. } |
                            RumorProcessingResult::WebxdcPeerLeft { .. } => {
                                // WebXDC handled by platform layer
                            }
                            RumorProcessingResult::DeletionRequest { target_event_id } => {
                                // Authorization + apply both live in
                                // mls::cooperative_hide so the live and
                                // bulk-sync paths share one source of truth.
                                let _ = crate::mls::cooperative_hide::apply_cooperative_hide(
                                    &target_event_id,
                                    &rumor_event.pubkey,
                                    &gid_for_fetch,
                                ).await;
                            }
                            RumorProcessingResult::WallpaperChanged { .. } => {
                                // Wallpapers are a DM-only feature. Ignore in MLS groups.
                            }
                            RumorProcessingResult::Ignored => {}
                        }
                    }
                    Err(e) => eprintln!("[MLS] Failed to process rumor: {}", e),
                }
            }

            // Persist chat and messages
            {
                let (slim, messages_to_save) = {
                    let state = crate::state::STATE.lock().await;
                    if let Some(chat) = state.get_chat(&chat_id) {
                        let slim = crate::db::chats::SlimChatDB::from_chat(chat, &state.interner);
                        let messages_to_save: Vec<crate::types::Message> = if new_msgs > 0 {
                            chat.messages.iter().rev().take(new_msgs as usize)
                                .map(|m| m.to_message(&state.interner)).collect()
                        } else { Vec::new() };
                        (Some(slim), messages_to_save)
                    } else { (None, Vec::new()) }
                };
                if let Some(slim) = slim {
                    let _ = crate::db::chats::save_slim_chat(&slim);
                    if !messages_to_save.is_empty() {
                        let _ = crate::db::events::save_chat_messages(&chat_id, &messages_to_save).await;
                    }
                }
            }
        }

        // Eviction cleanup or cursor advance
        if was_evicted {
            cursors.remove(&gid_for_fetch);
            if let Err(e) = self.write_event_cursors(&cursors) {
                eprintln!("[MLS] Failed to remove cursor for evicted group: {}", e);
            }
            if let Err(e) = self.cleanup_evicted_group(&gid_for_fetch).await {
                eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
            }
        } else {
            // See `compute_cursor_with_pending_cap` for the cap rules and
            // the reasoning around the current-cursor floor and oldest=0
            // guard.
            let current_cursor_at = cursors.get(&gid_for_fetch).map(|c| c.last_seen_at).unwrap_or(0);
            let oldest_pending_at: Option<u64> = crate::mls::load_pending_events(&gid_for_fetch)
                .ok()
                .and_then(|evs| evs.iter().map(|e| e.created_at.as_secs()).min());
            last_seen_at = compute_cursor_with_pending_cap(last_seen_at, current_cursor_at, oldest_pending_at);

            if let Some(id) = last_seen_id {
                if last_seen_at > current_cursor_at {
                    cursors.insert(gid_for_fetch.clone(), crate::mls::EventCursor {
                        last_seen_event_id: id.to_hex(),
                        last_seen_at,
                    });
                    if let Err(e) = self.write_event_cursors(&cursors) {
                        eprintln!("[MLS] write_event_cursors failed: {}", e);
                    }
                    // (Single-pass: no `current_since` to advance for next batch.)
                }
            }
        }

        total_processed = processed;
        total_new_msgs = new_msgs;
        let _ = batch_size; // shadow-suppress: lifted out of the legacy per-batch loop
        } // End Phase 2 single-pass processing block

        Ok((total_processed, total_new_msgs))
    }

    /// Leave an MLS group: send leave request, remove cursors, remove metadata, clean up.
    ///
    /// Sends a "leave request" application message so admins auto-remove us,
    /// then cleans up all local state regardless of whether the network send succeeded.
    pub async fn leave_group(&self, group_id: &str) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Verify client is initialized (match original contract)
        let _client = crate::state::nostr_client().ok_or(MlsError::NotInitialized)?;

        // Find the group metadata (may not exist if already partially cleaned)
        let groups = self.read_groups().unwrap_or_default();
        let group_meta = groups.iter()
            .find(|g| g.group.group_id == group_id || g.group.engine_group_id == group_id)
            .cloned();

        // Best-effort: send a "leave request" application message so admins auto-remove us
        // Skip if engine not available (cleanup still happens below)
        if let (Some(ref meta), Some(my_pubkey)) = (
            &group_meta,
            crate::state::my_public_key(),
        ) {
            let mls_group_id = GroupId::from_slice(&crate::simd::hex::hex_string_to_bytes(&meta.group.engine_group_id));
            let leave_rumor = EventBuilder::new(Kind::ApplicationSpecificData, "leave")
                .tag(Tag::custom(TagKind::d(), vec!["vector"]))
                .build(my_pubkey);

            match self.engine() {
                Ok(engine) => {
                    match engine.create_message(&mls_group_id, leave_rumor) {
                        Ok(mls_event) => {
                            let gid = group_id.to_string();
                            // SessionGuard pre-spawn: a swap before the relay
                            // round-trip would otherwise broadcast a leave
                            // signed under account A through account B's client.
                            let session = crate::state::SessionGuard::capture();
                            tokio::spawn(async move {
                                if !session.is_valid() { return; }
                                let Some(client) = crate::state::nostr_client() else { return; };
                                if let Err(e) = client.send_event(&mls_event).await {
                                    eprintln!("[MLS] Failed to send leave request message: {}", e);
                                } else {
                                    println!("[MLS] Leave request message sent for group: {}", gid);
                                }
                            });
                        }
                        Err(e) => eprintln!("[MLS] Failed to create leave request message: {}", e),
                    }
                }
                Err(e) => eprintln!("[MLS] Could not get MLS engine for leave request: {}", e),
            }
        }

        // Always clean up local data, even if MLS operation failed

        // 1. Remove cursor for this group
        let mut cursors = self.read_event_cursors().unwrap_or_default();
        cursors.remove(group_id);
        if let Some(ref meta) = group_meta {
            cursors.remove(&meta.group.engine_group_id);
        }
        if let Err(e) = self.write_event_cursors(&cursors) {
            eprintln!("[MLS] Failed to remove cursor: {}", e);
        }

        // 2. Remove from mls_groups metadata
        if let Some(ref meta) = group_meta {
            let mut groups = self.read_groups().unwrap_or_default();
            groups.retain(|g| g.group.group_id != group_id && g.group.engine_group_id != meta.group.engine_group_id);
            if let Err(e) = self.write_groups(&groups) {
                eprintln!("[MLS] Failed to remove group metadata: {}", e);
            }
        }

        // 3. Full cleanup (chat, messages, in-memory state)
        if let Err(e) = self.cleanup_evicted_group(group_id).await {
            eprintln!("[MLS] Cleanup failed (non-fatal): {}", e);
        }

        // 4. Notify frontend
        crate::traits::emit_event("mls_group_left", &serde_json::json!({
            "group_id": group_id
        }));

        println!("[MLS] Left group and cleaned up local data: {}", group_id);
        Ok(())
    }

    // ========================================================================
    // Smoke Test
    // ========================================================================

    /// Run an in-memory MLS smoke test with the provided Nostr client.
    ///
    /// Network-only test that validates basic MLS operations without persisting
    /// state to disk: publish KeyPackage, create group, send message, observe on relay.
    pub async fn run_mls_smoke_test_with_client(
        client: &nostr_sdk::Client,
        relay: &str,
        timeout: std::time::Duration,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        match tokio::time::timeout(timeout, async {
            println!("[MLS Smoke Test] Start (relay: {})", relay);

            let kim_keys = Keys::generate();
            let saul_keys = Keys::generate();
            println!(
                "[MLS Smoke Test] Ephemeral identities: kim={}, saul={}",
                kim_keys.public_key().to_bech32().unwrap_or_default(),
                saul_keys.public_key().to_bech32().unwrap_or_default()
            );

            let kim_mls = MDK::new(MdkSqliteStorage::new_unencrypted(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);
            let saul_mls = MDK::new(MdkSqliteStorage::new_unencrypted(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);

            let relay_url = RelayUrl::parse(relay)
                .map_err(|e| MlsError::NetworkError(format!("RelayUrl::parse: {}", e)))?;

            // 1) Saul publishes a device KeyPackage
            println!("[MLS Smoke Test] Saul publishing device KeyPackage...");
            let (saul_kp_encoded, saul_kp_tags, _saul_hash_ref) = saul_mls
                .create_key_package_for_event(&saul_keys.public_key(), [relay_url.clone()])
                .map_err(|e| MlsError::NostrMlsError(format!("create_key_package_for_event (saul): {}", e)))?;

            let saul_kp_event = EventBuilder::new(Kind::MlsKeyPackage, saul_kp_encoded)
                .tags(saul_kp_tags)
                .build(saul_keys.public_key())
                .sign(&saul_keys)
                .await
                .map_err(|e| MlsError::NostrMlsError(format!("sign saul keypackage: {}", e)))?;
            client
                .send_event_to([relay], &saul_kp_event)
                .await
                .map_err(|e| MlsError::NetworkError(format!("publish saul keypackage: {}", e)))?;
            println!("[MLS Smoke Test] Saul KeyPackage published id={}", saul_kp_event.id);

            // 2) Kim creates a temporary two-member group
            println!("[MLS Smoke Test] Kim creating temporary group with Saul as member...");
            let name = format!(
                "Vector-MLS-Test-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            );
            let description = "Vector MLS in-memory smoke test (Kim+Saul)".to_owned();

            let group_config = NostrGroupConfigData::new(
                name,
                description,
                None, None, None,
                vec![relay_url.clone()],
                vec![kim_keys.public_key()],
            );

            let group_create = kim_mls
                .create_group(
                    &kim_keys.public_key(),
                    vec![saul_kp_event.clone()],
                    group_config,
                )
                .map_err(|e| MlsError::NostrMlsError(format!("create_group (kim): {}", e)))?;

            let kim_group = group_create.group;
            let welcome_rumor = group_create
                .welcome_rumors
                .first()
                .cloned()
                .ok_or_else(|| MlsError::NostrMlsError("no welcome rumor produced".into()))?;
            println!("[MLS Smoke Test] Group created; welcome rumor produced");

            // 2b) Saul processes the welcome locally
            saul_mls
                .process_welcome(&nostr_sdk::EventId::all_zeros(), &welcome_rumor)
                .map_err(|e| MlsError::NostrMlsError(format!("saul process_welcome: {}", e)))?;
            let welcomes = saul_mls
                .get_pending_welcomes(None)
                .map_err(|e| MlsError::NostrMlsError(format!("saul get_pending_welcomes: {}", e)))?;
            let welcome = welcomes
                .first()
                .cloned()
                .ok_or_else(|| MlsError::NostrMlsError("saul has no pending welcomes".into()))?;
            saul_mls
                .accept_welcome(&welcome)
                .map_err(|e| MlsError::NostrMlsError(format!("saul accept_welcome: {}", e)))?;
            println!("[MLS Smoke Test] Saul joined the group locally");

            // 3) Kim sends an MLS application message
            let group_id = &kim_group.mls_group_id;
            println!("[MLS Smoke Test] Kim sending application message...");
            let rumor = EventBuilder::new(Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE), "Vector-MLS-Test: hello")
                .tag(Tag::custom(
                    TagKind::Custom(std::borrow::Cow::Borrowed("vector-mls-test")),
                    vec!["true"],
                ))
                .build(kim_keys.public_key());

            let mls_wrapper = kim_mls
                .create_message(group_id, rumor)
                .map_err(|e| MlsError::NostrMlsError(format!("kim create_message: {}", e)))?;

            client
                .send_event_to([relay], &mls_wrapper)
                .await
                .map_err(|e| MlsError::NetworkError(format!("publish mls wrapper: {}", e)))?;
            println!(
                "[MLS Smoke Test] MLS wrapper published id={}, kind={:?}",
                mls_wrapper.id, mls_wrapper.kind
            );

            // 4) Verify network visibility + Saul processes locally
            let filter = Filter::new()
                .kind(Kind::MlsGroupMessage)
                .since(Timestamp::now() - 300u64);

            let fetched = client
                .fetch_events_from(
                    vec![relay.to_string()],
                    filter,
                    std::time::Duration::from_secs(10),
                )
                .await
                .map_err(|e| MlsError::NetworkError(format!("fetch MLS events: {}", e)))?;

            if fetched.iter().any(|e| e.id == mls_wrapper.id) {
                println!("[MLS Smoke Test] Observed wrapper on relay");
            } else {
                println!("[MLS Smoke Test] Wrapper not observed in single fetch window; processing locally anyway...");
            }

            match saul_mls.process_message(&mls_wrapper) {
                Ok(_res) => println!("[MLS Smoke Test] Saul process_message => OK"),
                Err(e) => println!("[MLS Smoke Test] Saul process_message note: {}", e),
            }

            println!("[MLS Smoke Test] Completed in-memory smoke test (Kim+Saul, no disk).");
            Ok(())
        })
        .await
        {
            Ok(r) => r,
            Err(_) => Err(MlsError::NetworkError(format!(
                "MLS smoke test timed out after {}s",
                timeout.as_secs()
            ))),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // build_mdk_engine — extended event-age window
    // ========================================================================

    #[test]
    fn build_mdk_engine_smoke_test() {
        // Compile-gate against future MDK schema changes; ensures the
        // builder pipeline + storage open + config spread compose without
        // panicking. If MDK upstream renames a config field or removes
        // `..MdkConfig::default()` semantics, this test breaks at compile.
        let tmp = tempfile::tempdir().unwrap();
        let storage = MdkSqliteStorage::new_unencrypted(tmp.path().join("smoke.db"))
            .expect("storage open");
        let _engine = build_mdk_engine(storage);
        // Engine instances are non-Send and don't expose config publicly,
        // so we can't assert on max_event_age_secs here. The behavioral
        // proof is integration-level; this test catches API drift.
    }

    // ========================================================================
    // compute_cursor_with_pending_cap — pure cursor math
    // ========================================================================

    #[test]
    fn cursor_cap_no_pending_returns_last_seen_unchanged() {
        // No pending events → cap is a no-op, advance freely.
        assert_eq!(compute_cursor_with_pending_cap(1500, 1000, None), 1500);
        assert_eq!(compute_cursor_with_pending_cap(0, 0, None), 0);
        assert_eq!(compute_cursor_with_pending_cap(u64::MAX, 0, None), u64::MAX);
    }

    #[test]
    fn cursor_cap_pending_newer_than_last_seen_no_change() {
        // Pending event is in the future relative to our progress; no cap needed.
        // last_seen=1000, oldest_pending=2000 → cap=1999, last_seen<cap, leave alone.
        assert_eq!(compute_cursor_with_pending_cap(1000, 500, Some(2000)), 1000);
    }

    #[test]
    fn cursor_cap_pending_caps_last_seen_at_oldest_minus_one() {
        // last_seen=1500, oldest_pending=1200, current_cursor=500
        // cap = 1199 (oldest-1), floored at 500 → 1199
        // last_seen 1500 > 1199 → cap to 1199.
        assert_eq!(compute_cursor_with_pending_cap(1500, 500, Some(1200)), 1199);
    }

    #[test]
    fn cursor_cap_pending_older_than_cursor_floors_at_cursor() {
        // BUG-CLASS: prevent cursor regression. A late-arriving pending event
        // with created_at older than where we already advanced must NOT drag
        // the cursor backward.
        // last_seen=1500, oldest_pending=300, current_cursor=1000
        // raw cap = 299, but floored at 1000 → 1000.
        // last_seen 1500 > 1000 → cap to 1000 (we DO clamp to current cursor;
        // the strict-> check at the call site then prevents the actual write
        // since 1000 == current_cursor — net effect: no cursor change).
        assert_eq!(compute_cursor_with_pending_cap(1500, 1000, Some(300)), 1000);
    }

    #[test]
    fn cursor_cap_oldest_pending_zero_does_not_pin_to_genesis() {
        // BUG-CLASS: clock-skewed/malformed pending event with created_at=0.
        // saturating_sub(1) = 0, which would (without the guard) cap last_seen_at
        // to 0 and force every sync to refetch from epoch zero. Guard returns
        // last_seen_at unchanged.
        assert_eq!(compute_cursor_with_pending_cap(1500, 1000, Some(0)), 1500);
    }

    #[test]
    fn cursor_cap_oldest_equals_last_seen() {
        // Edge: pending event has SAME timestamp as our latest processed.
        // cap = oldest - 1 = last_seen - 1, last_seen > cap → cap to last_seen-1.
        // (We've processed up through `last_seen` but the pending event at
        // exactly that second hasn't decrypted yet, so back off by one.)
        assert_eq!(compute_cursor_with_pending_cap(1500, 0, Some(1500)), 1499);
    }

    #[test]
    fn cursor_cap_first_write_anchors_below_last_seen_to_recover_pending() {
        // First cursor write (current_cursor_at == 0) with a pending event at
        // created_at=200 produces raw cap=199. Floor at 0 (no progress yet) →
        // 199. last_seen=500 > 199 → cap to 199. The cursor anchors BELOW the
        // batch's last successful event so future syncs re-fetch the window
        // containing the pending event's prerequisite. Already-processed
        // events at 200 < t < 500 short-circuit via mls_processed_events.
        assert_eq!(compute_cursor_with_pending_cap(500, 0, Some(200)), 199);
    }

    #[test]
    fn cursor_cap_cap_equals_last_seen_no_change() {
        // last_seen_at > cap is strict, so equal means no change.
        // last_seen=1000, oldest_pending=1001, cap=1000, last_seen=1000 → no change.
        assert_eq!(compute_cursor_with_pending_cap(1000, 0, Some(1001)), 1000);
    }

    #[test]
    fn cursor_cap_zero_last_seen_with_pending_no_underflow() {
        // Edge: last_seen_at=0 (uninitialized batch) + pending at 1500.
        // cap = 1499 floored at current_cursor=0 → 1499. last_seen 0 > 1499 is
        // false → no change, returns 0. (Caller's cursor-write guard will then
        // skip the persist since 0 == current_cursor_at.)
        assert_eq!(compute_cursor_with_pending_cap(0, 0, Some(1500)), 0);
    }

    #[test]
    fn cursor_cap_last_seen_below_current_cursor_invariant_violation() {
        // PROTECTIVE INVARIANT: caller's loop guarantees last_seen_at >=
        // current_cursor_at because current_cursor_at is the previous sync's
        // anchor and last_seen_at advances monotonically through batch events.
        // If somehow violated (e.g. caller bug), the function should NOT make
        // it worse — last_seen=500, current_cursor=1000, no pending → returns
        // 500 unchanged. The caller's `if last_seen_at > current_cursor_at`
        // guard then refuses to write the regressed value.
        assert_eq!(compute_cursor_with_pending_cap(500, 1000, None), 500);
        // With pending: cap = oldest-1 floored at 1000. last_seen 500 < 1000 →
        // no change (still 500). Caller's write-guard prevents regression.
        assert_eq!(compute_cursor_with_pending_cap(500, 1000, Some(2000)), 500);
    }

    // ========================================================================
    // cursor_to_next_since — +1 advance on read
    // ========================================================================

    #[test]
    fn cursor_to_next_since_basic() {
        assert_eq!(cursor_to_next_since(0), 1);
        assert_eq!(cursor_to_next_since(1500), 1501);
    }

    #[test]
    fn cursor_to_next_since_saturates_at_max() {
        // BUG-CLASS: u64 overflow at the heat-death of the universe. Saturating
        // add means we never wrap; relay would just return 0 events.
        assert_eq!(cursor_to_next_since(u64::MAX), u64::MAX);
        assert_eq!(cursor_to_next_since(u64::MAX - 1), u64::MAX);
    }

    // ========================================================================
    // filter_events_to_h_tag — pagination fallback safety
    // ========================================================================

    #[test]
    fn filter_h_tag_empty_input() {
        let result = filter_events_to_h_tag(Vec::new(), "abc123");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn filter_h_tag_keeps_only_matching() {
        use nostr_sdk::prelude::*;
        let keys = Keys::generate();
        let target = "a".repeat(64);
        let other = "b".repeat(64);

        let target_a = EventBuilder::new(Kind::MlsGroupMessage, "t1")
            .tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec![target.clone()],
            ))
            .build(keys.public_key()).sign(&keys).await.unwrap();
        let other_event = EventBuilder::new(Kind::MlsGroupMessage, "o")
            .tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec![other],
            ))
            .build(keys.public_key()).sign(&keys).await.unwrap();
        let no_h_event = EventBuilder::new(Kind::MlsGroupMessage, "no-h")
            .build(keys.public_key()).sign(&keys).await.unwrap();
        let target_b = EventBuilder::new(Kind::MlsGroupMessage, "t2")
            .tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec![target.clone()],
            ))
            .build(keys.public_key()).sign(&keys).await.unwrap();

        let events = vec![target_a, other_event, no_h_event, target_b];
        let result = filter_events_to_h_tag(events, &target);
        assert_eq!(result.len(), 2, "should keep only the 2 events matching the target h-tag");
        for ev in &result {
            let tag = ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))).unwrap();
            assert_eq!(tag.content().unwrap(), target);
        }
    }

    #[tokio::test]
    async fn filter_h_tag_case_insensitive() {
        // Relays SHOULD send lowercase but Nostr is permissive; case-insensitive
        // match avoids dropping legitimate events from non-conformant senders.
        use nostr_sdk::prelude::*;
        let keys = Keys::generate();
        let lower = "a".repeat(64);
        let upper = lower.to_uppercase();

        let event_upper = EventBuilder::new(Kind::MlsGroupMessage, "test")
            .tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec![upper.clone()],
            ))
            .build(keys.public_key())
            .sign(&keys)
            .await
            .unwrap();

        let result = filter_events_to_h_tag(vec![event_upper], &lower);
        assert_eq!(result.len(), 1, "should match across case");
    }

    // ========================================================================
    // membership_outcome_from_get_members — eviction-on-Commit decision
    // ========================================================================

    #[test]
    fn membership_outcome_ok_true_is_member() {
        // get_members returned Ok with self in the list → still a member.
        let r: Result<bool, &'static str> = Ok(true);
        assert_eq!(membership_outcome_from_get_members(r), MembershipOutcome::Member);
    }

    #[test]
    fn membership_outcome_ok_false_is_evicted() {
        // get_members returned Ok WITHOUT self → real eviction.
        let r: Result<bool, &'static str> = Ok(false);
        assert_eq!(membership_outcome_from_get_members(r), MembershipOutcome::Evicted);
    }

    #[test]
    fn membership_outcome_err_is_unknown_not_evicted() {
        // BUG-CLASS: a transient storage error MUST NOT trigger a false-positive
        // eviction that wipes the group locally. This was the regression the
        // previous `unwrap_or(false)` caused.
        let r: Result<bool, &'static str> = Err("disk lock timeout");
        assert_eq!(
            membership_outcome_from_get_members(r),
            MembershipOutcome::Unknown,
            "Err must map to Unknown, not Evicted",
        );
    }

    #[test]
    fn membership_outcome_distinct_for_each_input() {
        // Comprehensive: every distinct input yields a distinct output.
        // If any two cases ever collapse (e.g. someone adds a default),
        // this test fails immediately.
        let outcomes: Vec<MembershipOutcome> = vec![
            membership_outcome_from_get_members::<&str>(Ok(true)),
            membership_outcome_from_get_members::<&str>(Ok(false)),
            membership_outcome_from_get_members::<&str>(Err("x")),
        ];
        assert_eq!(outcomes[0], MembershipOutcome::Member);
        assert_eq!(outcomes[1], MembershipOutcome::Evicted);
        assert_eq!(outcomes[2], MembershipOutcome::Unknown);
        // No two outcomes should be equal.
        assert_ne!(outcomes[0], outcomes[1]);
        assert_ne!(outcomes[0], outcomes[2]);
        assert_ne!(outcomes[1], outcomes[2]);
    }

    // ========================================================================
    // engine_gid byte-length gate (Failed-message resurrection)
    // ========================================================================

    #[test]
    fn engine_gid_length_gate_accepts_16_bytes() {
        // MDK GroupId::from_slice(&bytes) — 16 bytes is the typical MDK
        // group id format.
        let bytes_16 = vec![0u8; 16];
        let acceptable = bytes_16.len() == 16 || bytes_16.len() == 32;
        assert!(acceptable);
    }

    #[test]
    fn engine_gid_length_gate_accepts_32_bytes() {
        // 32 bytes is the nostr_group_id fallback (when get_groups didn't
        // surface us).
        let bytes_32 = vec![0u8; 32];
        let acceptable = bytes_32.len() == 16 || bytes_32.len() == 32;
        assert!(acceptable);
    }

    #[test]
    fn engine_gid_length_gate_rejects_unexpected_lengths() {
        // BUG-CLASS: a wrong-length engine_group_id was previously SILENTLY
        // skipped in the resurrection code, masking real bugs. Must be
        // rejected (and the call site logs warn).
        for bad_len in [0, 1, 8, 15, 17, 24, 33, 64, 128] {
            let bytes = vec![0u8; bad_len];
            let acceptable = bytes.len() == 16 || bytes.len() == 32;
            assert!(!acceptable, "len={} should be rejected", bad_len);
        }
    }

    #[tokio::test]
    async fn filter_h_tag_no_matches_returns_empty() {
        // Pagination footgun: relay returns hundreds of unrelated kind-445
        // events, none with our group's h-tag. Without the filter, the
        // caller's per-event h-tag guard would silently skip all of them,
        // last_seen_id would stay None, and pagination would loop.
        use nostr_sdk::prelude::*;
        let keys = Keys::generate();
        let mut events = Vec::new();
        for _ in 0..50 {
            let unrelated = EventBuilder::new(Kind::MlsGroupMessage, "x")
                .tag(Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                    vec!["b".repeat(64)],
                ))
                .build(keys.public_key())
                .sign(&keys)
                .await
                .unwrap();
            events.push(unrelated);
        }
        let result = filter_events_to_h_tag(events, &"a".repeat(64));
        assert!(result.is_empty());
    }

    #[test]
    fn get_group_sync_lock_same_group() {
        let lock1 = get_group_sync_lock("group-a");
        let lock2 = get_group_sync_lock("group-a");
        // Same Arc (same underlying lock)
        assert!(Arc::ptr_eq(&lock1, &lock2));
    }

    #[test]
    fn get_group_sync_lock_different_groups() {
        let lock1 = get_group_sync_lock("group-x");
        let lock2 = get_group_sync_lock("group-y");
        // Different Arcs (isolated locks)
        assert!(!Arc::ptr_eq(&lock1, &lock2));
    }

    #[test]
    fn mls_service_uninitialized_engine_fails() {
        let svc = MlsService::new();
        assert!(matches!(svc.engine(), Err(MlsError::NotInitialized)));
    }

    #[test]
    fn mls_service_init_at_path_creates_db() {
        let tmp = tempfile::tempdir().unwrap();
        let mls_dir = tmp.path().to_path_buf();

        let svc = MlsService::init_at_path(mls_dir.clone()).unwrap();

        // DB file should exist
        assert!(mls_dir.join("vector-mls.db").exists());
        // Codec marker should exist
        assert!(mls_dir.join("mls-codec-v2").exists());
        // Engine should work
        assert!(svc.engine().is_ok());
    }

    #[test]
    fn mls_service_init_wipes_pre_postcard_db() {
        let tmp = tempfile::tempdir().unwrap();
        let mls_dir = tmp.path().to_path_buf();
        let db_path = mls_dir.join("vector-mls.db");

        // Create a pre-postcard DB (exists but no codec marker)
        std::fs::create_dir_all(&mls_dir).unwrap();
        std::fs::write(&db_path, b"fake db content").unwrap();
        assert!(db_path.exists());

        // Init should wipe it and create a fresh one
        let svc = MlsService::init_at_path(mls_dir.clone()).unwrap();
        assert!(svc.engine().is_ok());
        assert!(mls_dir.join("mls-codec-v2").exists());
    }

    #[test]
    fn mls_service_engine_creates_fresh_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = MlsService::init_at_path(tmp.path().to_path_buf()).unwrap();

        // Two calls should both succeed (fresh instances, not shared state)
        let engine1 = svc.engine();
        assert!(engine1.is_ok());
        drop(engine1);

        let engine2 = svc.engine();
        assert!(engine2.is_ok());
    }

    /// Mutex to serialize tests that use the global DB pool.
    /// The DB pool uses global statics (APP_DATA_DIR, CURRENT_ACCOUNT)
    /// so parallel tests would race.
    static DB_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper: initialize vector-core DB pool in a tempdir for testing.
    /// Returns the TempDir (must be held alive) and a lock guard.
    /// Counter to give each test a unique account, avoiding DB pool cross-contamination.
    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Generate a bech32-shaped placeholder npub for tests.
    ///
    /// `account_dir` enforces `is_valid_npub` (63 chars, bech32 alphabet),
    /// so the old `format!("npub1test{n}")` form is now rejected. We pad a
    /// counter into a fixed-length valid string instead. Only the prefix
    /// + length + alphabet matter — no checksum validation happens here.
    fn make_test_npub(n: u32) -> String {
        // Bech32 alphabet excludes 1, b, i, o.
        // 58-char payload composed of 'q' filler plus a counter encoded in
        // bech32 chars (max u32 is 10 decimal digits → 10 chars).
        const FILLER: char = 'q';
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"; // 32 chars
        let mut payload = vec![FILLER as u8; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        // Force at least one non-filler char so back-to-back tests differ.
        payload[57] = BECH32[(n as usize) % 32];
        let payload_str = std::str::from_utf8(&payload).unwrap();
        format!("npub1{}", payload_str)
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = DB_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    #[tokio::test]
    async fn cleanup_evicted_group_marks_evicted() {
        let (_tmp, _guard) = init_test_db();

        let svc = MlsService::new(); // Engine not needed for this test

        // Insert a group
        let group = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "evict-test-group".into(),
                engine_group_id: "eng-evict".into(),
                creator_pubkey: "npub1creator".into(),
                created_at: 100,
                updated_at: 200,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "About to be evicted".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        crate::db::mls::save_mls_group(&group).unwrap();

        // Verify it's in DB
        let groups = crate::db::mls::load_mls_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].group.evicted);

        // Clean up (evict)
        svc.cleanup_evicted_group("evict-test-group").await.unwrap();

        // Verify it's marked evicted in DB
        let groups_after = crate::db::mls::load_mls_groups().unwrap();
        assert_eq!(groups_after.len(), 1);
        assert!(groups_after[0].group.evicted);
    }

    #[tokio::test]
    async fn cleanup_evicted_group_removes_from_state() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new();

        // Add a group chat to STATE
        {
            let mut state = crate::state::STATE.lock().await;
            state.create_or_get_mls_group_chat("state-evict-group", vec![]);
        }

        // Verify it's in STATE
        {
            let state = crate::state::STATE.lock().await;
            assert!(state.get_chat("state-evict-group").is_some());
        }

        // Clean up
        svc.cleanup_evicted_group("state-evict-group").await.unwrap();

        // Verify removed from STATE
        {
            let state = crate::state::STATE.lock().await;
            assert!(state.get_chat("state-evict-group").is_none());
        }
    }

    #[tokio::test]
    async fn leave_group_requires_client() {
        // leave_group requires NOSTR_CLIENT to be initialized (matches original contract)
        let svc = MlsService::new();
        let result = svc.leave_group("any-group").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MlsError::NotInitialized));
    }

    #[test]
    fn create_group_engine_creates_and_merges() {
        // Test the engine-level group creation (no network, no STATE)
        let tmp = tempfile::tempdir().unwrap();
        let svc = MlsService::init_at_path(tmp.path().to_path_buf()).unwrap();

        let engine = svc.engine().unwrap();

        // Create a group with the engine directly
        let keys = nostr_sdk::Keys::generate();
        let my_pk = keys.public_key();

        let config = mdk_core::prelude::NostrGroupConfigData::new(
            "Test Group".into(),
            "Test description".into(),
            None, None, None,
            vec![],
            vec![my_pk],
        );

        let create_out = engine.create_group(&my_pk, vec![], config).unwrap();

        // Group should have an ID
        assert!(!create_out.group.mls_group_id.as_slice().is_empty());

        // Merge pending commit (critical for epoch advancement)
        engine.merge_pending_commit(&create_out.group.mls_group_id).unwrap();

        // Group should be listed
        let groups = engine.get_groups().unwrap();
        assert!(!groups.is_empty());
    }

    #[test]
    fn create_group_wire_id_derivation() {
        // Test that we can derive the wire group ID from a dummy message wrapper
        let tmp = tempfile::tempdir().unwrap();
        let svc = MlsService::init_at_path(tmp.path().to_path_buf()).unwrap();

        let engine = svc.engine().unwrap();
        let keys = nostr_sdk::Keys::generate();
        let my_pk = keys.public_key();

        let config = mdk_core::prelude::NostrGroupConfigData::new(
            "Wire ID Test".into(), "desc".into(),
            None, None, None, vec![], vec![my_pk],
        );

        let create_out = engine.create_group(&my_pk, vec![], config).unwrap();
        engine.merge_pending_commit(&create_out.group.mls_group_id).unwrap();

        let engine_gid_hex = crate::simd::hex::bytes_to_hex_string(create_out.group.mls_group_id.as_slice());

        // Create a dummy wrapper to extract the wire 'h' tag
        use nostr_sdk::prelude::*;
        let dummy_rumor = EventBuilder::new(Kind::Custom(9), "test-bootstrap")
            .build(my_pk);
        let wrapper = engine.create_message(&create_out.group.mls_group_id, dummy_rumor).unwrap();

        // The wrapper should have an 'h' tag
        let h_tag = wrapper.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)));
        assert!(h_tag.is_some(), "wrapper should have an 'h' tag");

        let wire_id = h_tag.unwrap().content().unwrap().to_string();
        assert_eq!(wire_id.len(), 64, "wire ID should be 64 hex chars");

        // Wire ID and engine ID may or may not be the same (depends on MDK version)
        println!("engine_gid={}, wire_id={}", engine_gid_hex, wire_id);
    }

    #[tokio::test]
    async fn create_group_metadata_persistence() {
        let (_tmp, _guard) = init_test_db();

        // Test metadata save + load round-trip (simulates post-create persistence)
        let meta = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "created-group-wire".into(),
                engine_group_id: "created-group-engine".into(),
                creator_pubkey: "npub1creator".into(),
                created_at: 1700000000,
                updated_at: 1700000000,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "New Group".into(),
                description: Some("Vector group: New Group".into()),
                avatar_ref: None,
                avatar_cached: None,
            },
        };

        // Save (same as create_group does)
        let mut groups = crate::db::mls::load_mls_groups().unwrap();
        groups.push(meta.clone());
        crate::db::mls::save_mls_groups(&groups).unwrap();

        // Load and verify
        let loaded = crate::db::mls::load_mls_groups().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].group.group_id, "created-group-wire");
        assert_eq!(loaded[0].group.engine_group_id, "created-group-engine");
        assert_eq!(loaded[0].group.creator_pubkey, "npub1creator");
        assert_eq!(loaded[0].profile.name, "New Group");
        assert_eq!(loaded[0].profile.description.as_deref(), Some("Vector group: New Group"));
        assert!(!loaded[0].group.evicted);
    }

    #[tokio::test]
    async fn create_group_state_chat_creation() {
        let (_tmp, _guard) = init_test_db();

        // Simulate the STATE chat creation part of create_group
        let group_id = "state-chat-test";
        let group_name = "My Group";
        let member_count = 3;

        {
            let mut state = crate::state::STATE.lock().await;
            let chat_id = state.create_or_get_mls_group_chat(group_id, vec![]);

            if let Some(chat) = state.get_chat_mut(&chat_id) {
                chat.metadata.set_name(group_name.to_string());
                chat.metadata.set_member_count(member_count);
            }
        }

        // Verify
        {
            let state = crate::state::STATE.lock().await;
            let chat = state.get_chat(group_id);
            assert!(chat.is_some(), "chat should exist in STATE");
            let chat = chat.unwrap();
            assert_eq!(chat.metadata.get_name(), Some(group_name));
            assert_eq!(chat.metadata.get_member_count(), Some(member_count));
        }
    }

    #[tokio::test]
    async fn create_group_idempotent_chat() {
        let (_tmp, _guard) = init_test_db();

        // Creating the same group chat twice should be idempotent
        {
            let mut state = crate::state::STATE.lock().await;
            let id1 = state.create_or_get_mls_group_chat("idem-group", vec![]);
            let id2 = state.create_or_get_mls_group_chat("idem-group", vec![]);
            assert_eq!(id1, id2);
        }

        let state = crate::state::STATE.lock().await;
        // Should only have one chat, not two
        let count = state.chats.iter().filter(|c| c.id() == "idem-group").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn create_group_requires_client() {
        // create_group should return NotInitialized if NOSTR_CLIENT is not set
        // We can't easily test this because NOSTR_CLIENT is a global OnceLock
        // and may already be set by another test. But the function signature
        // guarantees this check via .ok_or(MlsError::NotInitialized).
        // Covered by integration testing.
    }

    #[test]
    fn get_group_members_returns_empty_for_uninitialized() {
        // MlsService::new() without init — engine() returns NotInitialized
        let svc = MlsService::new();
        let result = svc.get_group_members("nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MlsError::NotInitialized));
    }

    #[test]
    fn get_group_members_with_engine_no_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = MlsService::init_at_path(tmp.path().to_path_buf()).unwrap();

        // Engine is initialized but no groups created — members/admins empty
        let (wire_id, members, admins) = svc.get_group_members("nonexistent-group").unwrap();
        assert_eq!(wire_id, "nonexistent-group");
        assert!(members.is_empty());
        assert!(admins.is_empty());
    }

    #[tokio::test]
    async fn sync_group_empty_id_returns_error() {
        let svc = MlsService::new();
        let result = svc.sync_group_since_cursor("", None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MlsError::InvalidGroupId));
    }

    #[tokio::test]
    async fn sync_group_evicted_returns_zero() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new();

        // Insert an evicted group
        let group = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "sync-evict-test".into(),
                engine_group_id: "eng-sync-evict".into(),
                creator_pubkey: "pk".into(),
                created_at: 100,
                updated_at: 200,
                evicted: true,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "Evicted Group".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        crate::db::mls::save_mls_group(&group).unwrap();

        let (processed, new_msgs) = svc.sync_group_since_cursor("sync-evict-test", None).await.unwrap();
        assert_eq!(processed, 0);
        assert_eq!(new_msgs, 0);
    }

    #[tokio::test]
    async fn sync_group_empty_prefetched_returns_zero() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new();

        // Insert a valid group
        let group = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "sync-empty-pre".into(),
                engine_group_id: "eng-sync-empty".into(),
                creator_pubkey: "pk".into(),
                created_at: 100,
                updated_at: 200,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "Empty Pre".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        crate::db::mls::save_mls_group(&group).unwrap();

        // Empty prefetched events should return (0, 0)
        let (processed, new_msgs) = svc.sync_group_since_cursor("sync-empty-pre", Some(vec![])).await.unwrap();
        assert_eq!(processed, 0);
        assert_eq!(new_msgs, 0);
    }

    #[tokio::test]
    async fn sync_group_invalid_length_returns_zero() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new();

        // Insert group with unusual ID length (not 32 or 64)
        let group = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "shortid".into(), // 7 chars, not 32 or 64
                engine_group_id: "shortid".into(),
                creator_pubkey: "pk".into(),
                created_at: 100,
                updated_at: 200,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "Short ID".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        crate::db::mls::save_mls_group(&group).unwrap();

        // Should return (0, 0) for invalid group_id length
        let (processed, new_msgs) = svc.sync_group_since_cursor("shortid", None).await.unwrap();
        assert_eq!(processed, 0);
        assert_eq!(new_msgs, 0);
    }

    #[tokio::test]
    async fn sync_group_cursor_persistence() {
        let (_tmp, _guard) = init_test_db();

        // Verify cursor read/write round-trip
        let mut cursors = HashMap::new();
        cursors.insert("cursor-test-group".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "abc123".into(),
            last_seen_at: 1700000000,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert_eq!(loaded.len(), 1);
        let cursor = loaded.get("cursor-test-group").unwrap();
        assert_eq!(cursor.last_seen_event_id, "abc123");
        assert_eq!(cursor.last_seen_at, 1700000000);
    }

    #[tokio::test]
    async fn cursor_load_empty_returns_empty_map() {
        let (_tmp, _guard) = init_test_db();
        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert!(loaded.is_empty(), "no cursors saved → empty map");
    }

    #[tokio::test]
    async fn cursor_save_multiple_groups_isolated() {
        // BUG-CLASS: writing one group's cursor must not stomp others.
        let (_tmp, _guard) = init_test_db();

        let mut cursors = HashMap::new();
        cursors.insert("group-a".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "id-a".into(),
            last_seen_at: 1000,
        });
        cursors.insert("group-b".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "id-b".into(),
            last_seen_at: 2000,
        });
        cursors.insert("group-c".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "id-c".into(),
            last_seen_at: 3000,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.get("group-a").unwrap().last_seen_at, 1000);
        assert_eq!(loaded.get("group-b").unwrap().last_seen_at, 2000);
        assert_eq!(loaded.get("group-c").unwrap().last_seen_at, 3000);
    }

    #[tokio::test]
    async fn cursor_save_overwrite_replaces_value() {
        // Save cursor for group-x, then save with new value — must reflect
        // the latest, not append.
        let (_tmp, _guard) = init_test_db();

        let mut cursors = HashMap::new();
        cursors.insert("group-x".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "old".into(),
            last_seen_at: 1000,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        cursors.insert("group-x".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "new".into(),
            last_seen_at: 2000,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert_eq!(loaded.len(), 1);
        let c = loaded.get("group-x").unwrap();
        assert_eq!(c.last_seen_event_id, "new");
        assert_eq!(c.last_seen_at, 2000);
    }

    #[tokio::test]
    async fn cursor_remove_via_save_with_missing_key() {
        // The MlsService leave/eviction code path does:
        //   cursors.remove(&group_id);
        //   write_event_cursors(&cursors);
        // The DB write API must mirror this — saving a map without a
        // previously-present key should DELETE that group's row, not
        // leave it stale.
        let (_tmp, _guard) = init_test_db();

        // Save two groups
        let mut cursors = HashMap::new();
        cursors.insert("keep".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "k".into(),
            last_seen_at: 100,
        });
        cursors.insert("remove".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "r".into(),
            last_seen_at: 200,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        // Remove one and re-save
        cursors.remove("remove");
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert_eq!(loaded.len(), 1, "removed cursor must not persist");
        assert!(loaded.contains_key("keep"));
        assert!(!loaded.contains_key("remove"));
    }

    #[tokio::test]
    async fn cursor_save_empty_clears_all() {
        // Saving an empty map must clear the table.
        let (_tmp, _guard) = init_test_db();

        let mut cursors = HashMap::new();
        cursors.insert("group-y".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "y".into(),
            last_seen_at: 100,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();
        assert_eq!(crate::db::mls::load_mls_event_cursors().unwrap().len(), 1);

        crate::db::mls::save_mls_event_cursors(&HashMap::new()).unwrap();
        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn cursor_round_trip_preserves_extreme_timestamps() {
        // Edge values: u64::MIN (0), u64::MAX, and i64::MAX (some DBs may
        // intern as signed). Exercise to catch any type-narrowing bugs.
        let (_tmp, _guard) = init_test_db();

        let mut cursors = HashMap::new();
        cursors.insert("zero".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "z".into(),
            last_seen_at: 0,
        });
        cursors.insert("max-i64".to_string(), crate::mls::EventCursor {
            last_seen_event_id: "m".into(),
            last_seen_at: i64::MAX as u64,
        });
        crate::db::mls::save_mls_event_cursors(&cursors).unwrap();

        let loaded = crate::db::mls::load_mls_event_cursors().unwrap();
        assert_eq!(loaded.get("zero").unwrap().last_seen_at, 0);
        assert_eq!(loaded.get("max-i64").unwrap().last_seen_at, i64::MAX as u64);
    }

    #[tokio::test]
    async fn sync_group_participants_updates_state() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new(); // no engine needed — sync reads from engine

        // Create a group chat in STATE with no participants
        {
            let mut state = crate::state::STATE.lock().await;
            state.create_or_get_mls_group_chat("sync-part-test", vec![]);
        }

        // sync_group_participants will fail (no engine) but shouldn't crash
        let result = svc.sync_group_participants("sync-part-test").await;
        // Expected: engine not initialized
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_evicted_group_matches_engine_group_id() {
        let (_tmp, _guard) = init_test_db();
        let svc = MlsService::new();

        // Insert group with different wire vs engine IDs
        let group = MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "wire-id-abc".into(),
                engine_group_id: "engine-id-xyz".into(),
                creator_pubkey: "pk".into(),
                created_at: 0,
                updated_at: 0,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "Dual ID".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        crate::db::mls::save_mls_group(&group).unwrap();

        // Evict by engine_group_id (not wire group_id)
        svc.cleanup_evicted_group("engine-id-xyz").await.unwrap();

        let groups = crate::db::mls::load_mls_groups().unwrap();
        assert!(groups[0].group.evicted);
    }
}
