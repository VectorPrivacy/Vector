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

// === Signer (polymorphic: local vault vs. NIP-46 remote bunker) ===
pub mod signer;

// === Database ===
pub mod db;

// === Network ===
pub mod net;
pub mod negentropy;
pub mod blossom;
pub mod blossom_servers;
pub mod blossom_capabilities;
pub mod inbox_relays;
pub mod emoji_packs;
pub mod badges;
#[cfg(feature = "tor")]
pub mod tor;

/// Build a `nostr_sdk::ClientOptions` with the embedded-Tor SOCKS proxy
/// applied if (and only if) the `tor` feature is on AND `tor::TorService` is
/// currently active. When Tor is off, returns the default options unchanged.
///
/// Note: nostr-sdk's `proxy()` lives on `Connection`, not `ClientOptions`
/// directly — we build a `Connection` with the proxy mode and pass it via
/// `ClientOptions::connection(...)`. The `connection()` method itself is
/// `#[cfg(not(target_arch = "wasm32"))]`, but Vector targets are all native.
///
/// Callers should use this rather than `ClientOptions::new()` directly so the
/// Tor toggle automatically covers their relay traffic.
pub fn nostr_client_options() -> nostr_sdk::ClientOptions {
    let opts = nostr_sdk::ClientOptions::new();
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    {
        match tor::transport_state() {
            tor::TorTransportState::Active(addr) => {
                return opts.connection(nostr_sdk::client::Connection::new().proxy(addr));
            }
            tor::TorTransportState::RequiredButInactive => {
                // Tor failsafe: route to a blackhole so the relay socket can't
                // accidentally come up direct while Tor is mid-bootstrap.
                return opts.connection(
                    nostr_sdk::client::Connection::new().proxy(tor::blackhole_proxy_addr()),
                );
            }
            tor::TorTransportState::Disabled => {}
        }
    }
    opts
}

/// Augment a `RelayOptions` with the Tor connection mode when active. Used
/// by every site that adds a relay to the pool — default relays at boot,
/// custom user relays, MLS group relays, NIP-17 inbox relays — so they all
/// come up through Tor when the toggle is on.
///
/// Without this, `RelayOptions::new()` (or the per-mode helper) defaults to
/// `ConnectionMode::Direct`, and relays added at boot would stay direct even
/// after Tor is bootstrapped — `switch_relay_transport()` covers existing
/// relays on toggle, but not freshly-added ones.
pub fn tor_aware_relay_options(opts: nostr_sdk::RelayOptions) -> nostr_sdk::RelayOptions {
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    {
        match tor::transport_state() {
            tor::TorTransportState::Active(addr) => {
                return opts.connection_mode(nostr_sdk::pool::ConnectionMode::proxy(addr));
            }
            tor::TorTransportState::RequiredButInactive => {
                // Tor failsafe: pin to blackhole so this relay can never come
                // up direct while Tor isn't running.
                return opts.connection_mode(
                    nostr_sdk::pool::ConnectionMode::proxy(tor::blackhole_proxy_addr()),
                );
            }
            tor::TorTransportState::Disabled => {}
        }
    }
    opts
}

/// Relay options for a Community / "external" relay: the GOSSIP flag (+ PING for a 24/7 keepalive
/// connection). GOSSIP is read/write-capable when TARGETED — `can_read()` is `READ|GOSSIP|DISCOVERY`
/// and `can_write()` is `WRITE|GOSSIP`, so per-relay checks pass for `fetch_events_from` /
/// `send_event_to` / `subscribe_to`. But pool-wide ops select READ-only / WRITE-only relays, so the
/// DM/giftwrap subscription (`subscribe(None)`) and the user's outbox (`send_event`) skip GOSSIP
/// relays — the user's own traffic never touches relays they don't own. (A bare PING-only relay can
/// NOT be used: `can_write()`/`can_read()` are false → the relay layer returns WriteDisabled /
/// ReadDisabled.) An overlap relay that's ALSO a user relay keeps its existing READ+WRITE flags —
/// `add_relay` is a no-op (`Ok(false)`) for a url already pooled, reusing the one existing connection.
pub fn community_relay_options() -> nostr_sdk::RelayOptions {
    use nostr_sdk::RelayServiceFlags;
    tor_aware_relay_options(
        nostr_sdk::RelayOptions::new().flags(RelayServiceFlags::GOSSIP | RelayServiceFlags::PING),
    )
}

// === Event Storage ===
pub mod stored_event;

// === Rumor Processing ===
pub mod rumor;

// === Messaging ===
pub mod sending;

// === Per-DM Wallpapers ===
pub mod wallpaper;

// === Message Deletion (NIP-09 against retained gift-wraps) ===
pub mod deletion;

// === SIMD Operations ===
pub mod simd;

// === MLS Group Encryption ===

// === Community protocol (MLS successor — GROUP_PROTOCOL.md) ===
pub mod community;

// === Event Handler ===
pub mod event_handler;

