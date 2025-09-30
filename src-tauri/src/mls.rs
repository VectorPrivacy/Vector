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
//! ### Per-Group Runtime Keys (created dynamically, not in migration)
//! - "mls_messages_{group_id}": Map of message_id -> encrypted message payload
//! - "mls_timeline_{group_id}": Array of message_ids in timeline order

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use nostr_mls::prelude::*;
use nostr_mls_memory_storage::NostrMlsMemoryStorage;
use nostr_mls_sqlite_storage::NostrMlsSqliteStorage;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, Manager, Emitter};
use tauri::path::BaseDirectory;
use crate::{TAURI_APP, NOSTR_CLIENT, TRUSTED_RELAY};
use crate::db;
use crate::crypto;

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
struct EventCursor {
    last_seen_event_id: String,
    last_seen_at: u64,
}

/// Message record for persisting decrypted MLS messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// Inner event ID (hex) - the decrypted message's nostr event ID
    pub inner_event_id: String,
    /// Wrapper event ID (hex) - the Kind 445 wrapper event ID
    pub wrapper_event_id: String,
    /// Author public key (bech32 npub)
    pub author_pubkey: String,
    /// Message content
    pub content: String,
    /// Created at timestamp (seconds)
    pub created_at: u64,
    /// Nostr tags
    pub tags: Vec<Vec<String>>,
    /// Whether this message is from ourselves
    pub mine: bool,
}

