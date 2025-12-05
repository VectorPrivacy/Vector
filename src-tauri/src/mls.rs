//! MLS (Message Layer Security) Module
//! 
//! This module provides MLS group messaging capabilities using the nostr-mls crate.
//! We use nostr-mls defaults and communicate exclusively through TRUSTED_RELAY.
//! 
//! ## Storage Schema
//!
//! This module manages the following JSON store keys:
//!
//! ### Encrypted Keys (use crypto::internal_encrypt/decrypt)
//! - "mls_groups": Array of group metadata objects
//!   ```json
//!   [{
//!     "group_id": "...",
//!     "creator_pubkey": "...",
//!     "name": "...",
//!     "avatar_ref": "...",
//!     "created_at": 1234567890,
//!     "updated_at": 1234567890
//!   }]
//!   ```
//!
//! ### Plaintext Keys
//! - "mls_keypackage_index": Array tracking device keypackages
//!   ```json
//!   [{
//!     "owner_pubkey": "...",
//!     "device_id": "...",
//!     "keypackage_ref": "...",
//!     "fetched_at": 1234567890,
//!     "expires_at": 1234567890
//!   }]
//!   ```
//!
//! - "mls_event_cursors": Object mapping group_id to sync state
//!   ```json
//!   {
//!     "group_id": {
//!       "last_seen_event_id": "...",
//!       "last_seen_at": 1234567890
//!     }
//!   }
//!   ```
//!
//! ### Messages
//! Messages are stored in the unified Chat storage (see chat.rs), not in MLS-specific storage.
//! This allows protocol-agnostic message handling across DMs and MLS groups.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use mdk_core::prelude::*;
use mdk_sqlite_storage::MdkSqliteStorage;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, Emitter};
use crate::{TAURI_APP, NOSTR_CLIENT, TRUSTED_RELAY, STATE};
use crate::rumor::{RumorEvent, RumorContext, ConversationType, process_rumor, RumorProcessingResult};
use crate::db_migration::{save_chat, save_chat_messages};

/// MLS-specific error types following this crate's error style
#[derive(Debug)]
pub enum MlsError {
    NotInitialized,
    InvalidGroupId,
    InvalidKeyPackage,
    GroupNotFound,
    MemberNotFound,
    StorageError(String),
    NetworkError(String),
    CryptoError(String),
    NostrMlsError(String),
}

impl std::fmt::Display for MlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MlsError::NotInitialized => write!(f, "MLS service not initialized"),
            MlsError::InvalidGroupId => write!(f, "Invalid group ID"),
            MlsError::InvalidKeyPackage => write!(f, "Invalid key package"),
            MlsError::GroupNotFound => write!(f, "Group not found"),
            MlsError::MemberNotFound => write!(f, "Member not found"),
            MlsError::StorageError(e) => write!(f, "Storage error: {}", e),
            MlsError::NetworkError(e) => write!(f, "Network error: {}", e),
            MlsError::CryptoError(e) => write!(f, "Crypto error: {}", e),
            MlsError::NostrMlsError(e) => write!(f, "Nostr MLS error: {}", e),
        }
    }
}

impl std::error::Error for MlsError {}

/// MLS group metadata stored encrypted in "mls_groups"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsGroupMetadata {
    // Wire identifier used on the relay (wrapper 'h' tag). UI lists this value.
    pub group_id: String,
    // Engine identifier used locally by nostr-mls for group state lookups.
    // Backwards compatible with existing data via serde default.
    #[serde(default)]
    pub engine_group_id: String,
    pub creator_pubkey: String,
    pub name: String,
    pub avatar_ref: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    // Flag indicating if we were evicted/kicked from this group
    // When true, we skip syncing this group (unless it's a new welcome/invite)
    #[serde(default)]
    pub evicted: bool,
}

/// Keypackage index entry stored in "mls_keypackage_index"
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyPackageIndexEntry {
    owner_pubkey: String,
    device_id: String,
    keypackage_ref: String,
    fetched_at: u64,
    expires_at: u64,
}

/// Event cursor tracking for a group stored in "mls_event_cursors"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCursor {
    pub last_seen_event_id: String,
    pub last_seen_at: u64,
}

/// Message record for persisting decrypted MLS messages

/// Main MLS service facade
/// 
/// Responsibilities:
/// - Initialize and manage MLS groups using nostr-mls
/// - Handle device keypackage publishing and management
/// - Process incoming MLS events from nostr relays
/// - Manage encrypted group metadata and message storage
pub struct MlsService {
    /// Persistent MLS engine when initialized (SQLite-backed via mdk-sqlite-storage)
    engine: Option<Arc<MDK<MdkSqliteStorage>>>,
    _initialized: bool,
}