// === Re-exports for convenience ===
pub use types::{Message, Attachment, Reaction, EditEntry, ImageMetadata, SiteMetadata, LoginResult, AttachmentFile, mention, extract_mentions};
pub use profile::{Profile, ProfileFlags, SlimProfile, Status};
pub use chat::{Chat, ChatType, ChatMetadata, SerializableChat};
pub use compact::{CompactMessage, CompactMessageVec, NpubInterner};
pub use state::{
    ChatState, NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE, ENCRYPTION_KEY,
    nostr_client, my_public_key, has_active_session,
    set_nostr_client, set_my_public_key,
    take_nostr_client, clear_my_public_key,
    set_pending_bunker_setup, pending_bunker_setup, clear_pending_bunker_setup,
};
pub use crypto::{GuardedKey, GuardedSigner};
pub use signer::{
    SignerKind, signer_kind, set_signer_kind, is_bunker,
    BUNKER_SIGNER, bunker_signer, set_bunker_signer, take_bunker_signer,
    build_bunker_signer, prewarm_bunker, drain_bunker_state,
    parse_bunker_remote_pubkey, parse_bunker_relays,
    BunkerConnectionState, bunker_state, set_bunker_state,
    VectorAuthUrlHandler, attempt_bunker_login, WatchedBunkerSigner,
    vector_metadata, build_nostrconnect_uri, build_nostrconnect_session,
    VECTOR_APP_NAME, VECTOR_APP_URL, VECTOR_APP_ICON,
};
pub use error::{VectorError, Result};
pub use traits::{EventEmitter, NoOpEmitter, set_event_emitter, emit_event};
pub use db::{set_app_data_dir, get_app_data_dir};
pub use sending::{SendCallback, NoOpSendCallback, SendConfig, SendResult};
pub use deletion::{delete_own_dm, DeleteOutcome};
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
        state::set_my_public_key(public_key);

        // Initialize database for this account
        db::set_current_account(npub.clone())?;
        db::init_database(&npub)?;

        // Store nsec for encryption setup
        {
            let nsec = keys.secret_key().to_bech32()
                .map_err(|e| VectorError::Nostr(format!("Failed to encode nsec: {}", e)))?;
            *state::PENDING_NSEC.lock().unwrap() = Some(nsec.clone());

            // NEVER clobber an existing encrypted key with the plaintext nsec. An account with encryption
            // enabled keeps its key encrypted-at-rest (PIN-derived); overwriting it with the raw nsec — e.g.
            // a no-password headless/diagnostic login (the concord CLI) — would leave the GUI deriving the
            // right key from the correct PIN but trying to decrypt a value that's no longer ciphertext, i.e.
            // "incorrect pin" with the real key effectively lost. MY_SECRET_KEY is already set in-memory above,
            // so login works regardless; only persist the raw key when there's no encrypted key to protect.
            let existing_encrypted = db::get_pkey().ok().flatten().is_some_and(|v| !v.starts_with("nsec1"));
            if !(state::resolve_encryption_enabled_from_db() && existing_encrypted) {
                db::set_pkey(&nsec)?;
            }
        }

        // Use the canonical resolver so this high-level API agrees with
        // crypto::is_encryption_enabled and the Android bg-sync probe.
        let has_encryption = state::resolve_encryption_enabled_from_db();

        if has_encryption {
            if let Some(pwd) = password {
                let key = crate::crypto::hash_pass(pwd).await;
                state::ENCRYPTION_KEY.set(key, &[&state::MY_SECRET_KEY]);
            }
        }
        // Seed the atomic unconditionally — `is_encryption_enabled_fast()`
        // must agree with the DB regardless of branch.
        state::init_encryption_enabled();

        // Build Nostr client
        let client = ClientBuilder::new().signer(keys).build();

        // Add trusted relays
        for relay in state::TRUSTED_RELAYS {
            client.add_relay(*relay).await.ok();
        }

        // Connect
        client.connect().await;

        let _ = { state::set_nostr_client(client); Ok::<(), ()>(()) };

        Ok(LoginResult { npub, has_encryption })
    }

    /// Generate a fresh random account secret key (bech32 nsec). Lets a headless client spin up a
    /// brand-new identity (`add_account` with no key) without depending on nostr-sdk directly.
    pub fn generate_nsec(&self) -> Result<String> {
        use nostr_sdk::prelude::*;
        Keys::generate().secret_key().to_bech32()
            .map_err(|e| VectorError::Nostr(format!("Failed to encode nsec: {}", e)))
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
        state::my_public_key()
            .and_then(|pk| ToBech32::to_bech32(&pk).ok())
    }

    // === Communities (headless) ===
    // The GUI's Tauri commands carry optimistic-echo + emit machinery a headless client
    // doesn't need; these are the lean equivalents over the same `community::service` layer,
    // so a CLI / agent can join, read, post, and sync a Community.

    /// List every Community held locally (owned or joined), each with its channels.
    pub async fn list_communities(&self) -> Vec<serde_json::Value> {
        let ids = crate::db::community::list_community_ids().unwrap_or_default();
        let mut out = Vec::new();
        for id in ids {
            if let Ok(Some(c)) = crate::db::community::load_community(&id) {
                out.push(serde_json::json!({
                    "community_id": c.id.to_hex(),
                    "name": c.name,
                    "description": c.description,
                    "is_owner": crate::community::service::is_proven_owner(&c),
                    "channels": c.channels.iter()
                        .map(|ch| serde_json::json!({ "channel_id": ch.id.to_hex(), "name": ch.name }))
                        .collect::<Vec<_>>(),
                }));
            }
        }
        out
    }

    /// Join a Community from a public invite URL (`vectorapp.io/invite#...`). Fetches the
    /// token-encrypted bundle, persists the member-view Community, and registers its channels
    /// as chats. Returns a JSON summary.
    pub async fn join_community(&self, invite_url: &str) -> Result<serde_json::Value> {
        use crate::community::{public_invite, service, transport::LiveTransport};
        let (relays, token) = public_invite::parse_invite_url(invite_url)
            .map_err(|e| VectorError::Other(e.to_string()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let bundle = service::fetch_public_invite(&transport, &relays, &token)
            .await
            .map_err(VectorError::Other)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let community = service::accept_public_invite(&bundle, now).map_err(VectorError::Other)?;
        self.finalize_member_join(community, &transport).await
    }

    /// List the parked private invites (giftwrapped) awaiting acceptance. Each entry is the
    /// community id, its name (from the stored bundle), and the inviter's npub.
    pub fn list_pending_invites(&self) -> Result<Vec<serde_json::Value>> {
        let rows = crate::db::community::list_pending_invites().map_err(VectorError::Other)?;
        Ok(rows.iter().map(|p| {
            let name = crate::community::invite::CommunityInvite::from_json(&p.bundle_json)
                .ok().map(|i| i.name).unwrap_or_default();
            serde_json::json!({
                "community_id": p.community_id,
                "name": name,
                "inviter_npub": p.inviter_npub,
            })
        }).collect())
    }

    /// Accept a PARKED private invite by community id: rebuild the member-view Community from the stored
    /// bundle, finalize the join exactly like a public link, then drop the pending row. Mirrors the
    /// desktop's consent-then-join for an invite delivered over a gift wrap.
    pub async fn accept_pending_invite(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::{invite::{CommunityInvite, accept_invite}, transport::LiveTransport};
        let bundle_json = crate::db::community::get_pending_invite(community_id)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other(format!("no pending invite for {community_id}")))?;
        let invite = CommunityInvite::from_json(&bundle_json).map_err(VectorError::Other)?;
        let community = accept_invite(&invite).map_err(VectorError::Other)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let summary = self.finalize_member_join(community, &transport).await?;
        let _ = crate::db::community::delete_pending_invite(community_id);
        Ok(summary)
    }

    /// Shared finalization for joining a Community as a member — public link OR accepted private invite.
    /// Walks any base rekey, folds the LATEST control plane (so the joiner sees current metadata, not
    /// the bundle's genesis snapshot), refuses if banned, registers the channels as chats, and announces
    /// presence. Returns the JSON summary.
    pub(crate) async fn finalize_member_join<T: crate::community::transport::Transport + ?Sized>(
        &self,
        community: crate::community::Community,
        transport: &T,
    ) -> Result<serde_json::Value> {
        use crate::community::service;
        // Persist the member-view row up front: the catch-up, the control fold, and chat registration all
        // read it back from the DB. A private bundle (unlike a public one with a preview) arrives with no
        // display metadata, so nothing else would have saved it. UPSERT — re-saving a public join is a no-op.
        crate::db::community::save_community(&community).map_err(VectorError::Other)?;
        // The bundle's root can predate a base rotation, so walk any rekey first (no-op if none) — then
        // re-load so the control fold + registration happen at the CURRENT epoch.
        if let Ok(c) = service::catch_up_server_root(transport, &community).await {
            if c.removed {
                let _ = crate::db::community::delete_community(&community.id.to_hex());
                return Err(VectorError::Other("you have been removed from this community".into()));
            }
        }
        let community = crate::db::community::load_community(&community.id)
            .map_err(VectorError::Other)?
            .unwrap_or(community);
        // Fold the LATEST control plane before we register anything — the joiner should see the current
        // name/description/roster/mode immediately, not a stale snapshot. Banlist first: an honest client
        // REFUSES to join if this npub is banned (and the just-saved community is torn back down).
        let _ = service::fetch_and_apply_control(transport, &community).await;
        if service::am_i_banned(&community) {
            let _ = crate::db::community::delete_community(&community.id.to_hex());
            return Err(VectorError::Other("you are banned from this community".into()));
        }
        // Re-load so the chat we register + the summary we return carry the freshly-folded latest metadata.
        let community = crate::db::community::load_community(&community.id)
            .map_err(VectorError::Other)?
            .unwrap_or(community);
        let owner_npub = community
            .owner_attestation
            .as_ref()
            .and_then(|att| crate::community::owner::verify_owner_attestation(att, &community.id.to_hex()))
            .and_then(|pk| ToBech32::to_bech32(&pk).ok());
        {
            let created_at_ms = crate::db::community::community_created_at_ms(&community.id);
            let mut st = state::STATE.lock().await;
            for ch in &community.channels {
                st.upsert_community_chat(
                    &ch.id.to_hex(),
                    &community.name,
                    community.description.as_deref().unwrap_or(""),
                    &community.id.to_hex(),
                    crate::community::service::is_proven_owner(&community),
                    community.icon.is_some(),
                    owner_npub.as_deref(),
                    created_at_ms,
                    community.dissolved,
                );
            }
        }
        // Best-effort join announcement (kind 3306) into the primary channel so honest peers
        // see us in their member list even before we post. Failure must not fail the join.
        if let Some(primary) = community.channels.first() {
            let _ = service::publish_presence(transport, &community, primary, true, None).await;
        }
        Ok(serde_json::json!({
            "community_id": community.id.to_hex(),
            "name": community.name,
            "channels": community.channels.iter()
                .map(|c| serde_json::json!({ "channel_id": c.id.to_hex(), "name": c.name }))
                .collect::<Vec<_>>(),
        }))
    }

    /// Create a Community (single "general" channel) on the default trusted relays. Signs the
    /// owner attestation with this identity (so the creator is the proven owner), registers the
    /// channel as a chat, and returns a JSON summary.
    pub async fn create_community(&self, name: &str) -> Result<serde_json::Value> {
        use crate::community::{service, transport::LiveTransport};
        let relays: Vec<String> = crate::state::active_trusted_relays()
            .await
            .iter()
            .map(|s| s.to_string())
            .collect();
        if relays.is_empty() {
            return Err(VectorError::Other("no relays available to host the Community".into()));
        }
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let community = service::create_community(&transport, name, "general", relays)
            .await
            .map_err(VectorError::Other)?;
        let owner_npub = community
            .owner_attestation
            .as_ref()
            .and_then(|att| crate::community::owner::verify_owner_attestation(att, &community.id.to_hex()))
            .and_then(|pk| ToBech32::to_bech32(&pk).ok());
        {
            let created_at_ms = crate::db::community::community_created_at_ms(&community.id);
            let mut st = state::STATE.lock().await;
            for ch in &community.channels {
                st.upsert_community_chat(
                    &ch.id.to_hex(),
                    &community.name,
                    community.description.as_deref().unwrap_or(""),
                    &community.id.to_hex(),
                    crate::community::service::is_proven_owner(&community),
                    community.icon.is_some(),
                    owner_npub.as_deref(),
                    created_at_ms,
                    community.dissolved,
                );
            }
        }
        Ok(serde_json::json!({
            "community_id": community.id.to_hex(),
            "name": community.name,
            "channels": community.channels.iter()
                .map(|c| serde_json::json!({ "channel_id": c.id.to_hex(), "name": c.name }))
                .collect::<Vec<_>>(),
        }))
    }

    /// Mint a public invite link for a Community this identity owns. Returns the shareable URL.
    pub async fn create_public_invite(&self, community_id: &str) -> Result<String> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let community = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(community_id),
        ))
        .map_err(VectorError::Other)?
        .ok_or_else(|| VectorError::Other("community not found".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let (_token, url) = service::create_public_invite(&transport, &community, None, None)
            .await
            .map_err(VectorError::Other)?;
        Ok(url)
    }

    /// Send a PRIVATE invite: gift-wrap this Community's invite bundle directly to an npub over a NIP-17
    /// DM (the same transport as a regular DM). The invitee parks it pending consent (accept_pending_invite).
    /// Requires CREATE_INVITE; a banned npub can't be re-invited. Returns the wrap's event id + relays.
    pub async fn invite_to_community(&self, community_id: &str, invitee_npub: &str) -> Result<serde_json::Value> {
        use crate::community::{service, CommunityId};
        use crate::sending::{send_rumor_dm, NoOpSendCallback, SendCallback, SendConfig};

        let session = crate::state::SessionGuard::capture();
        let my_pk = crate::state::my_public_key()
            .ok_or_else(|| VectorError::Other("Public key not set".into()))?;

        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let community = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(community_id),
        ))
        .map_err(VectorError::Other)?
        .ok_or_else(|| VectorError::Other("community not found".into()))?;

        if !service::caller_has_permission(&community, crate::community::roles::Permissions::CREATE_INVITE) {
            return Err(VectorError::Other("You need the create-invite permission to invite someone".into()));
        }
        let invitee_hex = nostr_sdk::PublicKey::parse(invitee_npub)
            .map_err(|_| VectorError::Other("invalid npub".into()))?
            .to_hex();
        if crate::db::community::get_community_banlist(community_id)
            .map_err(VectorError::Other)?
            .iter()
            .any(|b| b == &invitee_hex)
        {
            return Err(VectorError::Other("That member is banned from this community and can't be invited".into()));
        }

        // The bundle is built from purely local state; bail if the account swapped before the gift-wrap.
        if !session.is_valid() {
            return Err(VectorError::Other("account changed during invite".into()));
        }

        let rumor = crate::community::invite::build_invite_rumor(&community, my_pk).map_err(VectorError::Other)?;
        let pending_id = format!("community-invite-{}", community_id);
        // self_send=false: the owner already holds the Community; the inbound guard would drop the echo.
        let config = SendConfig { self_send: false, ..SendConfig::gui() };
        let callback: Arc<dyn SendCallback> = Arc::new(NoOpSendCallback);

        let result = send_rumor_dm(invitee_npub, &pending_id, rumor, &config, callback)
            .await
            .map_err(VectorError::Other)?;

        Ok(serde_json::json!({
            "community_id": community_id,
            "invitee": invitee_npub,
            "wrap_event_id": result.event_id,
        }))
    }

    /// The public invite links this account holds for a Community (to list + revoke). Each carries the
    /// hex `token` (the link secret) needed by [`Self::revoke_public_invite`].
    pub fn list_public_invites(&self, community_id: &str) -> Result<Vec<crate::db::community::PublicInviteRecord>> {
        crate::db::community::list_public_invites(community_id).map_err(VectorError::Other)
    }

    /// Revoke a public invite link by its hex token. Retiring the LAST active link flips the Community to
    /// Private, which re-founds (rotates the base key + every channel key) to cut link-joined lurkers.
    /// Idempotent: a token this account doesn't hold is a no-op. Needs a local key when the revoke triggers
    /// the privatize rekey (a bunker account can't rotate).
    pub async fn revoke_public_invite(&self, community_id: &str, token: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let token_bytes = crate::simd::hex::hex_to_bytes_32(token);
        let community = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(community_id),
        ))
        .map_err(VectorError::Other)?
        .ok_or_else(|| VectorError::Other("community not found".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(20));
        service::revoke_public_invite(&transport, &community, &token_bytes)
            .await
            .map_err(VectorError::Other)
    }

    /// Post a text message to a Community channel. Returns the message id (the inner id).
    pub async fn send_community_message(
        &self,
        channel_id: &str,
        content: &str,
        replied_to: Option<&str>,
    ) -> Result<String> {
        use crate::community::{envelope, inbound, service, transport::LiveTransport};
        let (community, channel) = self.resolve_channel(channel_id)?;
        let author_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let reply = replied_to.filter(|r| !r.is_empty());
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let unsigned = envelope::build_inner_typed(
            author_pk,
            &channel.id,
            channel.epoch,
            crate::stored_event::event_kind::COMMUNITY_MESSAGE,
            content,
            ms,
            reply,
            &[],
        );
        let message_id = unsigned.id.ok_or_else(|| VectorError::Other("inner event has no id".into()))?.to_hex();
        let client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = client.signer().await.map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let inner = unsigned.sign(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let outer = service::send_signed_message(&transport, &community, &channel, &inner)
            .await
            .map_err(VectorError::Other)?;
        // Local echo so get_messages reflects the send (the relay echo dedups on inner id).
        let echoed = {
            let mut st = state::STATE.lock().await;
            inbound::process_incoming(&mut st, &outer, &channel, &author_pk)
        };
        if let Some(inbound::IncomingEvent::NewMessage(msg)) = echoed {
            let _ = crate::db::events::save_message(channel_id, &msg).await;
        }
        Ok(message_id)
    }

    /// Fetch the latest page of a Community channel from relays, ingesting messages,
    /// reactions, edits, and deletes. Returns the count of brand-new messages applied.
    /// Returns `(new_message_count, warnings)`. `warnings` are NON-FATAL errors hit during the sync
    /// (catch-up, control fold, read-cut resume) — surfaced rather than swallowed so a headless caller is
    /// never blind to "the sync ran but a re-founding couldn't be resumed."
    pub async fn sync_community_channel(&self, channel_id: &str, limit: usize) -> Result<(usize, Vec<String>)> {
        use crate::community::{inbound, send, service, transport::LiveTransport};
        let my_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let (community, _) = self.resolve_channel(channel_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let mut warnings: Vec<String> = Vec::new();

        // FIRST: walk any base (server-root) rotation — a privatize / private-ban rekey advances the
        // epoch and re-anchors the control plane under the NEW root, so we must follow it BEFORE reading
        // control/messages or we'd look at stale-epoch pseudonyms and silently fall off. No-op (one cheap
        // probe) when there's been no rotation. Re-resolve after: the base epoch + root may have advanced.
        // An AUTHORIZED base rotation that excluded us (private ban / read-cut) is a removal: erase local
        // community data, exactly like an observed banlist/kick. This is the catch-all for a cut member who
        // can no longer decrypt the new control plane to read the banlist the normal way (`am_i_banned`).
        match service::catch_up_server_root(&transport, &community).await {
            Ok(c) if c.removed => {
                // ban-rekey exclusion is a self-removal → retain the held epoch keys for later self-scrub.
                let _ = crate::db::community::delete_community_retain_keys(&community.id.to_hex());
                return Ok((0, warnings));
            }
            Ok(_) => {}
            Err(e) => warnings.push(format!("base catch-up failed: {e}")),
        }
        let (community, _) = self.resolve_channel(channel_id)?;

        // Headless clients have no realtime control-plane subscription, so fold the latest control editions
        // here (the desktop does the same on its own latest-page sync). Banlist FIRST: a ban that landed on
        // us self-removes like a kick (drop keys + local data, no rejoin). Then roles, the per-creator invite
        // links (Public/Private mode), and metadata (name/description/icon/channel-name) — so a rename, role,
        // ban, or mode change reaches this member on sync, not just in a realtime client.
        if let Err(e) = service::fetch_and_apply_control(&transport, &community).await {
            warnings.push(format!("control fold failed: {e}"));
        }
        if service::am_i_banned(&community) {
            // ban self-removal → retain the held epoch keys for later self-scrub.
            let _ = crate::db::community::delete_community_retain_keys(&community.id.to_hex());
            return Ok((0, warnings));
        }
        // Walk any CHANNEL rekey so we hold the current channel key before paging it, then re-resolve so the
        // batch below carries the fresh channel epoch/key + the freshly-folded banned set + metadata.
        let (community, channel) = self.resolve_channel(channel_id)?;
        if let Err(e) = service::catch_up_channel_rekeys(&transport, &community, &channel.id).await {
            warnings.push(format!("channel catch-up failed: {e}"));
        }
        // Resume any interrupted re-founding (a privatize/ban whose rotation aborted mid-way — e.g. a
        // transient relay miss on the re-anchor). The GUI's sync did this; the agent's path did NOT, so an
        // interrupted re-founding stayed `read_cut_pending` forever (channel frozen). Best-effort + surfaced.
        let (community, _) = self.resolve_channel(channel_id)?;
        if let Err(e) = service::retry_pending_read_cut(&transport, &community).await {
            warnings.push(format!("read-cut resume failed: {e}"));
        }
        let (community, channel) = self.resolve_channel(channel_id)?;

        let events = send::fetch_channel_page(&transport, &community, &channel, None, None, limit.max(1))
            .await
            .map_err(VectorError::Other)?;
        let outcomes = {
            let mut st = state::STATE.lock().await;
            inbound::process_channel_batch(&mut st, &events, &channel, &my_pk)
        };
        let mut new = 0usize;
        for o in &outcomes {
            match o {
                inbound::IncomingEvent::NewMessage(m) => {
                    let _ = crate::db::events::save_message(channel_id, m).await;
                    new += 1;
                }
                inbound::IncomingEvent::Updated { message, .. } => {
                    let _ = crate::db::events::save_message(channel_id, message).await;
                }
                inbound::IncomingEvent::Removed { target_id } => {
                    let _ = crate::db::events::delete_event(target_id).await;
                }
                inbound::IncomingEvent::Presence { npub, joined, event_id, created_at, invited_by, invited_label } => {
                    let et = if *joined {
                        crate::stored_event::SystemEventType::MemberJoined
                    } else {
                        crate::stored_event::SystemEventType::MemberLeft
                    };
                    // attribution persisted in the note: "invited_by[|label]".
                    let note = invited_by.as_ref().map(|by| match invited_label {
                        Some(l) if !l.is_empty() => format!("{by}|{l}"),
                        _ => by.clone(),
                    });
                    let _ = crate::db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), *created_at).await;
                }
                inbound::IncomingEvent::Kicked { community_id }
                | inbound::IncomingEvent::SelfLeft { community_id } => {
                    // self-removal (kick of me, or a leave I/another device authored): drop the
                    // community's local state but RETAIN the held epoch keys (later self-scrub). The core-level
                    // half of leaving; a client shell layers on subscription-refresh + chat-row teardown + UI.
                    // Stop the batch — the community is gone, so later same-batch writes would orphan rows.
                    let _ = crate::db::community::delete_community_retain_keys(community_id);
                    break;
                }
            }
        }
        Ok((new, warnings))
    }

    /// Observed members of a Community (best-effort: those who've posted or announced a join,
    /// minus anyone who's left or is banned). Each entry is `{npub, last_active}`.
    pub async fn get_community_members(&self, community_id: &str) -> Vec<serde_json::Value> {
        crate::db::community::community_member_activity(community_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(npub, last_active)| serde_json::json!({ "npub": npub, "last_active": last_active }))
            .collect()
    }

    // ── Community admin actions ── role-gated; vector-core re-checks authority on every action and peers
    // re-verify against the owner-rooted roster, so these can't forge standing. A bunker account can't ban
    // in a private community (the rekey needs a raw local key).

    fn load_community_hex(community_id: &str) -> Result<crate::community::Community> {
        use crate::community::CommunityId;
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        crate::db::community::load_community(&CommunityId(crate::simd::hex::hex_to_bytes_32(community_id)))
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("community not found".into()))
    }

    fn admin_role_id_of(community_id: &str) -> Result<String> {
        let roles = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
        roles.roles.iter()
            .find(|r| matches!(r.scope, crate::community::roles::RoleScope::Server)
                && r.permissions.contains(crate::community::roles::Permissions::ADMIN_ALL))
            .map(|r| r.role_id.clone())
            .ok_or_else(|| VectorError::Other("admin role not found (roster not synced?)".into()))
    }

    /// My effective management capabilities in a community (role engine — owner is just position 0). Use to
    /// confirm a promotion/demotion landed.
    pub fn community_capabilities(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::service;
        let community = Self::load_community_hex(community_id)?;
        let caps = service::caller_capabilities(&community);
        let manage_admin_role = Self::admin_role_id_of(community_id).ok()
            .map(|rid| service::caller_can_manage_role_id(&community, &rid))
            .unwrap_or(false);
        Ok(serde_json::json!({
            "manage_metadata": caps.manage_metadata, "manage_channels": caps.manage_channels,
            "create_invite": caps.create_invite, "kick": caps.kick, "ban": caps.ban,
            "manage_messages": caps.manage_messages, "manage_roles": caps.manage_roles,
            "manage_admin_role": manage_admin_role,
        }))
    }

    /// The community's owner npub + the admin npubs (role overview).
    pub fn community_roles(&self, community_id: &str) -> Result<serde_json::Value> {
        use nostr_sdk::prelude::{PublicKey, ToBech32};
        let community = Self::load_community_hex(community_id)?;
        let owner = community.owner_attestation.as_ref()
            .and_then(|att| crate::community::owner::verify_owner_attestation(att, &community.id.to_hex()))
            .and_then(|pk| ToBech32::to_bech32(&pk).ok());
        let roles = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
        let admins: Vec<String> = roles.grants.iter().filter(|g| roles.is_admin(&g.member))
            .filter_map(|g| PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()))
            .collect();
        Ok(serde_json::json!({ "owner": owner, "admins": admins }))
    }

    /// Grant a member the @admin role. Requires MANAGE_ROLES + outranking the role's position.
    pub async fn grant_admin(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let member = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let community = Self::load_community_hex(community_id)?;
        let role_id = Self::admin_role_id_of(community_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::grant_role(&transport, &community, member, &role_id).await.map_err(VectorError::Other)
    }

    /// Revoke a member's @admin role.
    pub async fn revoke_admin(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let member = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let community = Self::load_community_hex(community_id)?;
        let role_id = Self::admin_role_id_of(community_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::revoke_role(&transport, &community, member, &role_id).await.map_err(VectorError::Other)
    }

    /// Cooperatively kick a member (3309) — they self-remove but can rejoin. Requires KICK + outrank.
    pub async fn kick_member(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let hex = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?.to_hex();
        let community = Self::load_community_hex(community_id)?;
        let channel = community.channels.first().ok_or_else(|| VectorError::Other("community has no channel".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::publish_kick(&transport, &community, channel, &hex).await.map_err(VectorError::Other)
    }

    /// Ban (`true`) or unban (`false`) a member. Ban is terminal (no rejoin); in a private community it also
    /// fires the read-cut rekey (needs a local key). Requires BAN + outrank.
    pub async fn set_member_banned(&self, community_id: &str, npub: &str, banned: bool) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let hex = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?.to_hex();
        let community = Self::load_community_hex(community_id)?;
        // Recompute the full list (latest-wins): drop any existing entry, then add if banning.
        let mut list = crate::db::community::get_community_banlist(community_id).map_err(VectorError::Other)?;
        list.retain(|h| h != &hex);
        if banned { list.push(hex); }
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::publish_banlist(&transport, &community, &list).await.map_err(VectorError::Other)
    }

    /// Owner dissolution / "Delete Community": publish the terminal GroupDissolved tombstone (and
    /// retire the owner's own invite links, no rekey), sealing the community permanently. Owner-only
    /// (re-verified cryptographically in `service::dissolve_community`); irreversible.
    pub async fn dissolve_community(&self, community_id: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let community = Self::load_community_hex(community_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::dissolve_community(&transport, &community).await.map_err(VectorError::Other)
    }

    /// Edit community metadata (name / description) as an authorized member (MANAGE_METADATA). `None` leaves
    /// a field unchanged; an empty description clears it.
    pub async fn edit_community_metadata(&self, community_id: &str, name: Option<&str>, description: Option<&str>) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let mut community = Self::load_community_hex(community_id)?;
        if let Some(n) = name { community.name = n.to_string(); }
        if let Some(d) = description { community.description = if d.is_empty() { None } else { Some(d.to_string()) }; }
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::republish_community_metadata(&transport, &community).await.map_err(VectorError::Other)
    }

    /// Leave a Community: announce a best-effort "left" presence (before dropping keys), then
    /// drop the held keys + local channel chats. You need a fresh invite to rejoin.
    pub async fn leave_community(&self, community_id: &str) -> Result<()> {
        use crate::community::{transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let id = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        let community = crate::db::community::load_community(&id).map_err(VectorError::Other)?;
        let channel_ids: Vec<String> = community
            .as_ref()
            .map(|c| c.channels.iter().map(|ch| ch.id.to_hex()).collect())
            .unwrap_or_default();
        // "Left" announcement BEFORE dropping keys (afterward we can't sign/seal into the channel).
        if let Some(ref c) = community {
            if let Some(primary) = c.channels.first() {
                let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
                let _ = crate::community::service::publish_presence(&transport, c, primary, false, None).await;
            }
        }
        // voluntary leave is a self-removal → retain the held epoch keys for later self-scrub.
        crate::db::community::delete_community_retain_keys(community_id).map_err(VectorError::Other)?;
        {
            let mut st = state::STATE.lock().await;
            st.chats.retain(|c| !channel_ids.contains(&c.id));
        }
        Ok(())
    }

    /// Resolve a channel id to its owning Community + the Channel (with its secret key).
    fn resolve_channel(
        &self,
        channel_id: &str,
    ) -> Result<(crate::community::Community, crate::community::Channel)> {
        use crate::community::CommunityId;
        let community_id = crate::db::community::community_id_for_channel(channel_id)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("Unknown Community channel".into()))?;
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let community = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(&community_id),
        ))
        .map_err(VectorError::Other)?
        .ok_or_else(|| VectorError::Other("Community not found".into()))?;
        let channel = community
            .channels
            .iter()
            .find(|c| c.id.to_hex() == channel_id)
            .cloned()
            .ok_or_else(|| VectorError::Other("Channel not found in Community".into()))?;
        Ok((community, channel))
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

        let client = state::nostr_client()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::my_public_key()
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
        let client = state::nostr_client()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::my_public_key()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        let filter = Filter::new()
            .pubkey(my_pk)
            .kind(Kind::GiftWrap)
            .limit(0);

        let output = client.subscribe(filter, None).await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        Ok(output.val)
    }

    /// Start listening for incoming DMs.
    ///
    /// Blocks until the client disconnects. Processes GiftWraps
    /// (DMs, files) → prepare_event → commit_prepared_event.
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

        let client = state::nostr_client()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::my_public_key()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        // Subscribe to DMs (GiftWraps)
        let dm_sub_id = self.subscribe_dms().await?;

        let client_for_closure = client.clone();

        client.handle_notifications(move |notification| {
            let handler = handler.clone();
            let c = client_for_closure.clone();
            let dm_sid = dm_sub_id.clone();
            async move {
                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    if subscription_id == dm_sid {
                        // DMs, files, reactions
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
        if let Some(client) = state::nostr_client() {
            let _ = client.disconnect().await;
        }
        db::close_database();
    }

    /// Tear down the current session for an in-process account swap — the account-agnostic core of
    /// the app's `reset_session()`. Advances the session generation FIRST so any background task
    /// holding a `SessionGuard` short-circuits before it can touch the next account's storage; shuts
    /// the client down (which ends any `listen()` notification loop bound to it, so the old account's
    /// events can't land in the new account's DB); closes the DB pool; and clears the key vaults plus
    /// all in-memory per-account state. Follow with `login()` to bind the next account, then re-attach
    /// `listen()`. (The app's `reset_session()` additionally clears Tauri-only caches it owns.)
    pub async fn swap_session(&self) {
        // FIRST — invalidate every captured guard before any teardown begins.
        state::bump_session_generation();

        // Shut the client down before anything else: this detaches relay subscriptions and ends the
        // prior `listen()` loop, so it stops firing the old account's events into the new session.
        if let Some(client) = state::take_nostr_client() {
            let _ = client.shutdown().await;
        }
        db::close_database();

        // Key vaults + transient secrets.
        state::ENCRYPTION_KEY.clear(&[&state::MY_SECRET_KEY]);
        state::MY_SECRET_KEY.clear(&[&state::ENCRYPTION_KEY]);
        {
            use zeroize::Zeroize;
            if let Ok(mut g) = state::PENDING_NSEC.lock() {
                if let Some(s) = g.as_mut() { s.zeroize(); }
                *g = None;
            }
        }

        // In-memory per-account state owned by vector-core's globals.
        {
            let mut st = state::STATE.lock().await;
            st.profiles.clear();
            st.chats.clear();
            st.db_loaded = false;
            st.is_syncing = false;
        }
        state::WRAPPER_ID_CACHE.lock().await.clear();
        state::PENDING_EVENTS.lock().await.clear();
        state::set_active_chat(None);
        crate::profile::sync::clear_profile_sync_queue();
        crate::inbox_relays::clear_inbox_relay_cache();
        // Chat/user row-id caches are PER-ACCOUNT (row ids belong to the prior account's DB). Not clearing
        // them here let a swapped-in account resolve a channel/npub to the WRONG (prior-account) row id →
        // saves FK-failed silently + reads hit the wrong row (e.g. a community member vanished post-swap).
        crate::db::clear_id_caches();
        // Community sync RAM cache (page cursors, history-start, in-flight, invite preload) is
        // account-scoped — drop it so the next account can't read A's cursors/warmed pages. The
        // generation stamp self-invalidates too, but clear explicitly for parity with the GUI swap.
        crate::community::cache::clear();
        // Theme-pack emoji tags are account-scoped; leaving the prior account's set active would tag the
        // next account's outbound messages with A's theme shortcodes (leaking A's pack Blossom URLs). The
        // frontend re-registers the new account's theme, but only if it HAS one — clear to be safe.
        crate::emoji_packs::set_theme_emoji_tags(Vec::new());
    }
}
