//! MLS (Message Layer Security) Module
//!
//! This module provides MLS group messaging capabilities using the nostr-mls crate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use once_cell::sync::Lazy;
use tokio::sync::Mutex as TokioMutex;
use mdk_core::prelude::*;
use mdk_sqlite_storage::MdkSqliteStorage;
use tauri::{AppHandle, Runtime, Emitter};
use crate::{TAURI_APP, NOSTR_CLIENT, TRUSTED_RELAYS, active_trusted_relays, STATE, Message};
use crate::rumor::{RumorEvent, RumorContext, ConversationType, process_rumor, RumorProcessingResult};
use crate::db::save_chat_messages;
use crate::db::chats::{SlimChatDB, save_slim_chat};
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};

// Submodules
mod types;
mod messaging;
mod tracking;

// Re-exports
pub use types::{
    MlsError, MlsGroupMetadata, EventCursor,
    record_group_failure, record_group_success,
};
pub use messaging::{send_mls_message, emit_group_metadata_event, metadata_to_frontend};
pub use tracking::{is_mls_event_processed, track_mls_event_processed, cleanup_old_processed_events};
use types::{has_encoding_tag, KeyPackageIndexEntry};
use tracking::wipe_legacy_mls_database;

/// Per-group lock to ensure only one sync/process_message runs at a time for a given MLS group.
/// Prevents concurrent relay syncs from interleaving epoch-sequential commits.
static GROUP_SYNC_LOCKS: Lazy<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Get or create a per-group sync lock
pub fn get_group_sync_lock(group_id: &str) -> Arc<TokioMutex<()>> {
    let mut locks = GROUP_SYNC_LOCKS.lock().unwrap();
    locks.entry(group_id.to_string())
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone()
}