impl MlsService {
    /// Create a new MLS service instance (no engine initialized)
    pub fn new() -> Self {
        Self {
            engine: None,
            _initialized: false,
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

        // Initialize persistent storage and engine
        let storage = MdkSqliteStorage::new(&db_path)
            .map_err(|e| MlsError::StorageError(format!("init sqlite storage: {}", e)))?;
        let mdk = MDK::new(storage);

        Ok(Self {
            engine: Some(Arc::new(mdk)),
            _initialized: true,
        })
    }

    /// Get a clone of the persistent MLS engine (Arc)
    pub fn engine(&self) -> Result<Arc<MDK<MdkSqliteStorage>>, MlsError> {
        self.engine.clone().ok_or(MlsError::NotInitialized)
    }

    /// Publish the device's keypackage to enable others to add this device to groups
    /// 
    /// This will:
    /// 1. Generate a new keypackage for the device if needed
    /// 2. Publish it to TRUSTED_RELAY via nostr-mls
    /// 3. Update "mls_keypackage_index" with the reference
    pub async fn publish_device_keypackage(&self, device_id: &str) -> Result<(), MlsError> {
        // TODO: Use nostr-mls to generate and publish keypackage
        // TODO: Store keypackage reference in "mls_keypackage_index"
        // TODO: Use TRUSTED_RELAY for publishing
        
        // Stub implementation
        let _ = device_id;
        Ok(())
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
      1) Resolve creator pubkey and build NostrGroupConfigData scoped to TRUSTED_RELAY.
      2) Resolve each member device to its KeyPackage Event before touching the MLS engine:
         • Prefer local plaintext index "mls_keypackage_index" to get keypackage_ref by member npub + device_id.
         • If ref exists: fetch exact event by id; else: fetch latest Kind::MlsKeyPackage by author.
         • Any member device with no resolvable KeyPackage is skipped here (this is a safe-guard; the UI path pre-validates via [rust.create_group_chat()](src-tauri/src/lib.rs:3108) and should not reach here with missing devices).
      3) Create the group with the persistent sqlite-backed engine (no await while engine is in scope):
         • engine.create_group(my_pubkey, member_kp_events, admins=[my_pubkey], group_config)
         • Capture:
           - engine_group_id (internal engine id, hex) for local operations and send path.
           - wire group id used on relays (h tag). We derive a canonical 64-hex when possible; fallback to engine id.
      4) Publish welcome(s) to invited recipients 1:1 via gift_wrap_to on TRUSTED_RELAY.
      5) Persist encrypted UI metadata ("mls_groups") with:
         • group_id = wire id (relay filtering id, shown in UI)
         • engine_group_id = engine id (used by [rust.send_mls_group_message()](src-tauri/src/lib.rs:3144))
      6) Emit "mls_group_initial_sync" immediately so the frontend can refresh chat list without restart.

    - Error mapping (propagated as strings to the UI):
      • MlsError::NotInitialized: Nostr client/app handle not ready.
      • MlsError::NetworkError: signer resolution, relay parsing, or network fetch/publish failures.
      • MlsError::NostrMlsError: engine create_group/create_message failures (e.g., storage/codec issues).
      • MlsError::StorageError: reading/writing JSON store or sqlite engine initialization paths.
      • MlsError::CryptoError: bech32 conversions or encrypted store (de)serialization.
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
        initial_member_devices: &[(String, String)], // (member_pubkey, device_id) pairs
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
        let relay_url = RelayUrl::parse(TRUSTED_RELAY)
            .map_err(|e| MlsError::NetworkError(format!("RelayUrl::parse: {}", e)))?;
        let description = format!("Vector group: {}", name);
        let group_config = NostrGroupConfigData::new(
            name.to_string(),
            description,
            None, // image_hash
            None, // image_key
            None, // image_nonce
            vec![relay_url],
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
                    .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => events.into_iter().next(),
                    Err(e) => {
                        eprintln!("[MLS] Fetch KeyPackage by id failed ({}:{}): {}", member_npub, device_id, e);
                        None
                    }
                }
            } else {
                // Fallback: fetch latest KeyPackage by author from TRUSTED_RELAY
                let filter = Filter::new()
                    .author(member_pk)
                    .kind(Kind::MlsKeyPackage)
                    .limit(50);
                match NOSTR_CLIENT
                    .get()
                    .unwrap()
                    .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
                    .await
                {
                    Ok(events) => {
                        // Heuristic: pick newest by created_at
                        let selected = events.into_iter().max_by_key(|e| e.created_at.as_u64());
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

            // GroupId is already a GroupId type in MDK (no conversion needed)
            let gid_bytes = create_out.group.mls_group_id.as_slice();
            let engine_gid_hex = hex::encode(gid_bytes);

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
            for i in 0..min_len {
                let welcome = welcome_rumors[i].clone(); // UnsignedEvent
                let target = invited_recipients[i];
                match NOSTR_CLIENT
                    .get()
                    .unwrap()
                    .gift_wrap_to([TRUSTED_RELAY], &target, welcome, [])
                    .await
                {
                    Ok(wrapper_id) => {
                        let recipient = target.to_bech32().unwrap_or_default();
                        println!(
                            "[MLS][welcome][published] wrapper_id={}, recipient={}, relay={}",
                            wrapper_id.to_hex(),
                            recipient,
                            TRUSTED_RELAY
                        );
                    }
                    Err(e) => {
                        let recipient = target.to_bech32().unwrap_or_default();
                        eprintln!(
                            "[MLS][welcome][publish_error] recipient={}, relay={}, err={}",
                            recipient,
                            TRUSTED_RELAY,
                            e
                        );
                    }
                }
            }
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
            avatar_ref: avatar_ref.map(|s| s.to_string()),
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
            
            // Save chat to disk
            if let Some(handle) = TAURI_APP.get() {
                let handle_clone = handle.clone();
                if let Some(chat) = state.get_chat(&chat_id) {
                    if let Err(e) = save_chat(handle_clone, chat).await {
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
    /// 2. Add the device to the group via nostr-mls
    /// 3. Send the welcome message
    /// 4. Update group metadata
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
                    .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
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
                .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
                .await
            {
                kp_event = events.into_iter().max_by_key(|e| e.created_at.as_u64());
            }
        }

        let kp_event = kp_event.ok_or_else(|| {
            MlsError::NetworkError(format!("No keypackage found for {}:{}", member_pubkey, device_id))
        })?;

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;
        
        // Convert engine_group_id hex to GroupId
        let mls_group_id = GroupId::from_slice(
            &hex::decode(&group_meta.engine_group_id)
                .map_err(|e| MlsError::CryptoError(format!("Invalid group ID hex: {}", e)))?
        );

        // Perform engine operations (add member but DON'T merge yet)
        let (evolution_event, welcome_rumors) = {
            let engine = self.engine()?;
            
            // Add member to group - returns AddMembersResult with evolution_event and welcome_rumors
            let add_result = engine
                .add_members(&mls_group_id, std::slice::from_ref(&kp_event))
                .map_err(|e| {
                    eprintln!("[MLS] Failed to add member: {}", e);
                    MlsError::NostrMlsError(format!("Failed to add member: {}", e))
                })?;

            (add_result.evolution_event, add_result.welcome_rumors)
        };

        // Send welcome before merging commit (welcome is created for current epoch)
        if let Some(welcome_rumors) = welcome_rumors {
            for welcome in welcome_rumors {
                if let Err(e) = client.gift_wrap_to([TRUSTED_RELAY], &member_pk, welcome, []).await {
                    eprintln!("[MLS] Failed to send welcome: {}", e);
                }
            }
        }

        // Publish evolution event (commit) to the relay
        if let Err(e) = client.send_event(&evolution_event).await {
            eprintln!("[MLS] Failed to publish commit: {}", e);
        }

        // NOW merge the pending commit after welcome and evolution event are sent
        {
            let engine = self.engine()?;
            engine
                .merge_pending_commit(&mls_group_id)
                .map_err(|e| {
                    eprintln!("[MLS] Failed to merge commit: {}", e);
                    MlsError::NostrMlsError(format!("Failed to merge commit: {}", e))
                })?;
        }

        // Update group metadata timestamp
        let mut groups = self.read_groups().await?;
        if let Some(group) = groups.iter_mut().find(|g| g.group_id == group_id) {
            group.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            self.write_groups(&groups).await?;
        }

        // Emit event to refresh UI
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_group_updated", serde_json::json!({
                "group_id": group_id
            })).ok();
        }

        Ok(())
    }


    /// Leave a group voluntarily
    ///
    /// This will:
    /// 1. Create a leave proposal using MDK's leave_group()
    /// 2. Publish the evolution event to the relay
    /// 3. Remove the group from local metadata
    ///
    /// Note: The leave creates a proposal that needs to be committed by an admin
    pub async fn leave_group(&self, group_id: &str) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Resolve client
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;
        
        // Convert engine_group_id hex to GroupId
        let mls_group_id = GroupId::from_slice(
            &hex::decode(&group_meta.engine_group_id)
                .map_err(|e| MlsError::CryptoError(format!("Invalid group ID hex: {}", e)))?
        );

        // Perform engine operation (leave group)
        let evolution_event = {
            let engine = self.engine()?;
            
            // Leave the group - returns LeaveGroupResult with evolution_event
            let leave_result = engine
                .leave_group(&mls_group_id)
                .map_err(|e| {
                    eprintln!("[MLS] Failed to leave group: {}", e);
                    MlsError::NostrMlsError(format!("Failed to leave group: {}", e))
                })?;

            leave_result.evolution_event
        };

        // Publish the evolution event (leave proposal) to the relay
        if let Err(e) = client.send_event(&evolution_event).await {
            eprintln!("[MLS] Failed to publish leave proposal: {}", e);
        }

        // Remove the group from local metadata
        let mut groups = self.read_groups().await?;
        groups.retain(|g| g.group_id != group_id && g.engine_group_id != group_meta.engine_group_id);
        self.write_groups(&groups).await?;

        // Emit event to refresh UI
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_group_left", serde_json::json!({
                "group_id": group_id
            })).ok();
        }

        Ok(())
    }

    /// Remove a member device from a group (admin only)
    ///
    /// This will:
    /// 1. Remove the member using MDK's remove_members()
    /// 2. Publish the commit message to remaining group members
    /// 3. Merge the pending commit locally
    /// 4. Emit UI update event
    pub async fn remove_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        _device_id: &str,
    ) -> Result<(), MlsError> {
        use nostr_sdk::prelude::*;

        // Resolve client
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;

        // Parse member pubkey
        let member_pk = PublicKey::from_bech32(member_pubkey)
            .map_err(|e| MlsError::CryptoError(format!("Invalid member pubkey: {}", e)))?;

        // Find the group's MLS group ID
        let groups = self.read_groups().await?;
        let group_meta = groups.iter()
            .find(|g| g.group_id == group_id || g.engine_group_id == group_id)
            .ok_or(MlsError::GroupNotFound)?;
        
        // Convert engine_group_id hex to GroupId
        let mls_group_id = GroupId::from_slice(
            &hex::decode(&group_meta.engine_group_id)
                .map_err(|e| MlsError::CryptoError(format!("Invalid group ID hex: {}", e)))?
        );

        // Sync the group first to ensure we have the latest state
        if let Err(e) = self.sync_group_since_cursor(group_id).await {
            eprintln!("[MLS] Failed to sync group before removal: {}", e);
        }
        
        // Perform engine operation (remove member but DON'T merge yet)
        let evolution_event = {
            let engine = self.engine()?;
            
            // Verify the member exists in the group
            let current_members = engine.get_members(&mls_group_id)
                .map_err(|e| {
                    eprintln!("[MLS] Failed to get current members: {}", e);
                    MlsError::NostrMlsError(format!("Failed to get group members: {}", e))
                })?;
            
            if !current_members.contains(&member_pk) {
                eprintln!("[MLS] Member {} not found in group", member_pubkey);
                return Err(MlsError::NostrMlsError(
                    "Member not found in group. The group state may be out of sync.".to_string()
                ));
            }
            
            // Remove member from group - returns RemoveMembersResult with evolution_event
            let remove_result = engine
                .remove_members(&mls_group_id, &[member_pk])
                .map_err(|e| {
                    eprintln!("[MLS] Failed to remove member: {}", e);
                    MlsError::NostrMlsError(format!("Failed to remove member: {}", e))
                })?;

            remove_result.evolution_event
        };

        // Publish evolution event (commit) to the relay
        match client.send_event(&evolution_event).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("[MLS] Failed to publish commit: {}", e);
                return Err(MlsError::NetworkError(format!("Failed to publish commit: {}", e)));
            }
        }

        // NOW merge the pending commit after evolution event is sent
        {
            let engine = self.engine()?;
            engine
                .merge_pending_commit(&mls_group_id)
                .map_err(|e| {
                    eprintln!("[MLS] Failed to merge commit: {}", e);
                    MlsError::NostrMlsError(format!("Failed to merge commit: {}", e))
                })?;
        }

        // Emit event to refresh UI member list
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_group_updated", serde_json::json!({
                "group_id": group_id
            })).ok();
        }
        Ok(())
    }

    /// Sync group messages since last cursor position
    /// 
    /// This will:
    /// 1. Read cursor from "mls_event_cursors" for the group
    /// 2. Query TRUSTED_RELAY for events since cursor
    /// 3. Process each event via engine.process_message
    /// 4. Update cursor position
    /// 
    /// Returns (processed_events_count, new_messages_count)
    pub async fn sync_group_since_cursor(&self, group_id: &str) -> Result<(u32, u32), MlsError> {
        use nostr_sdk::prelude::*;

        if group_id.is_empty() {
            return Err(MlsError::InvalidGroupId);
        }

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

        // 2) Load last cursor and compute since/until window
        let mut cursors = self.read_event_cursors().await.unwrap_or_default();

        let now = Timestamp::now();
        
        let since = if let Some(cur) = cursors.get(group_id) {
            Timestamp::from_secs(cur.last_seen_at)
        } else {
            // No cursor: default to last 48h for initial backfill
            Timestamp::from_secs(now.as_u64().saturating_sub(60 * 60 * 48))
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

        // Canonical group: safe to use relay-side h-tag filter
        let mut filter = Filter::new()
            .kind(Kind::MlsGroupMessage)
            .since(since)
            .until(until)
            .custom_tag(SingleLetterTag::lowercase(Alphabet::H), &gid_for_fetch)
            .limit(1000);

        // 3) Fetch from TRUSTED_RELAY with reasonable timeout
        let mut used_fallback = false;
        let mut events = match client
            .fetch_events_from(
                vec![TRUSTED_RELAY],
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
                .since(since)
                .until(until)
                .limit(1000);

            events = match client
                .fetch_events_from(
                    vec![TRUSTED_RELAY],
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

        
        if events.is_empty() {
            return Ok((0, 0));
        }

        // 4) Sort by created_at ascending to ensure deterministic processing
        let mut ordered: Vec<nostr_sdk::Event> = events.into_iter().collect();
        ordered.sort_by_key(|e| e.created_at.as_u64());

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
        
        // Resolve my pubkey before entering engine scope (for mine flag)
        let my_pubkey_hex = if let Ok(signer) = client.signer().await {
            if let Ok(my_pubkey) = signer.get_public_key().await {
                my_pubkey.to_hex()
            } else {
                String::new()
            }
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
                let check_gid_bytes = match hex::decode(&check_id) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        eprintln!("[MLS] Invalid group_id hex for engine check: {}", check_id);
                        vec![]
                    }
                };
                
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
                if let Some(tag) = ev.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
                    if let Some(h_val) = tag.content() {
                        if h_val != gid_for_fetch {
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
                            }
                            MessageProcessingResult::Commit { mls_group_id: _ } => {
                                // Commit processed - member list may have changed
                                // Check if we're still a member of this group
                                // Use group_check_id (engine's group_id) instead of gid_for_fetch (wrapper id)
                                if let Some(ref check_id) = group_check_id {
                                    let check_gid_bytes = hex::decode(check_id).unwrap_or_default();
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
                                
                                processed = processed.saturating_add(1);
                            }
                            MessageProcessingResult::Proposal(_proposal) => {
                                // Proposal received (e.g., leave proposal)
                                // Emit event to notify UI that group state may have changed
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("mls_group_updated", serde_json::json!({
                                        "group_id": gid_for_fetch
                                    })).ok();
                                }
                                
                                processed = processed.saturating_add(1);
                            }
                            MessageProcessingResult::ExternalJoinProposal { mls_group_id: _ } => {
                                // No-op for local message persistence
                            }
                            MessageProcessingResult::Unprocessable { mls_group_id: _ } => {
                                // Log unprocessable events for debugging
                                eprintln!("[MLS] Unprocessable event: id={}, created_at={}",
                                         ev.id.to_hex(), ev.created_at.as_u64());
                            }
                        }
                
                        processed = processed.saturating_add(1);
                
                        last_seen_id = Some(ev.id);
                        last_seen_at = ev.created_at.as_u64();
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

        // Process buffered rumors and persist after engine scope ends using unified Chat storage
        // BUT: Skip if we were evicted from this group during sync
        if !rumors_to_process.is_empty() && !was_evicted {
            // Get or create the MLS group chat in STATE with metadata
            let chat_id = {
                let mut state = STATE.lock().await;
                
                // Get group metadata to populate Chat metadata
                let group_meta = self.read_groups().await.ok()
                    .and_then(|groups| groups.into_iter().find(|g| g.group_id == gid_for_fetch));
                
                // Create or get the chat
                let chat_id = state.create_or_get_mls_group_chat(&gid_for_fetch, vec![]);
                
                // Update metadata if we have group info
                let mut metadata_updated = false;
                if let Some(meta) = group_meta {
                    if let Some(chat) = state.get_chat_mut(&chat_id) {
                        chat.metadata.set_name(meta.name.clone());
                        metadata_updated = true;
                    }
                }
                
                // Save chat to disk if metadata was updated
                if metadata_updated {
                    if let Some(handle) = TAURI_APP.get() {
                        let handle_clone = handle.clone();
                        if let Some(chat) = state.get_chat(&chat_id) {
                            if let Err(e) = save_chat(handle_clone, chat).await {
                                eprintln!("[MLS] Failed to save chat after metadata update: {}", e);
                            }
                        }
                    }
                }
                
                chat_id
            };
            
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
                                    if let Ok(exists) = crate::db_migration::message_exists_in_db(&handle, &msg.id).await {
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
                                        let _ = crate::db_migration::save_message(handle.clone(), &chat_id, &msg).await;
                                    }
                                }
                            }
                            RumorProcessingResult::Reaction(reaction) => {
                                // Reactions now work with unified storage!
                                let (was_added, chat_id_for_save) = {
                                    let mut state = STATE.lock().await;
                                    let added = if let Some((chat_id, msg)) = state.find_chat_and_message_mut(&reaction.reference_id) {
                                        msg.add_reaction(reaction.clone(), Some(chat_id))
                                    } else {
                                        false
                                    };
                                    
                                    // Get chat_id for saving if reaction was added
                                    let chat_id_for_save = if added {
                                        state.find_message(&reaction.reference_id)
                                            .map(|(chat, _)| chat.id().clone())
                                    } else {
                                        None
                                    };
                                    
                                    (added, chat_id_for_save)
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
                                                let _ = crate::db_migration::save_message(handle.clone(), &chat_id, &msg).await;
                                            }
                                        }
                                    }
                                }
                            }
                            RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                // Update the chat's typing participants
                                let active_typers = {
                                    let mut state = STATE.lock().await;
                                    if let Some(chat) = state.get_chat_mut(&chat_id) {
                                        chat.update_typing_participant(profile_id.clone(), until);
                                        chat.get_active_typers()
                                    } else {
                                        vec![]
                                    }
                                };
                                
                                // Emit typing update event to frontend
                                if let Some(handle) = TAURI_APP.get() {
                                    let _ = handle.emit("typing-update", serde_json::json!({
                                        "conversation_id": gid_for_fetch,
                                        "typers": active_typers,
                                    }));
                                }
                            }
                            RumorProcessingResult::Ignored => {
                                // Rumor was ignored (e.g., unknown kind)
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
                let state = STATE.lock().await;
                if let Some(chat) = state.get_chat(&chat_id) {
                    // Save chat metadata
                    let _ = save_chat(handle.clone(), chat).await;
                    
                    // Only save the newly added messages (much more efficient!)
                    // Get the last N messages where N = number of new messages processed
                    if new_msgs > 0 {
                        let messages_to_save: Vec<_> = chat.messages.iter()
                            .rev()
                            .take(new_msgs as usize)
                            .cloned()
                            .collect();
                        
                        if !messages_to_save.is_empty() {
                            let _ = save_chat_messages(handle.clone(), &chat_id, &messages_to_save).await;
                        }
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
            // 7) Advance cursor if anything processed (only if not evicted)
            if processed > 0 {
                if let Some(id) = last_seen_id {
                    cursors.insert(
                        gid_for_fetch.clone(),
                        EventCursor {
                            last_seen_event_id: id.to_hex(),
                            last_seen_at,
                        },
                    );
                    // Persist updated cursors
                    if let Err(e) = self.write_event_cursors(&cursors).await {
                        eprintln!("[MLS] write_event_cursors failed: {}", e);
                    }
                }
            }
        }

        Ok((processed, new_msgs))
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
            if let Err(e) = crate::db_migration::save_mls_group(handle, &group_to_update).await {
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
            if let Err(e) = crate::db_migration::delete_chat(handle.clone(), group_id).await {
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

    // Internal helper methods for store access
    // These follow the read/modify/write pattern used in the codebase
    
    /// Read and decrypt group metadata from SQL/store
    pub async fn read_groups(&self) -> Result<Vec<MlsGroupMetadata>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db_migration::load_mls_groups(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write encrypted group metadata to SQL/store
    pub async fn write_groups(&self, groups: &[MlsGroupMetadata]) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db_migration::save_mls_groups(handle, groups)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read keypackage index from SQL/store
    #[allow(dead_code)]
    async fn read_keypackage_index(&self) -> Result<Vec<KeyPackageIndexEntry>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let packages = crate::db_migration::load_mls_keypackages(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))?;
        
        // Convert from JSON values to KeyPackageIndexEntry
        let entries: Vec<KeyPackageIndexEntry> = packages.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        
        Ok(entries)
    }

    /// Write keypackage index to SQL/store
    #[allow(dead_code)]
    async fn write_keypackage_index(&self, index: &[KeyPackageIndexEntry]) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        
        // Convert to JSON values
        let packages: Vec<serde_json::Value> = index.iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect();
        
        crate::db_migration::save_mls_keypackages(handle, &packages)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Read event cursors from SQL/store
    #[allow(dead_code)]
    pub async fn read_event_cursors(&self) -> Result<HashMap<String, EventCursor>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db_migration::load_mls_event_cursors(&handle)
            .await
            .map_err(|e| MlsError::StorageError(e))
    }

    /// Write event cursors to SQL/store
    #[allow(dead_code)]
    pub async fn write_event_cursors(&self, cursors: &HashMap<String, EventCursor>) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        crate::db_migration::save_mls_event_cursors(handle, cursors)
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
            let kim_mls = MDK::new(MdkSqliteStorage::new(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);
            let saul_mls = MDK::new(MdkSqliteStorage::new(":memory:").map_err(|e| MlsError::StorageError(e.to_string()))?);

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
                .get_pending_welcomes()
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
            let rumor = EventBuilder::new(Kind::PrivateDirectMessage, "Vector-MLS-Test: hello")
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

/// Send an MLS message (rumor) to a group
///
/// This function takes a group_id, an UnsignedEvent (rumor), and an optional pending_id,
/// and sends it through the MLS protocol.
/// It's used by the protocol-agnostic message sending system to route group messages through MLS.
///
/// If a pending_id is provided, it will update that pending message with the real message ID
/// and handle success/failure state updates.
pub async fn send_mls_message(group_id: &str, rumor: nostr_sdk::UnsignedEvent, pending_id: Option<String>) -> Result<(), String> {
    let group_id = group_id.to_string();
    let pending_id = pending_id.clone();
    
    // Run non-Send MLS engine work on blocking thread
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get()
            .ok_or_else(|| "App handle not initialized".to_string())?
            .clone();
        
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Get the Nostr client
            let client = NOSTR_CLIENT.get()
                .ok_or_else(|| "Nostr client not initialized".to_string())?;
            
            // Create MLS service instance
            let service = MlsService::new_persistent(&handle)
                .map_err(|e| format!("Failed to create MLS service: {}", e))?;
            
            // Look up the group to get the engine_group_id (do this before getting engine)
            let groups = service.read_groups().await
                .map_err(|e| format!("Failed to read groups: {}", e))?;
            
            let group_meta = groups.iter()
                .find(|g| g.group_id == group_id)
                .ok_or_else(|| format!("Group not found: {}", group_id))?;
            
            // Parse the engine group ID
            let engine_group_id = if group_meta.engine_group_id.is_empty() {
                return Err("Group has no engine_group_id".to_string());
            } else {
                GroupId::from_slice(
                    &hex::decode(&group_meta.engine_group_id)
                        .map_err(|e| format!("Invalid engine_group_id hex: {}", e))?
                )
            };
            
            // Now get the MLS engine and create message (no await while engine is in scope)
            let mls_wrapper_result = {
                let engine = service.engine()
                    .map_err(|e| format!("Failed to get MLS engine: {}", e))?;
                
                engine.create_message(&engine_group_id, rumor.clone())
            }; // engine dropped here
            
            // Check for eviction errors after engine is dropped
            let mls_wrapper = match mls_wrapper_result {
                Ok(wrapper) => wrapper,
                Err(e) => {
                    let error_msg = e.to_string();
                    
                    // Check if this is an eviction error
                    if error_msg.contains("own leaf not found") ||
                       error_msg.contains("after being evicted") ||
                       error_msg.contains("evicted from it") ||
                       error_msg.contains("group not found") {
                        eprintln!("[MLS] Eviction detected while sending to group: {}", group_id);
                        
                        // Perform cleanup (we're in an async context now)
                        if let Err(cleanup_err) = service.cleanup_evicted_group(&group_id).await {
                            eprintln!("[MLS] Failed to cleanup evicted group: {}", cleanup_err);
                        }
                    }
                    
                    // Mark pending message as failed if we have a pending_id
                    if let Some(ref pid) = pending_id {
                        let mut state = crate::STATE.lock().await;
                        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == group_id) {
                            if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == *pid) {
                                msg.failed = true;
                                msg.pending = false;
                                
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("message_update", serde_json::json!({
                                        "old_id": pid,
                                        "message": msg,
                                        "chat_id": &group_id
                                    })).ok();
                                }
                            }
                        }
                    }
                    
                    return Err(format!("Failed to create MLS message: {}", e));
                }
            };
            
            // Get the inner rumor ID for the final update
            let inner_event_id = rumor.id.map(|id| id.to_hex());
            
            // Check if this is a typing indicator and add expiration to wrapper if so
            let is_typing_indicator = rumor.kind == nostr_sdk::Kind::ApplicationSpecificData
                && rumor.content == "typing";
            
            // Send the message and handle success/failure
            let send_result = if is_typing_indicator {
                // For typing indicators, add a 30-second expiration to the wrapper event
                use nostr_sdk::{EventBuilder, Tag, Timestamp};
                
                let expiry_time = Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                );
                
                // Create a new wrapper event with expiration tag
                let mut wrapper_builder = EventBuilder::new(mls_wrapper.kind, &mls_wrapper.content);
                
                // Copy all existing tags
                for tag in mls_wrapper.tags.iter() {
                    wrapper_builder = wrapper_builder.tag(tag.clone());
                }
                
                // Add expiration tag
                wrapper_builder = wrapper_builder.tag(Tag::expiration(expiry_time));
                
                // Build and sign the wrapper
                let signer = client.signer().await
                    .map_err(|e| format!("Failed to get signer: {}", e))?;
                let wrapper_with_expiry = wrapper_builder.sign(&signer).await
                    .map_err(|e| format!("Failed to sign wrapper with expiration: {}", e))?;
                
                // Send the wrapper with expiration
                client
                    .send_event_to([TRUSTED_RELAY], &wrapper_with_expiry)
                    .await
            } else {
                // Send normal wrapper without expiration
                client
                    .send_event_to([TRUSTED_RELAY], &mls_wrapper)
                    .await
            };
            
            // Update pending message based on send result
            if let (Some(ref pid), Some(ref real_id)) = (&pending_id, &inner_event_id) {
                match send_result {
                    Ok(_) => {
                        // Mark message as successfully sent and update ID
                        let mut state = crate::STATE.lock().await;
                        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == group_id) {
                            if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == *pid) {
                                // Update to real ID and mark as sent
                                msg.id = real_id.clone();
                                msg.pending = false;
                                
                                // Emit update
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("message_update", serde_json::json!({
                                        "old_id": pid,
                                        "message": msg,
                                        "chat_id": &group_id
                                    })).ok();
                                }
                                
                                // Save to database
                                let msg_clone = msg.clone();
                                drop(state);
                                if let Some(handle) = TAURI_APP.get() {
                                    let _ = crate::db_migration::save_message(handle.clone(), &group_id, &msg_clone).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Mark message as failed (keep pending ID)
                        let mut state = crate::STATE.lock().await;
                        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == group_id) {
                            if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == *pid) {
                                msg.failed = true;
                                msg.pending = false;
                                
                                // Emit update
                                if let Some(handle) = TAURI_APP.get() {
                                    handle.emit("message_update", serde_json::json!({
                                        "old_id": pid,
                                        "message": msg,
                                        "chat_id": &group_id
                                    })).ok();
                                }
                            }
                        }
                        return Err(format!("Failed to send MLS wrapper: {}", e));
                    }
                }
            } else {
                // No pending_id provided, just return the send result
                send_result.map_err(|e| format!("Failed to send MLS wrapper: {}", e))?;
            }
            
            Ok(())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Emit a frontend event whenever MLS group metadata changes so the UI can hydrate quickly.
pub fn emit_group_metadata_event(meta: &MlsGroupMetadata) {
    if let Some(handle) = TAURI_APP.get() {
        if let Err(e) = handle.emit(
            "mls_group_metadata",
            serde_json::json!({ "metadata": metadata_to_frontend(meta) }),
        ) {
            eprintln!("[MLS] Failed to emit mls_group_metadata event: {}", e);
        }
    }
}

fn seconds_to_millis(value: u64) -> u64 {
    value.saturating_mul(1000)
}

pub fn metadata_to_frontend(meta: &MlsGroupMetadata) -> serde_json::Value {
    serde_json::json!({
        "group_id": meta.group_id,
        "engine_group_id": meta.engine_group_id,
        "creator_pubkey": meta.creator_pubkey,
        "name": meta.name,
        "avatar_ref": meta.avatar_ref,
        "created_at": seconds_to_millis(meta.created_at),
        "updated_at": seconds_to_millis(meta.updated_at),
        "evicted": meta.evicted,
    })
}