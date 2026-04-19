//! Vector Core — the single source of truth for all Vector clients, SDKs, and interfaces.
//!
//! This crate contains ALL of Vector's business logic, fully decoupled from Tauri.
//! It can be used by:
//! - **src-tauri**: The Tauri desktop/mobile app (thin command shell)
//! - **vector-cli**: Command-line interface
//! - **Vector SDK**: Bot and client libraries
//! - Any future interface (web, embedded, etc.)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │              vector-core                     │
//! │                                              │
//! │  types ─ compact ─ state ─ db ─ crypto       │
//! │  chat ─ profile ─ net ─ hex                  │
//! │                                              │
//! │  traits::EventEmitter (UI abstraction)       │
//! │  VectorCore (high-level API)                 │
//! └─────────────────────────────────────────────┘
//!        ▲              ▲              ▲
//!   src-tauri       vector-cli     Vector SDK
//! (AppHandle)      (terminal)     (callbacks)
//! ```

// === Logging (must be first — #[macro_export] macros used by all modules) ===
#[macro_use]
mod macros;

// === Foundation ===
pub mod error;
pub mod traits;

// Nostr SDK trait imports needed for bech32 operations
use nostr_sdk::prelude::ToBech32;

// === Core Types ===
pub mod types;
pub mod profile;
pub mod chat;
pub mod compact;

// === State ===
pub mod state;

// === Debug Stats ===
#[cfg(debug_assertions)]
pub mod stats;

// === Crypto ===
pub mod crypto;

// === Database ===
pub mod db;

// === Network ===
pub mod net;
pub mod blossom;
pub mod inbox_relays;

// === Event Storage ===
pub mod stored_event;

// === Rumor Processing ===
pub mod rumor;

// === Messaging ===
pub mod sending;

// === SIMD Operations ===
pub mod simd;

// === MLS Group Encryption ===
pub mod mls;

// === Event Handler ===
pub mod event_handler;

// === Re-exports for convenience ===
pub use types::{Message, Attachment, Reaction, EditEntry, ImageMetadata, SiteMetadata, LoginResult, AttachmentFile, mention, extract_mentions};
pub use profile::{Profile, ProfileFlags, SlimProfile, Status};
pub use chat::{Chat, ChatType, ChatMetadata, SerializableChat};
pub use compact::{CompactMessage, CompactMessageVec, NpubInterner};
pub use state::{ChatState, NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE, ENCRYPTION_KEY};
pub use crypto::{GuardedKey, GuardedSigner};
pub use error::{VectorError, Result};
pub use traits::{EventEmitter, NoOpEmitter, set_event_emitter, emit_event};
pub use db::{set_app_data_dir, get_app_data_dir};
pub use sending::{SendCallback, NoOpSendCallback, SendConfig, SendResult};
pub use stored_event::{StoredEvent, StoredEventBuilder, SystemEventType};
pub use rumor::{RumorEvent, RumorContext, ConversationType, RumorProcessingResult, process_rumor};
pub use profile::{SyncPriority, ProfileSyncHandler, NoOpProfileSyncHandler};
pub use event_handler::{InboundEventHandler, NoOpEventHandler, PreparedEvent, process_event};

use std::path::PathBuf;
use std::sync::Arc;

/// Current MLS group message subscription ID. Updated by `refresh_group_subscription()`.
static MLS_SUB_ID: std::sync::LazyLock<tokio::sync::Mutex<Option<nostr_sdk::SubscriptionId>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

// ============================================================================
// VectorCore — High-level API
// ============================================================================

/// Configuration for initializing VectorCore.
pub struct CoreConfig {
    /// Path to the app data directory (e.g., ~/.local/share/io.vectorapp/data/)
    pub data_dir: PathBuf,
    /// Optional event emitter for UI integration
    pub event_emitter: Option<Box<dyn EventEmitter>>,
}