/// Publish a nostr event to TRUSTED_RELAYS with retries and exponential backoff.
///
/// Filters TRUSTED_RELAYS to only those currently in the relay pool (the user
/// may have disabled some). 5 attempts, 250ms base backoff. Retries on all
/// transient errors. Only bails early on definitive rejections like "duplicate"
/// or "blocked".
/// Returns `Ok(())` when at least one relay confirms, `Err` after exhausting retries.
async fn publish_event_with_retries(
    client: &nostr_sdk::Client,
    event: &nostr_sdk::Event,
) -> Result<(), String> {
    use std::time::Duration;

    let active = active_trusted_relays().await;
    if active.is_empty() {
        return Err("no trusted relays connected".to_string());
    }

    let mut last_err: Option<String> = None;
    for attempt in 0..5u8 {
        match client
            .send_event_to(active.iter().copied(), event)
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
                // Only bail early on definitive, non-transient rejections
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

/// Main MLS service facade
///
/// Responsibilities:
/// - Initialize and manage MLS groups using MDK
/// - Process incoming MLS events from nostr relays
/// - Manage encrypted group metadata and message storage
///
/// Creates FRESH MDK instances for each operation to ensure we always read
/// current state from SQLite, avoiding stale cache issues.
pub struct MlsService {
    /// Path to the SQLite database for creating fresh MDK instances
    db_path: std::path::PathBuf,
}

impl MlsService {
    /// Create a new MLS service instance (not initialized - will fail on engine())
    pub fn new() -> Self {
        Self {
            db_path: std::path::PathBuf::new(),
        }
    }

    /// Create a new MLS service with persistent SQLite-backed storage at:
    ///   [AppData]/npub.../mls/vector-mls.db (account-specific)
    pub fn new_persistent<R: Runtime>(handle: &AppHandle<R>) -> Result<Self, MlsError> {
        // Get current account's MLS directory
        let npub = crate::account_manager::get_current_account()
            .map_err(|e| MlsError::StorageError(format!("No account selected: {}", e)))?;

        let mls_dir = crate::account_manager::get_mls_directory(handle, &npub)
            .map_err(|e| MlsError::StorageError(format!("Failed to get MLS directory: {}", e)))?;

        let db_path = mls_dir.join("vector-mls.db");

        // v0.2.x → v0.3.0: The old MDK used a dual-connection architecture with incompatible
        // OpenMLS storage. No migration path exists — wipe the MLS database file.
        // (Main DB group data is wiped by Migration 12 in account_manager.)
        // TODO(v0.3.2+): Remove this block once v0.2.x migration window has passed.
        if db_path.exists() {
            wipe_legacy_mls_database(&db_path);
        }

        // Verify we can create a storage instance (validates path)
        let _storage = MdkSqliteStorage::new_unencrypted(&db_path)
            .map_err(|e| MlsError::StorageError(format!("init sqlite storage: {}", e)))?;

        Ok(Self { db_path })
    }

    /// Create a FRESH MDK engine instance for this operation.
    ///
    /// Each operation gets a fresh MDK instance that reads current state from SQLite.
    /// This prevents stale in-memory cache issues that could occur when multiple
    /// handlers process events concurrently.
    ///
    /// The returned MDK should be used for a single logical operation and then dropped.
    pub fn engine(&self) -> Result<MDK<MdkSqliteStorage>, MlsError> {
        if self.db_path.as_os_str().is_empty() {
            return Err(MlsError::NotInitialized);
        }

        let storage = MdkSqliteStorage::new_unencrypted(&self.db_path)
            .map_err(|e| MlsError::StorageError(format!("open sqlite storage: {}", e)))?;
        Ok(MDK::new(storage))
    }

    /// Create a new MLS group
    /// 
    /// This will:
    /// 1. Create the group using nostr-mls
    /// 2. Add initial member devices
    /// 3. Store encrypted metadata in "mls_groups" using crypto::internal_encrypt
    /// 4. Initialize per-group message storage keys
    /*
    Flow and error surfaces for persistent group creation (UI-visible behavior)
    - Inputs:
      • name: UI-supplied group name (validated in [rust.create_group_chat()](src-tauri/src/lib.rs:3108))
      • avatar_ref: not used for this subtask (None)
      • initial_member_devices: Vec of (member_npub, device_id) pairs chosen by the caller

    - Steps:
      1) Resolve creator pubkey and build NostrGroupConfigData scoped to TRUSTED_RELAYS.
      2) Resolve each member device to its KeyPackage Event before touching the MLS engine:
         • Prefer local plaintext index "mls_keypackage_index" to get keypackage_ref by member npub + device_id.
         • If ref exists: fetch exact event by id; else: fetch latest Kind::MlsKeyPackage by author.
         • Any member device with no resolvable KeyPackage is skipped here (this is a safe-guard; the UI path pre-validates via [rust.create_group_chat()](src-tauri/src/lib.rs:3108) and should not reach here with missing devices).
      3) Create the group with the persistent sqlite-backed engine (no await while engine is in scope):
         • engine.create_group(my_pubkey, member_kp_events, admins=[my_pubkey], group_config)
         • Capture:
           - engine_group_id (internal engine id, hex) for local operations and send path.
           - wire group id used on relays (h tag). We derive a canonical 64-hex when possible; fallback to engine id.
      4) Publish welcome(s) to invited recipients 1:1 via gift_wrap_to on TRUSTED_RELAYS.
      5) Persist encrypted UI metadata ("mls_groups") with:
         • group_id = wire id (relay filtering id, shown in UI)
         • engine_group_id = engine id (used by [rust.send_mls_group_message()](src-tauri/src/lib.rs:3144))
      6) Emit "mls_group_initial_sync" immediately so the frontend can refresh chat list without restart.

    - Error mapping (propagated as strings to the UI):
      • MlsError::NotInitialized: Nostr client/app handle not ready.
      • MlsError::NetworkError: signer resolution, relay parsing, or network fetch/publish failures.
      • MlsError::NostrMlsError: engine create_group/create_message failures (e.g., storage/codec issues).
      • MlsError::StorageError: reading/writing SQL database or sqlite engine initialization paths.
      • MlsError::CryptoError: bech32 conversions or encrypted data (de)serialization.
      These are returned as Err(String) up to [rust.create_group_chat()](src-tauri/src/lib.rs:3108) and surfaced verbatim by the UI.

    - Persistence & discoverability:
      • The group metadata is written to "mls_groups" (encrypted) so it appears in list_mls_groups().
      • Event "mls_group_initial_sync" is emitted here for zero-latency list refresh.

    - Partial membership:
      • If some members had no resolvable KeyPackage at engine time, they are skipped here; however, the preflight in [rust.create_group_chat()](src-tauri/src/lib.rs:3108) aborts early on any missing device, ensuring atomic creation semantics for the UI flow.
    */
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
    ) -> Result<String, MlsError> {
        // Persistent group creation using sqlite-backed engine.
        // - Resolve signer and relay config
        // - Use engine.create_group() inside a no-await scope (avoid holding !Send across await)
        // - Publish welcome (if any) to TRUSTED_RELAY
        // - Store encrypted UI metadata to "mls_groups"
        //
        // TODO: Resolve `initial_member_devices` into Vec<Event> KeyPackages (from index or network).

        // Resolve client and my pubkey
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;
        let signer = client
            .signer()
            .await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let my_pubkey = signer
            .get_public_key()
            .await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let creator_pubkey_b32 = my_pubkey
            .to_bech32()
            .map_err(|e| MlsError::CryptoError(e.to_string()))?;

        // Build group config (relay-scoped)
        let relay_urls: Vec<RelayUrl> = active_trusted_relays().await
            .into_iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();
        let desc = match description.filter(|d| !d.is_empty()) {
            Some(d) => d.to_string(),
            None => format!("Vector group: {}", name),
        };
        let group_config = NostrGroupConfigData::new(
            name.to_string(),
            desc.clone(),
            image_hash,
            image_key,
            image_nonce,
            relay_urls,
            vec![my_pubkey], // admins - moved from create_group call
        );

        // Resolve member KeyPackage events before engine usage (awaits allowed here)
        use nostr_sdk::prelude::*;
        let mut member_kp_events: Vec<Event> = Vec::new();
        let mut invited_recipients: Vec<PublicKey> = Vec::new();

        // Load plaintext index
        let index = self.read_keypackage_index().await.unwrap_or_default();

        for (member_npub, device_id) in initial_member_devices.iter() {
            // Parse target public key (used for later gift-wrapping)
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
                // Fetch exact event by id from TRUSTED_RELAY
                let id = match EventId::from_hex(&id_hex) {
                    Ok(v) => v,
                    Err(_) => {
                        println!("[MLS] Invalid keypackage_ref in index for {}:{}", member_npub, device_id);
                        continue;
                    }
                };
                let filter = Filter::new().id(id).limit(1);
                match NOSTR_CLIENT
                    .get()
                    .unwrap()
                    .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => events.into_iter().next(),
                    Err(e) => {
                        eprintln!("[MLS] Fetch KeyPackage by id failed ({}:{}): {}", member_npub, device_id, e);
                        None
                    }
                }
            } else {
                // Fallback: fetch latest KeyPackage by author from TRUSTED_RELAYS
                let filter = Filter::new()
                    .author(member_pk)
                    .kind(Kind::MlsKeyPackage)
                    .limit(50);
                match NOSTR_CLIENT
                    .get()
                    .unwrap()
                    .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => {
                        // Heuristic: pick newest by created_at
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
                // Validate keypackage has encoding tag (MIP-00/MIP-02 requirement)
                if !has_encoding_tag(&ev) {
                    // Get display name for better error message
                    let display_name = {
                        let state = STATE.lock().await;
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
                // Continue without this member device (will create group without them)
                eprintln!("[MLS] Skipping member device {}:{} (no KeyPackage event)", member_npub, device_id);
            }
        }

        let invited_count = member_kp_events.len();

        // Perform engine operations without awaits in scope
        let (group_id_hex, engine_gid_hex, welcome_rumors) = {
            let engine = self.engine()?; // Arc to sqlite engine (may be !Send internally)
            let create_out = engine
                .create_group(
                    &my_pubkey,
                    member_kp_events,              // invited devices' keypackage events
                    group_config,                  // admins now in config
                )
                .map_err(|e| MlsError::NostrMlsError(format!("create_group: {}", e)))?;

            // CRITICAL: Merge the pending commit immediately!
            // MDK's create_group() leaves a pending commit that must be merged
            // to advance our epoch. Otherwise we stay at epoch 0 while recipients
            // who accept the welcome are at epoch 1, causing all their messages
            // to be unprocessable due to epoch mismatch.
            engine
                .merge_pending_commit(&create_out.group.mls_group_id)
                .map_err(|e| MlsError::NostrMlsError(format!("merge_pending_commit after create: {}", e)))?;

            // GroupId is already a GroupId type in MDK (no conversion needed)
            let gid_bytes = create_out.group.mls_group_id.as_slice();
            let engine_gid_hex = bytes_to_hex_string(gid_bytes);

            // Attempt to derive wire id (wrapper 'h' tag, 64-hex) using a non-published dummy wrapper.
            // If unavailable, fall back to engine id.
            let wire_gid_hex = {
                use nostr_sdk::prelude::*;
                let dummy_rumor = EventBuilder::new(Kind::Custom(9), "vector-mls-bootstrap")
                    .tag(Tag::custom(
                        TagKind::Custom(std::borrow::Cow::Borrowed("vector-mls-bootstrap")),
                        vec!["true"],
                    ))
                    .build(*&my_pubkey);
                if let Ok(wrapper) = engine.create_message(&create_out.group.mls_group_id, dummy_rumor) {
                    if let Some(h_tag) = wrapper
                        .tags
                        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
                    {
                        if let Some(canon) = h_tag.content() {
                            if canon.len() == 64 {
                                canon.to_string()
                            } else {
                                // Fallback to engine id if 'h' content is unexpected
                                engine_gid_hex.clone()
                            }
                        } else {
                            engine_gid_hex.clone()
                        }
                    } else {
                        engine_gid_hex.clone()
                    }
                } else {
                    engine_gid_hex.clone()
                }
            };

            // Use wire id for UI/store group_id (relay filtering), engine id for local engine ops.
            let gid_hex = wire_gid_hex;

            (gid_hex, engine_gid_hex, create_out.welcome_rumors)
        }; // engine dropped here before any await

        // Accept 32-hex or 64-hex group ids (engine/codec variability)
        if group_id_hex.len() != 32 && group_id_hex.len() != 64 {
            eprintln!(
                "[MLS] create_group: unexpected group_id length={}, proceeding may affect relay filtering",
                group_id_hex.len()
            );
        }

        // Publish welcomes (gift-wrapped) 1:1 with invited recipients where possible
        if !welcome_rumors.is_empty() {
            if welcome_rumors.len() != invited_count {
                eprintln!(
                    "[MLS] welcome/member count mismatch: welcomes={}, invited={}",
                    welcome_rumors.len(),
                    invited_count
                );
            }
            let min_len = std::cmp::min(welcome_rumors.len(), invited_recipients.len());
            let client = NOSTR_CLIENT.get().unwrap();
            let futs: Vec<_> = (0..min_len)
                .map(|i| {
                    let welcome = welcome_rumors[i].clone();
                    let target = invited_recipients[i];
                    async move {
                        match client
                            .gift_wrap_to(active_trusted_relays().await.into_iter(), &target, welcome, [])
                            .await
                        {
                            Ok(wrapper_id) => {
                                let recipient = target.to_bech32().unwrap_or_default();
                                println!(
                                    "[MLS][welcome][published] wrapper_id={}, recipient={}, relays={:?}",
                                    wrapper_id.to_hex(),
                                    recipient,
                                    TRUSTED_RELAYS
                                );
                            }
                            Err(e) => {
                                let recipient = target.to_bech32().unwrap_or_default();
                                eprintln!(
                                    "[MLS][welcome][publish_error] recipient={}, relays={:?}, err={}",
                                    recipient,
                                    TRUSTED_RELAYS,
                                    e
                                );
                            }
                        }
                    }
                })
                .collect();
            futures_util::future::join_all(futs).await;
        } else {
            println!(
                "[MLS] No welcome rumors (invited={}, self-only path likely)",
                invited_count
            );
        }

        // Persist encrypted "mls_groups"
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| MlsError::StorageError(format!("system time error: {}", e)))?
            .as_secs();

        let meta = MlsGroupMetadata {
            group_id: group_id_hex.clone(),        // wire id for UI/filtering
            engine_group_id: engine_gid_hex,       // engine id for local operations
            creator_pubkey: creator_pubkey_b32,
            name: name.to_string(),
            description: Some(desc),
            avatar_ref: avatar_ref.map(|s| s.to_string()),
            avatar_cached: avatar_cached.map(|s| s.to_string()),
            created_at: now_secs,
            updated_at: now_secs,
            evicted: false,                        // New groups are not evicted
        };

        let mut groups = self.read_groups().await?;
        groups.push(meta.clone());
        self.write_groups(&groups).await?;
        emit_group_metadata_event(&meta);
 
        // Create the Chat in STATE with metadata and save to disk
        {
            let mut state = STATE.lock().await;
            let chat_id = state.create_or_get_mls_group_chat(&group_id_hex, vec![]);
            
            // Set metadata from MlsGroupMetadata
            if let Some(chat) = state.get_chat_mut(&chat_id) {
                chat.metadata.set_name(meta.name.clone());
                chat.metadata.set_member_count(invited_count + 1); // +1 for creator
            }
            
            // Save chat to disk — build slim while locked, save after drop
            let slim = state.get_chat(&chat_id).map(|chat| {
                SlimChatDB::from_chat(chat, &state.interner)
            });
            drop(state);

            if let Some(slim) = slim {
                if let Some(handle) = TAURI_APP.get() {
                    if let Err(e) = save_slim_chat(handle.clone(), slim).await {
                        eprintln!("[MLS] Failed to save chat after group creation: {}", e);
                    }
                }
            }
        }

        // Notify UI: reuse the same event used after welcome-accept so creator also sees the group immediately.
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_group_initial_sync", serde_json::json!({
                "group_id": group_id_hex,
                "processed": 0u32,
                "new": 0u32
            })).unwrap_or_else(|e| {
                eprintln!("[MLS] Failed to emit mls_group_initial_sync after create: {}", e);
            });
        }
 
        println!(
            "[MLS] Created group (persistent) id={}, name=\"{}\", invited_devices_hint={}",
            group_id_hex,
            name,
            initial_member_devices.len()
        );
        Ok(group_id_hex)
    }

    /// Add a member device to an existing group
    ///
    /// This will:
    /// 1. Fetch the device's keypackage from the network
    /// 2. Create the add-member commit via MDK (does not merge yet)
    /// 3. Return immediately — relay publish, merge, welcome, and metadata
    ///    update happen in a background task (MIP-02 / MIP-03 ordering)
    ///
    /// Background ordering: relay confirm → merge_pending_commit → send welcomes → UI update
    pub async fn add_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        device_id: &str,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Resolve client
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;

        // Parse member public key
        let member_pk = PublicKey::from_bech32(member_pubkey)
            .map_err(|e| MlsError::CryptoError(format!("Invalid member npub: {}", e)))?;

        // Fetch member's keypackage from index or network
        let index = self.read_keypackage_index().await.unwrap_or_default();
        let mut kp_event: Option<Event> = None;

        // Try index first
        for entry in &index {
            if entry.owner_pubkey == member_pubkey && entry.device_id == device_id {
                let id = EventId::from_hex(&entry.keypackage_ref)
                    .map_err(|e| MlsError::CryptoError(format!("Invalid keypackage ref: {}", e)))?;
                let filter = Filter::new().id(id).limit(1);
                if let Ok(events) = client
                    .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    kp_event = events.into_iter().next();
                }
                break;
            }
        }

        // Fallback: fetch latest from network
        if kp_event.is_none() {
            let filter = Filter::new()
                .author(member_pk)
                .kind(Kind::MlsKeyPackage)
                .limit(50);
            if let Ok(events) = client
                .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                .await
            {
                kp_event = events.into_iter().max_by_key(|e| e.created_at.as_secs());
            }
        }

        let kp_event = kp_event.ok_or_else(|| {
            MlsError::NetworkError(format!("No keypackage found for {}:{}", member_pubkey, device_id))
        })?;

        // Validate keypackage has encoding tag (MIP-00/MIP-02 requirement)
        if !has_encoding_tag(&kp_event) {
            // Get display name for better error message
            let display_name = {
                let state = STATE.lock().await;
                state.get_profile(&member_pubkey)
                    .and_then(|p| {
                        if !p.name.is_empty() { Some(p.name.to_string()) }
                        else if !p.display_name.is_empty() { Some(p.display_name.to_string()) }
                        else { None }
                    })
                    .unwrap_or_else(|| member_pubkey.to_string())
            };
            return Err(MlsError::OutdatedKeyPackage(display_name));
        }

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        // Spawn background task: lock → create commit → publish → merge → welcomes → UI update.
        // Validation (keypackage, group lookup) was done above; the engine operation and
        // everything after it runs under the per-group sync lock to prevent races.
        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.engine_group_id.clone();
        tokio::spawn(async move {
            let client = NOSTR_CLIENT.get().unwrap();

            // Hold per-group lock for the entire commit → publish → merge → welcome flow
            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // 1. Create the commit under lock (prevents sync from advancing epoch mid-operation)
            let mls_group_id = GroupId::from_slice(&hex_string_to_bytes(&engine_group_id));

            let (evolution_event, welcome_rumors) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for add: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to open storage: {}", e)
                            })).ok();
                        }
                        return;
                    }
                };
                let engine = MDK::new(storage);
                match engine.add_members(&mls_group_id, std::slice::from_ref(&kp_event)) {
                    Ok(result) => (result.evolution_event, result.welcome_rumors),
                    Err(e) => {
                        eprintln!("[MLS] Failed to add member: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to add member: {}", e)
                            })).ok();
                        }
                        return;
                    }
                }
            }; // engine dropped before await

            // 2. Publish evolution event with retries
            if let Err(e) = publish_event_with_retries(client, &evolution_event).await {
                eprintln!("[MLS] Failed to publish commit after retries: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to publish invite: {}", e)
                    })).ok();
                }
                return;
            }

            // 3. Merge pending commit now that relay confirmed
            let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[MLS] Failed to open storage for merge: {}", e);
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("mls_error", serde_json::json!({
                            "group_id": group_id_owned,
                            "error": format!("Failed to open storage for merge: {}", e)
                        })).ok();
                    }
                    return;
                }
            };
            let engine = MDK::new(storage);
            if let Err(e) = engine.merge_pending_commit(&mls_group_id) {
                eprintln!("[MLS] Failed to merge commit after relay confirm: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to merge commit: {}", e)
                    })).ok();
                }
                return;
            }

            // Track the event as processed only after merge succeeds
            if let Some(handle) = TAURI_APP.get() {
                let _ = track_mls_event_processed(
                    handle,
                    &evolution_event.id.to_hex(),
                    &group_id_owned,
                    evolution_event.created_at.as_secs(),
                );
            }

            // 4. Send welcome messages (only after commit is on relay and merged)
            if let Some(welcome_rumors) = welcome_rumors {
                let futs: Vec<_> = welcome_rumors
                    .into_iter()
                    .map(|welcome| async move {
                        if let Err(e) = client.gift_wrap_to(active_trusted_relays().await.into_iter(), &member_pk, welcome, []).await {
                            eprintln!("[MLS] Failed to send welcome: {}", e);
                        }
                    })
                    .collect();
                futures_util::future::join_all(futs).await;
            }

            // 5. Sync participants + update metadata + emit UI refresh
            if let Err(e) = crate::commands::mls::sync_mls_group_participants(group_id_owned.clone()).await {
                eprintln!("[MLS] Failed to sync participants after add: {}", e);
            }
            if let Some(handle) = TAURI_APP.get() {
                if let Ok(mut groups) = crate::db::load_mls_groups(handle).await {
                    if let Some(group) = groups.iter_mut().find(|g| g.group_id == group_id_owned) {
                        group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let _ = crate::db::save_mls_groups(handle.clone(), &groups).await;
                    }
                }
                handle.emit("mls_group_updated", serde_json::json!({
                    "group_id": group_id_owned
                })).ok();
            }
        });

        Ok(())
    }


    /// Add multiple members to an existing MLS group in a single commit.
    ///
    /// This will:
    /// 1. Fetch all members' keypackages from the network
    /// 2. Create the add-members commit via MDK (does not merge yet)
    /// 3. Return immediately — relay publish, merge, welcomes, and metadata
    ///    update happen in a background task (MIP-02 / MIP-03 ordering)
    ///
    /// Background ordering: relay confirm → merge_pending_commit → send welcomes → UI update
    pub async fn add_member_devices(
        &self,
        group_id: &str,
        members: &[(String, String)], // (npub, device_id) pairs
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;
        let index = self.read_keypackage_index().await.unwrap_or_default();

        let mut member_kp_events: Vec<Event> = Vec::new();
        let mut invited_recipients: Vec<PublicKey> = Vec::new();

        // Fetch keypackages for all members
        for (member_npub, device_id) in members {
            let member_pk = PublicKey::from_bech32(member_npub)
                .map_err(|e| MlsError::CryptoError(format!("Invalid member npub: {}", e)))?;

            // Try index first
            let mut kp_event: Option<Event> = None;
            for entry in &index {
                if entry.owner_pubkey == *member_npub && entry.device_id == *device_id {
                    let id = EventId::from_hex(&entry.keypackage_ref)
                        .map_err(|e| MlsError::CryptoError(format!("Invalid keypackage ref: {}", e)))?;
                    let filter = Filter::new().id(id).limit(1);
                    if let Ok(events) = client
                        .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                        .await
                    {
                        kp_event = events.into_iter().next();
                    }
                    break;
                }
            }

            // Fallback: fetch latest from network
            if kp_event.is_none() {
                let filter = Filter::new()
                    .author(member_pk)
                    .kind(Kind::MlsKeyPackage)
                    .limit(50);
                if let Ok(events) = client
                    .fetch_events_from(active_trusted_relays().await, filter, std::time::Duration::from_secs(10))
                    .await
                {
                    kp_event = events.into_iter().max_by_key(|e| e.created_at.as_secs());
                }
            }

            let kp_event = kp_event.ok_or_else(|| {
                MlsError::NetworkError(format!("No keypackage found for {}:{}", member_npub, device_id))
            })?;

            // Validate keypackage has encoding tag
            if !has_encoding_tag(&kp_event) {
                let display_name = {
                    let state = STATE.lock().await;
                    state.get_profile(&member_npub)
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

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        // Spawn background task: lock → create commit → publish → merge → welcomes → UI update.
        // Validation (keypackages, group lookup) was done above; the engine operation and
        // everything after it runs under the per-group sync lock to prevent races.
        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.engine_group_id.clone();
        tokio::spawn(async move {
            let client = NOSTR_CLIENT.get().unwrap();

            // Hold per-group lock for the entire commit → publish → merge → welcome flow
            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // 1. Create the commit under lock (prevents sync from advancing epoch mid-operation)
            let mls_group_id = GroupId::from_slice(&hex_string_to_bytes(&engine_group_id));

            let (evolution_event, welcome_rumors) = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for add: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to open storage: {}", e)
                            })).ok();
                        }
                        return;
                    }
                };
                let engine = MDK::new(storage);
                match engine.add_members(&mls_group_id, &member_kp_events) {
                    Ok(result) => (result.evolution_event, result.welcome_rumors),
                    Err(e) => {
                        eprintln!("[MLS] Failed to add members: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to add members: {}", e)
                            })).ok();
                        }
                        return;
                    }
                }
            }; // engine dropped before await

            // 2. Publish evolution event with retries
            if let Err(e) = publish_event_with_retries(client, &evolution_event).await {
                eprintln!("[MLS] Failed to publish commit after retries: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to publish invite: {}", e)
                    })).ok();
                }
                return;
            }

            // 3. Merge pending commit now that relay confirmed
            let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[MLS] Failed to open storage for merge: {}", e);
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("mls_error", serde_json::json!({
                            "group_id": group_id_owned,
                            "error": format!("Failed to open storage for merge: {}", e)
                        })).ok();
                    }
                    return;
                }
            };
            let engine = MDK::new(storage);
            if let Err(e) = engine.merge_pending_commit(&mls_group_id) {
                eprintln!("[MLS] Failed to merge commit after relay confirm: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to merge commit: {}", e)
                    })).ok();
                }
                return;
            }

            // Track the event as processed only after merge succeeds
            if let Some(handle) = TAURI_APP.get() {
                let _ = track_mls_event_processed(
                    handle,
                    &evolution_event.id.to_hex(),
                    &group_id_owned,
                    evolution_event.created_at.as_secs(),
                );
            }

            // 4. Send welcome messages concurrently, pairing each welcome with its recipient
            if let Some(welcome_rumors) = welcome_rumors {
                let invited_count = invited_recipients.len();
                if welcome_rumors.len() != invited_count {
                    eprintln!(
                        "[MLS] welcome/member count mismatch: welcomes={}, invited={}",
                        welcome_rumors.len(),
                        invited_count
                    );
                }
                let min_len = std::cmp::min(welcome_rumors.len(), invited_recipients.len());
                let futs: Vec<_> = (0..min_len)
                    .map(|i| {
                        let welcome = welcome_rumors[i].clone();
                        let target = invited_recipients[i];
                        async move {
                            match client
                                .gift_wrap_to(active_trusted_relays().await.into_iter(), &target, welcome, [])
                                .await
                            {
                                Ok(wrapper_id) => {
                                    let recipient = target.to_bech32().unwrap_or_default();
                                    println!(
                                        "[MLS][welcome][published] wrapper_id={}, recipient={}, relays={:?}",
                                        wrapper_id.to_hex(),
                                        recipient,
                                        TRUSTED_RELAYS
                                    );
                                }
                                Err(e) => {
                                    let recipient = target.to_bech32().unwrap_or_default();
                                    eprintln!(
                                        "[MLS][welcome][publish_error] recipient={}, relays={:?}, err={}",
                                        recipient,
                                        TRUSTED_RELAYS,
                                        e
                                    );
                                }
                            }
                        }
                    })
                    .collect();
                futures_util::future::join_all(futs).await;
            }

            // 4. Sync participants + update metadata + emit UI refresh
            if let Err(e) = crate::commands::mls::sync_mls_group_participants(group_id_owned.clone()).await {
                eprintln!("[MLS] Failed to sync participants after add: {}", e);
            }
            if let Some(handle) = TAURI_APP.get() {
                if let Ok(mut groups) = crate::db::load_mls_groups(handle).await {
                    if let Some(group) = groups.iter_mut().find(|g| g.group_id == group_id_owned) {
                        group.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let _ = crate::db::save_mls_groups(handle.clone(), &groups).await;
                    }
                }
                handle.emit("mls_group_updated", serde_json::json!({
                    "group_id": group_id_owned
                })).ok();
            }
        });

        Ok(())
    }

    /// Leave a group voluntarily
    ///
    /// This will:
    /// 1. Create a leave proposal using MDK's leave_group() (best effort)
    /// 2. Publish the evolution event to the relay
    /// 3. Clean up ALL local data for this group
    ///
    /// Note: The leave creates a proposal that needs to be committed by an admin.
    /// Even if the MLS operation fails, local data is cleaned up to prevent ghost states.
    pub async fn leave_group(&self, group_id: &str) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Verify client is initialized (spawned tasks fetch their own reference)
        let _client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;

        // Find the group's MLS group ID (may not exist if already partially cleaned)
        let groups = self.read_groups().await.unwrap_or_default();
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .cloned();

        // Send a "leave request" application message so admins auto-remove us
        // This is more reliable than the MLS proposal which needs explicit commit
        if let Some(ref meta) = group_meta {
            let mls_group_id = GroupId::from_slice(&hex_string_to_bytes(&meta.engine_group_id));
            {
                // Get our pubkey for building the rumor
                if let Some(&my_pubkey) = crate::MY_PUBLIC_KEY.get() {
                    // Build the leave request rumor (like typing indicator)
                    let leave_rumor = EventBuilder::new(Kind::ApplicationSpecificData, "leave")
                        .tag(Tag::custom(TagKind::d(), vec!["vector"]))
                        .build(my_pubkey);

                    // Create and send the MLS message
                    match self.engine() {
                        Ok(engine) => {
                            match engine.create_message(&mls_group_id, leave_rumor) {
                                Ok(mls_event) => {
                                    let gid = group_id.to_string();
                                    tokio::spawn(async move {
                                        let client = NOSTR_CLIENT.get().unwrap();
                                        if let Err(e) = client.send_event(&mls_event).await {
                                            eprintln!("[MLS] Failed to send leave request message: {}", e);
                                        } else {
                                            println!("[MLS] Leave request message sent for group: {}", gid);
                                        }
                                    });
                                }
                                Err(e) => {
                                    eprintln!("[MLS] Failed to create leave request message: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[MLS] Could not get MLS engine for leave request: {}", e);
                        }
                    }
                }
            }
        }

        // Always clean up local data, even if MLS operation failed
        // This prevents ghost/stuck states

        // 1. Remove cursor for this group
        let mut cursors = self.read_event_cursors().await.unwrap_or_default();
        cursors.remove(group_id);
        if let Some(ref meta) = group_meta {
            cursors.remove(&meta.engine_group_id);
        }
        if let Err(e) = self.write_event_cursors(&cursors).await {
            eprintln!("[MLS] Failed to remove cursor: {}", e);
        }

        // 2. Remove from mls_groups metadata
        if let Some(ref meta) = group_meta {
            let mut groups = self.read_groups().await.unwrap_or_default();
            groups.retain(|g| g.group_id != group_id && g.engine_group_id != meta.engine_group_id);
            if let Err(e) = self.write_groups(&groups).await {
                eprintln!("[MLS] Failed to remove group metadata: {}", e);
            }
        }

        // 3. Full cleanup (chat, messages, in-memory state)
        if let Err(e) = self.cleanup_evicted_group(group_id).await {
            eprintln!("[MLS] Cleanup failed (non-fatal): {}", e);
        }

        // 4. Emit event to refresh UI (cleanup_evicted_group also emits, but ensure it happens)
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_group_left", serde_json::json!({
                "group_id": group_id
            })).ok();
        }

        println!("[MLS] Left group and cleaned up local data: {}", group_id);
        Ok(())
    }

    /// Remove a member device from a group (admin only)
    ///
    /// This will:
    /// 1. Validate pubkey and group lookup synchronously
    /// 2. Return immediately — member verification, commit creation, relay publish,
    ///    merge, and UI update all happen in a background task under the sync lock
    ///
    /// Background ordering: lock → verify member → create commit → relay confirm → merge → UI update
    pub async fn remove_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        _device_id: &str,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Verify client is initialized (spawned tasks fetch their own reference)
        let _client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;

        // Parse member pubkey
        let member_pk = PublicKey::from_bech32(member_pubkey)
            .map_err(|e| MlsError::CryptoError(format!("Invalid member pubkey: {}", e)))?;

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;

        // Spawn background task: lock → verify member → create commit → publish → merge → UI update.
        // Validation (pubkey parse, group lookup) was done above; the engine operation and
        // everything after it runs under the per-group sync lock to prevent races.
        //
        // Note: We intentionally do NOT sync before removal. Syncing can re-process
        // our own commits from the relay, which may corrupt the tree state after
        // multiple kick/re-invite cycles. A fresh engine reads the latest SQLite state.
        let db_path = self.db_path.clone();
        let group_id_owned = group_id.to_string();
        let engine_group_id = group_meta.engine_group_id.clone();
        let member_pubkey_owned = member_pubkey.to_string();
        tokio::spawn(async move {
            let client = NOSTR_CLIENT.get().unwrap();

            // Hold per-group lock for the entire commit → publish → merge flow
            let group_lock = get_group_sync_lock(&group_id_owned);
            let _guard = group_lock.lock().await;

            // 1. Create the remove commit under lock (prevents sync from advancing epoch mid-operation)
            let mls_group_id = GroupId::from_slice(&hex_string_to_bytes(&engine_group_id));

            let evolution_event = {
                let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[MLS] Failed to open storage for remove: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to open storage: {}", e)
                            })).ok();
                        }
                        return;
                    }
                };
                let engine = MDK::new(storage);

                // Verify the member exists in the group
                let current_members = match engine.get_members(&mls_group_id) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[MLS] Failed to get current members: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to get group members: {}", e)
                            })).ok();
                        }
                        return;
                    }
                };

                if !current_members.contains(&member_pk) {
                    eprintln!("[MLS] Member {} not found in group", member_pubkey_owned);
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("mls_error", serde_json::json!({
                            "group_id": group_id_owned,
                            "error": "Member not found in group. The group state may be out of sync."
                        })).ok();
                    }
                    return;
                }

                match engine.remove_members(&mls_group_id, &[member_pk]) {
                    Ok(result) => result.evolution_event,
                    Err(e) => {
                        eprintln!("[MLS] Failed to remove member: {}", e);
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_error", serde_json::json!({
                                "group_id": group_id_owned,
                                "error": format!("Failed to remove member: {}", e)
                            })).ok();
                        }
                        return;
                    }
                }
            }; // engine dropped before await

            // 2. Publish evolution event with retries
            if let Err(e) = publish_event_with_retries(client, &evolution_event).await {
                eprintln!("[MLS] Failed to publish remove commit after retries: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to publish remove commit: {}", e)
                    })).ok();
                }
                return;
            }

            // 3. Merge pending commit now that relay confirmed
            let storage = match MdkSqliteStorage::new_unencrypted(&db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[MLS] Failed to open storage for merge: {}", e);
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("mls_error", serde_json::json!({
                            "group_id": group_id_owned,
                            "error": format!("Failed to open storage for merge: {}", e)
                        })).ok();
                    }
                    return;
                }
            };
            let engine = MDK::new(storage);
            if let Err(e) = engine.merge_pending_commit(&mls_group_id) {
                eprintln!("[MLS] Failed to merge commit after relay confirm: {}", e);
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_error", serde_json::json!({
                        "group_id": group_id_owned,
                        "error": format!("Failed to merge commit: {}", e)
                    })).ok();
                }
                return;
            }

            // Track the event as processed only after merge succeeds
            if let Some(handle) = TAURI_APP.get() {
                let _ = track_mls_event_processed(
                    handle,
                    &evolution_event.id.to_hex(),
                    &group_id_owned,
                    evolution_event.created_at.as_secs(),
                );
            }

            // 3. Sync participants + emit UI refresh
            if let Err(e) = crate::commands::mls::sync_mls_group_participants(group_id_owned.clone()).await {
                eprintln!("[MLS] Failed to sync participants after remove: {}", e);
            }
            if let Some(handle) = TAURI_APP.get() {
                handle.emit("mls_group_updated", serde_json::json!({
                    "group_id": group_id_owned
                })).ok();
            }
        });

        Ok(())
    }

    /// Sync group messages since last cursor position
    ///
    /// This will:
    /// 1. Read cursor from "mls_event_cursors" for the group
    /// 2. Query TRUSTED_RELAYS for events since cursor
    /// 3. Process each event via engine.process_message
    /// 4. Update cursor position
    ///
    /// Returns (processed_events_count, new_messages_count)
    pub async fn sync_group_since_cursor(&self, group_id: &str) -> Result<(u32, u32), MlsError> {
        use nostr_sdk::prelude::*;

        if group_id.is_empty() {
            return Err(MlsError::InvalidGroupId);
        }

        // Acquire per-group lock to prevent concurrent syncs from interleaving epoch-sequential commits
        let group_lock = get_group_sync_lock(group_id);
        let _guard = group_lock.lock().await;

        // 1) Check if this group is marked as evicted
        let groups = self.read_groups().await.ok();
        let group_metadata = groups.as_ref().and_then(|gs| {
            gs.iter().find(|g| g.group_id == group_id || (!g.engine_group_id.is_empty() && g.engine_group_id == group_id))
        });
        
        if let Some(meta) = group_metadata {
            if meta.evicted {
                return Ok((0, 0)); // Skip sync for evicted group
            }
        }

        // EventTracker cleanup: Remove old processed events (older than 7 days) to prevent unbounded growth.
        // Run this once per sync cycle. Errors are logged but don't fail the sync.
        if let Some(handle) = TAURI_APP.get() {
            let seven_days_secs = 7 * 24 * 60 * 60;
            if let Err(e) = cleanup_old_processed_events(handle, seven_days_secs) {
                eprintln!("[MLS] EventTracker cleanup failed: {}", e);
            }
        }

        // 2) Load last cursor and compute since/until window
        let mut cursors = self.read_event_cursors().await.unwrap_or_default();

        let now = Timestamp::now();

        let since = if let Some(cur) = cursors.get(group_id) {
            // Have cursor: resume from last seen event
            Timestamp::from_secs(cur.last_seen_at)
        } else {
            // No cursor: this is the FIRST sync for this group.
            // We need to fetch ALL commits since we were invited, not just recent ones.
            //
            // Use meta.created_at which is now set to the invite-sent timestamp (not acceptance time).
            // This ensures we catch all commits that happened between invitation and acceptance.
            // Fall back to 1 year if created_at is 0 or missing (legacy/edge cases).
            if let Some(meta) = group_metadata {
                if meta.created_at > 0 {
                    println!("[MLS] First sync for group {}, fetching from invite time {}", group_id, meta.created_at);
                    Timestamp::from_secs(meta.created_at)
                } else {
                    println!("[MLS] First sync for group {} (no created_at), fetching 1 year history", group_id);
                    Timestamp::from_secs(now.as_secs().saturating_sub(60 * 60 * 24 * 365))
                }
            } else {
                println!("[MLS] First sync for group {} (no metadata), fetching 1 year history", group_id);
                Timestamp::from_secs(now.as_secs().saturating_sub(60 * 60 * 24 * 365))
            }
        };
        let until = now;

        // Working group id for fetch/processing; prefer wire id from stored metadata if available
        let gid_for_fetch = if let Some(meta) = group_metadata {
            meta.group_id.clone() // wire id used on relay 'h' tag
        } else {
            group_id.to_string()
        };
        // 2) Build filter for MLS wrapper events (Kind 445) with 'h' tag = gid_for_fetch
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;
        let group_id_len = gid_for_fetch.len();
        if group_id_len != 32 && group_id_len != 64 {
            eprintln!(
                "[MLS] sync_group_since_cursor: unsupported group_id length {} for id={}; skipping",
                group_id_len,
                gid_for_fetch
            );
            return Ok((0, 0));
        }

        // Pagination constants
        const BATCH_SIZE: usize = 1000;
        const MAX_BATCHES: usize = 100; // Safety limit: 100k events max

        // Running totals across all batches
        let mut total_processed: u32 = 0;
        let mut total_new_msgs: u32 = 0;
        let mut current_since = since;
        let mut batch_count: usize = 0;

        // Pagination loop: fetch and process in batches until we've caught up
        loop {
            batch_count += 1;
            if batch_count > MAX_BATCHES {
                eprintln!("[MLS] Pagination safety limit reached ({} batches) for group {}", MAX_BATCHES, gid_for_fetch);
                break;
            }

            // 3) Build filter for this batch
            let mut filter = Filter::new()
                .kind(Kind::MlsGroupMessage)
                .since(current_since)
                .until(until)
                .custom_tag(SingleLetterTag::lowercase(Alphabet::H), &gid_for_fetch)
                .limit(BATCH_SIZE);

            // Fetch from TRUSTED_RELAYS with reasonable timeout
            let mut used_fallback = false;
            let mut events = match client
                .fetch_events_from(
                    active_trusted_relays().await,
                    filter.clone(),
                    std::time::Duration::from_secs(15),
                )
                .await
            {
                Ok(evts) => evts,
                Err(e) => {
                    return Err(MlsError::NetworkError(format!(
                        "fetch MLS events (with h tag) failed: {}",
                        e
                    )))
                }
            };

            // Fallback: if zero results, try without 'h' tag
            if events.is_empty() {
                used_fallback = true;

                filter = Filter::new()
                    .kind(Kind::MlsGroupMessage)
                    .since(current_since)
                    .until(until)
                    .limit(BATCH_SIZE);

                events = match client
                    .fetch_events_from(
                        active_trusted_relays().await,
                        filter,
                        std::time::Duration::from_secs(15),
                    )
                    .await
                {
                    Ok(evts) => evts,
                    Err(e) => {
                        return Err(MlsError::NetworkError(format!(
                            "fetch MLS events (fallback) failed: {}",
                            e
                        )))
                    }
                };
            }

            // No more events - we're done
            if events.is_empty() {
                if batch_count == 1 {
                    return Ok((0, 0));
                }
                break;
            }

            let batch_size = events.len();
            if batch_count > 1 {
                println!("[MLS] Pagination batch {} for group {}: {} events", batch_count, gid_for_fetch, batch_size);
            }

        // 4) Sort by created_at ascending to ensure deterministic processing
        let mut ordered: Vec<nostr_sdk::Event> = events.into_iter().collect();
        ordered.sort_by_key(|e| e.created_at.as_secs());

        // 4b) If we had to fall back to a broad fetch (no 'h' tag in the filter),
        // first, log observed 'h' tags to verify encoding; then, ONLY IF we positively match, filter.
        if used_fallback {
            // Attempt to filter only if we observe any h tag; otherwise, do not filter and rely on engine.
            let saw_any_h = ordered
                .iter()
                .any(|ev| ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))).is_some());
            if saw_any_h {
                // Try to narrow to our group via h-tag; if none match, proceed unfiltered and let engine decide.
                let original = ordered.clone();
                let filtered: Vec<nostr_sdk::Event> = original
                    .into_iter()
                    .filter(|ev| {
                        match ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                            Some(tag) => tag.content().map(|s| s == gid_for_fetch).unwrap_or(false),
                            None => false,
                        }
                    })
                    .collect();

                if !filtered.is_empty() {
                    ordered = filtered;
                }
            }
        }

        // 5) Process with persistent engine in a no-await scope
        let mut processed: u32 = 0;
        let mut new_msgs: u32 = 0;
        let mut last_seen_id: Option<nostr_sdk::EventId> = None;
        let mut last_seen_at: u64 = 0;
        
        // Buffer for rumor events to process after engine scope
        let mut rumors_to_process: Vec<(RumorEvent, String, bool)> = Vec::new(); // (rumor, wrapper_id, is_mine)

        // Track if we were evicted from this group
        let mut was_evicted = false;

        // Buffer for events that couldn't be processed (will retry immediately)
        // Keep retrying until all events process successfully or max retries reached
        let mut pending_retry: Vec<nostr_sdk::Event> = Vec::new();

        // Track events to mark as processed after engine scope (EventTracker)
        let mut events_to_track: Vec<(String, u64)> = Vec::new();
        
        // Resolve my pubkey before entering engine scope (for mine flag)
        let my_pubkey_hex = if let Some(&pk) = crate::MY_PUBLIC_KEY.get() {
            pk.to_hex()
        } else {
            String::new()
        };
        
        // Read group metadata before entering engine scope
        let group_check_id = if let Ok(groups) = self.read_groups().await {
            if let Some(meta) = groups.iter().find(|g| g.group_id == gid_for_fetch || g.engine_group_id == gid_for_fetch) {
                // Use the engine_group_id for checking
                if !meta.engine_group_id.is_empty() {
                    Some(meta.engine_group_id.clone())
                } else {
                    Some(meta.group_id.clone())
                }
            } else {
                None
            }
        } else {
            None
        };

        {
            let engine = self.engine()?; // Arc<...> held in scope without awaits
            
            if let Some(ref check_id) = group_check_id {
                // Try to verify if the engine knows about this group
                // We'll attempt to create a dummy message to see if the group exists
                let check_gid_bytes = hex_string_to_bytes(check_id);

                if !check_gid_bytes.is_empty() {
                    use nostr_sdk::prelude::*;
                    let check_gid = GroupId::from_slice(&check_gid_bytes);
                    let dummy_rumor = EventBuilder::new(Kind::Custom(9), "engine_check")
                        .build(nostr_sdk::PublicKey::from_hex("000000000000000000000000000000000000000000000000000000000000dead").unwrap());
                    
                    if let Err(e) = engine.create_message(&check_gid, dummy_rumor) {
                        eprintln!("[MLS] Engine missing group: {}", e);
                        
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_group_needs_rejoin", serde_json::json!({
                                "group_id": gid_for_fetch,
                                "reason": "Group not found in MLS engine state"
                            })).ok();
                        }
                    }
                }
            }

            for ev in ordered.iter() {
                // Hard guard: only process/persist wrappers whose 'h' tag matches our target group's wire id.
                // This prevents cross-contamination when the fallback fetch returns events for other groups.
                // Use case-insensitive comparison to handle hex encoding differences.
                if let Some(tag) = ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                    if let Some(h_val) = tag.content() {
                        // Case-insensitive comparison for hex strings
                        if !h_val.eq_ignore_ascii_case(&gid_for_fetch) {
                            // Skip silently - this is expected when multiple groups exist
                            continue;
                        }
                    } else {
                        // Skip silently - empty h tag
                        continue;
                    }
                } else {
                    // Skip silently - no h tag
                    continue;
                }

                // EventTracker: Skip if already processed (pre-check before MDK call)
                // This avoids expensive process_message() calls for already-handled events.
                if let Some(handle) = TAURI_APP.get() {
                    if is_mls_event_processed(handle, &ev.id.to_hex()) {
                        // Already processed - just update cursor tracking
                        last_seen_id = Some(ev.id);
                        last_seen_at = ev.created_at.as_secs();
                        continue;
                    }
                }

                match engine.process_message(ev) {
                    Ok(res) => {
                        // Log what type of message we got
                        match res {
                            MessageProcessingResult::ApplicationMessage(msg) => {
                                // Convert MLS ApplicationMessage to RumorEvent for protocol-agnostic processing
                                let rumor_event = RumorEvent {
                                    id: msg.id,
                                    kind: msg.kind,
                                    content: msg.content.clone(),
                                    tags: msg.tags.clone(),
                                    created_at: msg.created_at,
                                    pubkey: msg.pubkey,
                                };

                                let is_mine = !my_pubkey_hex.is_empty() && msg.pubkey.to_hex() == my_pubkey_hex;
                                let wrapper_id = msg.wrapper_event_id.to_hex();

                                // Buffer the rumor for async processing after engine scope
                                rumors_to_process.push((rumor_event, wrapper_id, is_mine));
                                new_msgs = new_msgs.saturating_add(1);

                                // Successfully processed - advance cursor and queue for tracking
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::Commit { mls_group_id: _ } => {
                                // Commit processed - member list may have changed
                                // Check if we're still a member of this group
                                // Use group_check_id (engine's group_id) instead of gid_for_fetch (wrapper id)
                                if let Some(ref check_id) = group_check_id {
                                    let check_gid_bytes = hex_string_to_bytes(check_id);
                                    if !check_gid_bytes.is_empty() {
                                        let check_gid = GroupId::from_slice(&check_gid_bytes);
                                        let my_pk = nostr_sdk::PublicKey::from_hex(&my_pubkey_hex).ok();
                                        
                                        let still_member = if let Some(pk) = my_pk {
                                            engine.get_members(&check_gid)
                                                .ok()
                                                .map(|members| members.contains(&pk))
                                                .unwrap_or(false)
                                        } else {
                                            false
                                        };
                                        
                                        if !still_member {
                                            // We've been removed from the group!
                                            if let Some(handle) = TAURI_APP.get() {
                                                handle.emit("mls_group_left", serde_json::json!({
                                                    "group_id": gid_for_fetch
                                                })).ok();
                                            }
                                        } else {
                                            // Still a member, just update the UI
                                            if let Some(handle) = TAURI_APP.get() {
                                                handle.emit("mls_group_updated", serde_json::json!({
                                                    "group_id": gid_for_fetch
                                                })).ok();
                                            }
                                        }
                                    }
                                }
                                
                                // Successfully processed commit - advance cursor and queue for tracking
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::Proposal(_update_result) => {
                                // Proposal received (e.g., leave proposal)
                                // Emit event to notify UI that group state may have changed
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("mls_group_updated", serde_json::json!({
                                        "group_id": gid_for_fetch
                                    })).ok();
                                }

                                // Successfully processed proposal - advance cursor and queue for tracking
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::ExternalJoinProposal { mls_group_id: _ } => {
                                // External join proposals don't affect our state, but we should
                                // still advance cursor since we've seen them
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::PendingProposal { mls_group_id: _ } => {
                                // Pending proposal - advance cursor
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::IgnoredProposal { mls_group_id: _, reason: _ } => {
                                // Ignored proposal - advance cursor
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                            MessageProcessingResult::Unprocessable { mls_group_id } => {
                                // MDK returns Unprocessable for several reasons:
                                // 1. Own message not in cache (CannotDecryptOwnMessage + not found)
                                // 2. Already processed message
                                // 3. Wrong epoch (future commit we can't apply yet)
                                // 4. Decryption failed
                                //
                                // Log details to help diagnose. Note: ev.pubkey is ephemeral, not real sender.
                                println!("[MLS] Unprocessable event: group={}, mls_gid={}, id={}, created_at={}",
                                         gid_for_fetch,
                                         bytes_to_hex_string(mls_group_id.as_slice()),
                                         ev.id.to_hex(),
                                         ev.created_at.as_secs());

                                // Queue for retry - might succeed after other commits are processed
                                pending_retry.push(ev.clone());
                            }
                            MessageProcessingResult::PreviouslyFailed => {
                                // MDK detected this event previously failed processing -
                                // skip without retrying to avoid repeated failures
                                processed = processed.saturating_add(1);
                                last_seen_id = Some(ev.id);
                                last_seen_at = ev.created_at.as_secs();
                                events_to_track.push((ev.id.to_hex(), ev.created_at.as_secs()));
                            }
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        
                        // Check if this is an eviction error (user was removed from group)
                        if error_msg.contains("own leaf not found") ||
                           error_msg.contains("after being evicted") ||
                           error_msg.contains("evicted from it") {
                            eprintln!("[MLS] ⚠️  EVICTION DETECTED - We were removed from group: {}", gid_for_fetch);
                            
                            // Set flag to remove this group after engine scope
                            was_evicted = true;
                        } else if !error_msg.contains("group not found") {
                            eprintln!(
                                "[MLS] process_message failed (group_id={}, id={}): {}",
                                gid_for_fetch,
                                ev.id,
                                e
                            );
                        }
                        // Continue processing subsequent events
                    }
                }
            }
        } // engine dropped here before any await

        // EventTracker: Mark all successfully processed events as tracked
        if !events_to_track.is_empty() {
            if let Some(handle) = TAURI_APP.get() {
                for (event_id, created_at) in events_to_track.iter() {
                    if let Err(e) = track_mls_event_processed(handle, event_id, &gid_for_fetch, *created_at) {
                        eprintln!("[MLS] Failed to track processed event {}: {}", event_id, e);
                    }
                }
            }
        }

        // RETRY LOOP: Retry out-of-order events within the same batch using progress-based passes.
        // We keep retrying as long as each pass makes progress (resolves at least one event).
        // Events still unprocessable after no-progress pass will be retried on the NEXT sync cycle
        // (since we don't advance cursor past them).
        // Safety cap of 50 passes prevents infinite loops.
        if !pending_retry.is_empty() && !was_evicted {
            let max_retry_passes: u32 = 50; // Safety cap to prevent infinite loops
            let mut retry_attempt: u32 = 0;

            while !pending_retry.is_empty() && retry_attempt < max_retry_passes {
                retry_attempt += 1;

                println!("[MLS] Retry pass {}/{} for {} events",
                         retry_attempt, max_retry_passes, pending_retry.len());

                // Sort pending events by created_at to ensure chronological processing
                pending_retry.sort_by_key(|e| e.created_at.as_secs());

                // Create fresh engine for this retry round
                let engine = match self.engine() {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("[MLS] Failed to create engine for retry: {}", e);
                        break;
                    }
                };

                // Track which events succeeded this round
                let mut still_pending: Vec<nostr_sdk::Event> = Vec::new();

                for ev in pending_retry.iter() {
                    // EventTracker: Skip if already processed
                    if let Some(handle) = TAURI_APP.get() {
                        if is_mls_event_processed(handle, &ev.id.to_hex()) {
                            // Already processed (maybe by live handler) - skip
                            last_seen_id = Some(ev.id);
                            last_seen_at = ev.created_at.as_secs();
                            continue;
                        }
                    }

                    match engine.process_message(ev) {
                        Ok(res) => {
                            match res {
                                MessageProcessingResult::ApplicationMessage(msg) => {
                                    // Convert to RumorEvent for processing
                                    let rumor_event = RumorEvent {
                                        id: msg.id,
                                        kind: msg.kind,
                                        content: msg.content.clone(),
                                        tags: msg.tags.clone(),
                                        created_at: msg.created_at,
                                        pubkey: msg.pubkey,
                                    };
                                    let is_mine = !my_pubkey_hex.is_empty() && msg.pubkey.to_hex() == my_pubkey_hex;
                                    let wrapper_id = msg.wrapper_event_id.to_hex();
                                    rumors_to_process.push((rumor_event, wrapper_id, is_mine));
                                    new_msgs = new_msgs.saturating_add(1);
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    // Track as processed
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    println!("[MLS] ✓ Retry succeeded (message): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::Commit { mls_group_id: _ } => {
                                    // Check membership after commit
                                    if let Some(ref check_id) = group_check_id {
                                        let check_gid_bytes = hex_string_to_bytes(check_id);
                                        if !check_gid_bytes.is_empty() {
                                            let check_gid = GroupId::from_slice(&check_gid_bytes);
                                            let my_pk = nostr_sdk::PublicKey::from_hex(&my_pubkey_hex).ok();
                                            let still_member = if let Some(pk) = my_pk {
                                                engine.get_members(&check_gid)
                                                    .ok()
                                                    .map(|members| members.contains(&pk))
                                                    .unwrap_or(false)
                                            } else {
                                                false
                                            };
                                            if !still_member {
                                                was_evicted = true;
                                                if let Some(handle) = TAURI_APP.get() {
                                                    handle.emit("mls_group_left", serde_json::json!({
                                                        "group_id": gid_for_fetch
                                                    })).ok();
                                                }
                                            } else if let Some(handle) = TAURI_APP.get() {
                                                handle.emit("mls_group_updated", serde_json::json!({
                                                    "group_id": gid_for_fetch
                                                })).ok();
                                            }
                                        }
                                    }
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    // Track as processed
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    println!("[MLS] ✓ Retry succeeded (commit): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::Proposal(_) => {
                                    if let Some(handle) = TAURI_APP.get() {
                                        handle.emit("mls_group_updated", serde_json::json!({
                                            "group_id": gid_for_fetch
                                        })).ok();
                                        // Track as processed
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    println!("[MLS] ✓ Retry succeeded (proposal): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::ExternalJoinProposal { .. } => {
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    // Track as processed
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    println!("[MLS] ✓ Retry succeeded (external join): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::PendingProposal { .. } => {
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    println!("[MLS] ✓ Retry succeeded (pending proposal): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::IgnoredProposal { .. } => {
                                    processed = processed.saturating_add(1);
                                    last_seen_id = Some(ev.id);
                                    last_seen_at = ev.created_at.as_secs();
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                                    }
                                    println!("[MLS] ✓ Retry succeeded (ignored proposal): id={}", ev.id.to_hex());
                                }
                                MessageProcessingResult::Unprocessable { mls_group_id } => {
                                    // Still can't process - keep for next retry round
                                    println!("[MLS] ✗ Retry still unprocessable: id={}, mls_gid={}",
                                             ev.id.to_hex(), bytes_to_hex_string(mls_group_id.as_slice()));
                                    still_pending.push(ev.clone());
                                }
                                MessageProcessingResult::PreviouslyFailed => {
                                    // Previously failed - don't retry
                                    println!("[MLS] ✗ Retry skipped (previously failed): id={}", ev.id.to_hex());
                                }
                            }
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            if error_msg.contains("own leaf not found") ||
                               error_msg.contains("after being evicted") ||
                               error_msg.contains("evicted from it") {
                                was_evicted = true;
                                break;
                            }
                            // Other errors - keep for retry
                            still_pending.push(ev.clone());
                        }
                    }

                    // Stop if we got evicted
                    if was_evicted {
                        break;
                    }
                }

                // Check if we made progress this pass
                let made_progress = still_pending.len() < pending_retry.len();

                // Update pending list for next iteration
                pending_retry = still_pending;

                // If we processed all events or got evicted, we're done
                if pending_retry.is_empty() || was_evicted {
                    break;
                }

                // Stop if no progress (remaining events are genuinely stuck, not just out-of-order)
                if !made_progress {
                    eprintln!("[MLS] No progress in retry pass {} — {} events permanently unprocessable",
                             retry_attempt, pending_retry.len());
                    break;
                }

                println!("[MLS] {} events still pending after retry attempt {}",
                         pending_retry.len(), retry_attempt);
            }

            // Log final status
            if !pending_retry.is_empty() {
                // These events are permanently unprocessable (wrong epoch, already failed in MDK, etc.)
                // Track them as processed and advance cursor past them so they're never re-fetched.
                for ev in &pending_retry {
                    if let Some(handle) = TAURI_APP.get() {
                        let _ = track_mls_event_processed(handle, &ev.id.to_hex(), &gid_for_fetch, ev.created_at.as_secs());
                    }
                    // Advance cursor past these permanently-failed events
                    last_seen_id = Some(ev.id);
                    last_seen_at = ev.created_at.as_secs();
                    processed = processed.saturating_add(1);
                }

                eprintln!("[MLS] {} events permanently unprocessable after {} retry passes (skipped, cursor advancing past them)",
                         pending_retry.len(), retry_attempt);

                // Record one failure for desync detection (not per-event to avoid spam)
                if record_group_failure(&gid_for_fetch).await {
                    eprintln!("[MLS] ⚠️  Group {} appears to be desynced (too many consecutive failures)", gid_for_fetch);
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("mls_group_needs_rejoin", serde_json::json!({
                            "group_id": gid_for_fetch,
                            "reason": "Too many unprocessable events - group may be desynced"
                        })).ok();
                    }
                }
            } else if retry_attempt > 0 {
                println!("[MLS] ✓ All retry events processed successfully after {} passes", retry_attempt);
                record_group_success(&gid_for_fetch).await;
            }
        }

        // Process buffered rumors and persist after engine scope ends using unified Chat storage
        // BUT: Skip if we were evicted from this group during sync
        if !rumors_to_process.is_empty() && !was_evicted {
            // Get group metadata BEFORE locking STATE (involves file I/O)
            let group_meta = self.read_groups().await.ok()
                .and_then(|groups| groups.into_iter().find(|g| g.group_id == gid_for_fetch));

            // Get or create the MLS group chat in STATE with metadata
            let (chat_id, slim_to_save) = {
                let mut state = STATE.lock().await;

                // Create or get the chat
                let chat_id = state.create_or_get_mls_group_chat(&gid_for_fetch, vec![]);

                // Update metadata if we have group info, build slim in same lookup
                let mut slim_to_save = None;
                if let Some(meta) = group_meta {
                    if let Some(idx) = state.chats.iter().position(|c| c.id == chat_id) {
                        state.chats[idx].metadata.set_name(meta.name.clone());
                        slim_to_save = Some(SlimChatDB::from_chat(&state.chats[idx], &state.interner));
                    }
                }

                (chat_id, slim_to_save)
            }; // Drop STATE lock before DB I/O

            // Save chat to disk if metadata was updated
            if let Some(slim) = slim_to_save {
                if let Some(handle) = TAURI_APP.get() {
                    if let Err(e) = save_slim_chat(handle.clone(), slim).await {
                        eprintln!("[MLS] Failed to save chat after metadata update: {}", e);
                    }
                }
            }
            
            for (rumor_event, _wrapper_id, is_mine) in rumors_to_process.iter() {
                let rumor_context = RumorContext {
                    sender: rumor_event.pubkey,
                    is_mine: *is_mine,
                    conversation_id: gid_for_fetch.clone(),
                    conversation_type: ConversationType::MlsGroup,
                };

                // Process the rumor using our protocol-agnostic processor
                match process_rumor(rumor_event.clone(), rumor_context).await {
                    Ok(result) => {
                        match result {
                            RumorProcessingResult::TextMessage(msg) | RumorProcessingResult::FileAttachment(msg) => {
                                // Check if message already exists in database (important for sync with partial message loading)
                                if let Some(handle) = TAURI_APP.get() {
                                    if let Ok(exists) = crate::db::message_exists_in_db(&handle, &msg.id).await {
                                        if exists {
                                            // Message already in DB, skip processing
                                            continue;
                                        }
                                    }
                                }

                                // Add message to the unified Chat storage
                                let was_added = {
                                    let mut state = STATE.lock().await;
                                    state.add_message_to_chat(&chat_id, msg.clone())
                                };

                                if was_added {
                                    // Emit UI event for new message
                                    if let Some(handle) = TAURI_APP.get() {
                                        handle.emit("mls_message_new", serde_json::json!({
                                            "group_id": gid_for_fetch,
                                            "message": msg
                                        })).unwrap_or_else(|e| {
                                            eprintln!("[MLS] Failed to emit mls_message_new event: {}", e);
                                        });
                                    }
                                    
                                    // Save the new message to database immediately
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = crate::db::save_message(handle.clone(), &chat_id, &msg).await;
                                    }
                                }
                            }
                            RumorProcessingResult::Reaction(reaction) => {
                                // Reactions now work with unified storage!
                                let (was_added, chat_id_for_save) = {
                                    let mut state = STATE.lock().await;
                                    // Use helper that handles interner access via split borrowing
                                    if let Some((chat_id, added)) = state.add_reaction_to_message(&reaction.reference_id, reaction.clone()) {
                                        (added, if added { Some(chat_id) } else { None })
                                    } else {
                                        (false, None)
                                    }
                                };
                                
                                // Save the updated message to database immediately (like DM reactions)
                                if was_added {
                                    if let Some(chat_id) = chat_id_for_save {
                                        if let Some(handle) = TAURI_APP.get() {
                                            let updated_message = {
                                                let state = STATE.lock().await;
                                                state.find_message(&reaction.reference_id)
                                                    .map(|(_, msg)| msg.clone())
                                            };
                                            
                                            if let Some(msg) = updated_message {
                                                let _ = crate::db::save_message(handle.clone(), &chat_id, &msg).await;
                                            }
                                        }
                                    }
                                }
                            }
                            RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                // Update the chat's typing participants
                                let active_typers = {
                                    let mut state = STATE.lock().await;
                                    state.update_typing_and_get_active(&chat_id, &profile_id, until)
                                };

                                // Emit typing update event to frontend
                                if let Some(handle) = TAURI_APP.get() {
                                    let _ = handle.emit("typing-update", serde_json::json!({
                                        "conversation_id": gid_for_fetch,
                                        "typers": active_typers,
                                    }));
                                }
                            }
                            RumorProcessingResult::LeaveRequest { event_id, member_pubkey } => {
                                // Deduplicate by event ID
                                if let Some(handle) = TAURI_APP.get() {
                                    if crate::db::event_exists(handle, &event_id).unwrap_or(false) {
                                        println!("[MLS] Sync: Skipping duplicate leave request: {}", event_id);
                                        continue;
                                    }
                                }

                                // A member is requesting to leave - if we're admin, auto-remove them
                                println!("[MLS] Leave request received from {} in group {}", member_pubkey, gid_for_fetch);

                                // Get member's display name for the system event
                                let member_name = {
                                    let state = STATE.lock().await;
                                    state.get_profile(&member_pubkey)
                                        .map(|p| {
                                            if !p.nickname.is_empty() { p.nickname.to_string() }
                                            else if !p.name.is_empty() { p.name.to_string() }
                                            else { member_pubkey.chars().take(12).collect::<String>() + "..." }
                                        })
                                };

                                // Check if we're an admin for this group
                                let am_i_admin = if let Some(meta) = &group_metadata {
                                    if let Some(&my_pk) = crate::MY_PUBLIC_KEY.get() {
                                        let my_npub = my_pk.to_bech32().unwrap_or_default();
                                        let my_hex = my_pk.to_hex();
                                        meta.creator_pubkey == my_npub || meta.creator_pubkey == my_hex
                                    } else { false }
                                } else { false };

                                if am_i_admin {
                                    println!("[MLS] I'm admin, auto-removing member: {}", member_pubkey);
                                    if let Some(handle) = TAURI_APP.get() {
                                        // Save system event using the leave request event_id
                                        // Returns true if inserted, false if duplicate (INSERT OR IGNORE)
                                        let was_inserted = crate::db::save_system_event_by_id(
                                            handle,
                                            &event_id,
                                            &gid_for_fetch,
                                            crate::db::SystemEventType::MemberLeft,
                                            &member_pubkey,
                                            member_name.as_deref(),
                                        ).await.unwrap_or(false);

                                        if was_inserted {
                                            // Emit event to frontend only if we saved it (not a duplicate)
                                            let _ = handle.emit("system_event", serde_json::json!({
                                                "conversation_id": gid_for_fetch,
                                                "event_id": event_id,
                                                "event_type": crate::db::SystemEventType::MemberLeft.as_u8(),
                                                "member_pubkey": member_pubkey,
                                                "member_name": member_name,
                                            }));

                                            // Use a fresh MLS service for the removal
                                            let mls_service = match MlsService::new_persistent(handle) {
                                                Ok(s) => s,
                                                Err(e) => {
                                                    eprintln!("[MLS] Failed to create MLS service for auto-remove: {}", e);
                                                    continue;
                                                }
                                            };
                                            // Remove the member (device_id doesn't matter for removal)
                                            if let Err(e) = mls_service.remove_member_device(&gid_for_fetch, &member_pubkey, "").await {
                                                eprintln!("[MLS] Failed to auto-remove member {}: {}", member_pubkey, e);
                                            } else {
                                                println!("[MLS] Successfully removed member {} from group {}", member_pubkey, gid_for_fetch);
                                            }
                                        } else {
                                            println!("[MLS] Sync: Skipping duplicate leave request event: {}", event_id);
                                        }
                                    }
                                } else {
                                    println!("[MLS] Not admin, ignoring leave request from {}", member_pubkey);
                                }
                            }
                            RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                                // Handle WebXDC peer advertisement - add peer to realtime channel
                                crate::services::handle_webxdc_peer_advertisement(&topic_id, &node_addr).await;
                            }
                            RumorProcessingResult::UnknownEvent(mut event) => {
                                // Store unknown events for future compatibility
                                if let Some(handle) = TAURI_APP.get() {
                                    if let Ok(chat_int_id) = crate::db::get_chat_id_by_identifier(handle, &chat_id) {
                                        event.chat_id = chat_int_id;
                                        let _ = crate::db::save_event(handle, &event).await;
                                    }
                                }
                            }
                            RumorProcessingResult::Ignored => {
                                // Rumor was ignored (e.g., expired typing indicator)
                            }
                            RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                // Save PIVX payment event to database and emit to frontend
                                if let Some(handle) = TAURI_APP.get() {
                                    let event_timestamp = event.created_at;
                                    let _ = crate::db::save_pivx_payment_event(handle, &gid_for_fetch, event).await;

                                    handle.emit("pivx_payment_received", serde_json::json!({
                                        "conversation_id": gid_for_fetch,
                                        "gift_code": gift_code,
                                        "amount_piv": amount_piv,
                                        "address": address,
                                        "message_id": message_id,
                                        "sender": rumor_event.pubkey.to_hex(),
                                        "is_mine": *is_mine,
                                        "at": event_timestamp * 1000,
                                    })).unwrap_or_else(|e| {
                                        eprintln!("[MLS] Failed to emit pivx_payment_received event: {}", e);
                                    });
                                }
                            }
                            RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                // Skip if this edit event was already processed (deduplication)
                                if let Some(handle) = TAURI_APP.get() {
                                    if crate::db::event_exists(handle, &event.id).unwrap_or(false) {
                                        continue; // Already processed, skip
                                    }

                                    // Save edit event to database
                                    if let Ok(chat_int_id) = crate::db::get_chat_id_by_identifier(handle, &chat_id) {
                                        let mut event_with_chat = event;
                                        event_with_chat.chat_id = chat_int_id;
                                        let _ = crate::db::save_event(handle, &event_with_chat).await;
                                    }
                                }

                                // Update message in state and emit to frontend
                                let mut state = STATE.lock().await;
                                let chat_idx = state.chats.iter().position(|c| c.id == chat_id);
                                if let Some(idx) = chat_idx {
                                    // Mutate the message
                                    if let Some(msg) = state.chats[idx].get_message_mut(&message_id) {
                                        msg.apply_edit(new_content, edited_at);
                                    }
                                    // Convert to Message for emit
                                    if let Some(msg) = state.chats[idx].get_compact_message(&message_id) {
                                        let msg_for_emit = msg.to_message(&state.interner);
                                        if let Some(handle) = TAURI_APP.get() {
                                            let _ = handle.emit("message_update", serde_json::json!({
                                                "old_id": &message_id,
                                                "message": msg_for_emit,
                                                "chat_id": &chat_id
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[MLS] Failed to process rumor: {}", e);
                    }
                }
            }
            
            // Persist the chat and new messages using unified storage
            if let Some(handle) = TAURI_APP.get() {
                let (slim, messages_to_save) = {
                    let state = STATE.lock().await;
                    if let Some(chat) = state.get_chat(&chat_id) {
                        let slim = SlimChatDB::from_chat(chat, &state.interner);
                        let messages_to_save: Vec<Message> = if new_msgs > 0 {
                            chat.messages.iter()
                                .rev()
                                .take(new_msgs as usize)
                                .map(|m| m.to_message(&state.interner))
                                .collect()
                        } else {
                            Vec::new()
                        };
                        (Some(slim), messages_to_save)
                    } else {
                        (None, Vec::new())
                    }
                }; // Drop STATE lock before async DB operations

                if let Some(slim) = slim {
                    let _ = save_slim_chat(handle.clone(), slim).await;

                    if !messages_to_save.is_empty() {
                        let _ = save_chat_messages(handle.clone(), &chat_id, &messages_to_save).await;
                    }
                }
            }
        }

        // 6) Clean up if we were evicted from the group
        if was_evicted {
            // Remove cursor for this group (will be reset if re-invited)
            cursors.remove(&gid_for_fetch);
            if let Err(e) = self.write_event_cursors(&cursors).await {
                eprintln!("[MLS] Failed to remove cursor for evicted group: {}", e);
            }
            
            // Perform full cleanup using the helper method
            if let Err(e) = self.cleanup_evicted_group(&gid_for_fetch).await {
                eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
            }
        } else {
            // 7) Advance cursor to the last processed/skipped event.
            // Permanently unprocessable events were already tracked and included above.
            if let Some(id) = last_seen_id {
                let current_cursor_at = cursors.get(&gid_for_fetch).map(|c| c.last_seen_at).unwrap_or(0);

                if last_seen_at > current_cursor_at {
                    println!("[MLS] Saving cursor: group={}, processed={}, seen_at={} (advanced from {})",
                             gid_for_fetch, processed, last_seen_at, current_cursor_at);
                    cursors.insert(
                        gid_for_fetch.clone(),
                        EventCursor {
                            last_seen_event_id: id.to_hex(),
                            last_seen_at,
                        },
                    );
                    if let Err(e) = self.write_event_cursors(&cursors).await {
                        eprintln!("[MLS] write_event_cursors failed: {}", e);
                    }
                    current_since = Timestamp::from_secs(last_seen_at);
                } else {
                    println!("[MLS] Cursor already up-to-date for group={} (at {})", gid_for_fetch, current_cursor_at);
                }
            }
        }

        // Accumulate totals from this batch
        total_processed += processed;
        total_new_msgs += new_msgs;

        // If we got fewer events than the batch size, we've caught up
        if batch_size < BATCH_SIZE {
            break;
        }

        // If we were evicted, stop pagination
        if was_evicted {
            break;
        }
        } // End pagination loop

        Ok((total_processed, total_new_msgs))
    }

    /// Clean up an evicted group (mark as evicted, remove from STATE, delete from DB)
    /// This can be called from both sync and live subscription handlers
    pub async fn cleanup_evicted_group(&self, group_id: &str) -> Result<(), MlsError> {
        // 1. Find and mark the specific group as evicted in metadata
        let groups = self.read_groups().await.unwrap_or_default();
        let mut marked_group: Option<crate::mls::MlsGroupMetadata> = None;
        
        for group in &groups {
            if group.group_id == group_id || group.engine_group_id == group_id {
                let mut updated_group = group.clone();
                updated_group.evicted = true;
                marked_group = Some(updated_group);
                break;
            }
        }
        
        // 2. If we found the group, update only that specific group
        if let Some(group_to_update) = marked_group {
            let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
            if let Err(e) = crate::db::save_mls_group(handle, &group_to_update).await {
                eprintln!("[MLS] Failed to mark group as evicted: {}", e);
            }
        }
        
        // 3. Remove from in-memory STATE
        {
            let mut state = STATE.lock().await;
            state.chats.retain(|c| c.id() != group_id);
        }
        
        // 4. Delete from database
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = crate::db::delete_chat(handle.clone(), group_id).await {
                eprintln!("[MLS] Failed to delete chat from storage: {}", e);
            }
        }
        
        // 5. Emit event to frontend
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = handle.emit("mls_group_left", serde_json::json!({
                "group_id": group_id
            })) {
                eprintln!("[MLS] Failed to emit mls_group_left event: {}", e);
            }
        }
        
        Ok(())
    }

    // Internal helper methods for database access
    // These follow the read/modify/write pattern used in the codebase
    
    /// Read and decrypt group metadata from database
    pub async fn read_groups(&self) -> Result<Vec<MlsGroupMetadata>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db::load_mls_groups(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write encrypted group metadata to database
    pub async fn write_groups(&self, groups: &[MlsGroupMetadata]) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db::save_mls_groups(handle, groups)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read keypackage index from database
    #[allow(dead_code)]
    async fn read_keypackage_index(&self) -> Result<Vec<KeyPackageIndexEntry>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let packages = crate::db::load_mls_keypackages(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))?;
        
        // Convert from JSON values to KeyPackageIndexEntry
        let entries: Vec<KeyPackageIndexEntry> = packages.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        
        Ok(entries)
    }

    /// Write keypackage index to database
    #[allow(dead_code)]
    async fn write_keypackage_index(&self, index: &[KeyPackageIndexEntry]) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        
        // Convert to JSON values
        let packages: Vec<serde_json::Value> = index.iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect();
        
        crate::db::save_mls_keypackages(handle, &packages)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read event cursors from database
    #[allow(dead_code)]
    pub async fn read_event_cursors(&self) -> Result<HashMap<String, EventCursor>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db::load_mls_event_cursors(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write event cursors to database
    #[allow(dead_code)]
    pub async fn write_event_cursors(&self, cursors: &HashMap<String, EventCursor>) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db::save_mls_event_cursors(handle, cursors)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }
    
    /// Run an in-memory MLS smoke test with the provided Nostr client
    ///
    /// This is a network-only smoke test that validates basic MLS operations
    /// without persisting any state to disk. It performs the following:
    /// - Publishes Saul's device KeyPackage
    /// - Creates a temporary group (Kim creator, Saul member; Kim admin)
    /// - Sends one MLS application message
    /// - Observes the wrapper on the relay
    ///
    /// All operations are wrapped in a timeout to prevent hanging.
    pub async fn run_mls_smoke_test_with_client(
        client: &nostr_sdk::Client,
        relay: &str,
        timeout: std::time::Duration,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        match tokio::time::timeout(timeout, async {
            println!("[MLS Smoke Test] Start (relay: {})", relay);

            // Use two ephemeral identities (do NOT use the logged-in client's keys)
            use nostr_sdk::prelude::Keys;
            let kim_keys = Keys::generate();
            let saul_keys = Keys::generate();
            println!(
                "[MLS Smoke Test] Ephemeral identities: kim={}, saul={}",
                kim_keys.public_key().to_bech32().unwrap_or_default(),
                saul_keys.public_key().to_bech32().unwrap_or_default()
            );

            // Two independent in-memory MLS engines (no disk I/O)
            let kim_mls = MDK::new(MdkSqliteStorage::new_unencrypted(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);
            let saul_mls = MDK::new(MdkSqliteStorage::new_unencrypted(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);

            // RelayUrl (nostr-mls type)
            let relay_url = RelayUrl::parse(relay)
                .map_err(|e| MlsError::NetworkError(format!("RelayUrl::parse: {}", e)))?;

            // 1) Saul publishes a device KeyPackage (so Kim can add him)
            println!("[MLS Smoke Test] Saul publishing device KeyPackage...");
            let (saul_kp_encoded, saul_kp_tags) = saul_mls
                .create_key_package_for_event(&saul_keys.public_key(), [relay_url.clone()])
                .map_err(|e| MlsError::NostrMlsError(format!("create_key_package_for_event (saul): {}", e)))?;
    
            // Build + sign with Saul's ephemeral keys, then publish with the app's client
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

            // 2) Kim creates a temporary two-member group (Kim creator + Saul member)
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
                None, // image_hash
                None, // image_key
                None, // image_nonce
                vec![relay_url.clone()],
                vec![kim_keys.public_key()], // admins - moved from create_group call
            );
    
            // IMPORTANT: Non-empty member_key_package_events (Saul). The creator (Kim) must not be in member_key_package_events.
            let group_create = kim_mls
                .create_group(
                    &kim_keys.public_key(),
                    vec![saul_kp_event.clone()],      // Saul invited via his KeyPackage
                    group_config,                     // admins now in config
                )
                .map_err(|e| MlsError::NostrMlsError(format!("create_group (kim): {}", e)))?;
    
            let kim_group = group_create.group;
            let welcome_rumor = group_create
                .welcome_rumors
                .first()
                .cloned()
                .ok_or_else(|| MlsError::NostrMlsError("no welcome rumor produced".into()))?;
            println!("[MLS Smoke Test] Group created; welcome rumor produced");
    
            // 2b) Saul processes the welcome locally and joins (no network, purely in-memory)
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

            // 3) Kim sends an MLS application message and publishes the wrapper to the relay
            let group_id = &kim_group.mls_group_id; // Already a GroupId in MDK
            println!("[MLS Smoke Test] Kim sending application message...");
            let rumor = EventBuilder::new(Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE), "Vector-MLS-Test: hello")
                .tag(Tag::custom(
                    TagKind::Custom(std::borrow::Cow::Borrowed("vector-mls-test")),
                    vec!["true"],
                ))
                .build(kim_keys.public_key());
    
            let mls_wrapper = kim_mls
                .create_message(&group_id, rumor)
                .map_err(|e| MlsError::NostrMlsError(format!("kim create_message: {}", e)))?;
    
            client
                .send_event_to([relay], &mls_wrapper)
                .await
                .map_err(|e| MlsError::NetworkError(format!("publish mls wrapper: {}", e)))?;
            println!(
                "[MLS Smoke Test] MLS wrapper published id={}, kind={:?}",
                mls_wrapper.id,
                mls_wrapper.kind
            );

            // 4) Verify network visibility once and then process locally on Saul
            let filter = Filter::new()
                .kind(Kind::MlsGroupMessage)
                .since(Timestamp::now() - 300u64); // widen observation window (5 minutes)

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
                println!("[MLS Smoke Test] Saul processing locally after relay observation...");
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