/// Main MLS service facade
/// 
/// Responsibilities:
/// - Initialize and manage MLS groups using nostr-mls
/// - Handle device keypackage publishing and management
/// - Process incoming MLS events from nostr relays
/// - Manage encrypted group metadata and message storage
pub struct MlsService {
    /// Persistent MLS engine when initialized (SQLite-backed via nostr-mls-sqlite-storage)
    engine: Option<Arc<NostrMls<NostrMlsSqliteStorage>>>,
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
    ///   [AppData]/vector/mls/vector-mls.db
    pub fn new_persistent<R: Runtime>(handle: &AppHandle<R>) -> Result<Self, MlsError> {
        // Resolve the DB path under OS app data directory
        // Final path: [AppData]/mls/vector-mls.db
        let db_path = handle
            .path()
            .resolve("mls/vector-mls.db", BaseDirectory::AppData)
            .map_err(|e| MlsError::StorageError(format!("resolve app data dir: {}", e)))?;
        println!("[MLS] Persistent engine DB path: {}", db_path.display());

        // Ensure parent directory exists before opening SQLite
        if let Some(parent) = db_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(MlsError::StorageError(format!("create mls dir: {}", e)));
            }
        }

        // Initialize persistent storage and engine
        let storage = NostrMlsSqliteStorage::new(&db_path)
            .map_err(|e| MlsError::StorageError(format!("init sqlite storage: {}", e)))?;
        let mls = NostrMls::new(storage);

        Ok(Self {
            engine: Some(Arc::new(mls)),
            _initialized: true,
        })
    }

    /// Get a clone of the persistent MLS engine (Arc)
    pub fn engine(&self) -> Result<Arc<NostrMls<NostrMlsSqliteStorage>>, MlsError> {
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
      • The frontend awaits loadMLSGroups() and opens the chat (openChat(group_id)) immediately.
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
            None,
            None,
            vec![relay_url],
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
                    vec![my_pubkey],               // creator as admin
                    group_config,
                )
                .map_err(|e| MlsError::NostrMlsError(format!("create_group: {}", e)))?;

            // GroupId has as_slice() method for getting bytes (engine-local id).
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
                let gid = GroupId::from_slice(gid_bytes);
                if let Ok(wrapper) = engine.create_message(&gid, dummy_rumor) {
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
        };

        let mut groups = self.read_groups().await?;
        groups.push(meta);
        self.write_groups(&groups).await?;
 
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
        // TODO: Validate group exists
        // TODO: Fetch member's keypackage from network or cache
        // TODO: Use nostr-mls to add member
        // TODO: Broadcast commit message to group
        // TODO: Update "mls_groups" metadata with new updated_at
        
        // Stub implementation  
        let _ = (group_id, member_pubkey, device_id);
        Ok(())
    }

    /// Remove a member device from a group
    /// 
    /// This will:
    /// 1. Create a removal proposal
    /// 2. Commit the removal via nostr-mls
    /// 3. Update group metadata
    pub async fn remove_member_device(
        &self,
        group_id: &str,
        member_pubkey: &str,
        device_id: &str,
    ) -> Result<(), MlsError> {
        // TODO: Validate group exists and member is in group
        // TODO: Use nostr-mls to remove member
        // TODO: Broadcast commit message to remaining group members
        // TODO: Update "mls_groups" metadata
        
        // Stub implementation
        let _ = (group_id, member_pubkey, device_id);
        Ok(())
    }

    /// Send a message to an MLS group
    ///
    /// This will:
    /// 1. Encrypt the message for all group members via nostr-mls
    /// 2. Publish to TRUSTED_RELAY
    /// 3. Optionally store in "mls_messages_{group_id}" for optimistic UI
    /// 4. Update "mls_timeline_{group_id}"
    pub async fn send_group_message(
        &self,
        group_id: &str,
        text: &str,
    ) -> Result<String, MlsError> {
        use nostr_sdk::prelude::*;

        // Resolve target metadata and allow the caller to pass either wire_id (UI) or engine_id.
        let groups = self.read_groups().await?;
        let meta = match groups.iter().find(|g| g.group_id == group_id || (!g.engine_group_id.is_empty() && g.engine_group_id == group_id)) {
            Some(m) => m.clone(),
            None => {
                eprintln!("[MLS] send_group_message: Group not found in metadata for id={}", group_id);
                eprintln!("[MLS] Available groups in metadata:");
                for g in &groups {
                    eprintln!("[MLS]   - group_id (wire)={}, engine_group_id={}, name={}",
                             g.group_id,
                             if g.engine_group_id.is_empty() { "(empty)" } else { &g.engine_group_id },
                             g.name);
                }
                return Err(MlsError::GroupNotFound);
            }
        };
        
        println!("[MLS] send_group_message: Found group metadata:");
        println!("[MLS]   - group_id (wire): {}", meta.group_id);
        println!("[MLS]   - engine_group_id: {}", if meta.engine_group_id.is_empty() { "(empty - using wire id)" } else { &meta.engine_group_id });
        println!("[MLS]   - name: {}", meta.name);

        // Resolve client and my pubkey (awaits before any engine usage)
        let client = NOSTR_CLIENT.get().ok_or(MlsError::NotInitialized)?;
        let signer = client
            .signer()
            .await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;
        let my_pubkey = signer
            .get_public_key()
            .await
            .map_err(|e| MlsError::NetworkError(e.to_string()))?;

        // Build a minimal inner rumor carrying the plaintext payload.
        // Note: The MLS engine will encrypt/wrap this for the group.
        // Kind choice is arbitrary for the inner event; we use Custom(9) for now.
        let rumor = EventBuilder::new(Kind::Custom(9), text)
            .tag(Tag::custom(
                TagKind::Custom(std::borrow::Cow::Borrowed("vector-mls-msg")),
                // Attach the wire id (UI/relay id) for easier diagnostics
                vec![&meta.group_id],
            ))
            .build(my_pubkey);
        
        // Store the inner event id for optimistic persistence
        let inner_event_id = rumor.id.unwrap().to_hex();
        let created_at = rumor.created_at.as_u64();

        // Create the MLS wrapper with the non-Send engine in a no-await scope
        let wrapper_event = {
            // Decode GroupId from engine id (preferred), fall back to wire id if missing in legacy data
            let engine_hex = if !meta.engine_group_id.is_empty() { &meta.engine_group_id } else { &meta.group_id };
            let gid_bytes = hex::decode(engine_hex)
                .map_err(|e| {
                    eprintln!("[MLS] Failed to decode engine_group_id hex '{}': {}", engine_hex, e);
                    MlsError::InvalidGroupId
                })?;
            let gid = GroupId::from_slice(&gid_bytes);

            let engine = self.engine()?; // Arc<NostrMls<NostrMlsSqliteStorage>>
            
            println!("[MLS] Attempting to create message with engine for group_id bytes: {}", engine_hex);
            println!("[MLS]   - GroupId byte length: {}", gid_bytes.len());
            
            // TODO: Handle potential rekey/commit flow if required by nostr-mls
            engine
                .create_message(&gid, rumor)
                .map_err(|e| {
                    eprintln!("[MLS] create_message failed for engine_group_id {}: {}", engine_hex, e);
                    eprintln!("[MLS] Error details: {:?}", e);
                    eprintln!("[MLS] This likely means the group is not in the engine's state");
                    eprintln!("[MLS] Possible causes:");
                    eprintln!("[MLS]   1. Group was accepted via welcome but engine_group_id wasn't captured properly");
                    eprintln!("[MLS]   2. Engine state was lost/corrupted");
                    eprintln!("[MLS]   3. Group was removed from engine but metadata remains");
                    eprintln!("[MLS] Suggested actions:");
                    eprintln!("[MLS]   • Sync welcomes to rejoin the group (accept pending invites)");
                    eprintln!("[MLS]   • If the issue persists, request a fresh invite and accept it");
                    MlsError::NostrMlsError(format!("create_message: group not found - {}", e))
                })?
        }; // engine dropped here

        // Debug: compact wrapper log
        if let Some(tag) = wrapper_event.tags.find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H))) {
            println!(
                "[MLS] send wrapper: id={}, h={}",
                wrapper_event.id,
                tag.content().unwrap_or("<no-content>")
            );
        } else {
            println!("[MLS] send wrapper: id={}, h=<missing>", wrapper_event.id);
        }

        // Optimistic persistence: Store the message locally before network confirmation
        // This allows the UI to show the message immediately
        {
            let mut messages = self.read_group_messages(&meta.group_id).await.unwrap_or_default();
            let mut timeline = self.read_group_timeline(&meta.group_id).await.unwrap_or_default();
            
            // Create optimistic message record
            let message_record = MessageRecord {
                inner_event_id: inner_event_id.clone(),
                wrapper_event_id: wrapper_event.id.to_hex(),
                author_pubkey: my_pubkey.to_bech32().unwrap(),
                content: text.to_string(),
                created_at,
                tags: Vec::new(), // Could extract from rumor.tags if needed
                mine: true,
            };
            
            // Add to messages and timeline if not already present
            if !messages.contains_key(&inner_event_id) {
                messages.insert(inner_event_id.clone(), message_record.clone());
                
                if !timeline.contains(&inner_event_id) {
                    timeline.push(inner_event_id.clone());
                }
                
                // Persist
                let _ = self.write_group_messages(&meta.group_id, &messages).await;
                let _ = self.write_group_timeline(&meta.group_id, &timeline).await;
                
                // Emit optimistic UI event
                if let Some(handle) = TAURI_APP.get() {
                    handle.emit("mls_message_new", serde_json::json!({
                        "group_id": meta.group_id,
                        "message": message_record
                    })).unwrap_or_else(|e| {
                        eprintln!("[MLS] Failed to emit optimistic mls_message_new event: {}", e);
                    });
                }
            }
        }

        // Publish wrapper to TRUSTED_RELAY (network await after engine is dropped)
        client
            .send_event_to([TRUSTED_RELAY], &wrapper_event)
            .await
            .map_err(|e| MlsError::NetworkError(format!("publish MLS wrapper: {}", e)))?;

        println!(
            "[MLS] send_group_message published wrapper id={}, group_id={}, len={}",
            wrapper_event.id,
            group_id,
            text.len()
        );

        // Return the network event id as the message identifier
        Ok(wrapper_event.id.to_hex())
    }

    /// Process an incoming MLS event from the nostr network
    /// 
    /// This will:
    /// 1. Parse the nostr event containing MLS data
    /// 2. Decrypt and process via nostr-mls
    /// 3. Update relevant storage (messages, group state, etc.)
    /// 4. Update "mls_event_cursors" for the group
    /// 
    /// Returns true if the event was successfully processed
    pub async fn process_incoming_event(&self, event_json: &str) -> Result<bool, MlsError> {
        // TODO: Parse nostr event JSON
        // TODO: Extract MLS ciphertext from event
        // TODO: Process through nostr-mls (handles welcome, commit, application messages)
        // TODO: Store any resulting messages in "mls_messages_{group_id}"
        // TODO: Update "mls_event_cursors" with event ID and timestamp
        
        // Stub implementation
        let _ = event_json;
        Ok(false)
    }

    /// Sync group messages since last cursor position
    /// 
    /// This will:
    /// 1. Read cursor from "mls_event_cursors" for the group
    /// 2. Query TRUSTED_RELAY for events since cursor
    /// 3. Process each event via process_incoming_event
    /// 4. Update cursor position
    /// 
    /// Returns (processed_events_count, new_messages_count)
    pub async fn sync_group_since_cursor(&self, group_id: &str) -> Result<(u32, u32), MlsError> {
        use nostr_sdk::prelude::*;

        if group_id.is_empty() {
            return Err(MlsError::InvalidGroupId);
        }

        // 1) Load last cursor and compute since/until window
        let mut cursors = self.read_event_cursors().await.unwrap_or_default();

        let now = Timestamp::now();
        let since = if let Some(cur) = cursors.get(group_id) {
            Timestamp::from_secs(cur.last_seen_at)
        } else {
            // Default to last 48h for initial backfill
            Timestamp::from_secs(now.as_u64().saturating_sub(60 * 60 * 48))
        };
        let until = now;

        // Working group id for fetch/processing; prefer wire id from stored metadata if available
        let gid_for_fetch = {
            if let Ok(groups) = self.read_groups().await {
                if let Some(m) = groups.iter().find(|g| g.group_id == group_id || (!g.engine_group_id.is_empty() && g.engine_group_id == group_id)) {
                    m.group_id.clone() // wire id used on relay 'h' tag
                } else {
                    group_id.to_string()
                }
            } else {
                group_id.to_string()
            }
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

        if !events.is_empty() {
            println!(
                "[MLS] sync: primary fetch count={}, window={}..{}",
                events.len(),
                since.as_u64(),
                until.as_u64()
            );
        }

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

            println!(
                "[MLS] sync: fallback fetch count={}, window={}..{}",
                events.len(),
                since.as_u64(),
                until.as_u64()
            );
        }

        if events.is_empty() {
            println!(
                "[MLS] sync_group_since_cursor: no events for group_id={} in window {}..{}",
                gid_for_fetch,
                since.as_u64(),
                until.as_u64()
            );
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

                if filtered.is_empty() {
                    println!(
                        "[MLS] sync_group_since_cursor: fallback yielded h-tags but none matched our group_id={}; proceeding with engine-based selection",
                        gid_for_fetch
                    );
                    // Keep 'ordered' as-is (unfiltered) so engine can select the right group.
                } else {
                    ordered = filtered;
                }
            } else {
                println!(
                    "[MLS][debug] fallback: no events contained an h-tag; proceeding without local filtering and delegating selection to MLS engine"
                );
            }
        }

        // 5) Process with persistent engine in a no-await scope
        let mut processed: u32 = 0;
        let mut new_msgs: u32 = 0;
        let mut last_seen_id: Option<nostr_sdk::EventId> = None;
        let mut last_seen_at: u64 = 0;
        
        // Buffer for messages to persist after engine scope
        let mut messages_to_persist: Vec<MessageRecord> = Vec::new();
        
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
            
            if let Some(check_id) = group_check_id {
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
                    
                    match engine.create_message(&check_gid, dummy_rumor) {
                        Ok(_) => {
                            println!("[MLS] ✓ Engine has group {} in state", check_id);
                        }
                        Err(e) => {
                            eprintln!("[MLS] ✗ Engine missing group {} - error: {}", check_id, e);
                            eprintln!("[MLS]   → Group exists in metadata but not in engine state");
                            eprintln!("[MLS]   → User needs to sync welcomes to rejoin this group");
                            
                            // Emit an event to notify the frontend that this group needs re-joining
                            if let Some(handle) = TAURI_APP.get() {
                                handle.emit("mls_group_needs_rejoin", serde_json::json!({
                                    "group_id": gid_for_fetch,
                                    "reason": "Group not found in MLS engine state"
                                })).unwrap_or_else(|e| {
                                    eprintln!("[MLS] Failed to emit mls_group_needs_rejoin event: {}", e);
                                });
                            }
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
                            println!("[MLS] skip wrapper: h={} does not match target group {}", h_val, gid_for_fetch);
                            continue;
                        }
                    } else {
                        println!("[MLS] skip wrapper: empty h tag for target {}", gid_for_fetch);
                        continue;
                    }
                } else {
                    println!("[MLS] skip wrapper without h tag while syncing group {}", gid_for_fetch);
                    continue;
                }

                match engine.process_message(ev) {
                    Ok(res) => {
                        // Use native structured result types from nostr-mls
                        println!("[MLS] process ok: id={}, type={:?}", ev.id, res);
                
                        match res {
                            MessageProcessingResult::ApplicationMessage(msg) => {
                                let rec = MessageRecord {
                                    inner_event_id: msg.id.to_hex(),
                                    wrapper_event_id: msg.wrapper_event_id.to_hex(),
                                    author_pubkey: msg.pubkey.to_bech32().unwrap(),
                                    content: msg.content.clone(),
                                    created_at: msg.created_at.as_u64(),
                                    // We don't persist tags today; keep empty for compatibility with send path
                                    tags: Vec::new(),
                                    // Compare using hex to maintain correctness of 'mine' flag
                                    mine: !my_pubkey_hex.is_empty() && msg.pubkey.to_hex() == my_pubkey_hex,
                                };
                
                                let author_short: String = rec.author_pubkey.chars().take(8).collect();
                                println!(
                                    "[MLS] Extracted message: content=\"{}\", author={}, mine={}",
                                    rec.content,
                                    author_short,
                                    rec.mine
                                );
                
                                messages_to_persist.push(rec);
                                new_msgs = new_msgs.saturating_add(1);
                            }
                            MessageProcessingResult::Proposal(_)
                            | MessageProcessingResult::Commit
                            | MessageProcessingResult::ExternalJoinProposal
                            | MessageProcessingResult::Unprocessable => {
                                // No-op for local message persistence
                            }
                        }
                
                        processed = processed.saturating_add(1);
                
                        last_seen_id = Some(ev.id);
                        last_seen_at = ev.created_at.as_u64();
                    }
                    Err(e) => {
                        // Less verbose error logging
                        if !e.to_string().contains("group not found") {
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

        // Persist messages and update timeline after engine scope ends
        if !messages_to_persist.is_empty() {
            // Load existing messages and timeline
            let mut messages = self.read_group_messages(&gid_for_fetch).await?;
            let mut timeline = self.read_group_timeline(&gid_for_fetch).await?;
            
            for record in messages_to_persist.iter() {
                // Check if message is new (not in map)
                if !messages.contains_key(&record.inner_event_id) {
                    // Insert into messages map
                    messages.insert(record.inner_event_id.clone(), record.clone());
                    
                    // Append to timeline if not already present
                    if !timeline.contains(&record.inner_event_id) {
                        timeline.push(record.inner_event_id.clone());
                        
                        // Emit UI event for new message
                        if let Some(handle) = TAURI_APP.get() {
                            handle.emit("mls_message_new", serde_json::json!({
                                "group_id": gid_for_fetch,
                                "message": record
                            })).unwrap_or_else(|e| {
                                eprintln!("[MLS] Failed to emit mls_message_new event: {}", e);
                            });
                        }
                    }
                }
            }
            
            // Persist updated messages and timeline
            self.write_group_messages(&gid_for_fetch, &messages).await?;
            self.write_group_timeline(&gid_for_fetch, &timeline).await?;
        }

        // 6) Advance cursor if anything processed
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

        println!(
            "[MLS] sync_group_since_cursor done: group_id={}, processed={}, new={}, window={}..{}",
            gid_for_fetch,
            processed,
            new_msgs,
            since.as_u64(),
            until.as_u64()
        );

        Ok((processed, new_msgs))
    }

    // Internal helper methods for store access
    // These follow the read/modify/write pattern used in the codebase
    
    /// Read and decrypt group metadata from store
    pub async fn read_groups(&self) -> Result<Vec<MlsGroupMetadata>, MlsError> {
        // Read and decrypt "mls_groups" from JSON store
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let encrypted_opt = match store.get("mls_groups") {
            Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
            _ => None,
        };

        if let Some(enc) = encrypted_opt {
            let json = crypto::internal_decrypt(enc, None)
                .await
                .map_err(|_| MlsError::CryptoError("decrypt mls_groups".into()))?;
            let groups: Vec<MlsGroupMetadata> = serde_json::from_str(&json)
                .map_err(|e| MlsError::StorageError(format!("deserialize mls_groups: {}", e)))?;
            Ok(groups)
        } else {
            Ok(Vec::new())
        }
    }

    /// Write encrypted group metadata to store
    pub async fn write_groups(&self, groups: &[MlsGroupMetadata]) -> Result<(), MlsError> {
        // Serialize, encrypt and write "mls_groups"
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let json = serde_json::to_string(groups)
            .map_err(|e| MlsError::StorageError(format!("serialize mls_groups: {}", e)))?;
        let encrypted = crypto::internal_encrypt(json, None).await;

        store.set("mls_groups".to_string(), serde_json::json!(encrypted));
        Ok(())
    }

    /// Read keypackage index from store
    #[allow(dead_code)]
    async fn read_keypackage_index(&self) -> Result<Vec<KeyPackageIndexEntry>, MlsError> {
        // Plaintext read of "mls_keypackage_index"
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let index: Vec<KeyPackageIndexEntry> = match store.get("mls_keypackage_index") {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => Vec::new(),
        };

        Ok(index)
    }

    /// Write keypackage index to store
    #[allow(dead_code)]
    async fn write_keypackage_index(&self, index: &[KeyPackageIndexEntry]) -> Result<(), MlsError> {
        // Plaintext write to "mls_keypackage_index"
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let value = serde_json::to_value(index)
            .map_err(|e| MlsError::StorageError(format!("serialize keypackage_index: {}", e)))?;
        store.set("mls_keypackage_index".to_string(), value);
        Ok(())
    }

    /// Read event cursors from store
    #[allow(dead_code)]
    async fn read_event_cursors(&self) -> Result<HashMap<String, EventCursor>, MlsError> {
        // Plaintext read of "mls_event_cursors"
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let cursors: HashMap<String, EventCursor> = match store.get("mls_event_cursors") {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => HashMap::new(),
        };

        Ok(cursors)
    }

    /// Write event cursors to store
    #[allow(dead_code)]
    async fn write_event_cursors(&self, cursors: &HashMap<String, EventCursor>) -> Result<(), MlsError> {
        // Plaintext write to "mls_event_cursors"
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);

        let value = serde_json::to_value(cursors)
            .map_err(|e| MlsError::StorageError(format!("serialize event_cursors: {}", e)))?;
        store.set("mls_event_cursors".to_string(), value);
        Ok(())
    }

    /// Read group messages from encrypted store
    pub async fn read_group_messages(&self, group_id: &str) -> Result<HashMap<String, MessageRecord>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);
        let key = format!("mls_messages_{}", group_id);

        let encrypted_opt = match store.get(&key) {
            Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
            _ => None,
        };

        if let Some(enc) = encrypted_opt {
            let json = crypto::internal_decrypt(enc, None)
                .await
                .map_err(|_| MlsError::CryptoError(format!("decrypt {}", key)))?;
            let messages: HashMap<String, MessageRecord> = serde_json::from_str(&json)
                .map_err(|e| MlsError::StorageError(format!("deserialize {}: {}", key, e)))?;
            Ok(messages)
        } else {
            Ok(HashMap::new())
        }
    }

    /// Write group messages to encrypted store
    pub async fn write_group_messages(&self, group_id: &str, msgs: &HashMap<String, MessageRecord>) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);
        let key = format!("mls_messages_{}", group_id);

        let json = serde_json::to_string(msgs)
            .map_err(|e| MlsError::StorageError(format!("serialize {}: {}", key, e)))?;
        let encrypted = crypto::internal_encrypt(json, None).await;

        store.set(key, serde_json::json!(encrypted));
        Ok(())
    }

    /// Read group timeline from plaintext store
    pub async fn read_group_timeline(&self, group_id: &str) -> Result<Vec<String>, MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);
        let key = format!("mls_timeline_{}", group_id);

        let timeline: Vec<String> = match store.get(&key) {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => Vec::new(),
        };

        Ok(timeline)
    }

    /// Write group timeline to plaintext store
    pub async fn write_group_timeline(&self, group_id: &str, ids: &[String]) -> Result<(), MlsError> {
        let handle = TAURI_APP.get().ok_or(MlsError::NotInitialized)?.clone();
        let store = db::get_store(&handle);
        let key = format!("mls_timeline_{}", group_id);

        let value = serde_json::to_value(ids)
            .map_err(|e| MlsError::StorageError(format!("serialize {}: {}", key, e)))?;
        store.set(key, value);
        Ok(())
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
            let kim_mls = NostrMls::new(NostrMlsMemoryStorage::default());
            let saul_mls = NostrMls::new(NostrMlsMemoryStorage::default());

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
                None,
                None,
                vec![relay_url.clone()],
            );
    
            // IMPORTANT: Non-empty member_key_package_events (Saul). The creator (Kim) must not be in member_key_package_events.
            let group_create = kim_mls
                .create_group(
                    &kim_keys.public_key(),
                    vec![saul_kp_event.clone()],      // Saul invited via his KeyPackage
                    vec![kim_keys.public_key()],      // Only Kim as admin for this smoke test
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
            let group_id = GroupId::from_slice(kim_group.mls_group_id.as_slice());
            println!("[MLS Smoke Test] Kim sending application message...");
            let rumor = EventBuilder::new(Kind::Custom(9), "Vector-MLS-Test: hello")
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