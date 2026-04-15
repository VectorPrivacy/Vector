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
pub mod hex;

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

// === Event Handler ===
pub mod event_handler;

// === Re-exports for convenience ===
pub use types::{Message, Attachment, Reaction, EditEntry, ImageMetadata, SiteMetadata, LoginResult, AttachmentFile};
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

    /// Start listening for incoming DMs and process them through the event pipeline.
    ///
    /// Blocks until the client disconnects. Each event flows through:
    /// `prepare_event` (dedup, unwrap, parse) → `commit_prepared_event` (STATE, DB, emit, handler hooks)
    ///
    /// ```no_run
    /// use vector_core::*;
    /// use std::sync::Arc;
    ///
    /// struct EchoBot;
    /// impl InboundEventHandler for EchoBot {
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
    /// core.listen(Arc::new(EchoBot)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn listen(&self, handler: Arc<dyn InboundEventHandler>) -> Result<()> {
        let client = state::NOSTR_CLIENT.get()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::MY_PUBLIC_KEY.get()
            .copied()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        let sub_id = self.subscribe_dms().await?;
        let client_for_closure = client.clone();

        client.handle_notifications(move |notification| {
            let handler = handler.clone();
            let c = client_for_closure.clone();
            let sid = sub_id.clone();
            async move {
                if let nostr_sdk::RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    if subscription_id == sid {
                        let prepared = event_handler::prepare_event(*event, &c, my_pk).await;
                        event_handler::commit_prepared_event(prepared, true, &*handler).await;
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