/// The main entry point for Vector Core.
///
/// Provides a high-level API for all Vector operations. Internally uses
/// global state (same pattern as the Tauri backend) for compatibility.
///
/// ```no_run
/// use vector_core::{VectorCore, CoreConfig};
/// use std::path::PathBuf;
///
/// # async fn example() -> vector_core::Result<()> {
/// let core = VectorCore::init(CoreConfig {
///     data_dir: PathBuf::from("/tmp/vector-data"),
///     event_emitter: None,
/// })?;
///
/// // Login with nsec
/// let result = core.login("nsec1...", None).await?;
/// println!("Logged in as {}", result.npub);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct VectorCore;

impl VectorCore {
    /// Initialize Vector Core with the given configuration.
    pub fn init(config: CoreConfig) -> Result<Self> {
        // Set data directory
        db::set_app_data_dir(config.data_dir);

        // Set event emitter (or no-op)
        if let Some(emitter) = config.event_emitter {
            traits::set_event_emitter(emitter);
        }

        // Install rustls ring provider
        let _ = rustls::crypto::ring::default_provider().install_default();

        Ok(VectorCore)
    }

    /// Get all available accounts.
    pub fn accounts(&self) -> Result<Vec<String>> {
        db::get_accounts().map_err(VectorError::from)
    }

    /// Login with an nsec key or mnemonic seed phrase.
    pub async fn login(&self, key: &str, password: Option<&str>) -> Result<LoginResult> {
        use nostr_sdk::prelude::*;

        // Parse the key
        let keys = if key.starts_with("nsec1") {
            let secret = SecretKey::from_bech32(key)
                .map_err(|e| VectorError::Nostr(format!("Invalid nsec: {}", e)))?;
            Keys::new(secret)
        } else {
            // Treat as mnemonic (NIP-06: derive from BIP-39 seed)
            Keys::from_mnemonic(key, None)
                .map_err(|e| VectorError::Nostr(format!("Key derivation failed: {}", e)))?
        };

        let public_key = keys.public_key();
        let npub = public_key.to_bech32()
            .map_err(|e| VectorError::Nostr(format!("Failed to encode npub: {}", e)))?;

        // Store in GuardedKey vault (pass other vaults to protect during decoy writes)
        let secret_bytes = keys.secret_key().to_secret_bytes();
        state::MY_SECRET_KEY.set(secret_bytes, &[&state::ENCRYPTION_KEY]);
        let _ = state::MY_PUBLIC_KEY.set(public_key);

        // Initialize database for this account
        db::set_current_account(npub.clone())?;
        db::init_database(&npub)?;

        // Store nsec for encryption setup
        {
            let nsec = keys.secret_key().to_bech32()
                .map_err(|e| VectorError::Nostr(format!("Failed to encode nsec: {}", e)))?;
            *state::PENDING_NSEC.lock().unwrap() = Some(nsec.clone());

            // Store pkey in DB
            db::set_pkey(&nsec)?;
        }

        // Check if encryption is set up
        let has_encryption = db::get_sql_setting("encryption_enabled".to_string())
            .ok().flatten()
            .map(|v| v != "false")
            .unwrap_or(false);

        if has_encryption {
            if let Some(pwd) = password {
                let key = crate::crypto::hash_pass(pwd).await;
                state::ENCRYPTION_KEY.set(key, &[&state::MY_SECRET_KEY]);
            }
            state::init_encryption_enabled();
        }

        // Build Nostr client
        let client = ClientBuilder::new().signer(keys).build();

        // Add trusted relays
        for relay in state::TRUSTED_RELAYS {
            client.add_relay(*relay).await.ok();
        }

        // Connect
        client.connect().await;

        let _ = state::NOSTR_CLIENT.set(client);

        Ok(LoginResult { npub, has_encryption })
    }

    /// Send a NIP-17 gift-wrapped text DM using the full pipeline.
    pub async fn send_dm(&self, to_npub: &str, content: &str) -> Result<sending::SendResult> {
        sending::send_dm(to_npub, content, None, &SendConfig::default(), Arc::new(NoOpSendCallback)).await
            .map_err(|e| VectorError::Other(e))
    }

