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
pub mod emoji_usage;
pub mod badges;
pub mod bot_interface;
pub mod webxdc;
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
    // NIP-42: authenticate to relays that challenge, using the account signer. Many
    // Concord/Armada communities live on AUTH-gating relays (Ditto's default gates
    // kind-1059), where an unauthenticated client silently reads back ZERO events —
    // so a join's control-plane verify fails closed and every community fetch comes
    // up empty. Auto-auth unlocks those reads; a relay that doesn't challenge is
    // unaffected.
    let opts = nostr_sdk::ClientOptions::new().automatic_authentication(true);
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
/// custom user relays, community relays, NIP-17 inbox relays — so they all
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

/// Relay options for a Discovery Relay (see `state::DISCOVERY_RELAYS`): the same
/// GOSSIP|PING targeted-only isolation as Community relays — reachable via
/// `fetch_events_from` / `send_event_to`, invisible to pool-wide DM/profile ops.
/// An overlap with a user relay keeps the user's READ+WRITE flags (`add_relay`
/// no-ops on an already-pooled url).
pub fn discovery_relay_options() -> nostr_sdk::RelayOptions {
    community_relay_options()
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

// === Community protocol (GROUP_PROTOCOL.md) ===
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

        // Build Nostr client — tor-aware options so a headless consumer with
        // the Tor pref ON proxies (or blackholes) instead of dialing direct.
        let client = ClientBuilder::new()
            .signer(keys)
            .opts(nostr_client_options())
            // Relay health monitor — powers the reconnect-driven catch-up in `listen()`.
            .monitor(Monitor::new(1024))
            .build();

        // Add trusted relays
        for relay in state::TRUSTED_RELAYS {
            let opts = tor_aware_relay_options(nostr_sdk::RelayOptions::default());
            client.pool().add_relay(*relay, opts).await.ok();
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

    /// Send a NIP-17 gift-wrapped text DM using the full pipeline. Retries a
    /// transient publish miss (headless preset, 3 attempts) so an SDK/CLI bot rides
    /// out a relay blip instead of silently dropping the message on the first miss;
    /// `self_send: false` keeps it a plain send (no inbox self-copy).
    pub async fn send_dm(&self, to_npub: &str, content: &str) -> Result<sending::SendResult> {
        let config = SendConfig { self_send: false, ..SendConfig::headless() };
        sending::send_dm(to_npub, content, None, &config, Arc::new(NoOpSendCallback)).await
            .map_err(|e| VectorError::Other(e))
    }

    /// Send a DM as a threaded reply to `replied_to` (an existing message's event id).
    pub async fn send_dm_reply(&self, to_npub: &str, replied_to: &str, content: &str) -> Result<sending::SendResult> {
        let config = SendConfig { self_send: false, ..SendConfig::headless() };
        sending::send_dm(to_npub, content, Some(replied_to), &config, Arc::new(NoOpSendCallback)).await
            .map_err(|e| VectorError::Other(e))
    }

    /// Download a received attachment and decrypt it to plaintext bytes. Fetches the encrypted blob
    /// from its Blossom URL (SSRF/Tor-aware client, size-capped) and AES-decrypts with the
    /// attachment's embedded key + nonce.
    pub async fn download_attachment(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        use futures_util::StreamExt;
        const MAX_DOWNLOAD: usize = 256 * 1024 * 1024;
        if attachment.url.is_empty() {
            return Err(VectorError::Other("attachment has no URL".into()));
        }
        // SSRF guard: the URL is attacker-controlled (off an inbound message). build_http_client only
        // validates redirect HOPS, not the initial request — so validate it here (matches the native
        // download path). With Tor off this is the only egress guard.
        crate::net::validate_url_not_private(&attachment.url)
            .map_err(|e| VectorError::Other(e.to_string()))?;
        let client = crate::net::build_http_client(std::time::Duration::from_secs(120)).map_err(VectorError::Other)?;
        let resp = client.get(&attachment.url).send().await
            .map_err(|e| VectorError::Other(format!("download: {e}")))?;
        if !resp.status().is_success() {
            return Err(VectorError::Other(format!("download failed: HTTP {}", resp.status())));
        }
        // Stream with a cap so a hostile/oversized blob can't OOM the process.
        let mut encrypted: Vec<u8> = Vec::with_capacity(
            resp.content_length().map(|l| (l as usize).min(MAX_DOWNLOAD)).unwrap_or(64 * 1024),
        );
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| VectorError::Other(format!("read body: {e}")))?;
            if encrypted.len() + chunk.len() > MAX_DOWNLOAD {
                return Err(VectorError::Other("attachment exceeds 256 MiB cap".into()));
            }
            encrypted.extend_from_slice(&chunk);
        }
        crate::crypto::decrypt_data(&encrypted, &attachment.key, &attachment.nonce).map_err(VectorError::Other)
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

    /// Send a NIP-25 reaction to a DM message. `emoji_url` carries the NIP-30
    /// image URL when reacting with a custom-pack emoji (content stays
    /// `:shortcode:`). Returns the reaction's rumor id. Local echo + persistence
    /// are best-effort — the gift-wrap send is the source of truth.
    pub async fn send_reaction(
        &self,
        to_npub: &str,
        reference_id: &str,
        emoji: &str,
        emoji_url: Option<&str>,
    ) -> Result<String> {
        use nostr_sdk::prelude::*;

        let client = state::nostr_client().ok_or(VectorError::Other("Not connected".into()))?;
        let my_public_key = state::my_public_key().ok_or(VectorError::Other("Not logged in".into()))?;

        let reference_event = EventId::from_hex(reference_id)
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        let receiver_pubkey = PublicKey::from_bech32(to_npub)
            .map_err(|e| VectorError::Nostr(e.to_string()))?;

        // NIP-30 custom-emoji tag — only when content is `:shortcode:` and a URL is present.
        let custom_emoji_tag = emoji_url.and_then(|url| {
            if !emoji.starts_with(':') || !emoji.ends_with(':') || emoji.len() < 3 || url.is_empty() {
                return None;
            }
            let shortcode = &emoji[1..emoji.len() - 1];
            if shortcode.is_empty() { return None; }
            Some(Tag::custom(TagKind::custom("emoji"), [shortcode.to_string(), url.to_string()]))
        });

        let reaction_target = nostr_sdk::nips::nip25::ReactionTarget {
            event_id: reference_event,
            public_key: receiver_pubkey,
            coordinate: None,
            kind: Some(Kind::PrivateDirectMessage),
            relay_hint: None,
        };
        let mut builder = EventBuilder::reaction(reaction_target, emoji);
        if let Some(tag) = custom_emoji_tag {
            builder = builder.tag(tag);
        }
        let rumor = builder.build(my_public_key);
        let inner_rumor_id = rumor.id;
        let rumor_id = inner_rumor_id.ok_or(VectorError::Other("Failed to get rumor ID".into()))?.to_hex();

        // Retain the recipient wrap's ephemeral key + targeted relays so the
        // reaction can later be revoked with a NIP-09 relay nuke (mirrors the
        // DM message send path). Without retention the reaction is undeletable.
        let outcome = inbox_relays::send_gift_wrap_retained(&client, &receiver_pubkey, rumor.clone(), [])
            .await.map_err(VectorError::Other)?;
        if !outcome.output.success.is_empty() {
            if let Some(rid) = inner_rumor_id {
                if let Err(e) = db::nip17_keys::store_wrap_key(
                    &outcome.wrap_event_id, &rid, &receiver_pubkey,
                    db::nip17_keys::WrapRole::Recipient,
                    &outcome.wrap_secret, &outcome.targeted_relays,
                ) {
                    crate::log_warn!("[Reaction] failed to persist wrap key: {}", e);
                }
            }
        }

        // Self-wrap for multi-device recovery + retain its key too, so another
        // device (or this one) can later revoke. Bail on account swap.
        let self_wrap_client = client.clone();
        let self_wrap_session = state::SessionGuard::capture();
        tokio::spawn(async move {
            if !self_wrap_session.is_valid() { return; }
            if let Ok(self_outcome) = inbox_relays::send_gift_wrap_retained(
                &self_wrap_client, &my_public_key, rumor, [],
            ).await {
                if !self_wrap_session.is_valid() { return; }
                if !self_outcome.output.success.is_empty() {
                    if let Some(rid) = inner_rumor_id {
                        let _ = db::nip17_keys::store_wrap_key(
                            &self_outcome.wrap_event_id, &rid, &my_public_key,
                            db::nip17_keys::WrapRole::SelfSend,
                            &self_outcome.wrap_secret, &self_outcome.targeted_relays,
                        );
                    }
                }
            }
        });

        // Best-effort optimistic local echo + persistence.
        let reaction = Reaction {
            id: rumor_id.clone(),
            reference_id: reference_id.to_string(),
            author_id: my_public_key.to_bech32().unwrap_or_else(|_| my_public_key.to_hex()),
            emoji: emoji.to_string(),
            emoji_url: emoji_url.map(|s| s.to_string()),
        };
        let msg_for_save = {
            let mut st = state::STATE.lock().await;
            match st.add_reaction_to_message(reference_id, reaction) {
                Some((cid, true)) => st.find_message(reference_id).map(|(_, m)| (cid, m)),
                _ => None,
            }
        };
        if let Some((cid, msg)) = msg_for_save {
            let _ = db::events::save_message(&cid, &msg).await;
            traits::emit_event_json("message_update", serde_json::json!({
                "old_id": reference_id, "message": &msg, "chat_id": &cid
            }));
        }

        Ok(rumor_id)
    }

    /// Send an ephemeral typing indicator to a DM recipient. Fire-and-forget
    /// with a 30-second NIP-40 expiry so relays purge it quickly.
    pub async fn send_typing(&self, to_npub: &str) -> Result<()> {
        use nostr_sdk::prelude::*;

        let client = state::nostr_client().ok_or(VectorError::Other("Not connected".into()))?;
        let my_public_key = state::my_public_key().ok_or(VectorError::Other("Not logged in".into()))?;
        let pubkey = PublicKey::from_bech32(to_npub).map_err(|e| VectorError::Nostr(e.to_string()))?;

        let expiry = Timestamp::from_secs(Timestamp::now().as_secs() + 30);
        let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
            .tag(Tag::public_key(pubkey))
            .tag(Tag::custom(TagKind::d(), vec!["vector"]))
            .tag(Tag::expiration(expiry))
            .build(my_public_key);

        client.gift_wrap_to(
            state::active_trusted_relays().await,
            &pubkey,
            rumor,
            [Tag::expiration(expiry)],
        ).await.map_err(|e| VectorError::Nostr(e.to_string()))?;
        Ok(())
    }

    /// Edit a DM you previously sent (kind-16 edit) with an optimistic local
    /// echo. Returns the edit event id. Persistence is best-effort and only
    /// happens when the chat already exists locally.
    pub async fn edit_dm(&self, to_npub: &str, message_id: &str, new_content: &str) -> Result<String> {
        use nostr_sdk::prelude::*;

        let client = state::nostr_client().ok_or(VectorError::Other("Not connected".into()))?;
        let my_public_key = state::my_public_key().ok_or(VectorError::Other("Not logged in".into()))?;
        let my_npub = my_public_key.to_bech32().map_err(|e| VectorError::Nostr(e.to_string()))?;
        let receiver_pubkey = PublicKey::from_bech32(to_npub).map_err(|e| VectorError::Nostr(e.to_string()))?;
        let reference_event = EventId::from_hex(message_id).map_err(|e| VectorError::Nostr(e.to_string()))?;

        // NIP-30: resolve `:shortcode:` so the edit carries emoji image tags.
        let emoji_tags = emoji_packs::resolve_outbound_emoji_tags(new_content);

        let mut builder = EventBuilder::new(
            Kind::from_u16(stored_event::event_kind::MESSAGE_EDIT),
            new_content,
        ).tag(Tag::event(reference_event));
        for et in &emoji_tags {
            builder = builder.tag(Tag::custom(
                TagKind::custom("emoji"),
                [et.shortcode.clone(), et.url.clone()],
            ));
        }
        let rumor = builder.build(my_public_key);
        let edit_id = rumor.id.ok_or(VectorError::Other("Failed to get edit rumor ID".into()))?.to_hex();
        let edit_ts_ms = rumor.created_at.as_secs() * 1000;

        // Optimistic local echo + best-effort persistence.
        let msg_for_emit = {
            let mut st = state::STATE.lock().await;
            st.update_message_in_chat(to_npub, message_id, |msg| {
                msg.apply_edit(new_content.to_string(), edit_ts_ms, emoji_tags.clone());
                msg.preview_metadata = None;
            })
        };
        if let Some(msg) = msg_for_emit {
            traits::emit_event_json("message_update", serde_json::json!({
                "old_id": message_id, "message": &msg, "chat_id": to_npub
            }));
            if let Ok(db_chat_id) = db::id_cache::get_chat_id_by_identifier(to_npub) {
                let _ = db::events::save_edit_event(
                    &edit_id, message_id, new_content, &emoji_tags, db_chat_id, None, &my_npub,
                ).await;
            }
        }

        inbox_relays::send_gift_wrap(&client, &receiver_pubkey, rumor.clone(), [])
            .await.map_err(VectorError::Other)?;

        let self_wrap_client = client.clone();
        let self_wrap_session = state::SessionGuard::capture();
        tokio::spawn(async move {
            if !self_wrap_session.is_valid() { return; }
            let _ = self_wrap_client.gift_wrap(&my_public_key, rumor, []).await;
        });

        Ok(edit_id)
    }

    /// Delete a DM you sent (NIP-09 over the retained gift-wrap keys).
    pub async fn delete_dm(&self, message_id: &str) -> Result<deletion::DeleteOutcome> {
        use nostr_sdk::prelude::*;
        let rumor_id = EventId::from_hex(message_id).map_err(|e| VectorError::Nostr(e.to_string()))?;
        deletion::delete_own_dm(&rumor_id).await.map_err(VectorError::Other)
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

    /// Like [`update_profile`](Self::update_profile) but marks the profile as a bot (`bot: true` in
    /// the metadata). The SDK uses this for every bot; build human clients on `update_profile`.
    pub async fn update_bot_profile(&self, name: &str, avatar: &str, banner: &str, about: &str) -> bool {
        profile::sync::update_bot_profile(
            name.to_string(), avatar.to_string(), banner.to_string(), about.to_string(),
            &NoOpProfileSyncHandler,
        ).await
    }

    /// Update the current user's status and broadcast to relays.
    pub async fn update_status(&self, status: &str) -> bool {
        profile::sync::update_status(status.to_string()).await
    }

    /// Upload an image file to Blossom **unencrypted** and return its public URL — for avatars,
    /// banners, and other images other clients must fetch directly. (The opposite of
    /// [`send_file`](Self::send_file)'s encrypted attachments.) Pass the URL to [`update_profile`].
    ///
    /// [`update_profile`]: Self::update_profile
    pub async fn upload_public_image(&self, file_path: &str) -> Result<String> {
        let path = std::path::Path::new(file_path);
        let bytes = std::fs::read(path).map_err(VectorError::Io)?;
        if bytes.is_empty() {
            return Err(VectorError::Other("Empty image file".into()));
        }
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_lowercase();
        let mime = crate::crypto::mime_from_extension(&extension);
        let client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = client
            .signer()
            .await
            .map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let servers = crate::blossom_servers::compute_enabled_servers();
        if servers.is_empty() {
            return Err(VectorError::Other("No Blossom servers configured".into()));
        }
        crate::blossom::upload_blob_with_failover(signer, servers, std::sync::Arc::new(bytes), Some(mime))
            .await
            .map_err(VectorError::Other)
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
        use crate::community::ConcordProtocol;
        let ids = crate::db::community::list_community_ids().unwrap_or_default();
        let mut out = Vec::new();
        for id in ids {
            // Dual-stack: dispatch each held community by its stored protocol.
            match crate::db::community::community_protocol(&id).ok().flatten() {
                Some(ConcordProtocol::V2) => {
                    if let Ok(Some(c)) = crate::db::community::load_community_v2(&id) {
                        let me = state::my_public_key();
                        let is_owner = me.is_some_and(|m| c.owner().is_ok_and(|o| o == m));
                        out.push(serde_json::json!({
                            "community_id": crate::simd::hex::bytes_to_hex_32(&c.identity.community_id.0),
                            "version": 2,
                            "name": c.name,
                            "description": c.description,
                            "is_owner": is_owner,
                            "channels": c.channels.iter()
                                .map(|ch| serde_json::json!({ "channel_id": crate::simd::hex::bytes_to_hex_32(&ch.id.0), "name": ch.name, "private": ch.private }))
                                .collect::<Vec<_>>(),
                        }));
                    }
                }
                _ => {
                    if let Ok(Some(c)) = crate::db::community::load_community(&id) {
                        out.push(serde_json::json!({
                            "community_id": c.id.to_hex(),
                            "version": 1,
                            "name": c.name,
                            "description": c.description,
                            "is_owner": crate::community::service::is_proven_owner(&c),
                            "channels": c.channels.iter()
                                .map(|ch| serde_json::json!({ "channel_id": ch.id.to_hex(), "name": ch.name }))
                                .collect::<Vec<_>>(),
                        }));
                    }
                }
            }
        }
        out
    }

    /// Create a fresh **Concord v2** community owned by the local identity (the
    /// SDK's default; the GUI's `create_community` stays v1 during the migration
    /// window). Mints the self-certifying id + genesis, persists, publishes, and
    /// registers each channel as a chat. Returns a `version: 2` JSON summary.
    pub async fn create_community_v2(&self, name: &str) -> Result<serde_json::Value> {
        use crate::community::{v2::service as v2, transport::LiveTransport};
        let relays: Vec<String> = crate::state::active_trusted_relays()
            .await
            .iter()
            .map(|s| s.to_string())
            .collect();
        if relays.is_empty() {
            return Err(VectorError::Other("no relays available to host the Community".into()));
        }
        let session = state::SessionGuard::capture();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let community = v2::create_community(&transport, name, relays, None)
            .await
            .map_err(VectorError::Other)?;
        self.register_v2_chats(&community, &session).await;
        // Start streaming this community's planes right away.
        if let Some(client) = state::nostr_client() {
            crate::community::v2::realtime::refresh_subscription(&client).await;
        }
        Ok(Self::v2_summary(&community))
    }

    /// If `channel_id` belongs to a locally-held **v2** community, its
    /// `CommunityId`; `Ok(None)` for a v1 channel or unknown. The routing key for
    /// every dual-stack message op — a DB read error PROPAGATES (fail-closed)
    /// instead of silently routing a v2 channel down the v1 path on a transient
    /// failure.
    fn v2_community_for_channel(&self, channel_id: &str) -> Result<Option<crate::community::CommunityId>> {
        use crate::community::ConcordProtocol;
        let Some(cid_hex) = crate::db::community::community_id_for_channel(channel_id).map_err(VectorError::Other)? else {
            return Ok(None);
        };
        let cid = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(&cid_hex));
        Ok(match crate::db::community::community_protocol(&cid).map_err(VectorError::Other)? {
            Some(ConcordProtocol::V2) => Some(cid),
            _ => None,
        })
    }

    /// The `version: 2` JSON summary the SDK/facade hands back for a v2 community.
    fn v2_summary(community: &crate::community::v2::community::CommunityV2) -> serde_json::Value {
        let me = state::my_public_key();
        let is_owner = me.is_some_and(|m| community.owner().is_ok_and(|o| o == m));
        serde_json::json!({
            "community_id": crate::simd::hex::bytes_to_hex_32(&community.identity.community_id.0),
            "version": 2,
            "name": community.name,
            "description": community.description,
            "is_owner": is_owner,
            "channels": community.channels.iter()
                .map(|c| serde_json::json!({ "channel_id": crate::simd::hex::bytes_to_hex_32(&c.id.0), "name": c.name, "private": c.private }))
                .collect::<Vec<_>>(),
        })
    }

    /// Register each of a v2 community's channels as a chat row (so it surfaces in
    /// the chat list / `communities()`), mirroring the v1 create path. `session`
    /// is captured by the caller BEFORE its network I/O, so this STATE write is
    /// skipped if the account swapped mid-flight (else we'd write A's community
    /// into B's in-memory chats).
    pub async fn register_v2_chats(&self, community: &crate::community::v2::community::CommunityV2, session: &state::SessionGuard) {
        let owner_npub = community.owner().ok().and_then(|p| ToBech32::to_bech32(&p).ok());
        let me = state::my_public_key();
        let is_owner = me.is_some_and(|m| community.owner().is_ok_and(|o| o == m));
        let id_hex = crate::simd::hex::bytes_to_hex_32(&community.identity.community_id.0);
        // The chat list shows ONE row per community — the primary channel under the
        // community's metadata (v1-group parity; multi-channel UI is a later cut).
        let Some(primary) = community.primary_channel() else { return };
        let primary_hex = crate::simd::hex::bytes_to_hex_32(&primary.id.0);
        let sibling_ids: Vec<String> = community
            .channels
            .iter()
            .filter(|c| c.id.0 != primary.id.0)
            .map(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0))
            .collect();
        let slim = {
            let mut st = state::STATE.lock().await;
            if !session.is_valid() {
                return; // account swapped during the join/create — don't write into the new one.
            }
            st.upsert_community_chat(
                &primary_hex,
                &community.name,
                community.description.as_deref().unwrap_or(""),
                &id_hex,
                is_owner,
                community.icon.is_some(),
                owner_npub.as_deref(),
                Some(community.created_at_ms),
                community.dissolved,
            );
            // Sibling-channel rows the message persist auto-created are bare
            // anchors (their DB rows keep the history's FK) — never surfaced.
            st.chats.retain(|c| !sibling_ids.contains(&c.id));
            st.chats
                .iter()
                .find(|c| c.id == primary_hex)
                .map(|chat| crate::db::chats::SlimChatDB::from_chat(chat, &st.interner))
        };
        // Persist the row so a fresh boot reloads the community's name/metadata
        // instead of the bare auto-created anchor. Session re-check: don't write
        // account A's row into a swapped-in account B's DB.
        if !session.is_valid() {
            return;
        }
        if let Some(slim) = slim {
            let _ = crate::db::chats::save_slim_chat(&slim);
        }
    }

    /// Join a Community from a public invite URL (`vectorapp.io/invite#...`). Fetches the
    /// token-encrypted bundle, persists the member-view Community, and registers its channels
    /// as chats. Returns a JSON summary.
    pub async fn join_community(&self, invite_url: &str) -> Result<serde_json::Value> {
        use crate::community::{public_invite, service, transport::LiveTransport};
        // Dual-stack: a v2 link is `…/invite/<naddr>#<fragment>` (a naddr in the
        // path); a v1 link is `…/invite#<base64url>` (fragment only). Try the v2
        // parser first — it only succeeds on the v2 shape — then fall through to v1.
        if crate::community::v2::invite::parse_invite_link(invite_url).is_ok() {
            let session = state::SessionGuard::capture();
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            let community = crate::community::v2::service::accept_public_link(&transport, invite_url)
                .await
                .map_err(VectorError::Other)?;
            self.register_v2_chats(&community, &session).await;
            if let Some(client) = state::nostr_client() {
                crate::community::v2::realtime::refresh_subscription(&client).await;
            }
            // Seed the membership store post-join. With a live listen the follow
            // worker does it (and SURFACES the folded joins as presence lines —
            // the joiner sees the room's history, own join included); headless
            // callers seed directly (membership only, no feed to surface).
            if crate::community::v2::realtime::follow_worker_running() {
                crate::community::v2::realtime::enqueue_follow(community.id());
            } else {
                let seed_session = state::SessionGuard::capture();
                let seed_community = community.clone();
                tokio::spawn(async move {
                    if !seed_session.is_valid() {
                        return;
                    }
                    let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(20));
                    if matches!(
                        crate::community::v2::service::sync_guestbook(&transport, &seed_community, &seed_session).await,
                        Ok(fresh) if !fresh.is_empty()
                    ) {
                        let cid_hex = crate::simd::hex::bytes_to_hex_32(&seed_community.id().0);
                        emit_event("community_refreshed", &serde_json::json!({ "community_id": cid_hex }));
                    }
                });
            }
            return Ok(Self::v2_summary(&community));
        }
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
        // Attribute our join presence to the link we used (creator + label) so the owner's per-link
        // counter ticks. Mirrors the desktop public-join path.
        let attribution = bundle.creator_npub.clone().map(|by| (by, bundle.label.clone()));
        self.finalize_member_join(community, &transport, attribution).await
    }

    /// List the parked private invites (giftwrapped) awaiting acceptance. Each entry is the
    /// community id, its name (from the stored bundle), and the inviter's npub.
    pub fn list_pending_invites(&self) -> Result<Vec<serde_json::Value>> {
        let rows = crate::db::community::list_pending_invites().map_err(VectorError::Other)?;
        Ok(rows.iter().map(|p| {
            // A v2 bundle carries owner_salt/community_root and self-certifies its
            // owner; a successful (validating) v2 parse means the modern protocol.
            if let Ok(v2) = crate::community::v2::invite::CommunityInvite::from_bundle_json(&p.bundle_json) {
                serde_json::json!({
                    "community_id": p.community_id,
                    "name": v2.name,
                    "inviter_npub": p.inviter_npub,
                    "version": 2,
                })
            } else {
                let name = crate::community::invite::CommunityInvite::from_json(&p.bundle_json)
                    .ok().map(|i| i.name).unwrap_or_default();
                serde_json::json!({
                    "community_id": p.community_id,
                    "name": name,
                    "inviter_npub": p.inviter_npub,
                    "version": 1,
                })
            }
        }).collect())
    }

    /// Accept a PARKED private invite by community id: rebuild the member-view Community from the stored
    /// bundle, finalize the join exactly like a public link, then drop the pending row. Mirrors the
    /// desktop's consent-then-join for an invite delivered over a gift wrap.
    pub async fn accept_pending_invite(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::transport::LiveTransport;
        let bundle_json = crate::db::community::get_pending_invite(community_id)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other(format!("no pending invite for {community_id}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

        // Dual-stack: a validating v2 bundle parse means a v2 Direct Invite.
        if crate::community::v2::invite::CommunityInvite::from_bundle_json(&bundle_json).is_ok() {
            let session = state::SessionGuard::capture();
            // The inviter's hex (parked at receive) attributes the Guestbook Join.
            let inviter = crate::db::community::list_pending_invites()
                .ok()
                .and_then(|rows| rows.into_iter().find(|p| p.community_id == community_id).map(|p| p.inviter_npub));
            // On failure the parked row is LEFT INTACT for retry — we must NOT auto-delete
            // on a verify failure: the multi-relay transport launders an unreachable-relay
            // error into an empty fetch, which yields the same "could not verify" as a
            // forged root (and a control-plane flood does too), so an auto-delete would
            // erase a GENUINE invite on a transient blip or an attacker's flood. A
            // pre-planted forged-root bundle (deferred protocol residual) is instead
            // cleared by the user declining it.
            let community = crate::community::v2::service::accept_parked_invite(&transport, &bundle_json, inviter.as_deref())
                .await
                .map_err(VectorError::Other)?;
            if !session.is_valid() {
                return Err(VectorError::Other("account changed during join".into()));
            }
            self.register_v2_chats(&community, &session).await;
            if let Some(client) = state::nostr_client() {
                crate::community::v2::realtime::refresh_subscription(&client).await;
            }
            crate::community::v2::realtime::enqueue_follow(community.id());
            let _ = crate::db::community::delete_pending_invite(community_id);
            return Ok(Self::v2_summary(&community));
        }

        // v1 route.
        use crate::community::invite::{accept_invite, CommunityInvite};
        let invite = CommunityInvite::from_json(&bundle_json).map_err(VectorError::Other)?;
        let community = accept_invite(&invite).map_err(VectorError::Other)?;
        // Private invites carry no public-link label; the inviter attribution metric is link-only.
        let summary = self.finalize_member_join(community, &transport, None).await?;
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
        attribution: Option<(String, Option<String>)>,
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
            let _ = service::publish_presence(transport, &community, primary, true, attribution).await;
        }
        Ok(serde_json::json!({
            "community_id": community.id.to_hex(),
            "version": 1,
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
            "version": 1,
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
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        // Dual-stack: mint a v2 link for a v2 community (naddr#fragment).
        if let Some(Some(crate::community::ConcordProtocol::V2)) =
            crate::db::community::community_protocol(&cid).ok()
        {
            let community = crate::db::community::load_community_v2(&cid)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            // v2 `build_invite_url` appends its own `/invite/<naddr>`, so pass the
            // bare domain (strip the `/invite` the v1 constant carries).
            let base = crate::community::public_invite::INVITE_URL_BASE.trim_end_matches("/invite");
            let minted = crate::community::v2::service::mint_public_link(&transport, &community, base, None, None)
                .await
                .map_err(VectorError::Other)?;
            return Ok(minted.url);
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
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        // Dual-stack: a v2 community sends a Direct Invite (3313 giftwrap).
        // DELIBERATELY ungated, unlike v1's CREATE_INVITE + banlist pre-check: a
        // Direct Invite is an ungateable key handoff (CORD-05 §6 — "any keyholder
        // can whisper keys"), so any member may extend one; the real access cut is
        // the rekey, not a permission on inviting.
        if let Some(Some(crate::community::ConcordProtocol::V2)) =
            crate::db::community::community_protocol(&cid).ok()
        {
            let community = crate::db::community::load_community_v2(&cid)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let recipient = nostr_sdk::prelude::PublicKey::parse(invitee_npub)
                .map_err(|e| VectorError::Other(format!("bad invitee npub: {e}")))?;
            let client = crate::state::nostr_client().ok_or_else(|| VectorError::Other("Not connected".into()))?;
            // Gift-wrap the 3313 Direct-Invite rumor (the bundle JSON) to the RECIPIENT'S
            // inbox relays (kind-10050) — a not-yet-member sees it on their DM sub;
            // the community relays wouldn't reach them. `#k=3313` per CORD-05 §6.
            let bundle = crate::community::v2::service::bundle_of(&community, Some(my_pk), None, None);
            let bundle_json = serde_json::to_string(&bundle).map_err(|e| VectorError::Other(e.to_string()))?;
            let rumor = nostr_sdk::EventBuilder::new(
                nostr_sdk::Kind::Custom(crate::community::v2::kind::DIRECT_INVITE),
                bundle_json,
            )
            .build(my_pk);
            let k_tag = nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::Custom("k".into()),
                [crate::community::v2::kind::DIRECT_INVITE.to_string()],
            );
            if !session.is_valid() {
                return Err(VectorError::Other("account changed".into()));
            }
            crate::inbox_relays::send_gift_wrap(&client, &recipient, rumor, [k_tag])
                .await
                .map_err(VectorError::Other)?;
            return Ok(serde_json::json!({ "invited": invitee_npub, "version": 2 }));
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

    /// The public invite links this account minted for a Community (to list + revoke). Each carries
    /// the hex `token` (the link secret) needed by [`Self::revoke_public_invite`]. A local read for
    /// both protocols — links minted on this device (a v2 mint also syncs the cross-device 13303
    /// record; v2 `join_count` is not yet tracked and is always 0).
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
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(20));
        // Dual-stack: a v2 link is retired by its 16-byte token hex (re-post the
        // coordinate as a tombstone + tombstone the 13303 entry + refresh the Registry).
        if let Some(Some(crate::community::ConcordProtocol::V2)) = crate::db::community::community_protocol(&cid).ok() {
            let community = crate::db::community::load_community_v2(&cid)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            return crate::community::v2::service::revoke_public_link(&transport, &community, token)
                .await
                .map_err(VectorError::Other);
        }
        let token_bytes = crate::simd::hex::hex_to_bytes_32(token);
        let community = crate::db::community::load_community(&cid)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("community not found".into()))?;
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
        // Dual-stack: route by the owning community's stored protocol.
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            // The NIP-C7 q tag's author slot is a SHOULD — best-effort from the
            // held message, empty (= unknown) when the parent isn't in memory.
            let reply = match replied_to.filter(|r| !r.is_empty()) {
                Some(parent_id) => {
                    let author_hex = {
                        let st = state::STATE.lock().await;
                        st.find_message(parent_id)
                            .and_then(|(_, m)| m.npub.as_deref().and_then(|n| nostr_sdk::prelude::PublicKey::parse(n).ok()))
                            .map(|pk| pk.to_hex())
                            .unwrap_or_default()
                    };
                    Some((parent_id.to_string(), author_hex))
                }
                None => None,
            };
            let reply_ref = reply.as_ref().map(|(id, author)| (id.as_str(), author.as_str()));
            // NIP-30: resolve `:shortcode:` against subscribed packs so the rumor
            // carries `["emoji", ...]` pairs — parity with the v1 inner event.
            let emoji_owned = crate::emoji_packs::resolve_outbound_emoji_tags(content);
            let emoji_pairs: Vec<(&str, &str)> = emoji_owned.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();
            return crate::community::v2::service::send_chat_message(&transport, &community, &ch, content, reply_ref, &emoji_pairs, vec![])
                .await
                .map_err(VectorError::Other);
        }
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
        let session = state::SessionGuard::capture();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let outer = service::send_signed_message(&transport, &community, &channel, &inner)
            .await
            .map_err(VectorError::Other)?;
        // Local echo so get_messages reflects the send (the relay echo dedups on inner id).
        // A swap during the publish must not echo account A's message into account B.
        if !session.is_valid() {
            return Ok(message_id);
        }
        let echoed = {
            let mut st = state::STATE.lock().await;
            inbound::process_incoming(&mut st, &outer, &channel, &author_pk)
        };
        if let Some(inbound::IncomingEvent::NewMessage(msg)) = echoed {
            let _ = crate::db::events::save_message(channel_id, &msg).await;
        }
        Ok(message_id)
    }

    /// Send a file to a Community channel as an encrypted attachment. Returns the message id.
    /// Mirrors the DM file pipeline (encrypt → Blossom upload → NIP-92 `imeta`) but publishes
    /// over the community transport.
    pub async fn send_community_file(&self, channel_id: &str, file_path: &str) -> Result<String> {
        use crate::community::{attachments, envelope, inbound, service, transport::LiveTransport};
        let path = std::path::Path::new(file_path);
        let bytes = std::fs::read(path).map_err(VectorError::Io)?;
        if bytes.is_empty() {
            return Err(VectorError::Other("Empty file".into()));
        }
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_lowercase();

        // Snapshot the session BEFORE the upload: the destination below is resolved
        // from THIS account's DB, and the upload can outlive an account swap.
        let session = state::SessionGuard::capture();
        // Dual-stack: resolve the destination BEFORE the upload so a bad channel
        // fails fast (never spend an upload on an unroutable send).
        let v2_target = match self.v2_community_for_channel(channel_id)? {
            Some(id) => Some(
                crate::db::community::load_community_v2(&id)
                    .map_err(VectorError::Other)?
                    .ok_or_else(|| VectorError::Other("v2 community not found".into()))?,
            ),
            None => None,
        };
        let v1_target = match v2_target {
            Some(_) => None,
            None => Some(self.resolve_channel(channel_id)?),
        };
        let author_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;

        let file_hash = crate::crypto::sha256_hex(&bytes);
        let mime = crate::crypto::mime_from_extension(&extension);
        let img_meta = crate::crypto::generate_image_metadata(&bytes);

        // Save the plaintext locally (hash-keyed) so the sender previews it instantly.
        let download_dir = crate::db::get_download_dir();
        let _ = std::fs::create_dir_all(&download_dir);
        let local_name = if filename.is_empty() { format!("{}.{}", &file_hash, extension) } else { filename.clone() };
        let local_path = crate::crypto::resolve_unique_filename(&download_dir, &local_name);
        let _ = std::fs::write(&local_path, &bytes);

        // Encrypt → upload to Blossom (signer reused for the envelope below).
        let params = crate::crypto::generate_encryption_params();
        let encrypted = crate::crypto::encrypt_data(&bytes, &params)?;
        let encrypted_size = encrypted.len() as u64;

        let client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = client.signer().await.map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let servers = crate::blossom_servers::compute_enabled_servers();
        if servers.is_empty() {
            return Err(VectorError::Other("No Blossom servers configured".into()));
        }
        let noop_progress: crate::blossom::ProgressCallback = std::sync::Arc::new(|_, _| Ok(()));
        let url = crate::blossom::upload_blob_with_progress_and_failover(
            signer.clone(),
            servers,
            std::sync::Arc::new(encrypted),
            Some(mime),
            /* is_encrypted */ true,
            noop_progress,
            Some(3),
            Some(std::time::Duration::from_secs(2)),
            None,
        ).await.map_err(VectorError::Other)?;

        let attachment = crate::types::Attachment {
            id: file_hash.clone(),
            key: params.key.clone(),
            nonce: params.nonce.clone(),
            extension: extension.clone(),
            name: filename.clone(),
            url,
            path: local_path.to_string_lossy().to_string(),
            size: encrypted_size,
            img_meta,
            downloading: false,
            downloaded: true,
            ..Default::default()
        };
        let imeta = vec![attachments::attachment_to_imeta(&attachment)];

        // The upload straddled awaits — never publish a pre-swap destination.
        if !session.is_valid() {
            return Err(VectorError::Other("account changed during upload".into()));
        }
        // v2: the imeta rides the kind-9 rumor verbatim (NIP-92), content empty.
        if let Some(community) = v2_target {
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(30));
            return crate::community::v2::service::send_chat_message(&transport, &community, &ch, "", None, &[], imeta)
                .await
                .map_err(VectorError::Other);
        }
        let (community, channel) = v1_target.expect("v1 target resolved when no v2 community matched");
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let unsigned = envelope::build_inner_full(
            author_pk, &channel.id, channel.epoch,
            stored_event::event_kind::COMMUNITY_MESSAGE, "", ms, None, &[], &imeta,
        );
        let message_id = unsigned.id.ok_or_else(|| VectorError::Other("inner event has no id".into()))?.to_hex();
        let inner = unsigned.sign(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(30));
        let outer = service::send_signed_message(&transport, &community, &channel, &inner)
            .await.map_err(VectorError::Other)?;
        // Local echo so get_messages reflects the send.
        let echoed = {
            let mut st = state::STATE.lock().await;
            inbound::process_incoming(&mut st, &outer, &channel, &author_pk)
        };
        if let Some(inbound::IncomingEvent::NewMessage(m)) = echoed {
            let _ = crate::db::events::save_message(channel_id, &m).await;
        }
        Ok(message_id)
    }

    /// Send an ephemeral typing indicator to a Community channel.
    pub async fn send_community_typing(&self, channel_id: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(8));
            return crate::community::v2::service::send_typing(&transport, &community, &ch)
                .await
                .map_err(VectorError::Other);
        }
        let (community, channel) = self.resolve_channel(channel_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(8));
        service::publish_typing_signal(&transport, &community, &channel)
            .await
            .map_err(VectorError::Other)
    }

    /// React to a Community message. `emoji_url` carries the NIP-30 image URL for a custom
    /// `:shortcode:` reaction (parity with DMs).
    pub async fn send_community_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
        emoji_url: Option<&str>,
    ) -> Result<()> {
        let emoji_tags: Vec<crate::types::EmojiTag> = match emoji_url {
            Some(url) if emoji.starts_with(':') && emoji.ends_with(':') && emoji.len() >= 3 && !url.is_empty() => {
                vec![crate::types::EmojiTag { shortcode: emoji[1..emoji.len() - 1].to_string(), url: url.to_string() }]
            }
            _ => Vec::new(),
        };
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let session = state::SessionGuard::capture();
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            // NIP-25 names the reacted-to author (a required `p`). STATE first, then
            // the persisted row (v2 history + the send echo live in the shared events
            // store, so this almost always resolves locally); the channel-page fetch
            // is the last resort for a target this device never saw.
            let held = {
                let st = state::STATE.lock().await;
                st.find_message(message_id)
                    .and_then(|(_, m)| m.npub.as_deref().and_then(|n| nostr_sdk::prelude::PublicKey::parse(n).ok()))
            };
            let held = held.or_else(|| {
                crate::db::events::event_author(message_id)
                    .ok()
                    .flatten()
                    .and_then(|n| nostr_sdk::prelude::PublicKey::parse(&n).ok())
            });
            let target_author = match held {
                Some(pk) => pk,
                None => crate::community::v2::service::fetch_channel(&transport, &community, &ch, 500)
                    .await
                    .map_err(VectorError::Other)?
                    .iter()
                    .find(|f| f.event.opened().rumor_id.to_hex() == message_id)
                    .map(|f| f.event.opened().author)
                    .ok_or_else(|| VectorError::Other("reacted-to message not found".into()))?,
            };
            // The author lookup straddled awaits against THIS account's community.
            if !session.is_valid() {
                return Err(VectorError::Other("account changed before send".into()));
            }
            let pair = emoji_tags.first().map(|t| (t.shortcode.as_str(), t.url.as_str()));
            // The NIP-25 `k` names the target's rumor kind. Stored rows don't keep
            // wire-kind fidelity yet, so a reaction to a received kind-1111 thread
            // reply claims `9` — Armada's fold ignores reaction `k`, and exact
            // threading lands with the thread-aware GUI.
            return crate::community::v2::service::send_reaction(
                &transport, &community, &ch, message_id, &target_author.to_hex(), crate::community::v2::kind::MESSAGE, emoji, pair,
            )
            .await
            .map(|_| ())
            .map_err(VectorError::Other);
        }
        self.publish_community_control(
            channel_id, stored_event::event_kind::COMMUNITY_REACTION, emoji, message_id, &emoji_tags,
        ).await
    }

    /// Edit one of your own Community messages.
    pub async fn edit_community_message(&self, channel_id: &str, message_id: &str, new_content: &str) -> Result<()> {
        let emoji_tags = emoji_packs::resolve_outbound_emoji_tags(new_content);
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            return crate::community::v2::service::send_edit(&transport, &community, &ch, message_id, new_content)
                .await
                .map(|_| ())
                .map_err(VectorError::Other);
        }
        self.publish_community_control(
            channel_id, stored_event::event_kind::COMMUNITY_EDIT, new_content, message_id, &emoji_tags,
        ).await
    }

    /// Delete one of your own Community messages, resolving its channel from local
    /// state (the GUI path). A headless v2 consumer holds no local history — use
    /// [`Self::delete_community_message_in`] with the channel id instead.
    pub async fn delete_community_message(&self, message_id: &str) -> Result<()> {
        let channel_id = {
            let st = state::STATE.lock().await;
            match st.find_message(message_id) {
                Some((chat, _)) => chat.id.clone(),
                None => return Err(VectorError::Other("message not found (already deleted?)".into())),
            }
        };
        self.delete_community_message_in(&channel_id, message_id).await
    }

    /// Delete one of your own Community messages in `channel_id`: a NIP-09 relay nuke when the
    /// per-message key is held (v1) or the in-plane kind-5 (v2), plus a cooperative tombstone so
    /// peers hide it, plus best-effort attachment cleanup.
    pub async fn delete_community_message_in(&self, channel_id: &str, message_id: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let session = state::SessionGuard::capture();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

        // Attachment URLs come from local state when held (a headless v2 consumer
        // has none — blob cleanup is then the receiving peers' concern, not ours).
        let attachment_urls: Vec<String> = {
            let st = state::STATE.lock().await;
            st.find_message(message_id)
                .map(|(_, msg)| msg.attachments.iter().filter(|a| !a.url.is_empty()).map(|a| a.url.clone()).collect())
                .unwrap_or_default()
        };

        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            // v2: the cooperative in-plane kind-5 (the wrap-ciphertext scrub needs
            // the ephemeral wrap key, not retained in this cut).
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(&channel_id));
            crate::community::v2::service::send_delete(
                &transport, &community, &ch, message_id, crate::community::v2::kind::MESSAGE,
            )
            .await
            .map_err(VectorError::Other)?;
        } else {
            // Layer 1 — relay nuke against the retained per-message key (best-effort).
            if crate::db::community::get_message_key(message_id).map(|k| k.is_some()).unwrap_or(false) {
                let _ = service::delete_message(&transport, message_id).await;
            }
            // Layer 2 — cooperative tombstone so peers hide it.
            self.publish_community_control(
                &channel_id, stored_event::event_kind::COMMUNITY_DELETE, "", message_id, &[],
            ).await?;
        }
        // Layer 3 — best-effort attachment blob delete.
        if !attachment_urls.is_empty() {
            if let Some(client) = state::nostr_client() {
                if let Ok(signer) = client.signer().await {
                    crate::blossom::delete_blobs_best_effort(signer, attachment_urls);
                }
            }
        }
        // Local removal — the publishes above straddled awaits; a swap must not let this
        // strip the message from a swapped-in account's STATE + DB (message_id is global).
        if !session.is_valid() {
            return Ok(());
        }
        let removed_chat = {
            let mut st = state::STATE.lock().await;
            st.remove_message(message_id).map(|(cid, _)| cid)
        };
        let _ = crate::db::events::delete_event(message_id).await;
        traits::emit_event_json("message_removed", serde_json::json!({
            "id": message_id, "chat_id": removed_chat.as_deref().unwrap_or(&channel_id), "reason": "deleted",
        }));
        Ok(())
    }

    /// Shared community control-event publish (reaction / edit / delete tombstone): build the
    /// inner-typed envelope, sign, send over the community transport, then locally echo + persist + emit.
    async fn publish_community_control(
        &self,
        channel_id: &str,
        kind: u16,
        content: &str,
        target: &str,
        emoji_tags: &[crate::types::EmojiTag],
    ) -> Result<()> {
        use crate::community::{envelope, inbound, service, transport::LiveTransport};
        let (community, channel) = self.resolve_channel(channel_id)?;
        let author_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let unsigned = envelope::build_inner_typed(
            author_pk, &channel.id, channel.epoch, kind, content, ms, Some(target), emoji_tags,
        );
        let client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = client.signer().await.map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let inner = unsigned.sign(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
        let session = state::SessionGuard::capture();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let outer = service::send_signed_message(&transport, &community, &channel, &inner)
            .await.map_err(VectorError::Other)?;
        // Local echo + persist + emit (relay echo dedups on inner id). A swap during the
        // publish must not echo account A's control event into account B.
        if !session.is_valid() {
            return Ok(());
        }
        let outcome = {
            let mut st = state::STATE.lock().await;
            inbound::process_incoming(&mut st, &outer, &channel, &author_pk)
        };
        if let Some(inbound::IncomingEvent::Updated { target_id, message, edit_event }) = outcome {
            if let Some(ev) = edit_event {
                let mut ev = (*ev).clone();
                if let Ok(cid) = crate::db::id_cache::get_chat_id_by_identifier(channel_id) { ev.chat_id = cid; }
                let _ = crate::db::events::save_event(&ev).await;
            } else {
                let _ = crate::db::events::save_message(channel_id, &message).await;
            }
            traits::emit_event_json("message_update", serde_json::json!({
                "old_id": target_id, "message": &message, "chat_id": channel_id,
            }));
        }
        Ok(())
    }

    /// Catch a Community channel up from relays. v1: fetch + ingest the latest page of messages,
    /// reactions, edits, and deletes, returning how many were brand-new. v2: consensus catch-up
    /// only (rekeys + control refold) — chat history delivers over the live handler bridge, so the
    /// count is always 0. Returns `(new_message_count, warnings)`; `warnings` are NON-FATAL errors
    /// hit during the sync (catch-up, control fold, read-cut resume) — surfaced rather than
    /// swallowed so a headless caller is never blind to "the sync ran but a re-founding couldn't
    /// be resumed."
    pub async fn sync_community_channel(&self, channel_id: &str, limit: usize) -> Result<(usize, Vec<String>)> {
        use crate::community::{inbound, send, service, transport::LiveTransport};
        let my_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        // v2: consensus catch-up (rekeys then control refold) + chat backfill. With a
        // running listen() the coalescing worker owns the follow (never run inline beside
        // it — two concurrent follows can whole-row clobber); headless, walk it inline.
        // The chat page is fetched + persisted either way, so get_messages backfills.
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let warnings = if community::v2::realtime::follow_worker_running() {
                community::v2::realtime::enqueue_follow(&id);
                Vec::new()
            } else {
                Self::v2_inline_follow(&id).await
            };
            let new = Self::v2_backfill_channel(&id, channel_id, limit).await;
            return Ok((new, warnings));
        }
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
                inbound::IncomingEvent::ReactionRemoved { reaction_id, .. } => {
                    // save_message is additive, so a revoked reaction's kind-7 row must be
                    // dropped explicitly or it resurrects on reload.
                    let _ = crate::db::events::delete_event(reaction_id).await;
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
                    let _ = crate::db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), *created_at, invited_by.as_deref(), invited_label.as_deref()).await;
                }
                inbound::IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, event_id, created_at } => {
                    // Persist only (DM-parity row) — the miniapp layer bootstraps from the DB at
                    // game-open. Live gossip-feed pokes are the realtime subscription's job.
                    community::service::persist_webxdc_signal(
                        channel_id, npub, topic_id, node_addr.as_deref(), event_id, *created_at,
                    ).await;
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
                inbound::IncomingEvent::Typing { .. } => {
                    // Realtime-only ephemeral signal; never fetched in a sync batch. No-op.
                }
            }
        }
        Ok((new, warnings))
    }

    /// The composer's `/` picker snapshot for `chat_id`, answered INSTANTLY
    /// from local state: the chat's bot-flagged members (kind-0 `bot: true` —
    /// the SDK sets it on every bot it builds) and their last-known manifests
    /// from the persistent store. When the last refresh is older than a minute
    /// (or the bot set changed), ONE background REQ re-fetches every bot's
    /// manifest together (5s unification window), persists newer editions, and
    /// emits `chat_commands_updated` — the UI swaps the list in when it lands.
    /// Works for BOTH community protocols (an invocation is plain content; only
    /// the optional routing tag is v2-only) and DMs. The manifest REQ always
    /// includes the discovery indexers beside the chat's own relays, so an
    /// unreachable or stranger-dropping community relay can't blind the picker.
    pub async fn get_chat_commands(&self, chat_id: &str) -> crate::bot_interface::ChatCommandsSnapshot {
        use crate::bot_interface::{self, ChatCommandsSnapshot};
        use nostr_sdk::prelude::ToBech32;

        let mut bots: Vec<nostr_sdk::prelude::PublicKey> = Vec::new();
        let mut relays: Vec<String> = Vec::new();
        let community_hex = crate::db::community::community_id_for_channel(chat_id).ok().flatten();
        if let Some(cid_hex) = community_hex {
            let mut members: Vec<nostr_sdk::prelude::PublicKey> = Vec::new();
            if let Ok(Some(community)) = Self::load_v2_if_v2(&cid_hex) {
                members = community::v2::service::stored_memberlist(&community).unwrap_or_default();
                relays = community.relays.clone();
            } else {
                let id = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(&cid_hex));
                let Ok(Some(community)) = crate::db::community::load_community(&id) else {
                    return ChatCommandsSnapshot { bots: 0, commands: Vec::new(), fresh: true };
                };
                relays = community.relays.clone();
                for (npub, _) in crate::db::community::community_member_activity(&cid_hex).unwrap_or_default() {
                    if let Ok(pk) = nostr_sdk::prelude::PublicKey::parse(&npub) {
                        members.push(pk);
                    }
                }
            }
            let state = crate::state::STATE.lock().await;
            for pk in members {
                let Ok(npub) = pk.to_bech32() else { continue };
                if state.get_profile(&npub).map(|p| p.flags.is_bot()).unwrap_or(false) {
                    bots.push(pk);
                }
            }
        } else if chat_id.starts_with("npub1") {
            if let Ok(pk) = nostr_sdk::prelude::PublicKey::parse(chat_id) {
                let is_bot = {
                    let state = crate::state::STATE.lock().await;
                    state.get_profile(chat_id).map(|p| p.flags.is_bot()).unwrap_or(false)
                };
                if is_bot {
                    bots.push(pk);
                    // The counterpart published its manifest to its own login
                    // relays/indexers — our connected pool is the read set.
                    if let Some(client) = crate::state::nostr_client() {
                        relays = client.relays().await.keys().map(|u| u.to_string()).collect();
                    }
                }
            }
        }

        if bots.is_empty() {
            return ChatCommandsSnapshot { bots: 0, commands: Vec::new(), fresh: true };
        }
        // The chat's own relays PLUS the discovery indexers, one REQ across the
        // union — a room whose relays refuse kind 33304 still resolves.
        relays.extend(bot_interface::DISCOVERY_RELAYS.iter().map(|s| s.to_string()));
        relays.sort();
        relays.dedup();
        // Deterministic order: the freshness check compares the exact bot set,
        // and picker sections stay stable across refreshes.
        bots.sort_by_key(|p| p.to_hex());
        let bot_hexes: Vec<String> = bots.iter().map(|p| p.to_hex()).collect();
        let commands = bot_interface::assemble_from_store(&bot_hexes);
        let fresh = bot_interface::commands_fresh(chat_id, &bot_hexes);
        if !fresh {
            bot_interface::spawn_commands_refresh(chat_id.to_string(), bots.clone(), relays);
        }
        ChatCommandsSnapshot { bots: bots.len(), commands, fresh }
    }

    /// Observed members of a Community (best-effort: those who've posted or announced a join,
    /// minus anyone who's left or is banned). v1 entries are `{npub, last_active}`; a v2 entry
    /// is `{npub}` (the Complete Memberlist carries no activity time). Best-effort throughout:
    /// a transport failure yields an empty list, never an error.
    pub async fn get_community_members(&self, community_id: &str) -> Vec<serde_json::Value> {
        use nostr_sdk::prelude::ToBech32;
        // v2: the Complete Memberlist from LOCAL state (persisted guestbook +
        // observed authors + roster grantees − banlist). The store is seeded
        // post-join and cursor-caught-up by the follow worker (boot/reconnect) +
        // live ingest; a cold store (a hold predating the store) seeds in the
        // background and refreshes the UI when it lands.
        match Self::load_v2_if_v2(community_id) {
            Ok(Some(community)) => {
                let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
                let (_, cursor) = crate::db::community::get_guestbook(&cid_hex).unwrap_or_default();
                if cursor == 0 {
                    if crate::community::v2::realtime::follow_worker_running() {
                        crate::community::v2::realtime::enqueue_follow(community.id());
                    } else {
                        let session = state::SessionGuard::capture();
                        let c2 = community.clone();
                        tokio::spawn(async move {
                            if !session.is_valid() {
                                return;
                            }
                            let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(20));
                            if matches!(crate::community::v2::service::sync_guestbook(&transport, &c2, &session).await, Ok(fresh) if !fresh.is_empty()) {
                                emit_event("community_refreshed", &serde_json::json!({ "community_id": cid_hex }));
                            }
                        });
                    }
                }
                return crate::community::v2::service::stored_memberlist(&community)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|pk| pk.to_bech32().ok())
                    .map(|npub| serde_json::json!({ "npub": npub }))
                    .collect();
            }
            Ok(None) => {} // genuinely v1 / unknown — fall through.
            // Can't determine the protocol: best-effort empty, never a v1 guess.
            Err(_) => return Vec::new(),
        }
        crate::db::community::community_member_activity(community_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(npub, last_active)| serde_json::json!({ "npub": npub, "last_active": last_active }))
            .collect()
    }

    /// One synchronous v2 follow pass — rekeys first (a base adopt moves the
    /// control address), then a control refold on the FRESH state, the same order
    /// the live follow worker runs. Returns non-fatal warnings.
    async fn v2_inline_follow(id: &crate::community::CommunityId) -> Vec<String> {
        use crate::community::transport::LiveTransport;
        let session = state::SessionGuard::capture();
        // Serialize with the live follow worker: `follow_worker_running` is
        // check-then-act, so a worker can spawn right after a caller saw `false` —
        // this shared per-community lock is what actually prevents two follows of
        // one community interleaving their whole-row saves.
        let lock = crate::community::v2::realtime::follow_lock(id);
        let _guard = lock.lock().await;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let mut warnings: Vec<String> = Vec::new();
        let Ok(Some(community)) = crate::db::community::load_community_v2(id) else {
            warnings.push("v2 community not found".to_string());
            return warnings;
        };
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&id.0);
        match crate::community::v2::service::follow_rekeys(&transport, &community, &session).await {
            // A tombstone surfaced during catch-up — sealed read-only; stop here.
            Ok(f) if f.dissolved => return warnings,
            Ok(f) if f.self_removed => {
                // An authorized rotation that excluded us IS a removal — but the
                // follow straddled awaits, so never delete from a swapped-in DB.
                if session.is_valid() {
                    let _ = crate::db::community::delete_community(&cid_hex);
                }
                return warnings;
            }
            Ok(_) => {}
            Err(e) => warnings.push(format!("v2 rekey follow failed: {e}")),
        }
        if let Ok(Some(fresh)) = crate::db::community::load_community_v2(id) {
            match crate::community::v2::service::follow_control(&transport, &fresh, &session).await {
                // A control change can reveal rekey work that predates it (a
                // just-announced private channel's key crate already sits on its
                // rekey plane), so walk the rekeys once more on the fresh state.
                Ok(Some(changed)) => {
                    if let Err(e) = crate::community::v2::service::follow_rekeys(&transport, &changed, &session).await {
                        warnings.push(format!("v2 rekey follow failed: {e}"));
                    }
                }
                Ok(None) => {}
                Err(e) => warnings.push(format!("v2 control follow failed: {e}")),
            }
        }
        warnings
    }

    /// Fetch a v2 channel's recent chat history and PERSIST it into the shared events
    /// tables (the same store v1 uses), so `get_messages`/`get_new_messages` backfill for
    /// v2 exactly like v1. PAGES backwards until it reaches messages it already holds
    /// (bounded), so a reconnecting bot that slept through more than one page of traffic
    /// still catches the whole gap instead of only the newest `limit`. Reuses the v2
    /// inbound bridge (dedup + STATE aggregate) + the v1 save path. Returns the count of
    /// brand-new messages applied. Best-effort: a fetch failure is 0.
    async fn v2_backfill_channel(id: &crate::community::CommunityId, channel_id: &str, limit: usize) -> usize {
        use crate::community::v2::inbound::{apply_chat_to_state, persist_chat, ChatPersist};
        /// Deepest catch-up walk: pages × page-size bounds one reconnect's fetch.
        const MAX_BACKFILL_PAGES: usize = 8;
        // Guard straddles the fetch: a swap mid-fetch must not persist account A's chat
        // into account B's STATE/DB (the message ids are global).
        let session = state::SessionGuard::capture();
        let Some(my_pk) = state::my_public_key() else { return 0 };
        // CORD-02 §9: a dissolved community honors no NEW events — old history reads
        // through the explicit paths, but a catch-up sweep must not ingest anything
        // authored into the grave.
        if crate::db::community::get_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&id.0)).unwrap_or(false) {
            return 0;
        }
        let Ok(Some(community)) = crate::db::community::load_community_v2(id) else { return 0 };
        let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let Ok(page) = crate::community::v2::service::fetch_channel_history(
            &transport,
            &community,
            &ch,
            limit.max(50),
            MAX_BACKFILL_PAGES,
            // Keep paging while a page still contains a MESSAGE we don't hold; a page
            // whose messages are all known means we've reached our own history. Only
            // message kinds get their own rows (reactions/edits fold into their
            // targets), so a page with no messages is undecidable — keep paging.
            |page| {
                let mut saw_message = false;
                for f in page {
                    if matches!(&f.event, crate::community::v2::chat::ChatEvent::Message { .. }) {
                        saw_message = true;
                        if !crate::db::events::event_exists(&f.event.opened().rumor_id.to_hex()).unwrap_or(false) {
                            return true;
                        }
                    }
                }
                !saw_message
            },
        )
        .await
        else {
            return 0;
        };
        let mut new = 0usize;
        for f in &page {
            // Re-check every iteration — each persists a DB write, and a swap can land
            // between them.
            if !session.is_valid() {
                break;
            }
            // A backfilled WebXDC peer ad persists through the shared 30078 row
            // (recency-gated at read) so a reopening lobby lists peers who
            // advertised while this device was closed — v1 sync parity. Own
            // echoes drop; the ad is not a chat row.
            if let crate::community::v2::chat::ChatEvent::Webxdc { opened } = &f.event {
                if opened.author != my_pk {
                    if let Some((topic, addr)) = crate::webxdc::parse_peer_signal(&opened.rumor.content) {
                        if let Ok(npub) = ToBech32::to_bech32(&opened.author) {
                            crate::community::service::persist_webxdc_signal(
                                channel_id,
                                &npub,
                                &topic,
                                addr.as_deref(),
                                &opened.rumor_id.to_hex(),
                                opened.at_ms / 1000,
                            )
                            .await;
                        }
                    }
                }
                continue;
            }
            // STATE mutation under the lock; the async DB persist after it drops.
            let outcome = {
                let mut st = state::STATE.lock().await;
                apply_chat_to_state(&mut st, &f.event, channel_id, &my_pk)
            };
            if let Some(outcome) = outcome {
                if matches!(outcome, ChatPersist::New(_)) {
                    new += 1;
                }
                persist_chat(channel_id, &outcome).await;
            }
        }
        new
    }

    /// The held v2 community when `community_id` names one; `Ok(None)` for v1 (or
    /// unknown). A DB read error PROPAGATES (fail-closed) instead of falling open
    /// to the v1 route on a transient failure.
    fn load_v2_if_v2(community_id: &str) -> Result<Option<crate::community::v2::community::CommunityV2>> {
        if community_id.len() != 64 {
            return Ok(None);
        }
        let cid = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        match crate::db::community::community_protocol(&cid).map_err(VectorError::Other)? {
            Some(crate::community::ConcordProtocol::V2) => crate::db::community::load_community_v2(&cid).map_err(VectorError::Other),
            _ => Ok(None),
        }
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
    /// confirm a promotion/demotion landed. A local read: the roster is folded + persisted by the passive
    /// sync (v1) / control follow (v2), never fetched here.
    pub fn community_capabilities(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::service;
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            use crate::community::roles::Permissions;
            let me = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?.to_hex();
            let owner_hex = v2.owner().map_err(VectorError::Other)?.to_hex();
            let roster = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
            // A banned member holds no standing (CORD-04 §4), even if a since-skipped
            // roster persist still lists their grant — the banlist advances on its own gate.
            let banned = crate::db::community::get_community_banlist(community_id).unwrap_or_default();
            if banned.contains(&me) && me != owner_hex {
                return Ok(serde_json::json!({
                    "manage_metadata": false, "manage_channels": false, "create_invite": false, "kick": false,
                    "ban": false, "manage_messages": false, "manage_roles": false, "manage_admin_role": false,
                }));
            }
            let has = |p: u64| roster.is_authorized(&me, Some(&owner_hex), p);
            return Ok(serde_json::json!({
                "manage_metadata": has(Permissions::MANAGE_METADATA), "manage_channels": has(Permissions::MANAGE_CHANNELS),
                "create_invite": has(Permissions::CREATE_INVITE), "kick": has(Permissions::KICK), "ban": has(Permissions::BAN),
                "manage_messages": has(Permissions::MANAGE_MESSAGES), "manage_roles": has(Permissions::MANAGE_ROLES),
                // Only the owner (position 0) strictly outranks the position-1 Admin role.
                "manage_admin_role": me == owner_hex,
            }));
        }
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

    /// The community's owner npub + the admin npubs (role overview). A local read,
    /// like [`Self::community_capabilities`].
    pub fn community_roles(&self, community_id: &str) -> Result<serde_json::Value> {
        use nostr_sdk::prelude::{PublicKey, ToBech32};
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            let owner = v2.owner().map_err(VectorError::Other)?;
            let roster = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
            // Exclude banned members from the admin list (a banned npub vanishes, §4).
            let banned = crate::db::community::get_community_banlist(community_id).unwrap_or_default();
            let admins: Vec<String> = roster.grants.iter()
                .filter(|g| roster.is_admin(&g.member) && !banned.contains(&g.member))
                .filter_map(|g| PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()))
                .collect();
            return Ok(serde_json::json!({ "owner": owner.to_bech32().ok(), "admins": admins }));
        }
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
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            return crate::community::v2::service::grant_admin(&transport, &v2, &member)
                .await
                .map_err(VectorError::Other);
        }
        let community = Self::load_community_hex(community_id)?;
        let role_id = Self::admin_role_id_of(community_id)?;
        service::grant_role(&transport, &community, member, &role_id).await.map_err(VectorError::Other)
    }

    /// Revoke a member's @admin role.
    pub async fn revoke_admin(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let member = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            return crate::community::v2::service::revoke_admin(&transport, &v2, &member)
                .await
                .map_err(VectorError::Other);
        }
        let community = Self::load_community_hex(community_id)?;
        let role_id = Self::admin_role_id_of(community_id)?;
        service::revoke_role(&transport, &community, member, &role_id).await.map_err(VectorError::Other)
    }

    /// Cooperatively kick a member — they self-remove but can rejoin. Requires KICK + outrank.
    pub async fn kick_member(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let pk = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            return crate::community::v2::service::kick_member(&transport, &v2, &pk)
                .await
                .map_err(VectorError::Other);
        }
        let community = Self::load_community_hex(community_id)?;
        let channel = community.channels.first().ok_or_else(|| VectorError::Other("community has no channel".into()))?;
        service::publish_kick(&transport, &community, channel, &pk.to_hex()).await.map(|_| ()).map_err(VectorError::Other)
    }

    /// Ban (`true`) or unban (`false`) a member. Ban is terminal (no rejoin); in a private community it also
    /// fires the read-cut rekey (needs a local key). Requires BAN + outrank.
    pub async fn set_member_banned(&self, community_id: &str, npub: &str, banned: bool) -> Result<()> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        let pk = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let hex = pk.to_hex();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        // Recompute the full list (latest-wins): drop any existing entry, then add if banning.
        let mut list = crate::db::community::get_community_banlist(community_id).map_err(VectorError::Other)?;
        list.retain(|h| h != &hex);
        if banned {
            list.push(hex);
        }
        // Dual-stack: a v2 Ban is the CORD-04 §6 three-removal composition, in order —
        // the Banlist edition first (instant silence), then the Grant strip (authority
        // removal), then the Refounding read-cut (cryptographic severance).
        if community_id.len() == 64 {
            let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
            if let Some(Some(crate::community::ConcordProtocol::V2)) = crate::db::community::community_protocol(&cid).ok() {
                let community = crate::db::community::load_community_v2(&cid)
                    .map_err(VectorError::Other)?
                    .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
                crate::community::v2::service::set_banlist(&transport, &community, &list).await.map_err(VectorError::Other)?;
                if banned {
                    crate::community::v2::service::grant_roles(&transport, &community, &pk, vec![]).await.map_err(VectorError::Other)?;
                    crate::community::v2::service::refound_community(&transport, &community, &[pk]).await.map_err(VectorError::Other)?;
                }
                return Ok(());
            }
        }
        let community = Self::load_community_hex(community_id)?;
        service::publish_banlist(&transport, &community, &list).await.map_err(VectorError::Other)
    }

    /// Owner dissolution / "Delete Community": publish the terminal GroupDissolved tombstone (and
    /// retire the owner's own invite links, no rekey), sealing the community permanently. Owner-only
    /// (re-verified cryptographically in `service::dissolve_community`); irreversible.
    pub async fn dissolve_community(&self, community_id: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        // Dual-stack: a v2 community dissolves at its own `community_id`-derived
        // dissolved plane (CORD-02 §9), NOT v1's control-plane roster edition.
        if let Some(Some(crate::community::ConcordProtocol::V2)) = crate::db::community::community_protocol(&cid).ok() {
            let community = crate::db::community::load_community_v2(&cid)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            return crate::community::v2::service::dissolve_community(&transport, &community)
                .await
                .map_err(VectorError::Other);
        }
        let community = Self::load_community_hex(community_id)?;
        service::dissolve_community(&transport, &community).await.map_err(VectorError::Other)
    }

    /// Edit community metadata (name / description) as an authorized member (MANAGE_METADATA). `None` leaves
    /// a field unchanged; an empty description clears it.
    pub async fn edit_community_metadata(&self, community_id: &str, name: Option<&str>, description: Option<&str>) -> Result<()> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        // Dual-stack: a v2 metadata edit is an authorized vsk-0 control edition.
        // Overlay onto the FULL held document (`CommunityV2::metadata()`) — an
        // edition replaces the entity, so a bare name edit would otherwise wipe
        // the icon/banner for every member (CORD-02 §6).
        if community_id.len() == 64 {
            let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
            if let Some(Some(crate::community::ConcordProtocol::V2)) = crate::db::community::community_protocol(&cid).ok() {
                let community = crate::db::community::load_community_v2(&cid)
                    .map_err(VectorError::Other)?
                    .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
                let mut meta = community.metadata();
                if let Some(n) = name {
                    meta.name = n.to_string();
                }
                if let Some(d) = description {
                    meta.description = if d.is_empty() { None } else { Some(d.to_string()) };
                }
                return crate::community::v2::service::edit_community_metadata(&transport, &community, &meta)
                    .await
                    .map_err(VectorError::Other);
            }
        }
        let mut community = Self::load_community_hex(community_id)?;
        if let Some(n) = name { community.name = n.to_string(); }
        if let Some(d) = description { community.description = if d.is_empty() { None } else { Some(d.to_string()) }; }
        service::republish_community_metadata(&transport, &community).await.map_err(VectorError::Other)
    }

    /// Create a new channel in a v2 community. A PUBLIC channel derives from the
    /// community_root, so peers fold it in with nothing to distribute; a PRIVATE one
    /// mints an independent key at channel-epoch 1 and delivers it to every current
    /// member over the rekey plane (CORD-03 §2 / CORD-06). Requires MANAGE_CHANNELS.
    /// Returns the new channel id (hex).
    pub async fn create_community_channel(&self, community_id: &str, name: &str, private: bool) -> Result<String> {
        let v2 = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("channel creation is available on v2 communities".into()))?;
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let id = if private {
            crate::community::v2::service::create_private_channel(&transport, &v2, name).await
        } else {
            crate::community::v2::service::create_public_channel(&transport, &v2, name).await
        }
        .map_err(VectorError::Other)?;
        // Subscribe the new channel's chat plane now — waiting on the round-trip of
        // our own vsk-2 edition would leave the creator deaf to first replies.
        if let Some(client) = state::nostr_client() {
            crate::community::v2::realtime::refresh_subscription(&client).await;
        }
        Ok(crate::simd::hex::bytes_to_hex_32(&id.0))
    }

    /// Delete (tombstone) a v2 community channel. Requires MANAGE_CHANNELS (reader-gated).
    pub async fn delete_community_channel(&self, community_id: &str, channel_id: &str) -> Result<()> {
        let v2 = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("channel deletion is available on v2 communities".into()))?;
        let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let name = v2.channels.iter().find(|c| c.id.0 == ch.0).map(|c| c.name.clone()).unwrap_or_default();
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        crate::community::v2::service::delete_channel(&transport, &v2, &ch, &name)
            .await
            .map_err(VectorError::Other)
    }

    /// Leave a Community: announce a best-effort "left" presence (before dropping keys), then
    /// drop the held keys + local channel chats. You need a fresh invite to rejoin.
    pub async fn leave_community(&self, community_id: &str) -> Result<()> {
        use crate::community::{transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let id = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        // v2: guestbook Leave + cross-device List tombstone + local delete, in the service.
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            let session = state::SessionGuard::capture();
            let channel_ids: Vec<String> =
                v2.channels.iter().map(|ch| crate::simd::hex::bytes_to_hex_32(&ch.id.0)).collect();
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            crate::community::v2::service::leave_community(&transport, &v2)
                .await
                .map_err(VectorError::Other)?;
            if !session.is_valid() {
                return Err(VectorError::Other("account changed during leave".into()));
            }
            let mut st = state::STATE.lock().await;
            st.chats.retain(|c| !channel_ids.contains(&c.id));
            return Ok(());
        }
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

    /// Catch up every locally-held Community: fold control / re-foundings / rekeys / banlist and
    /// fetch recent messages into local state for each channel. State-only (does not replay to an
    /// [`InboundEventHandler`]). Called at `listen()` start and periodically for outage resilience;
    /// also safe to call manually after a known disconnect.
    ///
    /// Catch up every locally-held Community. v1 channels are synced inline; a v2
    /// community is ENQUEUED for the follow worker (control/rekey re-fold + adopt),
    /// non-blocking. State-only (no handler replay of history). Called at `listen()`
    /// start and on reconnect; safe to call manually — the v2 enqueue is a no-op if
    /// no `listen()` worker is running.
    pub async fn sync_communities(&self) -> Result<()> {
        // Discover + rehydrate memberships from the 13302 across devices (CORD-02 §8),
        // bootstrapping from the client's connected relays so even a fresh device that
        // holds no community yet can find them. Best-effort.
        {
            use crate::community::{transport::LiveTransport, v2::service as v2};
            let bootstrap: Vec<String> = match crate::state::nostr_client() {
                Some(client) => client.relays().await.keys().map(|r| r.to_string()).collect(),
                None => Vec::new(),
            };
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            if let Ok(joined) = v2::sync_community_list(&transport, &bootstrap).await {
                for c in &joined {
                    if community::v2::realtime::follow_worker_running() {
                        community::v2::realtime::enqueue_follow(c.id());
                    } else {
                        let _ = Self::v2_inline_follow(c.id()).await;
                    }
                }
                if !joined.is_empty() {
                    if let Some(client) = crate::state::nostr_client() {
                        community::v2::realtime::refresh_subscription(&client).await;
                    }
                }
            }
        }

        let ids = db::community::list_community_ids().map_err(VectorError::from)?;
        for id in ids {
            if matches!(db::community::community_protocol(&id).ok().flatten(), Some(crate::community::ConcordProtocol::V2)) {
                // With a live listen() the coalescing worker owns the follow; headless
                // (no worker) it would be dropped, so walk it inline instead.
                if community::v2::realtime::follow_worker_running() {
                    community::v2::realtime::enqueue_follow(&id);
                } else {
                    let _ = Self::v2_inline_follow(&id).await;
                }
                continue;
            }
            if let Ok(Some(community)) = db::community::load_community(&id) {
                for ch in &community.channels {
                    let _ = self.sync_community_channel(&ch.id.to_hex(), 50).await;
                }
            }
        }
        Ok(())
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

        // Start the stream-AUTH responder BEFORE any relay interaction: a gating
        // relay issues its NIP-42 challenge ONCE per connection, and the DM
        // subscribe below consumes it via nostr-sdk's user auto-auth — if the
        // responder isn't already watching, that challenge is never remembered
        // and the stream keys registered later can NEVER authenticate (the relay
        // won't re-challenge an authed connection; the v2 sub dies silently).
        community::v2::streamauth::ensure_responder(&client);

        // Outage resilience — catch up on connect, then re-sync periodically.
        //
        // Catch up BEFORE going realtime so a bot that was offline folds any missed re-foundings /
        // metadata / banlist changes (and recent messages) into local state, and subscribes at the
        // CURRENT epoch pseudonyms. This is state-only: historical messages are not replayed to the
        // handler (matches the gateway model) — query them via `get_messages`.
        // Spawn the single per-community follow worker for this session; the v2
        // follow queue (fed by dispatch, catch-up, and sync) drains through it.
        community::v2::realtime::spawn_follow_worker(handler.clone());
        let _ = self.sync_communities().await;
        let _ = self.sync_dms(None, &NoOpEventHandler).await;

        // Subscribe to DMs (GiftWraps) AND Community channel events — one loop dispatches both
        // through the same handler, so `on_dm_received`/`on_community_message` share a sink.
        let dm_sub_id = self.subscribe_dms().await?;
        community::realtime::refresh_subscription(&client).await;
        community::v2::realtime::refresh_subscription(&client).await;

        // Outage resilience via the relay Monitor — event-driven, not polling.
        //
        // (1) Reconnect-driven catch-up: a `limit(0)` realtime sub never replays what was published
        // while we were down, so a relay (re)connecting is exactly when we must catch up. On each
        // Connected transition we refold consensus + reconcile DMs (NIP-77 negentropy → only the
        // diff) and re-track the realtime sub at the current epochs. Idle when healthy. Stops on swap.
        if let Some(monitor) = client.monitor() {
            let mut rx = monitor.subscribe();
            let session = state::SessionGuard::capture();
            tokio::spawn(async move {
                // Debounce reconnect bursts: StatusChanged is per-relay, but one catch-up queries the
                // whole pool — so coalesce Connected transitions within a short window into one resync.
                let mut last_resync: Option<std::time::Instant> = None;
                while let Ok(notification) = rx.recv().await {
                    if !session.is_valid() {
                        return;
                    }
                    let MonitorNotification::StatusChanged { status, .. } = notification;
                    if status == RelayStatus::Connected {
                        if last_resync.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3)) {
                            continue;
                        }
                        let _ = VectorCore.sync_communities().await;
                        let _ = VectorCore.sync_dms(None, &NoOpEventHandler).await;
                        if let Some(c) = state::nostr_client() {
                            community::realtime::refresh_subscription(&c).await;
                            community::v2::realtime::refresh_subscription(&c).await;
                        }
                        last_resync = Some(std::time::Instant::now());
                    }
                }
            });
        }

        // (2) Health probe: a relay can report Connected while silently dead. Every 60s probe each
        // with a tiny query + timeout; a zombie is force-reconnected (which fires the monitor above
        // → catch-up), and Disconnected/Terminated relays are reconnected directly.
        {
            let client_health = client.clone();
            let session = state::SessionGuard::capture();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await; // warm-up
                loop {
                    if !session.is_valid() {
                        return;
                    }
                    for (url, relay) in client_health.relays().await {
                        match relay.status() {
                            RelayStatus::Connected => {
                                let probe = tokio::time::timeout(
                                    std::time::Duration::from_secs(10),
                                    client_health.fetch_events_from(
                                        vec![url.to_string()],
                                        Filter::new().kind(Kind::Metadata).limit(1),
                                        std::time::Duration::from_secs(8),
                                    ),
                                )
                                .await;
                                if !matches!(probe, Ok(Ok(_))) {
                                    let _ = relay.disconnect();
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                    let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                                }
                            }
                            RelayStatus::Terminated | RelayStatus::Disconnected => {
                                let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                            }
                            _ => {}
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            });
        }

        let client_for_closure = client.clone();

        client.handle_notifications(move |notification| {
            let handler = handler.clone();
            let c = client_for_closure.clone();
            let dm_sid = dm_sub_id.clone();
            async move {
                // Relay OKs feed the send pipeline: an OK that outlives the
                // per-attempt wait still confirms delivery, and can rescue a
                // message already marked Failed.
                if let RelayPoolNotification::Message {
                    message: nostr_sdk::RelayMessage::Ok { event_id, status, .. }, ..
                } = &notification {
                    sending::note_relay_ok(event_id, *status);
                }
                if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                    if subscription_id == dm_sid {
                        // DMs, files, reactions
                        let prepared = event_handler::prepare_event(*event, &c, my_pk).await;
                        event_handler::commit_prepared_event(prepared, true, &*handler).await;
                    } else if community::realtime::subscription_id().await.as_ref() == Some(&subscription_id)
                        || community::realtime::poolwide_subscription_id().await.as_ref() == Some(&subscription_id)
                    {
                        // Community (v1) channel messages / reactions / edits / control editions.
                        // OR the pool-wide sub (the path that streams on Android) — else v1 events
                        // arriving under it match no branch and are silently dropped.
                        let session = state::SessionGuard::capture();
                        community::realtime::dispatch_event(&session, *event, handler.clone()).await;
                    } else if community::v2::realtime::subscription_id().await.as_ref() == Some(&subscription_id)
                        || community::v2::realtime::poolwide_subscription_id().await.as_ref() == Some(&subscription_id)
                    {
                        // Concord v2 plane events (authors-addressed kind-1059/21059).
                        let session = state::SessionGuard::capture();
                        community::v2::realtime::dispatch_event(&session, *event, handler.clone()).await;
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
            if let Ok(mut g) = state::MNEMONIC_SEED.lock() {
                if let Some(s) = g.as_mut() { s.zeroize(); }
                *g = None;
            }
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
        // In-flight wrap confirmations carry the prior account's chat and
        // message ids — a late OK must not "rescue" into the new session.
        crate::sending::clear_wrap_confirms();
        crate::emoji_packs::clear_nip65_cache();
        // Chat/user row-id caches are PER-ACCOUNT (row ids belong to the prior account's DB). Not clearing
        // them here let a swapped-in account resolve a channel/npub to the WRONG (prior-account) row id →
        // saves FK-failed silently + reads hit the wrong row (e.g. a community member vanished post-swap).
        crate::db::clear_id_caches();
        // Community sync RAM cache (page cursors, history-start, in-flight, invite preload) is
        // account-scoped — drop it so the next account can't read A's cursors/warmed pages. The
        // generation stamp self-invalidates too, but clear explicitly for parity with the GUI swap.
        crate::community::cache::clear();
        // Community realtime route/subscription state is account-scoped (channel keys + banned sets);
        // drop it so a swapped-in account can't listen on the prior account's pseudonyms.
        crate::community::realtime::clear().await;
        crate::community::v2::realtime::clear().await;
        // Theme-pack emoji tags are account-scoped; leaving the prior account's set active would tag the
        // next account's outbound messages with A's theme shortcodes (leaking A's pack Blossom URLs). The
        // frontend re-registers the new account's theme, but only if it HAS one — clear to be safe.
        crate::emoji_packs::set_theme_emoji_tags(Vec::new());
    }
}

#[cfg(test)]
mod facade_tests {
    use super::*;

    /// SSRF regression: `download_attachment` must reject a private/link-local URL via
    /// `validate_url_not_private` BEFORE any network fetch (the URL is attacker-controlled).
    #[tokio::test]
    async fn download_attachment_rejects_private_url() {
        let att = crate::types::Attachment {
            url: "http://169.254.169.254/latest/meta-data/".to_string(),
            ..Default::default()
        };
        match VectorCore.download_attachment(&att).await {
            Err(VectorError::Other(msg)) => {
                assert!(msg.contains("Private/internal"), "expected SSRF rejection, got: {msg}")
            }
            other => panic!("expected SSRF rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_attachment_rejects_empty_url() {
        let att = crate::types::Attachment::default();
        assert!(VectorCore.download_attachment(&att).await.is_err());
    }

    /// The facade dual-stack dispatch: a v2 community surfaces in `list_communities`
    /// with `version: 2`, and `v2_community_for_channel` routes its channels to the
    /// v2 send path — while a v1 community is untouched (version 1).
    #[tokio::test]
    async fn list_communities_and_channel_routing_are_protocol_aware() {
        use crate::community::transport::memory::MemoryRelay;
        use nostr_sdk::prelude::Keys;

        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        // A valid bech32-charset, npub-length account dir name.
        let acct = {
            const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
            let mut s = String::from("npub1");
            for i in 0..58 {
                s.push(B[(i * 7 + 3) % 32] as char);
            }
            s
        };
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());

        // Create a v2 community directly through the v2 service (offline).
        let relay = MemoryRelay::new();
        let community = crate::community::v2::service::create_community(&relay, "V2 Guild", vec!["wss://r".into()], None)
            .await
            .unwrap();
        let channel_hex = crate::simd::hex::bytes_to_hex_32(&community.channels[0].id.0);

        // The facade lists it as version 2, owned by me.
        let listed = VectorCore.list_communities().await;
        let v2 = listed.iter().find(|c| c["version"] == 2).expect("the v2 community is listed");
        assert_eq!(v2["name"], "V2 Guild");
        assert_eq!(v2["is_owner"], true);
        assert_eq!(v2["channels"][0]["channel_id"], channel_hex);

        // The channel routes to the v2 send path.
        assert_eq!(
            VectorCore.v2_community_for_channel(&channel_hex).unwrap(),
            Some(community.identity.community_id),
            "a v2 channel is routed to v2"
        );
        // An unknown channel routes nowhere (would fall through to v1).
        assert_eq!(VectorCore.v2_community_for_channel(&"00".repeat(32)).unwrap(), None);
    }

    /// The facade builds a v2 invite URL by trimming `/invite` off the v1
    /// constant (v2's `build_invite_url` re-appends its own `/invite/<naddr>`).
    /// Lock that the derived URL is v2-shaped and round-trips through the v2
    /// parser — a stale constant or a double-`/invite` would silently break joins.
    #[test]
    fn v2_invite_url_base_derivation_round_trips() {
        use crate::community::v2::derive::TOKEN_LEN;
        use crate::community::v2::invite::{build_invite_url, parse_invite_link};
        use nostr_sdk::prelude::Keys;
        let base = crate::community::public_invite::INVITE_URL_BASE.trim_end_matches("/invite");
        assert!(!base.ends_with("/invite"), "the bare domain must not carry /invite");
        let signer = Keys::generate();
        let token = [0x07u8; TOKEN_LEN];
        let url = build_invite_url(base, &signer.public_key(), &token, &[]).unwrap();
        assert!(url.contains("/invite/"), "a v2 URL carries the naddr path");
        assert!(!url.contains("/invite/invite/"), "no doubled /invite from the base");
        let parsed = parse_invite_link(&url).unwrap();
        assert_eq!(parsed.link_signer, signer.public_key());
        assert_eq!(parsed.token, token);
    }
}