    /// Send a NIP-17 gift-wrapped file attachment DM.
    pub async fn send_file(&self, to_npub: &str, file_path: &str) -> Result<sending::SendResult> {
        let path = std::path::Path::new(file_path);
        let bytes = std::fs::read(path)
            .map_err(|e| VectorError::Io(e))?;
        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");

        sending::send_file_dm(
            to_npub,
            std::sync::Arc::new(bytes),
            filename,
            extension,
            None,
            &SendConfig::default(),
            Arc::new(NoOpSendCallback),
        ).await.map_err(|e| VectorError::Other(e))
    }

    /// Get chats from the in-memory state.
    pub async fn get_chats(&self) -> Vec<SerializableChat> {
        let state = state::STATE.lock().await;
        state.chats.iter()
            .map(|c| c.to_serializable_with_last_n(1, &state.interner))
            .collect()
    }

    /// Get messages for a chat (paginated).
    pub async fn get_messages(&self, chat_id: &str, limit: usize, offset: usize) -> Vec<Message> {
        let state = state::STATE.lock().await;
        if let Some(chat) = state.get_chat(chat_id) {
            let msgs = chat.get_all_messages(&state.interner);
            let start = offset.min(msgs.len());
            let end = (offset + limit).min(msgs.len());
            msgs[start..end].to_vec()
        } else {
            Vec::new()
        }
    }

    /// Get a profile by npub.
    pub async fn get_profile(&self, npub: &str) -> Option<SlimProfile> {
        let state = state::STATE.lock().await;
        state.get_profile(npub)
            .map(|p| SlimProfile::from_profile(p, &state.interner))
    }

    /// Fetch a profile's metadata and status from relays.
    pub async fn load_profile(&self, npub: &str) -> bool {
        profile::sync::load_profile(npub.to_string(), &NoOpProfileSyncHandler).await
    }

    /// Update the current user's profile metadata and broadcast to relays.
    pub async fn update_profile(&self, name: &str, avatar: &str, banner: &str, about: &str) -> bool {
        profile::sync::update_profile(
            name.to_string(), avatar.to_string(), banner.to_string(), about.to_string(),
            &NoOpProfileSyncHandler,
        ).await
    }

    /// Update the current user's status and broadcast to relays.
    pub async fn update_status(&self, status: &str) -> bool {
        profile::sync::update_status(status.to_string()).await
    }

    /// Block a user by npub.
    pub async fn block_user(&self, npub: &str) -> bool {
        profile::sync::block_user(npub.to_string(), &NoOpProfileSyncHandler).await
    }

    /// Unblock a user by npub.
    pub async fn unblock_user(&self, npub: &str) -> bool {
        profile::sync::unblock_user(npub.to_string(), &NoOpProfileSyncHandler).await
    }

    /// Set a nickname for a profile.
    pub async fn set_nickname(&self, npub: &str, nickname: &str) -> bool {
        profile::sync::set_nickname(npub.to_string(), nickname.to_string(), &NoOpProfileSyncHandler).await
    }

    /// Get all blocked profiles.
    pub async fn get_blocked_users(&self) -> Vec<SlimProfile> {
        profile::sync::get_blocked_users().await
    }

    /// Queue a profile for background sync.
    pub fn queue_profile_sync(&self, npub: &str, priority: SyncPriority) {
        profile::sync::queue_profile_sync(npub.to_string(), priority, false);
    }

    /// Get the current user's npub.
    pub fn my_npub(&self) -> Option<String> {
        state::MY_PUBLIC_KEY.get()
            .and_then(|pk| ToBech32::to_bech32(pk).ok())
    }

    // ========================================================================
    // MLS Groups
    // ========================================================================

    /// Create a new MLS group and invite members.
    ///
    /// Returns the wire group_id (64-hex, used for relay filtering and UI).
    ///
    /// ```no_run
    /// # async fn example() -> vector_core::Result<()> {
    /// let core = vector_core::VectorCore;
    /// let group_id = core.create_group(
    ///     "My Group",
    ///     &[("npub1alice...", "device1"), ("npub1bob...", "device2")],
    /// ).await?;
    /// core.send_group_message(&group_id, "Hello group!").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create_group(
        &self,
        name: &str,
        member_devices: &[(&str, &str)], // (npub, device_id) pairs
    ) -> Result<String> {
        let devices: Vec<(String, String)> = member_devices.iter()
            .map(|(npub, did)| (npub.to_string(), did.to_string()))
            .collect();

        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;

        let group_id = svc.create_group(name, None, None, &devices, None, None, None, None, &[])
            .await
            .map_err(|e| VectorError::Other(e.to_string()))?;

        // Refresh subscription so listen() picks up the new group
        let _ = self.refresh_group_subscription().await;

        // MLS security: rotate keypackage so the next join uses a fresh one.
        // Reusing a KeyPackage across multiple group joins breaks forward secrecy.
        // Fire-and-forget — group creation already succeeded.
        tokio::spawn(async move {
            if let Err(e) = mls::publish_keypackage(false).await {
                log_warn!("[MLS] KeyPackage rotation after create_group failed: {}", e);
            }
        });

        Ok(group_id)
    }

    /// Send a text message to an MLS group.
    pub async fn send_group_message(&self, group_id: &str, content: &str) -> Result<()> {
        use nostr_sdk::prelude::*;

        let my_pk = state::MY_PUBLIC_KEY.get()
            .copied()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        let milliseconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_millis() % 1000;

        let rumor = EventBuilder::new(
            Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE),
            content,
        )
        .tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]))
        .build(my_pk);

        let pending_id = format!("pending-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());

        // Add to STATE as pending
        let msg = Message {
            id: pending_id.clone(),
            content: content.to_string(),
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap()
                .as_millis() as u64,
            pending: true,
            mine: true,
            npub: my_pk.to_bech32().ok(),
            ..Default::default()
        };
        {
            let mut state_guard = state::STATE.lock().await;
            state_guard.create_or_get_mls_group_chat(group_id, vec![]);
            state_guard.add_message_to_chat(group_id, msg);
        }

        mls::send_mls_message(group_id, rumor, Some(pending_id))
            .await
            .map_err(|e| VectorError::Other(e))
    }

    /// List all MLS groups.
    pub async fn list_groups(&self) -> Result<Vec<mls::MlsGroupFull>> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.read_groups()
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Get members of an MLS group.
    ///
    /// Returns (wire_group_id, member_npubs, admin_npubs).
    pub fn get_group_members(&self, group_id: &str) -> Result<(String, Vec<String>, Vec<String>)> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.get_group_members(group_id)
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Leave an MLS group.
    pub async fn leave_group(&self, group_id: &str) -> Result<()> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.leave_group(group_id).await
            .map_err(|e| VectorError::Other(e.to_string()))?;
        let _ = self.refresh_group_subscription().await;
        Ok(())
    }

    /// Fetch a user's published MLS keypackages from relays.
    ///
    /// Returns a list of (device_id, created_at) pairs, newest first.
    /// Device IDs are the keypackage event IDs (hex). Also persists results
    /// to the local keypackage index (deduplicated, merged with any local entries).
    pub async fn fetch_keypackages(&self, npub: &str) -> Result<Vec<(String, u64)>> {
        use futures_util::StreamExt;
        use nostr_sdk::prelude::*;

        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let contact_pubkey = PublicKey::from_bech32(npub)
            .map_err(|e| VectorError::Nostr(format!("Invalid npub: {}", e)))?;

        let filter = Filter::new()
            .author(contact_pubkey)
            .kind(Kind::MlsKeyPackage)
            .limit(10);

        let mut events = client
            .stream_events_from(
                state::active_trusted_relays().await,
                filter,
                std::time::Duration::from_secs(10),
            )
            .await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;

        let owner_pubkey_b32 = contact_pubkey.to_bech32()
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        let mut results: Vec<(String, u64)> = Vec::new();
        let mut new_entries: Vec<serde_json::Value> = Vec::new();

        while let Some(e) = events.next().await {
            let device_id = e.id.to_hex();
            let keypackage_ref = e.id.to_hex();
            let created_at = e.created_at.as_secs();
            results.push((device_id.clone(), created_at));
            new_entries.push(serde_json::json!({
                "owner_pubkey": owner_pubkey_b32,
                "device_id": device_id,
                "keypackage_ref": keypackage_ref,
                "created_at": created_at,
                "fetched_at": Timestamp::now().as_secs(),
                "expires_at": 0u64
            }));
        }

        // Update local plaintext index (dedup + merge)
        let mut index = db::mls::load_mls_keypackages().unwrap_or_default();

        // Dedup existing entries by keypackage_ref
        {
            let mut seen_refs = std::collections::HashSet::new();
            index.retain(|entry| {
                let r = entry.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                seen_refs.insert(r)
            });
        }

        // Merge new entries, preserving local entries with matching keypackage_ref
        let mut index_changed = false;
        for new_entry in new_entries {
            let new_ref = new_entry.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or_default();
            let new_owner = new_entry.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or_default();
            let new_device = new_entry.get("device_id").and_then(|v| v.as_str()).unwrap_or_default();

            if index.iter().any(|entry| entry.get("keypackage_ref").and_then(|v| v.as_str()) == Some(new_ref)) {
                continue;
            }

            index.retain(|entry| {
                let same_owner = entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(new_owner);
                let same_device = entry.get("device_id").and_then(|v| v.as_str()) == Some(new_device);
                !(same_owner && same_device)
            });
            index.push(new_entry);
            index_changed = true;
        }

        if index_changed {
            let _ = db::mls::save_mls_keypackages(&index);
        }

        // Newest first
        results.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(results)
    }

    /// Invite a member to an existing MLS group.
    ///
    /// Fetches the member's keypackage, creates a commit, publishes to relays,
    /// and sends a welcome message. Runs in a background task.
    pub async fn invite_member(&self, group_id: &str, member_npub: &str, device_id: &str) -> Result<()> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.add_member_device(group_id, member_npub, device_id).await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Invite a member to a group by npub only — fetches their latest keypackage automatically.
    ///
    /// Abstracts away keypackage discovery. Returns the device_id that was used.
    pub async fn invite(&self, group_id: &str, member_npub: &str) -> Result<String> {
        let keypackages = self.fetch_keypackages(member_npub).await?;
        let (device_id, _) = keypackages.into_iter().next()
            .ok_or(VectorError::Other(format!("No keypackages found for {}", member_npub)))?;
        self.invite_member(group_id, member_npub, &device_id).await?;
        Ok(device_id)
    }

    /// Invite multiple members to an existing MLS group in a single commit.
    pub async fn invite_members(&self, group_id: &str, members: &[(&str, &str)]) -> Result<()> {
        let devices: Vec<(String, String)> = members.iter()
            .map(|(npub, did)| (npub.to_string(), did.to_string()))
            .collect();
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.add_member_devices(group_id, &devices).await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Remove a member from an MLS group (admin only).
    pub async fn remove_member(&self, group_id: &str, member_npub: &str) -> Result<()> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.remove_member_device(group_id, member_npub, "").await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Update MLS group metadata (name, description, admins).
    pub async fn update_group(
        &self,
        group_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        admin_npubs: Option<&[&str]>,
    ) -> Result<()> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;
        svc.update_group_data(
            group_id,
            name.map(|s| s.to_string()),
            description.map(|s| s.to_string()),
            admin_npubs.map(|npubs| npubs.iter().map(|s| s.to_string()).collect()),
            None, None, None,
        ).await.map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Publish this device's MLS KeyPackage to relays.
    ///
    /// Required before anyone can invite you to MLS groups. If `use_cache` is true,
    /// reuses an existing valid keypackage if one exists on relay. Otherwise generates
    /// and publishes a fresh one.
    pub async fn publish_keypackage(&self, use_cache: bool) -> Result<mls::PublishedKeyPackage> {
        mls::publish_keypackage(use_cache).await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// List all pending MLS group invites (unaccepted welcomes).
    pub async fn list_invites(&self) -> Result<Vec<mls::PendingInvite>> {
        mls::list_invites().await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Accept a pending MLS group invite by welcome event ID.
    ///
    /// Joins the group, persists metadata, creates chat, syncs participants,
    /// and does an initial message sync. Returns the wire group_id.
    pub async fn accept_invite(&self, welcome_event_id: &str) -> Result<String> {
        let group_id = mls::accept_invite(welcome_event_id).await
            .map_err(|e| VectorError::Other(e.to_string()))?;
        // Refresh subscription so listen() picks up the new group
        let _ = self.refresh_group_subscription().await;

        // MLS security: rotate keypackage so our NEXT invite uses a fresh one.
        // The keypackage we just consumed is now burnt — reusing it would break
        // forward secrecy and can corrupt MDK's internal state.
        // Fire-and-forget — we're already in the group.
        tokio::spawn(async move {
            if let Err(e) = mls::publish_keypackage(false).await {
                log_warn!("[MLS] KeyPackage rotation after accept_invite failed: {}", e);
            }
        });

        Ok(group_id)
    }

    /// Decline a pending MLS group invite (removes it without joining).
    pub async fn decline_invite(&self, welcome_event_id: &str) -> Result<()> {
        mls::decline_invite(welcome_event_id).await
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Sync all MLS groups from relays.
    ///
    /// Returns total (processed_events, new_messages) across all groups.
    pub async fn sync_groups(&self) -> Result<(u32, u32)> {
        let svc = mls::MlsService::new_persistent_static()
            .map_err(|e| VectorError::Other(e.to_string()))?;

        let groups = svc.read_groups()
            .map_err(|e| VectorError::Other(e.to_string()))?;

        let mut total_processed = 0u32;
        let mut total_new = 0u32;

        for group in &groups {
            if group.evicted { continue; }
            match svc.sync_group_since_cursor(&group.group_id, None).await {
                Ok((p, n)) => {
                    total_processed += p;
                    total_new += n;
                }
                Err(e) => eprintln!("[VectorCore] sync_group {} failed: {}", &group.group_id[..8.min(group.group_id.len())], e),
            }
        }

        Ok((total_processed, total_new))
    }

    /// Sync DM history from relays using NIP-77 negentropy set reconciliation.
    ///
    /// Reconciles local wrapper history with relay state, fetches missing events,
    /// and processes them through the standard prepare → commit pipeline.
    ///
    /// Returns (total_events, new_messages).
    ///
    /// ```no_run
    /// # async fn example() -> vector_core::Result<()> {
    /// let core = vector_core::VectorCore;
    /// // Sync last 7 days of DMs
    /// let (events, new) = core.sync_dms(Some(7), &vector_core::NoOpEventHandler).await?;
    /// println!("Processed {} events, {} new messages", events, new);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn sync_dms(
        &self,
        since_days: Option<u64>,
        handler: &dyn InboundEventHandler,
    ) -> Result<(u32, u32)> {
        use futures_util::StreamExt;
        use nostr_sdk::prelude::*;

        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::MY_PUBLIC_KEY.get()
            .copied()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        // Load known wrapper IDs + timestamps for negentropy fingerprinting
        let all_items = db::wrappers::load_negentropy_items().unwrap_or_default();

        // Filter items to time window (or use all for full sync)
        let (items, filter) = if let Some(days) = since_days {
            let since_ts = Timestamp::now().as_secs().saturating_sub(days * 24 * 3600);
            let items: Vec<(EventId, Timestamp)> = all_items.iter()
                .filter(|(_, ts)| ts.as_secs() >= since_ts)
                .cloned()
                .collect();
            let filter = Filter::new()
                .pubkey(my_pk)
                .kind(Kind::GiftWrap)
                .since(Timestamp::from_secs(since_ts));
            (items, filter)
        } else {
            let filter = Filter::new()
                .pubkey(my_pk)
                .kind(Kind::GiftWrap);
            (all_items, filter)
        };

        log_info!("[SyncDMs] {} negentropy items, since_days={:?}", items.len(), since_days);

        // Dry-run negentropy: exchange fingerprints to identify missing events
        let sync_opts = nostr_sdk::SyncOptions::new()
            .direction(nostr_sdk::SyncDirection::Down)
            .initial_timeout(std::time::Duration::from_secs(10))
            .dry_run();

        // Race all relays — first to reconcile drives the fetch
        let relay_map = client.relays().await;
        let all_relays: Vec<(RelayUrl, Relay)> = relay_map.iter()
            .map(|(url, relay)| (url.clone(), relay.clone()))
            .collect();
        drop(relay_map);

        let mut relay_futs = futures_util::stream::FuturesUnordered::new();
        for (url, relay) in &all_relays {
            let url = url.clone();
            let relay = relay.clone();
            let f = filter.clone();
            let i = items.clone();
            let o = sync_opts.clone();
            relay_futs.push(async move {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    relay.sync_with_items(f, i, &o),
                ).await;
                (url, result)
            });
        }

        // Collect missing IDs from all relays
        let mut all_missing: std::collections::HashSet<EventId> = std::collections::HashSet::new();
        while let Some((url, result)) = relay_futs.next().await {
            match result {
                Ok(Ok(recon)) => {
                    let count = recon.remote.len();
                    all_missing.extend(recon.remote);
                    log_info!("[SyncDMs] {} reconciled: {} missing", url, count);
                }
                Ok(Err(e)) => log_warn!("[SyncDMs] {} failed: {}", url, e),
                Err(_) => log_warn!("[SyncDMs] {} timed out (10s)", url),
            }
        }

        if all_missing.is_empty() {
            log_info!("[SyncDMs] No missing events");
            return Ok((0, 0));
        }

        // Fetch missing events in batches
        log_info!("[SyncDMs] Fetching {} missing events", all_missing.len());
        let ids: Vec<EventId> = all_missing.into_iter().collect();
        let relay_strs: Vec<String> = client.relays().await.keys()
            .map(|u| u.to_string()).collect();

        let mut total_events = 0u32;
        let mut new_messages = 0u32;
        const BATCH_SIZE: usize = 500;

        for batch in ids.chunks(BATCH_SIZE) {
            let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap);
            match client.stream_events_from(
                relay_strs.clone(), f,
                std::time::Duration::from_secs(30),
            ).await {
                Ok(stream) => {
                    let client_clone = client.clone();
                    let prepared_stream = stream
                        .map(move |event| {
                            let c = client_clone.clone();
                            tokio::spawn(async move {
                                event_handler::prepare_event(event, &c, my_pk).await
                            })
                        })
                        .buffer_unordered(8);
                    tokio::pin!(prepared_stream);

                    while let Some(result) = prepared_stream.next().await {
                        total_events += 1;
                        if let Ok(prepared) = result {
                            if event_handler::commit_prepared_event(prepared, false, handler).await {
                                new_messages += 1;
                            }
                        }
                    }
                }
                Err(e) => log_warn!("[SyncDMs] Batch fetch error: {}", e),
            }
        }

        log_info!("[SyncDMs] Complete: {} events processed, {} new messages", total_events, new_messages);
        Ok((total_events, new_messages))
    }

    // ========================================================================
    // Event Subscription
    // ========================================================================

    /// Subscribe to incoming DM events (NIP-17 GiftWraps).
    ///
    /// Returns the subscription ID for use in a custom notification loop.
    /// For a complete listen-and-process loop, use [`listen()`](Self::listen) instead.
    pub async fn subscribe_dms(&self) -> Result<nostr_sdk::SubscriptionId> {
        use nostr_sdk::prelude::*;
        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::MY_PUBLIC_KEY.get()
            .copied()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        let filter = Filter::new()
            .pubkey(my_pk)
            .kind(Kind::GiftWrap)
            .limit(0);

        let output = client.subscribe(filter, None).await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        Ok(output.val)
    }

    /// Refresh the MLS group message subscription to match current group membership.
    ///
    /// Unsubscribes the old subscription (if any) and creates a new one scoped to
    /// all non-evicted groups. Called automatically by `create_group`, `leave_group`,
    /// `invite_member`, and `remove_member`. Can also be called manually.
    pub async fn refresh_group_subscription(&self) -> Result<()> {
        use nostr_sdk::prelude::*;
        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;

        let mut sub_guard = MLS_SUB_ID.lock().await;

        // Unsubscribe old
        if let Some(old_id) = sub_guard.take() {
            client.unsubscribe(&old_id).await;
        }

        // Subscribe with current group IDs
        let group_ids: Vec<String> = db::mls::load_mls_groups()
            .unwrap_or_default()
            .into_iter()
            .filter(|g| !g.evicted)
            .map(|g| g.group.group_id)
            .collect();

        if !group_ids.is_empty() {
            let filter = Filter::new()
                .kind(Kind::MlsGroupMessage)
                .custom_tags(SingleLetterTag::lowercase(Alphabet::H), group_ids)
                .limit(0);
            match client.subscribe(filter, None).await {
                Ok(output) => { *sub_guard = Some(output.val); }
                Err(e) => eprintln!("[VectorCore] Failed to subscribe to MLS groups: {}", e),
            }
        }

        Ok(())
    }

    /// Start listening for incoming DMs AND MLS group messages.
    ///
    /// Blocks until the client disconnects. Processes both event types:
    /// - GiftWraps (DMs, files, MLS welcomes) → prepare_event → commit_prepared_event
    /// - MLS group messages (Kind 445) → handle_mls_group_message
    ///
    /// MLS group subscriptions are automatically refreshed when groups change
    /// (via `create_group`, `leave_group`, etc.).
    ///
    /// ```no_run
    /// use vector_core::*;
    /// use std::sync::Arc;
    ///
    /// struct MyBot;
    /// impl InboundEventHandler for MyBot {
    ///     fn on_dm_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
    ///         if msg.mine { return; }
    ///         let to = chat_id.to_string();
    ///         let reply = format!("Echo: {}", msg.content);
    ///         tokio::spawn(async move {
    ///             let _ = VectorCore.send_dm(&to, &reply).await;
    ///         });
    ///     }
    /// }
    ///
    /// # async fn example() -> vector_core::Result<()> {
    /// let core = VectorCore::init(CoreConfig {
    ///     data_dir: "/tmp/bot-data".into(),
    ///     event_emitter: None,
    /// })?;
    /// core.login("nsec1...", None).await?;
    /// core.listen(Arc::new(MyBot)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn listen(&self, handler: Arc<dyn InboundEventHandler>) -> Result<()> {
        use nostr_sdk::prelude::*;

        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::MY_PUBLIC_KEY.get()
            .copied()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        // Subscribe to DMs (GiftWraps)
        let dm_sub_id = self.subscribe_dms().await?;

        // Initial MLS group subscription (refreshed automatically on group changes)
        self.refresh_group_subscription().await?;

        let client_for_closure = client.clone();

        client.handle_notifications(move |notification| {
            let handler = handler.clone();
            let c = client_for_closure.clone();
            let dm_sid = dm_sub_id.clone();
            async move {
                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    if subscription_id == dm_sid {
                        // DMs, files, reactions, MLS welcomes
                        let prepared = event_handler::prepare_event(*event, &c, my_pk).await;
                        event_handler::commit_prepared_event(prepared, true, &*handler).await;
                    } else if MLS_SUB_ID.lock().await.as_ref() == Some(&subscription_id) {
                        // MLS group messages — pass handler for on_group_message callback
                        mls::handle_mls_group_message_with_handler(*event, my_pk, Some(&*handler)).await;
                    }
                }
                Ok(false)
            }
        }).await.map_err(|e| VectorError::Nostr(e.to_string()))?;

        Ok(())
    }

    /// Disconnect and clean up.
    pub async fn logout(&self) {
        if let Some(client) = state::NOSTR_CLIENT.get() {
            let _ = client.disconnect().await;
        }
        db::close_database();
    }
}
