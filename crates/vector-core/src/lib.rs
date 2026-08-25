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
pub mod logging;
pub mod error;
pub mod traits;

// Nostr SDK trait imports needed for bech32 operations
use crate::event_ext::FinalizeUnsignedWithId;
use nostr_sdk::prelude::{FinalizeEventAsync, ToBech32};

// === Core Types ===
pub mod event_ext;
pub mod tags;
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

// === NIP-55 offline signer (on-device Amber over Android IPC) ===
pub mod nip55;

// === Database ===
pub mod db;
/// The "every task carries its account" check, run by this crate's suite and
/// by every crate that spawns per-account work against vector-core.
pub mod spawn_audit;

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

/// NIP-42 authenticator.
///
/// Many Concord/Armada communities live on AUTH-gating relays (Ditto's default
/// gates kind-1059), where an unauthenticated client silently reads back ZERO
/// events — a join's control-plane verify then fails closed and every community
/// fetch comes up empty. Registering this is what unlocks those reads; a relay
/// that doesn't challenge is unaffected.
///
/// The signer is resolved per challenge rather than captured at client
/// construction, so a bunker that connects later (or an account swap) authks
/// under the identity that is live *now*.
#[derive(Debug)]
pub struct VectorAuthenticator;

impl nostr_sdk::prelude::Authenticator for VectorAuthenticator {
    fn make_auth_event<'a>(
        &'a self,
        relay_url: &'a nostr_sdk::prelude::RelayUrl,
        challenge: &'a str,
    ) -> signer::BoxedFuture<'a, std::result::Result<nostr_sdk::prelude::Event, nostr_sdk::prelude::Error>>
    {
        Box::pin(async move {
            let signer =
                signer::active_signer().map_err(nostr_sdk::prelude::Error::other)?;
            Ok(
                nostr_sdk::prelude::ClientAuthentication::new(challenge, relay_url.clone())
                    .finalize_async(&signer)
                    .await?,
            )
        })
    }
}

/// A `ClientBuilder` carrying Vector's client-wide policy: NIP-42 auth plus the
/// embedded-Tor SOCKS proxy.
///
/// Callers should start from this rather than `ClientBuilder::new()` so both come
/// along automatically.
///
/// The proxy is a closure, not a fixed address: nostr resolves it per connection
/// attempt, so it reads the *current* Tor state. That covers relays added later
/// in the session, which previously needed the transport re-applied per
/// `add_relay`.
pub fn nostr_client_builder() -> nostr_sdk::prelude::ClientBuilder {
    apply_tor_proxy(
        nostr_sdk::prelude::ClientBuilder::new()
            .authenticator(VectorAuthenticator)
            // The pool's own attempts need the Tor floor too, not just our explicit
            // `try_connect` calls (0.45 default is 15s — under a circuit build).
            .connect_timeout(relay_connect_timeout(std::time::Duration::from_secs(15))),
    )
}

/// Register a relay with the pool's own auto-reconnect disabled.
///
/// Vector drives every reconnect from its reconcile loop, because it needs the
/// connection lifecycle to be observable and to sequence with health checks and
/// the Tor transport switch. The pool's retry is invisible to all of that, so two
/// schedules end up fighting over one socket.
///
/// This exists as a helper rather than a per-call `.reconnect(false)` because
/// `reconnect` is the one relay option `ClientBuilder` cannot default: it lives
/// only on `RelayOptions`, so every registration site has to opt out by hand, and
/// most of them silently didn't.
pub trait ClientRelayExt {
    /// `Client::add_relay` with `reconnect(false)` already applied.
    fn add_managed_relay<'client, 'url, U>(
        &'client self,
        url: U,
    ) -> nostr_sdk::prelude::AddRelay<'client, 'url>
    where
        U: Into<nostr_sdk::prelude::RelayUrlArg<'url>>;
}

impl ClientRelayExt for nostr_sdk::prelude::Client {
    fn add_managed_relay<'client, 'url, U>(
        &'client self,
        url: U,
    ) -> nostr_sdk::prelude::AddRelay<'client, 'url>
    where
        U: Into<nostr_sdk::prelude::RelayUrlArg<'url>>,
    {
        self.add_relay(url).reconnect(false)
    }
}

/// Re-send every live subscription this relay is supposed to carry, after VECTOR
/// reconnected it.
///
/// Vector owns every reconnect (`add_managed_relay` ⇒ `reconnect(false)`), and the
/// pool re-applies live subs only inside its own retry path — which is exactly the
/// path that was turned off. So a dropped socket comes back carrying NOTHING, and a
/// catch-up fetch is not a subscription: the data lands once and then the stream is
/// silent forever. Only an AUTH-gating relay healed, via its post-challenge re-send.
///
/// Driven off the pool's own subscription table rather than a list of known
/// subscriptions, so a future subscription is covered without touching this. Only
/// ids the pool already associates with `relay` are re-sent, so a relay-targeted
/// subscription is never widened onto a relay it deliberately excluded. Same-id
/// REQs are idempotent.
pub async fn resubscribe_relay_after_reconnect(
    client: &nostr_sdk::prelude::Client,
    relay: &nostr_sdk::prelude::RelayUrl,
) {
    for (id, per_relay) in client.subscriptions().await {
        let Some(filters) = per_relay.get(relay) else { continue };
        if filters.is_empty() {
            continue;
        }
        let _ = client
            .subscribe(nostr_sdk::prelude::ReqTarget::single(relay.clone(), filters.clone()))
            .with_id(id)
            .await;
    }
}

/// Minimum a relay connect attempt gets while Tor is on.
///
/// Circuit construction dominates the handshake and routinely runs tens of
/// seconds, especially on the first connection after the toggle.
#[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
const TOR_RELAY_CONNECT_FLOOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Adjust a relay connect budget for the transport actually in use.
///
/// Clearnet TCP+TLS settles in well under a second, so the tight per-call budgets
/// are right there and each caller's intent is preserved. Under Tor those same
/// budgets expire mid-circuit, and because the health-check and reconcile loops
/// treat a timeout as "unhealthy" they call `disconnect()` — which terminates the
/// connection task — then retry, so a relay churns `pending → terminated` forever
/// and never connects. Raising the floor lets the circuit finish.
pub fn relay_connect_timeout(clearnet: std::time::Duration) -> std::time::Duration {
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    {
        if !matches!(tor::transport_state(), tor::TorTransportState::Disabled) {
            return clearnet.max(TOR_RELAY_CONNECT_FLOOR);
        }
    }
    clearnet
}

/// Floor for a relay round-trip (request → response) while Tor is active.
#[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
const TOR_RELAY_REQUEST_FLOOR: std::time::Duration = std::time::Duration::from_secs(30);

/// Adjust a relay request budget for the transport actually in use.
///
/// Companion to [`relay_connect_timeout`] for round trips rather than connections.
/// A relay that answers a probe in 200ms direct can take many seconds through three
/// hops, so a clearnet-sized budget reads a healthy relay as dead.
pub fn relay_request_timeout(clearnet: std::time::Duration) -> std::time::Duration {
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    {
        if !matches!(tor::transport_state(), tor::TorTransportState::Disabled) {
            return clearnet.max(TOR_RELAY_REQUEST_FLOOR);
        }
    }
    clearnet
}

/// Apply the Tor proxy policy to any `ClientBuilder`.
///
/// Separate from [`nostr_client_builder`] because a client that authenticates as
/// something other than the user (the Concord stream-auth plane key) still needs
/// the same transport: without it the plane fetch connects direct and ties the
/// user's IP to community membership.
pub fn apply_tor_proxy(
    builder: nostr_sdk::prelude::ClientBuilder,
) -> nostr_sdk::prelude::ClientBuilder {
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    let builder = builder.proxy(nostr_sdk::prelude::Proxy::custom(|_url| tor_proxy_target()));
    builder
}

/// Resolve the proxy every connection attempt must use, for the transport in use.
///
/// Named rather than inlined into the `Proxy::custom` closure so the failsafe is
/// testable: returning `None` here means "connect direct", so the only leak-safe
/// answer while Tor is the chosen transport but not yet up is the blackhole.
#[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
fn tor_proxy_target() -> Option<std::net::SocketAddr> {
    match tor::transport_state() {
        tor::TorTransportState::Active(addr) => Some(addr),
        // Tor failsafe: route to a blackhole so a relay socket can't come up
        // direct while Tor is mid-bootstrap.
        tor::TorTransportState::RequiredButInactive => Some(tor::blackhole_proxy_addr()),
        tor::TorTransportState::Disabled => None,
    }
}

/// Sign an `EventBuilder` with the session signer.
///
/// Stands in for 0.44's `Client::sign_event_builder`, which went away when the
/// client stopped owning a signer.
pub async fn sign_builder(
    builder: nostr_sdk::prelude::EventBuilder,
) -> std::result::Result<nostr_sdk::prelude::Event, String> {
    let signer = signer::active_signer()?;
    builder
        .finalize_async(&signer)
        .await
        .map_err(|e| e.to_string())
}

/// Sign an `EventBuilder` with the session signer and publish it.
///
/// Stands in for 0.44's `Client::send_event_builder`.
pub async fn sign_and_send(
    client: &nostr_sdk::prelude::Client,
    builder: nostr_sdk::prelude::EventBuilder,
) -> std::result::Result<nostr_sdk::prelude::SendEventOutput, String> {
    let event = sign_builder(builder).await?;
    client
        .send_event(&event)
        .await
        .map_err(|e| e.to_string())
}

/// Seal, wrap and publish a rumor to `receiver`.
///
/// Stands in for 0.44's `Client::gift_wrap` / `gift_wrap_to`, which went away
/// with the client's signer. An empty `relays` publishes pool-wide, matching
/// `gift_wrap`; a non-empty one targets those relays, matching `gift_wrap_to`.
pub async fn send_gift_wrap<'u, I, U, T>(
    client: &nostr_sdk::prelude::Client,
    relays: I,
    receiver: &nostr_sdk::prelude::PublicKey,
    rumor: nostr_sdk::prelude::UnsignedEvent,
    extra_tags: T,
) -> std::result::Result<nostr_sdk::prelude::SendEventOutput, String>
where
    I: IntoIterator<Item = U>,
    U: Into<nostr_sdk::prelude::RelayUrlArg<'u>>,
    T: IntoIterator<Item = nostr_sdk::prelude::Tag>,
{
    let signer = signer::active_signer()?;
    let wrap = nostr_sdk::prelude::GiftWrapBuilder::new(*receiver, rumor)
        .extra_tags(extra_tags)
        .finalize_async(&signer)
        .await
        .map_err(|e| e.to_string())?;
    let targets: Vec<nostr_sdk::prelude::RelayUrlArg<'u>> =
        relays.into_iter().map(Into::into).collect();
    if targets.is_empty() {
        client.send_event(&wrap).await.map_err(|e| e.to_string())
    } else {
        client
            .send_event(&wrap)
            .to(targets)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Capabilities for a Community / "external" relay: GOSSIP only.
///
/// GOSSIP is read/write-capable when TARGETED — `can_read()` is
/// `READ|GOSSIP|DISCOVERY` and `can_write()` is `WRITE|GOSSIP`, so per-relay
/// targeted ops pass. But pool-wide ops select READ-only / WRITE-only relays, so
/// the DM/giftwrap subscription and the user's outbox skip GOSSIP relays — the
/// user's own traffic never touches relays they don't own.
///
/// No PING counterpart any more: 0.45 demoted PING from a capability flag to a
/// per-relay option (`AddRelay::ping`) that already defaults to true, with
/// `sleep_when_idle` defaulting to false. The 24/7 keepalive this used to buy is
/// now the default, so it doesn't belong in the capability set.
pub fn community_relay_capabilities() -> nostr_sdk::prelude::RelayCapabilities {
    nostr_sdk::prelude::RelayCapabilities::GOSSIP
}

/// Relay options for a Discovery Relay (see `state::DISCOVERY_RELAYS`): the same
/// GOSSIP|PING targeted-only isolation as Community relays — reachable via
/// `fetch_events_from` / `send_event_to`, invisible to pool-wide DM/profile ops.
/// An overlap with a user relay keeps the user's READ+WRITE flags (`add_relay`
/// no-ops on an already-pooled url).
pub fn discovery_relay_capabilities() -> nostr_sdk::prelude::RelayCapabilities {
    community_relay_capabilities()
}

// === Event Storage ===
pub mod stored_event;

// === Rumor Processing ===
pub mod rumor;

// === Messaging ===
pub mod sending;

// === Per-DM Wallpapers ===
pub mod pinned_chats;
pub mod synced_prefs;
pub mod wallpaper;

// === Message Deletion (NIP-09 against retained gift-wraps) ===
pub mod deletion;
pub mod self_destruct;

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
    ChatState, MY_SECRET_KEY, STATE, ENCRYPTION_KEY,
    nostr_client, my_public_key, has_active_session,
    set_nostr_client, set_my_public_key,
    take_nostr_client, clear_my_public_key,
    set_pending_bunker_setup, pending_bunker_setup, clear_pending_bunker_setup,
    set_pending_nip55_setup, pending_nip55_setup, clear_pending_nip55_setup,
};
pub use crypto::{GuardedKey, GuardedSigner};
pub use signer::{
    SignerKind, signer_kind, set_signer_kind, is_bunker, is_keyless,
    BUNKER_SIGNER, bunker_signer, set_bunker_signer, take_bunker_signer,
    build_bunker_signer, prewarm_bunker, drain_bunker_state,
    parse_bunker_remote_pubkey, parse_bunker_relays,
    BunkerConnectionState, bunker_state, set_bunker_state,
    VectorAuthUrlHandler, attempt_bunker_login, WatchedBunkerSigner,
    vector_metadata, build_nostrconnect_uri, build_nostrconnect_session,
    VECTOR_APP_NAME, VECTOR_APP_URL, VECTOR_APP_ICON,
};
pub use nip55::{
    Nip55Backend, Nip55Error, Nip55ResolverOutcome, Nip55Signer, Nip55State,
    set_nip55_backend, nip55_backend, nip55_state, set_nip55_state, drain_nip55_state,
    nip55_is_installed, nip55_pair, nip55_perms_json,
    VECTOR_NIP55_SIGN_KINDS, VECTOR_NIP55_ENCRYPT_TYPES,
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

/// What one catch-up page cost and what it yielded.
///
/// `fetched` is the relay's answer — zero means there is genuinely nothing
/// further back, which is the only sound "reached the start" signal.
/// `new_messages` counts what this client had never seen, which is what a UI
/// reports; a page of entirely-known history is 0 new with a non-empty fetch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillCount {
    pub fetched: usize,
    pub new_messages: usize,
}

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
        let client = crate::nostr_client_builder()
            // Relay health monitor — powers the reconnect-driven catch-up in `listen()`.
            .monitor(Monitor::new(1024))
            .build();

        // Add trusted relays
        for relay in state::TRUSTED_RELAYS {
            client.add_managed_relay(*relay).await.ok();
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
    /// attachment's embedded key + nonce. Walks the primary URL then any BUD-04 `fallback`
    /// mirrors (same ciphertext on other hosts) until one serves. Prefer
    /// [`download_attachment_from`](Self::download_attachment_from) when the message author is
    /// known — it adds the BUD-03 hash-swap over the author's advertised servers.
    pub async fn download_attachment(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        self.download_attachment_from(attachment, None).await
    }

    /// [`download_attachment`](Self::download_attachment) with the full source walk: primary URL →
    /// embedded `fallback` mirrors → BUD-03 hash-swap (the same content-address on each of the
    /// author's kind-10063 servers). `author_npub` is the message author (your own npub for your
    /// own messages); `None` skips the hash-swap stage.
    pub async fn download_attachment_from(
        &self,
        attachment: &Attachment,
        author_npub: Option<&str>,
    ) -> Result<Vec<u8>> {
        use futures_util::StreamExt;
        const MAX_DOWNLOAD: usize = 256 * 1024 * 1024;
        if attachment.url.is_empty() {
            return Err(VectorError::Other("attachment has no URL".into()));
        }
        let client = crate::net::build_http_client(std::time::Duration::from_secs(120)).map_err(VectorError::Other)?;
        let mut last_err = String::from("download failed");
        let mut candidates: Vec<String> = vec![attachment.url.clone()];
        candidates.extend(attachment.fallback_urls.iter().cloned());
        let mut hash_swap_tried = false;
        let mut i = 0;
        'sources: while i < candidates.len() {
            let url = candidates[i].clone();
            i += 1;
            // One-time last resort once every embedded source has failed: the author's advertised
            // servers may hold the blob under the same content-address.
            let extend_with_swap = |candidates: &mut Vec<String>, servers: &[String]| {
                let extra = crate::blossom::hash_swap_candidates(&attachment.url, servers);
                for c in extra {
                    if !candidates.contains(&c) {
                        candidates.push(c);
                    }
                }
            };
            macro_rules! next_source {
                () => {{
                    log_net_fail!("[Download] source failed ({}): {}", url, last_err);
                    if i == candidates.len() && !hash_swap_tried {
                        hash_swap_tried = true;
                        let servers = crate::blossom_servers::author_swap_servers(author_npub, false).await;
                        extend_with_swap(&mut candidates, &servers);
                    }
                    continue 'sources;
                }};
            }
            // SSRF guard: URLs are attacker-controlled (off an inbound message). build_http_client
            // only validates redirect HOPS, not the initial request — so validate each source here
            // (matches the native download path). With Tor off this is the only egress guard.
            if let Err(e) = crate::net::validate_url_not_private(&url) {
                last_err = e.to_string();
                next_source!();
            }
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("download: {e}");
                    next_source!();
                }
            };
            if !resp.status().is_success() {
                last_err = format!("download failed: HTTP {}", resp.status());
                next_source!();
            }
            // Stream with a cap so a hostile/oversized blob can't OOM the process. The cap is
            // permanent — every mirror serves the same blob, so don't bother trying the next.
            let mut encrypted: Vec<u8> = Vec::with_capacity(
                resp.content_length().map(|l| (l as usize).min(MAX_DOWNLOAD)).unwrap_or(64 * 1024),
            );
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        last_err = format!("read body: {e}");
                        next_source!();
                    }
                };
                if encrypted.len() + chunk.len() > MAX_DOWNLOAD {
                    return Err(VectorError::Other("attachment exceeds 256 MiB cap".into()));
                }
                encrypted.extend_from_slice(&chunk);
            }
            match crate::crypto::decrypt_data(&encrypted, &attachment.key, &attachment.nonce) {
                Ok(plain) => {
                    if i > 1 {
                        log_net_info!("[Download] fallback source {}/{} served {}", i, candidates.len(), url);
                    }
                    return Ok(plain);
                }
                Err(e) => {
                    // A host serving wrong bytes under the right URL must not
                    // veto sources still holding the real ciphertext.
                    last_err = format!("decrypt: {e}");
                    next_source!();
                }
            }
        }
        log_net_fail!("[Download] all {} source(s) failed for {}: {}", candidates.len(), attachment.url, last_err);
        Err(VectorError::Other(last_err))
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

    /// The id of our own reaction to `message_id` with `emoji`, if we already have
    /// one. A reaction set is keyed by (author, emoji), so a repeat is something
    /// every receiver collapses anyway: refusing it here spends no publish, no
    /// gift-wrap and no relay round-trip on an event that changes nothing.
    async fn own_reaction_id(message_id: &str, emoji: &str) -> Option<String> {
        use nostr_sdk::prelude::ToBech32;
        let me = state::my_public_key()?.to_bech32().ok()?;
        let st = state::STATE.lock().await;
        let (_, message) = st.find_message(message_id)?;
        message
            .reactions
            .iter()
            .find(|r| r.author_id == me && r.emoji == emoji)
            .map(|r| r.id.clone())
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

        // Already ours: hand back the reaction we hold rather than minting a second.
        if let Some(existing) = Self::own_reaction_id(reference_id, emoji).await {
            return Ok(existing);
        }

        // Group ceiling + per-tier fresh-reaction allowance (joins always pass)
        badges::check_new_reaction_allowance(reference_id, emoji)
            .await
            .map_err(VectorError::Other)?;

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
            Some(Tag::custom("emoji", [shortcode.to_string(), url.to_string()]))
        });

        let reaction_target = nostr_sdk::prelude::nip25::ReactionTarget {
            event_id: reference_event,
            public_key: receiver_pubkey,
            coordinate: None,
            kind: Some(Kind::PrivateDirectMessage),
            relay_hint: None,
        };
        let mut builder =
            nostr_sdk::prelude::nip25::ReactionBuilder::new(reaction_target, emoji)
                .into_event_builder();
        if let Some(tag) = custom_emoji_tag {
            builder = builder.tag(tag);
        }
        let rumor = builder.finalize_unsigned_with_id(my_public_key);
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
        db::spawn_bound(async move {
            if let Ok(self_outcome) = inbox_relays::send_gift_wrap_retained(
                &self_wrap_client, &my_public_key, rumor, [],
            ).await {
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
        if let Some((cid, mut msg)) = msg_for_save {
            let _ = db::events::save_message(&cid, &msg).await;
            traits::emit_message_update(&cid, reference_id, &mut msg).await;
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
            .tag(Tag::custom("d", vec!["vector"]))
            .tag(Tag::expiration(expiry))
            .finalize_unsigned_with_id(my_public_key);

        // Client no longer wraps: build the wrap, then publish it to the target relays.
        let signer = signer::active_signer().map_err(VectorError::Other)?;
        let wrap = nostr_sdk::prelude::GiftWrapBuilder::new(pubkey, rumor.clone())
            .extra_tags([Tag::expiration(expiry)])
            .finalize_async(&signer)
            .await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        client
            .send_event(&wrap)
            .to(state::active_trusted_relays().await)
            .await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        Ok(())
    }

    /// Edit a DM you previously sent (kind-16 edit) with an optimistic local
    /// echo. Returns the edit event id. Persistence is best-effort and only
    /// happens when the chat already exists locally.
    pub async fn edit_dm(&self, to_npub: &str, message_id: &str, new_content: &str) -> Result<String> {
        crate::db::scoped(async move {
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
                    "emoji",
                    [et.shortcode.clone(), et.url.clone()],
                ));
            }
            let rumor = builder.finalize_unsigned_with_id(my_public_key);
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
            if let Some(mut msg) = msg_for_emit {
                traits::emit_message_update(to_npub, message_id, &mut msg).await;
                if let Ok(db_chat_id) = db::id_cache::get_chat_id_by_identifier(to_npub) {
                    let _ = db::events::save_edit_event(
                        &edit_id, message_id, new_content, &emoji_tags, db_chat_id, None, &my_npub,
                    ).await;
                }
            }

            inbox_relays::send_gift_wrap(&client, &receiver_pubkey, rumor.clone(), [])
                .await.map_err(VectorError::Other)?;

            let self_wrap_client = client.clone();
            let self_wrap_session = crate::db::current_session();
            db::spawn_bound(async move {
                if !self_wrap_session.is_live() { return; }
                let Ok(signer) = signer::active_signer() else { return };
                if let Ok(wrap) = nostr_sdk::prelude::GiftWrapBuilder::new(my_public_key, rumor)
                    .finalize_async(&signer)
                    .await
                {
                    let _ = self_wrap_client.send_event(&wrap).await;
                }
            });

            Ok(edit_id)
        })
        .await
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

    /// One message by id, with the chat it was said in. Reads STATE first, then
    /// the store for a row that has paged out of the loaded window.
    ///
    /// This is what a moderation citation needs: it names a message id and
    /// nothing else, so the room has to come back with it. `None` is an
    /// ordinary answer — a citation outlives its message (a delete, a hide, an
    /// expiry, history never synced), and callers must read absence as absence
    /// rather than as a fault.
    pub async fn get_message(&self, message_id: &str) -> Option<(String, Message)> {
        {
            let state = state::STATE.lock().await;
            if let Some((chat, msg)) = state.find_message(message_id) {
                return Some((chat.id.clone(), msg));
            }
        }
        db::events::get_message_by_id(message_id).await.ok().flatten()
    }

    /// Every message carrying an attachment with this content hash, newest
    /// first. `Attachment.id` IS that hash, and so is a citation's
    /// `content_hash`, so a classifier verdict about a blob resolves straight
    /// back to what carried it.
    ///
    /// A list, not one answer: one blob rides many messages — a forward, a
    /// re-post, or forty accounts posting the same image, which is exactly the
    /// case worth being able to see whole.
    pub async fn get_messages_with_attachment(&self, content_hash: &str) -> Vec<(String, Message)> {
        let Ok(ids) = db::attachments::events_with_attachment_hash(content_hash) else { return Vec::new() };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(found) = self.get_message(&id).await {
                out.push(found);
            }
        }
        out
    }

    /// Cursor-paged local read: up to `limit` messages strictly before the
    /// `(at_ms, id)` cursor, chronological. `None` returns the newest `limit`.
    ///
    /// The cursor is compared BY VALUE, never resolved to a stored row, so it
    /// keeps working after its message is gone (NIP-09 delete, moderation hide,
    /// a self-destruct timer) — a persisted cursor must not wedge the walk. The
    /// id tiebreaks inside a same-millisecond burst, giving a total order that
    /// can't skip or re-serve across pages. Local only: sync older history in
    /// from relays first.
    pub async fn get_messages_before(
        &self,
        chat_id: &str,
        before: Option<(u64, &str)>,
        limit: usize,
    ) -> Vec<Message> {
        let state = state::STATE.lock().await;
        let Some(chat) = state.get_chat(chat_id) else {
            return Vec::new();
        };
        let mut msgs = chat.get_all_messages(&state.interner);
        if let Some((at, id)) = before {
            msgs.retain(|m| (m.at, m.id.as_str()) < (at, id));
        }
        msgs.sort_by(|a, b| (a.at, a.id.as_str()).cmp(&(b.at, b.id.as_str())));
        if msgs.len() > limit {
            msgs.drain(..msgs.len() - limit);
        }
        msgs
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
        let _client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = crate::signer::active_signer()
            .map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let servers = crate::blossom_servers::compute_enabled_servers();
        if servers.is_empty() {
            return Err(VectorError::Other("No Blossom servers configured".into()));
        }
        // Avatars/banners run larger than emojis (up to ~1MB), so give a more generous
        // 20s idle window before treating a silent server as dead and failing over.
        crate::blossom::upload_blob_with_failover(
            signer,
            servers,
            std::sync::Arc::new(bytes),
            Some(mime),
            Some(std::time::Duration::from_secs(20)),
        )
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
                            // A dissolved community is SEALED: the row survives (history
                            // is never auto-deleted) but no write is ever accepted again.
                            // Without this a bot cannot tell it from a live one and
                            // retries sends into a tombstone forever.
                            "dissolved": c.dissolved,
                            // `readable` is the field a bot needs and could never
                            // compute: a private channel we've been told about but
                            // hold no key for is enumerable-but-unreadable, which
                            // is what distinguishes "not a member" from "member
                            // awaiting the key vend".
                            "channels": c.channels.iter()
                                .map(|ch| serde_json::json!({
                                    "channel_id": crate::simd::hex::bytes_to_hex_32(&ch.id.0),
                                    "name": ch.name,
                                    "private": ch.private,
                                    "readable": !(ch.private && ch.key.is_none()),
                                    "epoch": ch.epoch.0,
                                }))
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
                            "dissolved": c.dissolved,
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
        let session = crate::db::current_session();
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
            // The primary channel id, so the frontend can stamp every grafted chat row
            // the way `register_v2_chats` stamps the persisted ones. Without it each
            // channel claimed primary via the render fallback and a multi-channel
            // community drew one list row per channel until the next full reload.
            "primary_channel": community.primary_channel().map(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0)),
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
    pub async fn register_v2_chats(&self, community: &crate::community::v2::community::CommunityV2, __session: &std::sync::Arc<crate::db::Session>) {
        register_v2_chats_inner(community).await
    }
}

/// Free-function body of [`VectorCore::register_v2_chats`] — also the migration finalize's
/// chat stamp (it runs from a spawned task with no facade handle; only globals are touched).
pub(crate) async fn register_v2_chats_inner(community: &crate::community::v2::community::CommunityV2) {
    crate::db::scoped(async move {
        let owner_npub = community.owner().ok().and_then(|p| ToBech32::to_bech32(&p).ok());
            let me = state::my_public_key();
            let is_owner = me.is_some_and(|m| community.owner().is_ok_and(|o| o == m));
            let id_hex = crate::simd::hex::bytes_to_hex_32(&community.identity.community_id.0);
            // The chat list shows ONE row per community — the primary channel under the
            // community's metadata (v1-group parity; multi-channel UI is a later cut).
            let Some(primary) = community.primary_channel() else { return };
            let primary_hex = crate::simd::hex::bytes_to_hex_32(&primary.id.0);
            // Every channel gets a real chat row carrying its own name plus the community's
            // primary id. The chat list still shows ONE row per community (it renders only the
            // primary), but the sibling rows are now addressable, which is what lets the UI
            // reach a multi-channel community's other channels.
            let slims = {
                let mut st = state::STATE.lock().await;
                let mut slims = Vec::new();
                for ch in &community.channels {
                    let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
                    st.upsert_community_chat(
                        &ch_hex,
                        &community.name,
                        community.description.as_deref().unwrap_or(""),
                        &id_hex,
                        is_owner,
                        community.icon.is_some(),
                        owner_npub.as_deref(),
                        Some(community.created_at_ms),
                        community.dissolved,
                        crate::community::ConcordProtocol::V2,
                        &ch.name,
                        &primary_hex,
                    );
                    if let Some(chat) = st.chats.iter().find(|c| c.id == ch_hex) {
                        slims.push(crate::db::chats::SlimChatDB::from_chat(chat, &st.interner));
                    }
                }
                slims
            };
            // Persist the rows so a fresh boot reloads each channel's name/metadata
            for slim in &slims {
                let _ = crate::db::chats::save_slim_chat(slim);
            }
    })
    .await
}

/// How long a community's raid verdict stays warm. Long enough that opening the panel
/// right after seeing its badge is instant, short enough that a wave arriving now shows
/// up without the moderator reloading anything.
const RAID_REPORT_TTL_SECS: u64 = 90;

/// Per-account: the verdict is derived from this account's database and its roster.
struct RaidReportCache;

#[allow(clippy::type_complexity)]
struct PolicyReportCache;

/// The engine's console report, memoised like the assessor's was: evaluating
/// decrypts a four-thousand-message window and clusters every author, so a
/// header badge asking on every render would be a real cost.
fn policy_report_cache(
) -> std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (u64, std::sync::Arc<serde_json::Value>)>>> {
    crate::db::current_session().scoped::<PolicyReportCache, _>()
}

fn raid_report_cache(
) -> std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (u64, std::sync::Arc<crate::community::raid::RaidReport>)>>> {
    crate::db::current_session().scoped::<RaidReportCache, _>()
}

impl VectorCore {
    /// Join a Community from a public invite URL (`vectorapp.io/invite#...`). Fetches the
    /// token-encrypted bundle, persists the member-view Community, and registers its channels
    /// as chats. Returns a JSON summary.
    pub async fn join_community(&self, invite_url: &str) -> Result<serde_json::Value> {
        use crate::community::{public_invite, service, transport::LiveTransport};
        // Dual-stack: a v2 link is `…/invite/<naddr>#<fragment>` (a naddr in the
        // path); a v1 link is `…/invite#<base64url>` (fragment only). Try the v2
        // parser first — it only succeeds on the v2 shape — then fall through to v1.
        if crate::community::v2::invite::parse_invite_link(invite_url).is_ok() {
            let session = crate::db::current_session();
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
                let seed_community = community.clone();
                db::spawn_bound(async move {
                    let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(20));
                    if matches!(
                        crate::community::v2::service::sync_guestbook(&transport, &seed_community).await,
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
        // Post-timelock door: a FRESH v1 join needs a migration carrier (the v2 on-ramp)
        // or it is refused. Decode-only view — nothing persists unless the gate passes.
        let probe_view = crate::community::invite::accept_invite(&bundle.join).map_err(VectorError::Other)?;
        crate::community::migration::gate_fresh_v1_join(&transport, &probe_view, now)
            .await
            .map_err(VectorError::Other)?;
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
        crate::db::scoped(async move {
            use crate::community::transport::LiveTransport;
            let bundle_json = crate::db::community::get_pending_invite(community_id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other(format!("no pending invite for {community_id}")))?;
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

            // Dual-stack: a validating v2 bundle parse means a v2 Direct Invite.
            if crate::community::v2::invite::CommunityInvite::from_bundle_json(&bundle_json).is_ok() {
                let session = crate::db::current_session();
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
                //
                // ONE exception: a verified dissolution is a verdict, not a maybe — an
                // owner-signed grave has no undo (§9), so that invite can never succeed
                // and is retired everywhere instead of parking a permanent error loop.
                let community = match crate::community::v2::service::accept_parked_invite(&transport, &bundle_json, inviter.as_deref()).await {
                    Ok(c) => c,
                    Err(e) if e == crate::community::v2::service::ERR_DISSOLVED => {
                        let relays = crate::community::v2::invite::CommunityInvite::from_bundle_json(&bundle_json)
                            .map(|i| i.relays)
                            .unwrap_or_default();
                        crate::community::v2::service::retire_dead_invite(&transport, community_id, &relays).await;
                        return Err(VectorError::Other("this community has been dissolved — the invite was removed".into()));
                    }
                    Err(e) => return Err(VectorError::Other(e)),
                };
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
            // Post-timelock door: a FRESH v1 join needs a migration carrier (the v2 on-ramp) or it
            // is refused — before finalize persists anything. The migrated fence inside
            // finalize_member_join still wins for held communities.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            crate::community::migration::gate_fresh_v1_join(&transport, &community, now)
                .await
                .map_err(VectorError::Other)?;
            // Private invites carry no public-link label; the inviter attribution metric is link-only.
            let summary = self.finalize_member_join(community, &transport, None).await?;
            let _ = crate::db::community::delete_pending_invite(community_id);
            Ok(summary)
        })
        .await
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
        // Migration fence: if this v1 community already flipped to v2, a stale parked
        // invite or a lingering link must NOT re-run `save_community` — its blind UPSERT would
        // re-parent the stitched channel rows back to v1 with v1 keys (the catastrophic mixed
        // state). Short-circuit to "already upgraded"; the rows stay v2-owned. This is
        // `migrated_to`-aware (not a blind dissolved gate) precisely so a FRESH joiner redeeming
        // a still-live link stays on the open on-ramp path below.
        if let Ok(Some(v2)) = crate::db::community::get_migrated_to(&community.id.to_hex()) {
            return Ok(serde_json::json!({
                "community_id": v2,
                "version": 2,
                "migrated": true,
            }));
        }
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
            let primary_hex = community.channels.first().map(|c| c.id.to_hex()).unwrap_or_default();
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
                    crate::community::ConcordProtocol::V1,
                    &ch.name,
                    &primary_hex,
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


    // ── Channels (CORD-03) ───────────────────────────────────────────────────

    /// Resolve a v2 community by id, or explain why it isn't one.
    fn v2_community(community_id: &str) -> Result<crate::community::v2::community::CommunityV2> {
        use crate::community::CommunityId;
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        match crate::db::community::community_protocol(&cid).ok().flatten() {
            Some(crate::community::ConcordProtocol::V2) => crate::db::community::load_community_v2(&cid)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into())),
            Some(_) => Err(VectorError::Other(
                "channel management is Concord v2 only — this community still uses the legacy protocol".into(),
            )),
            None => Err(VectorError::Other("community not found".into())),
        }
    }

    fn channel_id_of(channel_id: &str) -> Result<crate::community::ChannelId> {
        crate::simd::hex::hex_to_bytes_32_checked(channel_id)
            .map(crate::community::ChannelId)
            .ok_or_else(|| VectorError::Other("malformed channel id".into()))
    }

    /// Create a channel. A private one mints its own key plus the channel-scoped
    /// access Role that is its access list (CORD-03/04); grant that role with
    /// [`grant_channel_access`](Self::grant_channel_access) to let someone read it.
    /// Returns the new channel id (32-byte hex).
    pub async fn create_channel(&self, community_id: &str, name: &str, private: bool) -> Result<String> {
        use crate::community::{v2::service, transport::LiveTransport};
        let community = Self::v2_community(community_id)?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let id = if private {
            service::create_private_channel(&transport, &community, name).await
        } else {
            service::create_public_channel(&transport, &community, name).await
        }
        .map_err(VectorError::Other)?;
        // Subscribe the new channel's chat plane now — waiting on the round-trip of
        // our own vsk-2 edition would leave the creator deaf to first replies.
        if let Some(client) = state::nostr_client() {
            crate::community::v2::realtime::refresh_subscription(&client).await;
        }
        Ok(crate::simd::hex::bytes_to_hex_32(&id.0))
    }

    /// Rename a channel (a versioned edit of the same entity, so its history and
    /// id survive). Other folded fields are carried through untouched.
    pub async fn rename_channel(&self, community_id: &str, channel_id: &str, name: &str) -> Result<()> {
        use crate::community::{v2::service, transport::LiveTransport};
        let community = Self::v2_community(community_id)?;
        let id = Self::channel_id_of(channel_id)?;
        let mut meta = community
            .channel(&id)
            .ok_or_else(|| VectorError::Other("unknown channel".into()))?
            .metadata();
        meta.name = name.to_string();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::edit_channel_metadata(&transport, &community, &id, &meta)
            .await
            .map_err(VectorError::Other)
    }

    /// Tombstone a channel. Deletion is terminal and the id is never reused;
    /// history stays readable to anyone who already holds its keys.
    pub async fn delete_channel(&self, community_id: &str, channel_id: &str) -> Result<()> {
        use crate::community::{v2::service, transport::LiveTransport};
        let community = Self::v2_community(community_id)?;
        let id = Self::channel_id_of(channel_id)?;
        let name = community.channel(&id).map(|c| c.name.clone()).unwrap_or_default();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::delete_channel(&transport, &community, &id, &name)
            .await
            .map_err(VectorError::Other)
    }

    /// Grant `npub` read access to a private channel: adds the channel's access
    /// role and vends the key to them (CORD-03 "delivered on grant").
    pub async fn grant_channel_access(&self, community_id: &str, channel_id: &str, npub: &str) -> Result<()> {
        use crate::community::{v2::service, transport::LiveTransport};
        let community = Self::v2_community(community_id)?;
        let id = Self::channel_id_of(channel_id)?;
        let member = nostr_sdk::prelude::PublicKey::parse(npub)
            .map_err(|e| VectorError::Other(format!("bad npub: {e}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::grant_channel_access(&transport, &community, &id, &member)
            .await
            .map_err(VectorError::Other)
    }

    /// Revoke `npub`'s read access: drops the access role and rekeys the channel
    /// so the removal actually severs them (CORD-06). They keep whatever history
    /// they already read — a rekey protects the future, never the past.
    pub async fn revoke_channel_access(&self, community_id: &str, channel_id: &str, npub: &str) -> Result<()> {
        use crate::community::{v2::service, transport::LiveTransport};
        let community = Self::v2_community(community_id)?;
        let id = Self::channel_id_of(channel_id)?;
        let member = nostr_sdk::prelude::PublicKey::parse(npub)
            .map_err(|e| VectorError::Other(format!("bad npub: {e}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        service::revoke_channel_access(&transport, &community, &id, &member)
            .await
            .map_err(VectorError::Other)
    }

    /// Who may read a private channel: the channel-scoped Roles that are its
    /// access list (CORD-04 §2) and the members holding them.
    ///
    /// Sync read off the folded roster. `members` is exactly the access-role
    /// holders; `owner` is reported alongside because the owner is entitled
    /// whether or not they hold one (a channel's creator is granted the role, so
    /// an owner-created channel lists them in both).
    pub fn channel_access(&self, community_id: &str, channel_id: &str) -> Result<serde_json::Value> {
        use nostr_sdk::prelude::{PublicKey, ToBech32};
        let community = Self::v2_community(community_id)?;
        let id = Self::channel_id_of(channel_id)?;
        let ch = community
            .channel(&id)
            .ok_or_else(|| VectorError::Other("unknown channel".into()))?;
        // Normalised id, not the caller's string: an uppercase-hex argument loads
        // the community but would miss the roster row and silently report nobody.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let roster = crate::db::community::get_community_roles(&cid_hex).map_err(VectorError::Other)?;
        let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
        let chan_hex = crate::simd::hex::bytes_to_hex_32(&id.0);
        let access_ids = roster.channel_role_ids(&chan_hex);
        let roles: Vec<serde_json::Value> = roster
            .channel_roles(&chan_hex)
            .into_iter()
            .map(|r| serde_json::json!({ "role_id": r.role_id, "name": r.name }))
            .collect();
        let members: Vec<String> = roster
            .grants
            .iter()
            .filter(|g| !banned.contains(&g.member))
            .filter(|g| g.role_ids.iter().any(|rid| access_ids.contains(rid)))
            .filter_map(|g| PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()))
            .collect();
        Ok(serde_json::json!({
            "channel_id": chan_hex,
            "private": ch.private,
            "readable": !(ch.private && ch.key.is_none()),
            "owner": community.owner().ok().and_then(|o| o.to_bech32().ok()),
            "roles": roles,
            "members": members,
        }))
    }

    /// Mint a public invite link for a Community this identity owns. Returns the shareable URL.
    /// `expires_at_ms` is an ABSOLUTE unix timestamp in MILLISECONDS (v2's native unit and the
    /// `InviteEntry` wire's); the v1 path takes seconds, so it is converted here rather than at
    /// each call site. `label` is the attribution bucket shown as "joined via <label>".
    pub async fn create_public_invite(
        &self,
        community_id: &str,
        expires_at_ms: Option<u64>,
        label: Option<String>,
    ) -> Result<String> {
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
            let minted =
                crate::community::v2::service::mint_public_link(&transport, &community, base, expires_at_ms, label)
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
        let expires_at_secs = expires_at_ms.map(|ms| ms / 1000);
        let (_token, url) = service::create_public_invite(&transport, &community, expires_at_secs, label)
            .await
            .map_err(VectorError::Other)?;
        Ok(url)
    }

    /// Send a PRIVATE invite: gift-wrap this Community's invite bundle directly to an npub over a NIP-17
    /// DM (the same transport as a regular DM). The invitee parks it pending consent (accept_pending_invite).
    /// Requires CREATE_INVITE; a banned npub can't be re-invited. Returns the wrap's event id + relays.
    pub async fn invite_to_community(&self, community_id: &str, invitee_npub: &str) -> Result<serde_json::Value> {
        crate::db::scoped(async move {
            use crate::community::{service, CommunityId};
            use crate::sending::{send_rumor_dm, NoOpSendCallback, SendCallback, SendConfig};

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
                let recipient = nostr_sdk::prelude::PublicKey::parse(invitee_npub)
                    .map_err(|e| VectorError::Other(format!("bad invitee npub: {e}")))?;
                let client = crate::state::nostr_client().ok_or_else(|| VectorError::Other("Not connected".into()))?;
                // Gift-wrap the 3313 Direct-Invite rumor (the bundle JSON) to the RECIPIENT'S
                // inbox relays (kind-10050) — a not-yet-member sees it on their DM sub;
                // the community relays wouldn't reach them. `#k=3313` per CORD-05 §6.
                //
                // Load + snapshot UNDER the rotation lock: a bundle minted while a Ban's
                // refound is mid-rotation carries the root being buried, and its joiner
                // lands on a dead epoch only to self-evict on the rekey exclusion.
                let bundle = {
                    let lock = crate::community::v2::realtime::follow_lock(&cid);
                    let _rotation = lock.lock().await;
                    let community = crate::db::community::load_community_v2(&cid)
                        .map_err(VectorError::Other)?
                        .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
                    crate::community::v2::service::bundle_of(
                        &community,
                        crate::community::v2::service::BundleAudience::Member(recipient),
                        Some(my_pk),
                        None,
                        None,
                    )
                };
                let bundle_json = serde_json::to_string(&bundle).map_err(|e| VectorError::Other(e.to_string()))?;
                // Same 24h NIP-40 expiry as v1 (invite::DIRECT_INVITE_EXPIRY_SECS): a bundle is
                // live key material for a community that keeps rotating, so it must not linger.
                let expires_at = nostr_sdk::prelude::Timestamp::now().as_secs()
                    + crate::community::invite::DIRECT_INVITE_EXPIRY_SECS;
                let expiry_tag = nostr_sdk::prelude::Tag::expiration(nostr_sdk::prelude::Timestamp::from_secs(expires_at));
                let rumor = nostr_sdk::prelude::EventBuilder::new(
                    nostr_sdk::prelude::Kind::Custom(crate::community::v2::kind::DIRECT_INVITE),
                    bundle_json,
                )
                .tag(expiry_tag.clone())
                .finalize_unsigned_with_id(my_pk);
                let k_tag = nostr_sdk::prelude::Tag::custom(
                    "k",
                    [crate::community::v2::kind::DIRECT_INVITE.to_string()],
                );
                crate::inbox_relays::send_gift_wrap(&client, &recipient, rumor, [k_tag, expiry_tag])
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
            let invitee_hex = nostr_sdk::prelude::PublicKey::parse(invitee_npub)
                .map_err(|_| VectorError::Other("invalid npub".into()))?
                .to_hex();
            if crate::db::community::get_community_banlist(community_id)
                .map_err(VectorError::Other)?
                .iter()
                .any(|b| b == &invitee_hex)
            {
                return Err(VectorError::Other("That member is banned from this community and can't be invited".into()));
            }


            let now = nostr_sdk::prelude::Timestamp::now().as_secs();
            let rumor = crate::community::invite::build_invite_rumor(&community, my_pk, now)
                .map_err(VectorError::Other)?;
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
        })
        .await
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
        self.send_community_message_expiring(channel_id, content, replied_to, None).await
    }

    /// Post a message that deletes itself after `expires_in_secs`.
    ///
    /// A NIP-40 `expiration` tag on the rumor, which is what relays drop on and
    /// every member's client purges against — the same mechanism as the
    /// per-channel Self-Destruct Timer, decided per message instead of per
    /// channel. For a bot posting a public moderation notice: the warning is
    /// seen, then it stops being a permanent monument to somebody's worst day.
    ///
    /// v2 only. A v1 community has no expiry, and posting a permanent message
    /// where a vanishing one was asked for is the kind of surprise that ends up
    /// in a screenshot, so it refuses rather than silently keeping it forever.
    pub async fn send_community_message_expiring(
        &self,
        channel_id: &str,
        content: &str,
        replied_to: Option<&str>,
        expires_in_secs: Option<u64>,
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
            let mut extra_tags = Vec::new();
            if let Some(secs) = expires_in_secs.filter(|s| *s > 0) {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    .saturating_add(secs);
                extra_tags.push(nostr_sdk::prelude::Tag::expiration(nostr_sdk::prelude::Timestamp::from_secs(at)));
            }
            return crate::community::v2::service::send_chat_message(&transport, &community, &ch, content, reply_ref, &emoji_pairs, extra_tags)
                .await
                .map_err(VectorError::Other);
        }
        if expires_in_secs.is_some_and(|s| s > 0) {
            return Err(VectorError::Other(
                "this Community predates self-destructing messages; the message was not sent".into(),
            ));
        }
        let (community, channel) = self.resolve_channel(channel_id)?;
        Self::ensure_v1_writable(&community)?;
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
        let _client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = crate::signer::active_signer().map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let inner = unsigned.finalize_async(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
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

    /// Send a file to a Community channel as an encrypted attachment. Returns the message id.
    /// Mirrors the DM file pipeline (encrypt → Blossom upload → NIP-92 `imeta`) but publishes
    /// over the community transport.
    pub async fn send_community_file(&self, channel_id: &str, file_path: &str) -> Result<String> {
        crate::db::scoped(async move {
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
            // Same fail-fast rationale as the routing check above: a sealed community
            // (CORD-02 §9) accepts nothing, so refuse before the encrypt + upload rather
            // than burning a Blossom round-trip on a send that can never land. v2's own
            // gate is inside the send, which is too late to save the upload.
            match (&v2_target, &v1_target) {
                (Some(c), _) => {
                    let cid = crate::simd::hex::bytes_to_hex_32(&c.id().0);
                    if crate::db::community::get_community_dissolved(&cid).unwrap_or(false) {
                        return Err(VectorError::Other("this community has been dissolved".into()));
                    }
                }
                (None, Some((c, _))) => Self::ensure_v1_writable(c)?,
                _ => {}
            }
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

            let _client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
            let signer = crate::signer::active_signer().map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
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
            let inner = unsigned.finalize_async(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
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
        })
        .await
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
        Self::ensure_v1_writable(&community)?;
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
        crate::db::scoped(async move {
            // Already ours — nothing to publish; the set can't hold it twice.
            if Self::own_reaction_id(message_id, emoji).await.is_some() {
                return Ok(());
            }
            let emoji_tags: Vec<crate::types::EmojiTag> = match emoji_url {
                Some(url) if emoji.starts_with(':') && emoji.ends_with(':') && emoji.len() >= 3 && !url.is_empty() => {
                    vec![crate::types::EmojiTag { shortcode: emoji[1..emoji.len() - 1].to_string(), url: url.to_string() }]
                }
                _ => Vec::new(),
            };
            if let Some(id) = self.v2_community_for_channel(channel_id)? {
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
        })
        .await
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
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

        // Attachment URLs come from local state when held (a headless v2 consumer
        // has none — blob cleanup is then the receiving peers' concern, not ours).
        let attachment_urls: Vec<String> = {
            let st = state::STATE.lock().await;
            st.find_message(message_id)
                .map(|(_, msg)| msg.attachments.iter().flat_map(|a| a.all_urls().map(str::to_string)).collect())
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
            if let Some(_client) = state::nostr_client() {
                if let Ok(signer) = crate::signer::active_signer() {
                    crate::blossom::delete_blobs_best_effort(signer, attachment_urls);
                }
            }
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

    /// Moderation-hide someone ELSE's community message under `MANAGE_MESSAGES`
    /// (CORD-04 §3/§5). Protocol-agnostic: v2 seals the same kind-5 its authors
    /// use, v1 publishes its 3305 tombstone; both re-derive the actor's authority
    /// from the signed inner against the folded Roster, so this is an authority
    /// claim peers verify, never a local suppression.
    pub async fn hide_community_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        use crate::community::transport::LiveTransport;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

        // You can only moderate a message you can see: the author resolves from
        // STATE, then the store for a row that has paged out of the window.
        let author_npub = {
            let st = state::STATE.lock().await;
            st.find_message(message_id).and_then(|(_, m)| m.npub)
        };
        let author_npub = match author_npub {
            Some(n) => n,
            None => crate::db::events::event_author(message_id)
                .ok()
                .flatten()
                .ok_or_else(|| VectorError::Other("can't resolve the target message's author".into()))?,
        };
        let author = nostr_sdk::prelude::PublicKey::parse(&author_npub)
            .map_err(|_| VectorError::Other("target message has an unreadable author".into()))?;

        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            crate::community::v2::service::moderation_delete(
                &transport, &community, &ch, message_id, crate::community::v2::kind::MESSAGE, &author,
            )
            .await
            .map_err(VectorError::Other)?;
        } else {
            let cid = crate::db::community::community_id_for_channel(channel_id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("unknown community channel".into()))?;
            let community = crate::db::community::load_community(&crate::community::CommunityId(
                crate::simd::hex::hex_to_bytes_32(&cid),
            ))
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("community not found".into()))?;
            let channel = community
                .channels
                .iter()
                .find(|c| c.id.to_hex() == channel_id)
                .cloned()
                .ok_or_else(|| VectorError::Other("channel not found in community".into()))?;
            crate::community::service::publish_owner_hide(&transport, &community, &channel, message_id)
                .await
                .map_err(VectorError::Other)?;
        }

        let removed_chat = {
            let mut st = state::STATE.lock().await;
            st.remove_message(message_id).map(|(cid, _)| cid)
        };
        let _ = crate::db::events::delete_event(message_id).await;
        traits::emit_event_json("message_removed", serde_json::json!({
            "id": message_id, "chat_id": removed_chat.as_deref().unwrap_or(channel_id), "reason": "hidden",
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
        Self::ensure_v1_writable(&community)?;
        let author_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let unsigned = envelope::build_inner_typed(
            author_pk, &channel.id, channel.epoch, kind, content, ms, Some(target), emoji_tags,
        );
        let _client = state::nostr_client().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let signer = crate::signer::active_signer().map_err(|e| VectorError::Other(format!("Signer unavailable: {e}")))?;
        let inner = unsigned.finalize_async(&signer).await.map_err(|e| VectorError::Other(format!("sign: {e}")))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let outer = service::send_signed_message(&transport, &community, &channel, &inner)
            .await.map_err(VectorError::Other)?;
        // Local echo + persist + emit (relay echo dedups on inner id). A swap during the
        // publish must not echo account A's control event into account B.
        let outcome = {
            let mut st = state::STATE.lock().await;
            inbound::process_incoming(&mut st, &outer, &channel, &author_pk)
        };
        if let Some(inbound::IncomingEvent::Updated { target_id, mut message, edit_event }) = outcome {
            if let Some(ev) = edit_event {
                let mut ev = (*ev).clone();
                if let Ok(cid) = crate::db::id_cache::get_chat_id_by_identifier(channel_id) { ev.chat_id = cid; }
                let _ = crate::db::events::save_event(&ev).await;
            } else {
                let _ = crate::db::events::save_message(channel_id, &message).await;
            }
            traits::emit_message_update(channel_id, &target_id, &mut message).await;
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
        self.sync_community_channel_before(channel_id, limit, None).await
    }

    /// One page of a v2 channel catch-up: what the relay served, and how much of it
    /// this client had never seen.
    pub async fn sync_community_channel_page(
        &self,
        channel_id: &str,
        limit: usize,
        before_secs: Option<u64>,
        since_secs: Option<u64>,
    ) -> Result<(BackfillCount, Vec<String>)> {
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let warnings = if community::v2::realtime::follow_worker_running() {
                community::v2::realtime::enqueue_follow(&id);
                Vec::new()
            } else {
                Self::v2_inline_follow(&id).await
            };
            let count = Self::v2_backfill_channel_counted(
                &id, channel_id, limit, 8, since_secs, before_secs,
                crate::community::transport::Evidence::Fast, 12,
            ).await;
            return Ok((count, warnings));
        }
        // v1 has its own paging path; report the new-count and leave `fetched`
        // meaningless rather than inventing a number a caller might trust.
        let (new_messages, warnings) = self.sync_community_channel(channel_id, limit).await?;
        Ok((BackfillCount { fetched: new_messages, new_messages }, warnings))
    }

    /// [`sync_community_channel`](Self::sync_community_channel) with a back-paging
    /// cursor: `before_secs` bounds the fetch to messages at or older than it.
    ///
    /// The cursor is what reaches a v2 channel's PRE-ROTATION history. An
    /// uncursored catch-up reads the live epoch only (rotations move the
    /// conversation, so that is where it is); each epoch keeps its own plane
    /// address, and a bounded fetch spans all of them at once because messages are
    /// selected by time, not by epoch.
    pub async fn sync_community_channel_before(
        &self,
        channel_id: &str,
        limit: usize,
        before_secs: Option<u64>,
    ) -> Result<(usize, Vec<String>)> {
        use crate::community::{send, service, transport::LiveTransport};
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
            // Deepest catch-up walk: pages × page-size bounds one reconnect's fetch.
            // Chat plane = fetch ASAP. NOTE: fetch_plane does not consult the
            // evidence tier yet (#370) — until it does, the transport-seconds
            // bound is the effective limit; the declared Fast records intent.
            let new = Self::v2_backfill_channel(
                &id, channel_id, limit, 8, None, before_secs,
                crate::community::transport::Evidence::Fast, 12,
            ).await;
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

        // Guard straddles the fetch: the persist walk below writes this account's DB.
        let session = crate::db::current_session();
        let events = send::fetch_channel_page(&transport, &community, &channel, None, None, limit.max(1))
            .await
            .map_err(VectorError::Other)?;
        let new = Self::v1_ingest_channel_page(channel_id, &events, &channel, my_pk, &session).await;
        Ok((new, warnings))
    }

    /// Ingest one fetched v1 channel page: STATE apply, batched persist with
    /// delete barriers, presence + WebXDC rows, self-removal teardown. Shared by
    /// the full sync above and the cursor-paged sync — the two must never drift,
    /// or paged history would skip the moderation/teardown arms.
    async fn v1_ingest_channel_page(
        channel_id: &str,
        events: &[nostr_sdk::prelude::Event],
        channel: &crate::community::Channel,
        my_pk: nostr_sdk::prelude::PublicKey,
        session: &std::sync::Arc<crate::db::Session>,
    ) -> usize {
        crate::db::scoped(async move {
            use crate::community::inbound;
            let outcomes = {
                let mut st = state::STATE.lock().await;
                inbound::process_channel_batch(&mut st, &events, &channel, &my_pk)
            };
            let mut new = 0usize;
            // Message saves COLLECT into one batched transaction; deletes are flush barriers
            // (see flush_message_batch — a save committing after a delete it preceded on the
            // wire would resurrect the deleted row).
            let mut pending: Vec<&crate::types::Message> = Vec::new();
            for o in &outcomes {
                // Every arm below writes this account's DB — a swap can land between them.
                if !session.is_live() {
                    pending.clear();
                    break;
                }
                match o {
                    inbound::IncomingEvent::NewMessage(m) => {
                        pending.push(m);
                        new += 1;
                    }
                    inbound::IncomingEvent::Updated { message, .. } => {
                        pending.push(message);
                    }
                    inbound::IncomingEvent::Removed { target_id } => {
                        crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
                        // Tombstone FIRST, exactly as the DM delete does. Dropping the row
                        // alone only hides it until the next full sync re-serves the
                        // original from a relay: nothing recorded that it was removed, so
                        // it re-ingests cleanly and a moderator's deletion silently undoes
                        // itself. The deletion may also arrive BEFORE its target, where
                        // there is no row to drop and this is the only thing that lands.
                        crate::state::note_message_deleted(target_id);
                        if let Err(e) = crate::db::events::add_message_tombstone(target_id) {
                            crate::log_warn!("[Community delete] tombstone write failed: {e}");
                        }
                        let _ = crate::db::events::delete_event(target_id).await;
                    }
                    inbound::IncomingEvent::ReactionRemoved { reaction_id, .. } => {
                        // save_message is additive, so a revoked reaction's kind-7 row must be
                        // dropped explicitly or it resurrects on reload.
                        crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
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
                        crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
                        let _ = crate::db::community::delete_community_retain_keys(community_id);
                        break;
                    }
                    inbound::IncomingEvent::Typing { .. } => {
                        // Realtime-only ephemeral signal; never fetched in a sync batch. No-op.
                    }
                }
            }
            crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
            new
        })
        .await
    }

    /// Cursor-paged relay sync: fetch ONE page of up to `max_events` events from
    /// the channel's relays (per relay — divergent relays can push the union
    /// higher), ingest it, and return how many MESSAGES were new. A return of 0
    /// means the page held nothing new, not that the channel is empty.
    ///
    /// `until_s` / `since_s` are unix-seconds bounds (the back-paging cursor and
    /// the time floor). Depth comes from looping, not from a bigger page:
    /// `max_events` is clamped to 500 (relays clamp theirs anyway) and values
    /// under 50 behave as 50 on v2 (the shared pager's floor).
    ///
    /// Consensus (root rotations, control folds, ban checks) is the FULL sync's
    /// job — [`sync_community_channel`](Self::sync_community_channel) — so a
    /// hundred-page walk doesn't re-fold control a hundred times. Run one full
    /// sync (or be listening) before deep walks.
    pub async fn sync_channel_events(
        &self,
        channel_id: &str,
        max_events: usize,
        until_s: Option<u64>,
        since_s: Option<u64>,
    ) -> Result<usize> {
        use crate::community::{send, transport::LiveTransport};
        let max = max_events.clamp(1, 500);
        if let Some(id) = self.v2_community_for_channel(channel_id)? {
            let community = crate::db::community::load_community_v2(&id)
                .map_err(VectorError::Other)?
                .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
            if community.dissolved {
                return Err(VectorError::Other("this community has been dissolved".into()));
            }
            let ch_id = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let ch = community
                .channel(&ch_id)
                .ok_or_else(|| VectorError::Other("no such channel in this community".into()))?;
            // Err, not a silent 0 — "granted but the key hasn't arrived" must be
            // distinguishable from "nothing new" or the caller diagnoses a mute bot.
            if ch.private && ch.key.is_none() {
                return Err(VectorError::Other(
                    "this private channel has no key yet (awaiting rekey delivery)".into(),
                ));
            }
            let new = Self::v2_backfill_channel(
                &id, channel_id, max, 1, since_s, until_s,
                crate::community::transport::Evidence::Fast, 12,
            )
            .await;
            return Ok(new);
        }
        let (community, channel) = self.resolve_channel(channel_id)?;
        Self::ensure_v1_writable(&community)?;
        let my_pk = state::my_public_key().ok_or_else(|| VectorError::Other("Not logged in".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let session = crate::db::current_session();
        let events = send::fetch_channel_page(&transport, &community, &channel, until_s, since_s, max)
            .await
            .map_err(VectorError::Other)?;
        Ok(Self::v1_ingest_channel_page(channel_id, &events, &channel, my_pk, &session).await)
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
                let Ok(npub) = pk.to_bech32();
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
        // union — a room whose relays refuse kind 10304 still resolves.
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
                        let c2 = community.clone();
                        db::spawn_bound(async move {
                            let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(20));
                            if matches!(crate::community::v2::service::sync_guestbook(&transport, &c2).await, Ok(fresh) if !fresh.is_empty()) {
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

    /// A Community's banlist, as npubs.
    ///
    /// The counterpart to [`Self::set_member_banned`], which had none: setting a
    /// ban succeeds identically whether or not one was already in place, so a
    /// caller lifting a ban could not tell whether it changed anything, and
    /// anything reporting on it was guessing.
    ///
    /// Read from local state, so it reflects the last banlist edition this
    /// client folded rather than a fresh fetch.
    pub fn get_community_banned(&self, community_id: &str) -> Vec<String> {
        use nostr_sdk::prelude::{PublicKey, ToBech32};
        // Resolve the id the same way the moderation panel does: a caller may
        // hold a logical id that is not the row key.
        let cid_hex = match Self::load_v2_if_v2(community_id) {
            Ok(Some(c)) => crate::simd::hex::bytes_to_hex_32(&c.id().0),
            _ => community_id.to_string(),
        };
        crate::db::community::get_community_banlist(&cid_hex)
            .unwrap_or_default()
            .iter()
            .filter_map(|hex| PublicKey::parse(hex).ok())
            .filter_map(|pk| pk.to_bech32().ok())
            .collect()
    }

    /// Everything the moderation panel needs about one Community, read once so the
    /// parts agree with each other: every member scored against the raid evidence
    /// ([`crate::community::raid`]), the duplicate-text cohorts behind those verdicts,
    /// the live invite links, and what the caller may actually do about it.
    ///
    /// v2 only — the containment this feeds (batch ban, refounding) are v2 verbs.
    pub fn community_moderation_intel(&self, community_id: &str) -> Result<serde_json::Value> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("moderation tools require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let report = Self::policy_console_report(&cid_hex, &community)?;
        let invites = crate::db::community::list_public_invites(&cid_hex).unwrap_or_default();
        let banlist = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
        let caps = self.community_capabilities(community_id).unwrap_or_else(|_| serde_json::json!({}));
        let owner_b32 = community.owner().map_err(VectorError::Other)?.to_bech32().unwrap_or_default();

        Ok(serde_json::json!({
            "community_id": cid_hex,
            "name": community.name,
            "owner_npub": owner_b32,
            "epoch": community.root_epoch.0,
            "report": &*report,
            "invites": invites.iter().map(|i| serde_json::json!({
                "token": i.token,
                "label": i.label,
                "created_at": i.created_at,
                "expires_at": i.expires_at,
                "join_count": i.join_count,
            })).collect::<Vec<_>>(),
            "banlist_count": banlist.len(),
            "banlist_max": crate::community::v2::roles::MAX_BANLIST,
            "capabilities": caps,
            // The designer offers per-channel exemptions, and this is the
            // console's own data source — one place to ask, one answer.
            "channels": community.channels.iter().map(|c| serde_json::json!({
                "id": crate::simd::hex::bytes_to_hex_32(&c.id.0),
                "name": c.name,
            })).collect::<Vec<_>>(),
        }))
    }

    /// Screen ONE message immediately, without waiting for a sweep.
    ///
    /// Runs only the stateless rules — words, links, regex, mentions — because
    /// those are the only ones a single message can answer. Rate, repetition
    /// and cohorts describe a window and stay with the periodic evaluation.
    ///
    /// Costs no corpus read and no cache: the whole point is that a word filter
    /// should answer in milliseconds, not on the next 90-second tick.
    pub fn screen_community_message(
        &self,
        channel_id: &str,
        author_npub: &str,
        text: &str,
    ) -> Result<serde_json::Value> {
        use nostr_sdk::prelude::PublicKey;
        let Some(id) = self.v2_community_for_channel(channel_id)? else {
            return Ok(serde_json::json!({ "findings": [] }));
        };
        let community = crate::db::community::load_community_v2(&id)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("v2 community not found".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let owner = community.owner().map_err(VectorError::Other)?;
        let author = PublicKey::parse(author_npub).map_err(|_| VectorError::Other("unreadable author".into()))?;

        // Same staff definition the console uses: a role carrying a moderation
        // permission bit, never mere role membership.
        use crate::community::roles::Permissions;
        const MOD_MASK: u64 = Permissions::MANAGE_ROLES
            | Permissions::MANAGE_CHANNELS
            | Permissions::MANAGE_METADATA
            | Permissions::KICK
            | Permissions::BAN
            | Permissions::MANAGE_MESSAGES;
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
        let hex = author.to_hex();
        let roles: Vec<String> =
            roster.grants.iter().find(|g| g.member == hex).map(|g| g.role_ids.clone()).unwrap_or_default();
        let is_staff = roles
            .iter()
            .any(|rid| roster.roles.iter().any(|r| &r.role_id == rid && (r.permissions.0 & MOD_MASK) != 0));

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let findings = crate::community::policy::harness::screen_message(
            &cid_hex, &owner, &author, &roles, is_staff, channel_id, text, now_ms,
        );
        Ok(serde_json::json!({ "findings": findings }))
    }

    /// The raid verdict alone, for the badge on a community header. Same assessment as
    /// the panel and served from the same cache, so surfacing the alert costs nothing
    /// once and then nothing at all until it expires.
    pub fn check_community_raid(&self, community_id: &str) -> Result<serde_json::Value> {
        let Some(community) = Self::load_v2_if_v2(community_id)? else {
            return Ok(serde_json::json!({ "detected": false }));
        };
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let report = Self::policy_console_report(&cid_hex, &community)?;
        let biggest = report["cohorts"].get(0);
        Ok(serde_json::json!({
            "detected": report["raid_detected"].as_bool().unwrap_or(false),
            "suspects": report["suspects"].as_u64().unwrap_or(0),
            "cohort": biggest.and_then(|c| c["size"].as_u64()).unwrap_or(0),
            "sample": biggest
                .and_then(|c| c["sample"].as_str())
                .map(|s| s.chars().take(60).collect::<String>())
                .unwrap_or_default(),
        }))
    }

    /// Assess a community, memoised for [`RAID_REPORT_TTL_SECS`]. The read decrypts a
    /// four-thousand-message window, so a header badge asking on every render would be a
    /// real cost; the panel opening moments later reuses the same answer.
    fn raid_report(cid_hex: &str) -> Result<std::sync::Arc<crate::community::raid::RaidReport>> {
        use crate::community::raid;
        use crate::community::v2::guestbook::GuestbookEntry;
        use nostr_sdk::prelude::PublicKey;
        use std::collections::{HashMap, HashSet};

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        {
            let cache = raid_report_cache();
            let map = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, report)) = map.get(cid_hex) {
                if now_secs.saturating_sub(*at) < RAID_REPORT_TTL_SECS {
                    return Ok(report.clone());
                }
            }
        }
        let community = crate::db::community::load_community_v2(&crate::community::CommunityId(
            crate::simd::hex::hex_to_bytes_32(cid_hex),
        ))
        .map_err(VectorError::Other)?
        .ok_or_else(|| VectorError::Other("moderation tools require a Concord v2 community".into()))?;
        let owner_b32 = community.owner().map_err(VectorError::Other)?.to_bech32().unwrap_or_default();
        let me_b32 = state::my_public_key().and_then(|p| p.to_bech32().ok()).unwrap_or_default();

        // Anyone holding a live grant is staff, whatever the role is called.
        let roster = crate::db::community::get_community_roles(cid_hex).unwrap_or_default();
        let staff: HashSet<String> = roster
            .grants
            .iter()
            .filter(|g| !g.role_ids.is_empty())
            .filter_map(|g| PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()))
            .collect();

        // The same fold the member list paints, so the panel can't disagree with it.
        let members: Vec<String> = crate::community::v2::service::stored_memberlist(&community)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pk| pk.to_bech32().ok())
            .collect();

        // Guestbook: authoritative joins, with the invite each one came through.
        let (events, _cursor) = crate::db::community::get_guestbook(cid_hex).unwrap_or_default();
        let mut joined_at: HashMap<String, u64> = HashMap::new();
        let mut invite_label: HashMap<String, String> = HashMap::new();
        for e in &events {
            let GuestbookEntry::Join { member, invited_by, at_ms } = &e.entry else { continue };
            let Ok(b32) = member.to_bech32();
            // Earliest join wins: a rejoin must not erase real tenure.
            let slot = joined_at.entry(b32.clone()).or_insert(*at_ms);
            *slot = (*slot).min(*at_ms);
            if let Some((_creator, label)) = invited_by {
                invite_label.entry(b32).or_insert_with(|| label.clone());
            }
        }

        let footprints: HashMap<String, crate::db::community::AuthorFootprint> =
            crate::db::community::community_author_footprints(cid_hex)
                .unwrap_or_default()
                .into_iter()
                .map(|f| (f.npub.clone(), f))
                .collect();

        // Cohort evidence comes from the recent window only — a raid is by definition
        // recent, and the whole history would cost a vault decrypt per row.
        const TEXT_WINDOW: usize = 4_000;
        let mut texts: HashMap<String, Vec<String>> = HashMap::new();
        for (npub, _at, text) in crate::db::community::community_recent_texts(cid_hex, TEXT_WINDOW).unwrap_or_default() {
            texts.entry(npub).or_default().push(text);
        }

        let signals: Vec<raid::MemberSignals> = members
            .iter()
            .map(|npub| {
                let f = footprints.get(npub);
                raid::MemberSignals {
                    npub: npub.clone(),
                    joined_at_ms: joined_at.get(npub).copied().unwrap_or(0),
                    invite_label: invite_label.get(npub).cloned(),
                    messages: f.map(|f| f.messages).unwrap_or(0),
                    first_secs: f.map(|f| f.first_secs).unwrap_or(0),
                    last_secs: f.map(|f| f.last_secs).unwrap_or(0),
                    texts: texts.get(npub).cloned().unwrap_or_default(),
                    is_owner: *npub == owner_b32,
                    is_admin: staff.contains(npub),
                    is_me: *npub == me_b32,
                }
            })
            .collect();

        let params = raid::RaidParams {
            known_shortcodes: crate::emoji_packs::known_shortcodes(),
            ..Default::default()
        };
        let report = std::sync::Arc::new(raid::assess(&signals, now_secs, &params));
        raid_report_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cid_hex.to_string(), (now_secs, report.clone()));
        Ok(report)
    }

    /// The preset library: what a designer offers instead of a blank document.
    pub fn policy_presets(&self) -> Result<serde_json::Value> {
        let presets: Vec<serde_json::Value> = crate::community::policy::presets::all()
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "description": p.description,
                    "example": p.example,
                    "caveat": p.caveat,
                    "dials": p.dials,
                    // Every rule in plain language: a template whose behaviour
                    // you can only infer from its name is the same black box
                    // the shipped defaults used to be.
                    "rules": crate::community::policy::presets::describe(&p.policy),
                    "bytes": serde_json::to_string(&p.policy).unwrap_or_default(),
                })
            })
            .collect();
        // The from-scratch builder's catalogue rides along: it is fetched at the
        // same moment, and its numbers must come from the same place the
        // presets' do.
        let rule_kinds: Vec<serde_json::Value> = crate::community::policy::presets::rule_kinds()
            .into_iter()
            .map(|k| {
                serde_json::json!({
                    "id": k.id,
                    "label": k.label,
                    "description": k.description,
                    "input": k.input,
                    "input_label": k.input_label,
                    "input_hint": k.input_hint,
                    "rule": k.rule,
                })
            })
            .collect();
        Ok(serde_json::json!({ "presets": presets, "rule_kinds": rule_kinds }))
    }

    /// What a candidate policy WOULD do, against real history. Stores nothing,
    /// publishes nothing, removes nobody — the designer never enables a policy
    /// from a form alone, only from a preview that named who it would catch.
    pub fn preview_community_policy(&self, community_id: &str, bytes: &str) -> Result<serde_json::Value> {
        use crate::community::v2::guestbook::GuestbookEntry;
        use nostr_sdk::prelude::PublicKey;
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("policies require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let owner = community.owner().map_err(VectorError::Other)?;
        let me = state::my_public_key();

        use crate::community::roles::Permissions;
        const MOD_MASK: u64 = Permissions::MANAGE_ROLES
            | Permissions::MANAGE_CHANNELS
            | Permissions::MANAGE_METADATA
            | Permissions::KICK
            | Permissions::BAN
            | Permissions::MANAGE_MESSAGES;
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
        let staff: std::collections::HashSet<String> = roster
            .grants
            .iter()
            .filter(|g| {
                g.role_ids
                    .iter()
                    .any(|rid| roster.roles.iter().any(|r| &r.role_id == rid && (r.permissions.0 & MOD_MASK) != 0))
            })
            .map(|g| g.member.clone())
            .collect();
        let roles_of: std::collections::HashMap<String, Vec<String>> =
            roster.grants.iter().map(|g| (g.member.clone(), g.role_ids.clone())).collect();

        let (events, _cursor) = crate::db::community::get_guestbook(&cid_hex).unwrap_or_default();
        let mut joined_at: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut invite_label: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for e in &events {
            let GuestbookEntry::Join { member, invited_by, at_ms } = &e.entry else { continue };
            let hex = member.to_hex();
            let slot = joined_at.entry(hex.clone()).or_insert(*at_ms);
            *slot = (*slot).min(*at_ms);
            if let Some((_c, label)) = invited_by {
                invite_label.entry(hex).or_insert_with(|| label.clone());
            }
        }
        let members: Vec<(PublicKey, Option<u64>, bool, Vec<String>, Option<String>)> =
            crate::community::v2::service::stored_memberlist(&community)
                .unwrap_or_default()
                .into_iter()
                .map(|pk| {
                    let hex = pk.to_hex();
                    (
                        pk,
                        joined_at.get(&hex).copied(),
                        staff.contains(&hex),
                        roles_of.get(&hex).cloned().unwrap_or_default(),
                        invite_label.get(&hex).cloned(),
                    )
                })
                .collect();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let assembled =
            crate::community::policy::harness::assemble(&cid_hex, &owner, me.as_ref(), &members, now_ms)
                .map_err(VectorError::Other)?;
        let preview = crate::community::policy::harness::preview_policy(&assembled, bytes, now_ms);
        serde_json::to_value(preview).map_err(|e| VectorError::Other(e.to_string()))
    }

    /// A community's policies, as stored. Returns the exact bytes, so an editor
    /// round-trips what was written rather than a re-serialization of it.
    pub fn list_community_policies(&self, community_id: &str) -> Result<serde_json::Value> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("policies require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let stored = crate::db::community::get_community_policies(&cid_hex).map_err(VectorError::Other)?;
        let rows: Vec<serde_json::Value> = stored
            .iter()
            .map(|p| {
                // Report what the validator says NOW: a policy stored under an
                // older engine can stop validating, and a moderator must see
                // that rather than assume their rules are running.
                let verdict = serde_json::from_str::<crate::community::policy::document::Policy>(&p.bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|doc| doc.validate().map_err(|r| format!("{r:?}")));
                serde_json::json!({
                    "policy_id": p.policy_id,
                    "hash": p.hash,
                    "enabled": p.enabled,
                    "updated_at": p.updated_at,
                    "bytes": p.bytes,
                    "valid": verdict.is_ok(),
                    "error": verdict.err(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "community_id": cid_hex,
            "policies": rows,
            // Must agree with `select_policies`, which is the thing that
            // actually decides: the shipped defaults run unless the community
            // has forked them. A flag derived any other way tells a moderator
            // their cover is on while the engine has stood it down.
            "using_builtin": !stored
                .iter()
                .any(|p| p.policy_id == crate::community::policy::harness::DEFAULTS_POLICY_ID),
        }))
    }

    /// Store a policy, validating FIRST. An invalid policy is a rejected edit,
    /// never a stored one that quietly evaluates to nothing.
    pub fn set_community_policy(&self, community_id: &str, policy_id: &str, bytes: &str, enabled: bool) -> Result<serde_json::Value> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("policies require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        if bytes.len() > crate::community::policy::types::caps::MAX_POLICY_BYTES {
            return Err(VectorError::Other("policy exceeds the maximum document size".into()));
        }
        let doc: crate::community::policy::document::Policy =
            serde_json::from_str(bytes).map_err(|e| VectorError::Other(format!("policy is not valid JSON: {e}")))?;
        doc.validate().map_err(|r| VectorError::Other(format!("policy rejected: {r:?}")))?;

        let hash = crate::community::policy::harness::hash_policy_bytes(bytes.as_bytes());
        let hash_hex = crate::simd::hex::bytes_to_hex_32(&hash.0);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        crate::db::community::set_community_policy(&cid_hex, policy_id, bytes, &hash_hex, enabled, now_secs)
            .map_err(VectorError::Other)?;
        // The console must not keep serving verdicts from the rules that were
        // in force a minute ago.
        Self::invalidate_raid_report(&cid_hex);
        Ok(serde_json::json!({ "policy_id": policy_id, "hash": hash_hex, "enabled": enabled }))
    }

    pub fn delete_community_policy(&self, community_id: &str, policy_id: &str) -> Result<()> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("policies require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::delete_community_policy(&cid_hex, policy_id).map_err(VectorError::Other)?;
        Self::invalidate_raid_report(&cid_hex);
        Ok(())
    }

    /// The built-in policy, as editable bytes — the starting point an editor
    /// offers instead of a blank document.
    pub fn builtin_policy_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&crate::community::policy::harness::default_policy())
            .map_err(|e| VectorError::Other(e.to_string()))
    }

    /// The moderation console's report, from the POLICY ENGINE. Memoised for
    /// [`RAID_REPORT_TTL_SECS`], same as the assessor it replaces.
    ///
    /// `raid.rs` still runs — [`Self::policy_side_by_side`] diffs the two — but
    /// the panel reads this. The engine earned it on live data: it clustered a
    /// real raid out of local storage and stayed silent on a healthy community
    /// across six runs.
    fn policy_console_report(cid_hex: &str, community: &crate::community::v2::community::CommunityV2) -> Result<std::sync::Arc<serde_json::Value>> {
        use crate::community::v2::guestbook::GuestbookEntry;
        use nostr_sdk::prelude::PublicKey;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        {
            let cache = policy_report_cache();
            let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, cached)) = guard.get(cid_hex) {
                if now_secs.saturating_sub(*at) < RAID_REPORT_TTL_SECS {
                    return Ok(cached.clone());
                }
            }
        }

        let owner = community.owner().map_err(VectorError::Other)?;
        let me = state::my_public_key();

        // Staff = a role carrying moderation permissions. NOT mere role
        // membership: a cosmetic or self-serve role must never confer immunity
        // from the rules this engine exists to enforce.
        use crate::community::roles::Permissions;
        const MOD_MASK: u64 = Permissions::MANAGE_ROLES
            | Permissions::MANAGE_CHANNELS
            | Permissions::MANAGE_METADATA
            | Permissions::KICK
            | Permissions::BAN
            | Permissions::MANAGE_MESSAGES;
        let roster = crate::db::community::get_community_roles(cid_hex).unwrap_or_default();
        let staff: std::collections::HashSet<String> = roster
            .grants
            .iter()
            .filter(|g| {
                g.role_ids
                    .iter()
                    .any(|rid| roster.roles.iter().any(|r| &r.role_id == rid && (r.permissions.0 & MOD_MASK) != 0))
            })
            .map(|g| g.member.clone())
            .collect();
        let roles_of: std::collections::HashMap<String, Vec<String>> =
            roster.grants.iter().map(|g| (g.member.clone(), g.role_ids.clone())).collect();

        let (events, _cursor) = crate::db::community::get_guestbook(cid_hex).unwrap_or_default();
        let mut joined_at: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut invite_label: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for e in &events {
            let GuestbookEntry::Join { member, invited_by, at_ms } = &e.entry else { continue };
            let hex = member.to_hex();
            // Earliest join wins: a rejoin must not erase real tenure.
            let slot = joined_at.entry(hex.clone()).or_insert(*at_ms);
            *slot = (*slot).min(*at_ms);
            if let Some((_creator, label)) = invited_by {
                invite_label.entry(hex).or_insert_with(|| label.clone());
            }
        }

        let members: Vec<(PublicKey, Option<u64>, bool, Vec<String>, Option<String>)> =
            crate::community::v2::service::stored_memberlist(community)
                .unwrap_or_default()
                .into_iter()
                .map(|pk| {
                    let hex = pk.to_hex();
                    (
                        pk,
                        joined_at.get(&hex).copied(),
                        staff.contains(&hex),
                        roles_of.get(&hex).cloned().unwrap_or_default(),
                        invite_label.get(&hex).cloned(),
                    )
                })
                .collect();

        let now_ms = now_secs.saturating_mul(1000);
        let assembled =
            crate::community::policy::harness::assemble(cid_hex, &owner, me.as_ref(), &members, now_ms)
                .map_err(VectorError::Other)?;
        let report = std::sync::Arc::new(
            crate::community::policy::harness::evaluate_for_console(cid_hex, &assembled, now_ms)
                .map_err(VectorError::Other)?,
        );
        policy_report_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cid_hex.to_string(), (now_secs, report.clone()));
        Ok(report)
    }

    /// Run the policy engine beside the shipped assessor and report where they
    /// disagree. Diagnostic only: the engine convicts nothing in production
    /// until this diff has been read against real data.
    pub fn policy_side_by_side(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::v2::guestbook::GuestbookEntry;
        use nostr_sdk::prelude::PublicKey;
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("moderation tools require a Concord v2 community".into()))?;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let owner = community.owner().map_err(VectorError::Other)?;

        // The moderation permission bits: holding one of these is staff. Mere
        // role membership is NOT — a cosmetic role must not confer immunity.
        use crate::community::roles::Permissions;
        const MOD_MASK: u64 = Permissions::MANAGE_ROLES
            | Permissions::MANAGE_CHANNELS
            | Permissions::MANAGE_METADATA
            | Permissions::KICK
            | Permissions::BAN
            | Permissions::MANAGE_MESSAGES;
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
        let staff: std::collections::HashSet<String> = roster
            .grants
            .iter()
            .filter(|g| {
                g.role_ids.iter().any(|rid| {
                    roster.roles.iter().any(|r| &r.role_id == rid && (r.permissions.0 & MOD_MASK) != 0)
                })
            })
            .map(|g| g.member.clone())
            .collect();
        let roles_of: std::collections::HashMap<String, Vec<String>> =
            roster.grants.iter().map(|g| (g.member.clone(), g.role_ids.clone())).collect();

        let (events, _cursor) = crate::db::community::get_guestbook(&cid_hex).unwrap_or_default();
        let mut joined_at: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for e in &events {
            let GuestbookEntry::Join { member, at_ms, .. } = &e.entry else { continue };
            let hex = member.to_hex();
            let slot = joined_at.entry(hex).or_insert(*at_ms);
            *slot = (*slot).min(*at_ms);
        }

        let members: Vec<(PublicKey, Option<u64>, bool, Vec<String>)> =
            crate::community::v2::service::stored_memberlist(&community)
                .unwrap_or_default()
                .into_iter()
                .map(|pk| {
                    let hex = pk.to_hex();
                    (pk, joined_at.get(&hex).copied(), staff.contains(&hex), roles_of.get(&hex).cloned().unwrap_or_default())
                })
                .collect();

        // The assessor's own verdicts, for the diff.
        let assessments: Vec<(String, String)> = Self::raid_report(&cid_hex)
            .map(|r| {
                r.members
                    .iter()
                    .map(|m| (m.npub.clone(), format!("{:?}", m.verdict).to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let diff = crate::community::policy::harness::run_side_by_side(
            &cid_hex, &owner, &members, &assessments, now_ms,
        )
        .map_err(VectorError::Other)?;
        serde_json::to_value(diff).map_err(|e| VectorError::Other(e.to_string()))
    }

    /// Drop a community's memoised verdict. Every moderation action changes who is a
    /// member, so serving the pre-action answer would leave the badge accusing people
    /// who are already gone.
    pub fn invalidate_raid_report(community_id: &str) {
        policy_report_cache().lock().unwrap_or_else(|e| e.into_inner()).remove(community_id);
        raid_report_cache().lock().unwrap_or_else(|e| e.into_inner()).remove(community_id);
    }

    /// Rotate the Community's keys with no banlist edit: mint a new root epoch that
    /// only `retain` can follow. This is the containment that scales — a wave of
    /// thousands cannot fit in a 500-entry banlist, but it can be locked out of the
    /// next epoch in one publish. `retain` is a keep-list; everyone else is cut.
    ///
    /// Passing an empty `retain` rotates without removing anyone, which is what you
    /// want when the invite link leaked but the members are all real.
    pub async fn refound_community(&self, community_id: &str, retain: &[&str]) -> Result<()> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("re-founding requires a Concord v2 community".into()))?;
        let removed = Self::members_to_remove(&community, retain)?;
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(30));
        crate::community::v2::service::refound_community(&transport, &community, &removed)
            .await
            .map_err(VectorError::Other)?;
        Ok(())
    }

    /// The members a `retain` keep-list would cut.
    ///
    /// Folded from the LOCAL store, and that is not an oversight. The network memberlist
    /// is epoch-scoped — its Guestbook plane holds only the current epoch's snapshot, and
    /// its channel reads only return what is decryptable now — so members stranded on a
    /// buried epoch are already absent from it. Those are precisely the phantoms this
    /// panel exists to clear: they survive ONLY in the local store, which accumulates
    /// across epochs. Folding the network list here returns an empty removal set and the
    /// sweep does nothing.
    ///
    /// The two lists answer different questions and must not be unified. "Who receives a
    /// new key" is the rotation's own network fold, which it does for itself. "Who is a
    /// phantom to evict" is this one, and being local-only is the definition of it.
    ///
    /// An empty `retain` removes nobody — a rotation for its own sake. You and the owner
    /// are never in the result: a keep-list omitting either is a caller mistake, and
    /// honouring it would lock the community's own staff out of an epoch they just minted.
    fn members_to_remove(
        community: &crate::community::v2::community::CommunityV2,
        retain: &[&str],
    ) -> Result<Vec<nostr_sdk::prelude::PublicKey>> {
        use nostr_sdk::prelude::PublicKey;
        let keep: std::collections::HashSet<PublicKey> =
            retain.iter().filter_map(|n| PublicKey::parse(n).ok()).collect();
        if keep.is_empty() {
            return Ok(Vec::new());
        }
        let me = state::my_public_key();
        let owner = community.owner().ok();
        Ok(crate::community::v2::service::stored_memberlist(community)
            .map_err(VectorError::Other)?
            .into_iter()
            .filter(|pk| !keep.contains(pk))
            .filter(|pk| Some(*pk) != me && Some(*pk) != owner)
            .collect())
    }

    /// Rotate the keys around the survivors, then kick the removed off the roster.
    ///
    /// The order is load-bearing, and it is ROTATE FIRST. A Guestbook plane is derived
    /// per epoch, so kicks published before the rotation land on a plane the rotation
    /// immediately buries: every reader then has to receive them in the window before it
    /// follows the rekey, and whatever is still in flight is never folded by anyone.
    /// Publishing onto the NEW epoch instead puts the departures on the plane every
    /// survivor will read from now on, so a client that catches up an hour later still
    /// folds them. It also closes the re-admission window — the removed hold no key at
    /// the new epoch, so they cannot post their way back into the memberlist between
    /// their own kick and the rotation.
    ///
    /// The rotation alone would not clear the roster: the guestbook store is keyed by
    /// community and appended across epochs, and the memberlist re-admits observed
    /// authors from local history regardless of epoch. The kicks are what evict them.
    pub async fn purge_and_refound(
        &self,
        community_id: &str,
        retain: &[&str],
        on_progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<serde_json::Value> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("re-founding requires a Concord v2 community".into()))?;
        let removed = Self::members_to_remove(&community, retain)?;
        // The operator's list IS the recipient set. `removed` is derived from the
        // memberlist at THIS instant and only drives the kick sweep below; the refound
        // fetches its own, later. Passing removal alone let an account that appeared in
        // between be vended a key by default — implicitly retained by the purge.
        let keep: Vec<nostr_sdk::prelude::PublicKey> = retain
            .iter()
            .filter_map(|n| nostr_sdk::prelude::PublicKey::parse(n).ok())
            .collect();
        log_info!("[Moderation] purge: {} to remove, {} retained", removed.len(), keep.len());
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(30));
        crate::community::v2::service::refound_community_retaining(&transport, &community, &removed, &keep)
            .await
            .map_err(VectorError::Other)?;
        // Re-load onto the epoch the rotation just minted: kicking against the stale
        // struct would address the plane we just buried, which is the whole bug this
        // ordering exists to avoid.
        let fresh = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("community gone during the purge".into()))?;
        let sweep = crate::community::v2::service::kick_members(&transport, &fresh, &removed, on_progress)
            .await
            .map_err(VectorError::Other)?;
        // Fold our own directives back. Without this the local roster only shows what
        // the live subscription happened to echo before the sweep finished.
        let _ = crate::community::v2::service::sync_guestbook(&transport, &fresh).await;
        log_info!(
            "[Moderation] purge: rotated to epoch {}, kicked {}/{} ({} refused, {} failed)",
            fresh.root_epoch.0,
            sweep.kicked.len(),
            removed.len(),
            sweep.refused.len(),
            sweep.failed.len()
        );
        Ok(serde_json::json!({
            "kicked": sweep.kicked.len(),
            "refused": sweep.refused.len(),
            "failed": sweep.failed.len(),
            "rotated": true,
        }))
    }

    /// Kick many members without rotating. Exposed for callers that want the roster
    /// cleaned but the keys left alone.
    pub async fn kick_community_members(
        &self,
        community_id: &str,
        npubs: &[&str],
        on_progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<serde_json::Value> {
        let community = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("kicking requires a Concord v2 community".into()))?;
        use nostr_sdk::prelude::PublicKey;
        let targets: Vec<PublicKey> = npubs.iter().filter_map(|n| PublicKey::parse(n).ok()).collect();
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(30));
        let sweep = crate::community::v2::service::kick_members(&transport, &community, &targets, on_progress)
            .await
            .map_err(VectorError::Other)?;
        let _ = crate::community::v2::service::sync_guestbook(&transport, &community).await;
        log_info!(
            "[Moderation] kick sweep: {}/{} kicked ({} refused, {} failed)",
            sweep.kicked.len(),
            targets.len(),
            sweep.refused.len(),
            sweep.failed.len()
        );
        Ok(serde_json::json!({
            "kicked": sweep.kicked.len(),
            "refused": sweep.refused.iter().map(|(_, why)| why.clone()).collect::<Vec<_>>(),
            "failed": sweep.failed.len(),
        }))
    }

    /// Retire every public invite link this Community holds. A raid arrives through a
    /// link, and revoking them one at a time loses the race — the whole set has to go
    /// in one pass before the rotation, or the next wave walks in through the survivor.
    /// Returns `(revoked, failed)`.
    pub async fn revoke_all_public_invites(&self, community_id: &str) -> Result<(usize, usize)> {
        let tokens: Vec<String> = crate::db::community::list_public_invites(community_id)
            .map_err(VectorError::Other)?
            .into_iter()
            .map(|i| i.token)
            .collect();
        let mut revoked = 0usize;
        let mut failed = 0usize;
        for token in &tokens {
            match self.revoke_public_invite(community_id, token).await {
                Ok(()) => revoked += 1,
                Err(_) => failed += 1,
            }
        }
        Ok((revoked, failed))
    }

    /// One synchronous v2 follow pass — rekeys first (a base adopt moves the
    /// control address), then a control refold on the FRESH state, the same order
    /// the live follow worker runs. Returns non-fatal warnings.
    /// Walk one community's rekeys + control fold NOW, serialized against the live
    /// follow worker by the per-community lock. Public so boot can converge a community
    /// the probe saw move BEFORE paging its content — content read at a superseded
    /// epoch is wasted work, and may not open at all.
    pub async fn v2_inline_follow(id: &crate::community::CommunityId) -> Vec<String> {
        crate::db::scoped(async move {
            use crate::community::transport::LiveTransport;
            let session = crate::db::current_session();
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
                    // An authorized rotation that excluded us IS a removal — but keep the
                    // epoch keys, exactly as every other self-removal path does. They grant
                    // no future read (post-removal keys are never delivered); dropping them
                    // only destroys the ability to author a 3305 self-delete of our OWN past
                    // messages, each sealed under the epoch it was sent at. That is the one
                    // thing a removed member should still be able to do — and doubly so if
                    // the removal turns out to have been a false positive.
                    let _ = crate::db::community::delete_community_retain_keys(&cid_hex);
                    return warnings;
                }
                Ok(_) => {}
                Err(e) => warnings.push(format!("v2 rekey follow failed: {e}")),
            }
            if let Ok(Some(fresh)) = crate::db::community::load_community_v2(id) {
                match crate::community::v2::service::follow_control(&transport, &fresh).await {
                    // Walk the rekeys once more whenever the control fold moved ANYTHING.
                    // The document half reveals rekey work that predates it (a just-announced
                    // private channel's key crate already sits on its rekey plane). The
                    // AUTHORITY half is the one that strands: the walk above gates every
                    // rotation on the roster and citation heads this fold just wrote, so a
                    // promote-then-rotate is refused on pass 1 and, without this, never
                    // reconsidered. Re-read from the DB — the authority half moves columns
                    // the returned document does not carry.
                    Ok(control) if control.moved() => {
                        if let Ok(Some(latest)) = crate::db::community::load_community_v2(id) {
                            if let Err(e) = crate::community::v2::service::follow_rekeys(&transport, &latest, &session).await {
                                warnings.push(format!("v2 rekey follow failed: {e}"));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => warnings.push(format!("v2 control follow failed: {e}")),
                }
            }
            // Banned by the freshly-folded banlist: a removal just like the rotation
            // exclusion above, and it arrives FIRST (CORD-04 §6 orders the Banlist edition
            // before the Refounding), so keying removal solely off the rotation leaves a
            // banned headless client running against a community that already dropped it.
            if let Some(me) = crate::my_public_key() {
                if crate::db::community::is_author_banned(&cid_hex, &me) {
                    let _ = crate::db::community::delete_community_retain_keys(&cid_hex);
                }
            }
            warnings
        })
        .await
    }

    /// Fetch a v2 channel's recent chat history and PERSIST it into the shared events
    /// tables (the same store v1 uses), so `get_messages`/`get_new_messages` backfill for
    /// v2 exactly like v1. PAGES backwards until it reaches messages it already holds
    /// (bounded), so a reconnecting bot that slept through more than one page of traffic
    /// still catches the whole gap instead of only the newest `limit`. Reuses the v2
    /// inbound bridge (dedup + STATE aggregate) + the v1 save path. Returns the count of
    /// brand-new messages applied. Best-effort: a fetch failure is 0.
    /// Reconnect/boot catch-up for one v2 channel: fetches the newest pages
    /// and PAGES backwards until it reaches messages it already holds, then
    /// ingests through the shared pipeline. The boot volley fetches its own
    /// batches and shares only [`Self::v2_ingest_chat_page`].
    pub(crate) async fn v2_backfill_channel(
        id: &crate::community::CommunityId,
        channel_id: &str,
        limit: usize,
        max_pages: usize,
        since: Option<u64>,
        until: Option<u64>,
        evidence: crate::community::transport::Evidence,
        transport_secs: u64,
    ) -> usize {
        Self::v2_backfill_channel_counted(id, channel_id, limit, max_pages, since, until, evidence, transport_secs)
            .await
            .new_messages
    }

    /// [`v2_backfill_channel`](Self::v2_backfill_channel) reporting what the RELAY
    /// served as well as what was new. A back-page over history we already hold is
    /// new-count 0 with a non-empty fetch, so only `fetched` can answer "is there
    /// anything older" — new-count alone stops a scroll at the first known page.
    pub(crate) async fn v2_backfill_channel_counted(
        id: &crate::community::CommunityId,
        channel_id: &str,
        limit: usize,
        max_pages: usize,
        since: Option<u64>,
        until: Option<u64>,
        evidence: crate::community::transport::Evidence,
        transport_secs: u64,
    ) -> BackfillCount {
        // Guard straddles the fetch: a swap mid-fetch must not persist account A's chat
        // into account B's STATE/DB (the message ids are global).
        let session = crate::db::current_session();
        let Some(my_pk) = state::my_public_key() else { return BackfillCount::default() };
        // CORD-02 §9: a dissolved community honors no NEW events — old history reads
        // through the explicit paths, but a catch-up sweep must not ingest anything
        // authored into the grave.
        if crate::db::community::get_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&id.0)).unwrap_or(false) {
            return BackfillCount::default();
        }
        let Ok(Some(community)) = crate::db::community::load_community_v2(id) else { return BackfillCount::default() };
        let ch = crate::community::ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(transport_secs));
        let Ok(page) = crate::community::v2::service::fetch_channel_history(
            &transport,
            &community,
            &ch,
            limit.max(50),
            max_pages,
            since,
            until,
            evidence,
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
            return BackfillCount::default();
        };
        let fetched = page.len();
        let new_messages = Self::v2_ingest_chat_page(channel_id, my_pk, session, page).await;
        BackfillCount { fetched, new_messages }
    }

    /// Ingest a fetched chat page: STATE apply, batched persist with delete
    /// barriers, then UI surfacing — shared by the reconnect backfill and the
    /// boot volley's batched paint path.
    pub(crate) async fn v2_ingest_chat_page(
        channel_id: &str,
        my_pk: nostr_sdk::prelude::PublicKey,
        session: std::sync::Arc<crate::db::Session>,
        page: Vec<crate::community::v2::service::FetchedEvent>,
    ) -> usize {
        crate::db::scoped(async move {
            use crate::community::v2::inbound::{apply_chat_to_state, ChatPersist};
            let mut new = 0usize;
            // Pass 1 — apply to STATE (per-item lock) and COLLECT outcomes in wire order.
            let mut outcomes: Vec<ChatPersist> = Vec::with_capacity(page.len());
            for f in &page {
                // A backfilled WebXDC peer ad persists through the shared 30078 row
                // (recency-gated at read) so a reopening lobby lists peers who
                // advertised while this device was closed — v1 sync parity. Own
                // echoes drop; the ad is not a chat row.
                if let crate::community::v2::chat::ChatEvent::Webxdc { opened } = &f.event {
                    if opened.author != my_pk {
                        if let Some((topic, addr)) = crate::webxdc::parse_peer_signal(&opened.rumor.content) {
                            let Ok(npub) = ToBech32::to_bech32(&opened.author);
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
                    continue;
                }
                let outcome = {
                    let mut st = state::STATE.lock().await;
                    apply_chat_to_state(&mut st, &f.event, channel_id, &my_pk)
                };
                if let Some(outcome) = outcome {
                    if matches!(outcome, ChatPersist::New(_)) {
                        new += 1;
                    }
                    outcomes.push(outcome);
                }
            }
            // Pass 2 — persist: message saves COLLECT into batched transactions; deletes are
            // flush barriers (a save committing after a delete it preceded on the wire would
            // resurrect the deleted row). One tx per page in the common no-delete case.
            let mut pending: Vec<&crate::types::Message> = Vec::new();
            for outcome in &outcomes {
                if !session.is_live() {
                    pending.clear();
                    break;
                }
                match outcome {
                    ChatPersist::New(m) => pending.push(m),
                    ChatPersist::Updated { message, edit_event } => match edit_event {
                        Some(ev) => {
                            let mut ev = (**ev).clone();
                            // get-or-CREATE: a lookup-only id would leave a fresh channel's edit at
                            // chat_id 0 (orphaned, dropped on the reload fold).
                            if let Ok(cid) = crate::db::id_cache::get_or_create_chat_id(channel_id) {
                                ev.chat_id = cid;
                            }
                            let _ = crate::db::events::save_event(&ev).await;
                        }
                        None => pending.push(message),
                    },
                    ChatPersist::Removed(target_id) => {
                        crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
                        let _ = crate::db::events::delete_event(target_id).await;
                    }
                    ChatPersist::ReactionRemoved { reaction_id, message } => {
                        crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
                        let _ = crate::db::events::delete_event(reaction_id).await;
                        pending.push(message);
                    }
                }
            }
            crate::db::events::flush_message_batch(channel_id, &mut pending, &session).await;
            drop(pending);
            // Resolve each reply's quote before the emit: the renderer has no other source
            // for a parent outside its window and never retries. Placed AFTER the persists
            // so a parent carried by this same page resolves too. One query per page.
            {
                let quoted: Vec<&mut crate::types::Message> = outcomes
                    .iter_mut()
                    .filter_map(|o| match o {
                        ChatPersist::New(m)
                        | ChatPersist::Updated { message: m, .. }
                        | ChatPersist::ReactionRemoved { message: m, .. } => Some(m),
                        ChatPersist::Removed(_) => None,
                    })
                    .collect();
                let _ = crate::db::events::populate_reply_contexts(quoted).await;
            }
            // Pass 3 — surface to the live UI, mirroring v1's sweep + the live dispatch handler:
            // a silent DB-only backfill left the chat-list preview, unread badge, and sort order
            // stale until the channel was opened. Raw emits (no notification ping) — a boot
            // catch-up must not fire an OS ping per message. Headless consumers register no
            // emitter, so these are a no-op there. After the persists so nothing surfaces unsaved.
            for outcome in &outcomes {
                match outcome {
                    ChatPersist::New(msg) => crate::traits::emit_event(
                        "message_new",
                        &serde_json::json!({ "message": msg, "chat_id": channel_id }),
                    ),
                    ChatPersist::Updated { message, .. }
                    | ChatPersist::ReactionRemoved { message, .. } => {
                        let mut message = message.clone();
                        let target_id = message.id.clone();
                        crate::traits::emit_message_update(channel_id, &target_id, &mut message).await;
                    }
                    ChatPersist::Removed(target_id) => crate::traits::emit_event(
                        "message_removed",
                        &serde_json::json!({ "id": target_id, "chat_id": channel_id, "reason": "deleted" }),
                    ),
                }
            }
            new
        })
        .await
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
        // Founding mask, not ADMIN_ALL: identifies Admin roles published before
        // later bits (PIN_MESSAGES...) existed.
        roles.roles.iter()
            .find(|r| matches!(r.scope, crate::community::roles::RoleScope::Server)
                && r.permissions.contains(crate::community::roles::Permissions::ADMIN_FOUNDING_MASK))
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
                    "pin_messages": false, "control_write": false,
                }));
            }
            // Actions that land as Control editions additionally need the plane's
            // write key (CORD-02 §2): a role-authorized staffer whose control_wrap
            // hasn't arrived yet factually cannot publish, and an honest false here
            // beats a bot retry-looping a publish every reader drops. KICK
            // (Guestbook) and MANAGE_MESSAGES (Chat planes) never need it.
            let control_write = crate::community::v2::control::ControlPlane::of(&v2).can_write();
            let has = |p: u64| roster.is_authorized(&me, Some(&owner_hex), p);
            let has_ctl = |p: u64| control_write && has(p);
            return Ok(serde_json::json!({
                "manage_metadata": has_ctl(Permissions::MANAGE_METADATA), "manage_channels": has_ctl(Permissions::MANAGE_CHANNELS),
                "create_invite": has_ctl(Permissions::CREATE_INVITE), "kick": has(Permissions::KICK), "ban": has_ctl(Permissions::BAN),
                "manage_messages": has(Permissions::MANAGE_MESSAGES), "manage_roles": has_ctl(Permissions::MANAGE_ROLES),
                // Only the owner (position 0) strictly outranks the position-1 Admin role.
                "manage_admin_role": me == owner_hex && control_write,
                "pin_messages": has_ctl(Permissions::PIN_MESSAGES),
                "control_write": control_write,
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
            // Pins are Concord v2 only (CORD-04 §7) — v1 has no control-plane entity for them.
            "pin_messages": false,
        }))
    }

    /// Pin a message in a v2 community channel (CORD-04 §7). The proof is
    /// rebuilt from the message's recovered wrap; see `service::pin_message`.
    pub async fn pin_community_message(&self, community_id: &str, channel_id: &str, message_id: &str) -> Result<()> {
        use crate::community::{transport::LiveTransport, ChannelId};
        let v2 = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("pins are only available in Concord v2 communities".into()))?;
        let ch = ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        crate::community::v2::service::pin_message(&transport, &v2, &ch, message_id)
            .await
            .map_err(VectorError::Other)
    }

    /// Unpin a message: the next Pin List edition without the entry.
    pub async fn unpin_community_message(&self, community_id: &str, channel_id: &str, message_id: &str) -> Result<()> {
        use crate::community::{transport::LiveTransport, ChannelId};
        let v2 = Self::load_v2_if_v2(community_id)?
            .ok_or_else(|| VectorError::Other("pins are only available in Concord v2 communities".into()))?;
        let ch = ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        crate::community::v2::service::unpin_message(&transport, &v2, &ch, message_id)
            .await
            .map_err(VectorError::Other)
    }

    /// Fetch a pinned attachment FROM THE PIN ALONE (CORD-04 §7's whole point:
    /// the proof carries the rumor, whose imeta carries the blob URL and the
    /// file's decryption material — no chat history and no old epoch key
    /// needed). Serves the already-downloaded copy when its content hash
    /// verifies; otherwise runs the full download walk (mirrors + hash-swap),
    /// decrypts, verifies, and caches at the same content-addressed path the
    /// chat pipeline uses. Returns `{ path, mime, name }`.
    pub async fn fetch_pinned_attachment(
        &self,
        community_id: &str,
        channel_id: &str,
        rumor_id: &str,
    ) -> Result<serde_json::Value> {
        crate::db::scoped(async move {
            use crate::community::ChannelId;
            let v2 = Self::load_v2_if_v2(community_id)?
                .ok_or_else(|| VectorError::Other("pins are only available in Concord v2 communities".into()))?;
            let ch = ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
            let pins = crate::community::v2::service::read_channel_pins(&v2, &ch).map_err(VectorError::Other)?;
            let pin = pins
                .pins
                .iter()
                .find(|p| p.rumor_id == rumor_id)
                .ok_or_else(|| VectorError::Other("that message is not pinned".into()))?;
            let dir = crate::db::get_download_dir();
            let tag = pin
                .tags
                .iter()
                .find(|t| t.first().map(String::as_str) == Some("imeta"))
                .map(|t| nostr_sdk::prelude::Tag::custom("imeta", t[1..].to_vec()))
                .ok_or_else(|| VectorError::Other("this pin carries no attachment".into()))?;
            let attachment = crate::community::attachments::attachment_from_imeta(&tag, &dir)
                .ok_or_else(|| VectorError::Other("this pin's attachment metadata is malformed".into()))?;

            let respond = |path: &std::path::Path| {
                serde_json::json!({
                    "path": path.to_string_lossy(),
                    "name": attachment.name.to_string(),
                    "extension": attachment.extension.to_string(),
                })
            };

            // The author-committed content address (the pin verified the rumor, so
            // `ox` carries the author's signature-weight claim — the same trust the
            // chat pipeline extends). Reuse and fresh downloads both verify it.
            let expected = attachment.original_hash.as_deref();
            let path = std::path::PathBuf::from(&*attachment.path);
            if let Ok(bytes) = std::fs::read(&path) {
                match expected {
                    Some(want) if crate::crypto::sha256_hex(&bytes) == want => return Ok(respond(&path)),
                    None => return Ok(respond(&path)),
                    _ => {} // stale or foreign bytes at the path: re-download
                }
            }

            let author_npub = nostr_sdk::prelude::PublicKey::from_hex(&pin.author)
                .ok()
                .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok());
            let bytes = self.download_attachment_from(&attachment, author_npub.as_deref()).await?;
            if let Some(want) = expected {
                if crate::crypto::sha256_hex(&bytes) != want {
                    return Err(VectorError::Other("downloaded bytes do not match the pinned content hash".into()));
                }
            }
            std::fs::write(&path, &bytes).map_err(|e| VectorError::Other(format!("could not cache the attachment: {e}")))?;
            Ok(respond(&path))
        })
        .await
    }

    /// A channel's verified pins from the locally folded head — local-only read.
    /// `sealed: true` means the list exists but is unreadable on this device;
    /// render it as unavailable, never as empty.
    pub fn get_channel_pins(&self, community_id: &str, channel_id: &str) -> Result<serde_json::Value> {
        use crate::community::ChannelId;
        let Some(v2) = Self::load_v2_if_v2(community_id)? else {
            // v1 channels simply have no pins — an empty read, not an error, so
            // one frontend path serves both stacks.
            return Ok(serde_json::json!({ "pins": [], "sealed": false, "version": 0 }));
        };
        let ch = ChannelId(crate::simd::hex::hex_to_bytes_32(channel_id));
        let pins = crate::community::v2::service::read_channel_pins(&v2, &ch).map_err(VectorError::Other)?;
        serde_json::to_value(&pins).map_err(|e| VectorError::Other(e.to_string()))
    }

    /// The community's owner npub + the admin npubs (role overview). A local read,
    /// like [`Self::community_capabilities`].
    /// The whole role graph: every role with its position and colour, and who
    /// holds what.
    ///
    /// `community_roles` above answers only "who is an admin", which is all the
    /// crown ever needed. A member list that groups people by rank needs the
    /// hierarchy itself, and npubs rather than hex so it can match a roster it
    /// already holds.
    pub fn community_role_graph(&self, community_id: &str) -> Result<serde_json::Value> {
        use nostr_sdk::prelude::{PublicKey, ToBech32};
        let roster = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
        // A banned member is gone from every read (§4), roles included.
        let banned = crate::db::community::get_community_banlist(community_id).unwrap_or_default();
        let roles: Vec<serde_json::Value> = roster
            .roles
            .iter()
            .map(|r| {
                serde_json::json!({
                    "role_id": r.role_id,
                    "name": r.name,
                    "position": r.position,
                    "color": r.color,
                })
            })
            .collect();
        let grants: Vec<serde_json::Value> = roster
            .grants
            .iter()
            .filter(|g| !banned.contains(&g.member))
            .filter_map(|g| {
                let npub = PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok())?;
                Some(serde_json::json!({ "npub": npub, "role_ids": g.role_ids }))
            })
            .collect();
        Ok(serde_json::json!({ "roles": roles, "grants": grants }))
    }

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

    /// Fold the v2 control plane back in right after publishing an authority change, so the
    /// LOCAL roster/banlist — which is what every read is served from (crowns, in-chat tags,
    /// capabilities, moderation gates) — is current by the time the call returns. `publish`
    /// only returns once a relay ACKed, so this refetch sees our own edition; the fold
    /// announces `community_refreshed` itself when the roster actually moved.
    ///
    /// Best-effort: the edition is already published, so a failed refold is a stale local
    /// cache the next follow repairs, never a failed action.
    async fn converge_v2_authority(
        transport: &crate::community::transport::LiveTransport,
        community_id: &str,
    ) {
        crate::db::scoped(async move {
            // Reload rather than reuse the caller's clone: the publish advanced edition floors,
            // and a rekey/refound may have moved the control address under us.
            if let Ok(Some(fresh)) = Self::load_v2_if_v2(community_id) {
                let _ = crate::community::v2::service::follow_control(transport, &fresh).await;
                // Membership is part of the view being converged: an unban must
                // re-fetch the Guestbook, because a Join that legally raced the ban
                // window may exist only on the relays — and our own just-published
                // edition doesn't echo back to trigger a follow.
                if let Ok(added) = crate::community::v2::service::sync_guestbook(transport, &fresh).await {
                    if !added.is_empty() {
                        traits::emit_event_json(
                            "community_refreshed",
                            serde_json::json!({ "community_id": community_id }),
                        );
                    }
                }
            }
        })
        .await
    }

    /// Grant a member the @admin role. Requires MANAGE_ROLES + outranking the role's position.
    pub async fn grant_admin(&self, community_id: &str, npub: &str) -> Result<()> {
        use crate::community::{service, transport::LiveTransport};
        let member = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        if let Some(v2) = Self::load_v2_if_v2(community_id)? {
            crate::community::v2::service::grant_admin(&transport, &v2, &member)
                .await
                .map_err(VectorError::Other)?;
            Self::converge_v2_authority(&transport, community_id).await;
            return Ok(());
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
            crate::community::v2::service::revoke_admin(&transport, &v2, &member)
                .await
                .map_err(VectorError::Other)?;
            Self::converge_v2_authority(&transport, community_id).await;
            return Ok(());
        }
        let community = Self::load_community_hex(community_id)?;
        let role_id = Self::admin_role_id_of(community_id)?;
        service::revoke_role(&transport, &community, member, &role_id).await.map_err(VectorError::Other)
    }

    /// Cooperatively kick a member — they self-remove but can rejoin. Requires KICK + outrank.
    pub async fn kick_member(&self, community_id: &str, npub: &str) -> Result<()> {
        crate::db::scoped(async move {
            use crate::community::{service, transport::LiveTransport};
            let pk = nostr_sdk::prelude::PublicKey::parse(npub).map_err(|_| VectorError::Other("invalid npub".into()))?;
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            if let Some(v2) = Self::load_v2_if_v2(community_id)? {
                crate::community::v2::service::kick_member(&transport, &v2, &pk)
                    .await
                    .map_err(VectorError::Other)?;
                // Catch the local Guestbook up on our own Kick, so the memberlist read
                // (which folds the STORE, not the network) drops them before this returns
                // instead of waiting on the relay echo. The control fold follows because a
                // Kick strips roles first (CORD-04 §6), which moves the roster too.
                if let Ok(fresh) = crate::community::v2::service::sync_guestbook(&transport, &v2).await {
                    if !fresh.is_empty() {
                        emit_event("community_refreshed", &serde_json::json!({ "community_id": community_id }));
                    }
                }
                Self::converge_v2_authority(&transport, community_id).await;
                return Ok(());
            }
            let community = Self::load_community_hex(community_id)?;
            let channel = community.channels.first().ok_or_else(|| VectorError::Other("community has no channel".into()))?;
            service::publish_kick(&transport, &community, channel, &pk.to_hex()).await.map(|_| ()).map_err(VectorError::Other)
        })
        .await
    }

    /// Ban (`true`) or unban (`false`) a member. Ban is terminal (no rejoin). The read cut:
    /// a private community re-founds (full rotation); a public one rotates just the private
    /// channels the member could read (CORD-05 §5 — no refound, the link refresh would
    /// re-hand them the root). Requires BAN + outrank, checked against the folded roster
    /// BEFORE any publish.
    pub async fn set_member_banned(&self, community_id: &str, npub: &str, banned: bool) -> Result<()> {
        self.set_members_banned(community_id, &[npub], banned).await
    }

    /// Ban (`true`) or unban (`false`) a whole batch as ONE moderation unit: one
    /// Banlist edition, one grant-strip pass, one read cut — a private community
    /// re-founds ONCE with every target removed, however many there are. Additive
    /// on ban, reductive on unban, over the freshest folded list either way.
    ///
    /// Serialized per community (with every other ban/unban) so concurrent
    /// moderation composes in arrival order instead of the last write erasing
    /// its siblings. The wire caps a Banlist at 500 entries — a batch that would
    /// exceed it fails before anything publishes.
    pub async fn set_members_banned(&self, community_id: &str, npubs: &[&str], banned: bool) -> Result<()> {
        use crate::community::{service, transport::LiveTransport, CommunityId};
        if npubs.is_empty() {
            return Ok(());
        }
        // Parse every target up front — one bad npub fails the batch before any publish.
        let mut pks: Vec<nostr_sdk::prelude::PublicKey> = Vec::with_capacity(npubs.len());
        for n in npubs {
            let pk = nostr_sdk::prelude::PublicKey::parse(n).map_err(|_| VectorError::Other(format!("invalid npub: {n}")))?;
            if !pks.contains(&pk) {
                pks.push(pk);
            }
        }
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        // Dual-stack: a v2 Ban is the CORD-04 §6 three-removal composition, in order —
        // the Banlist edition first (instant silence), then the Grant strip (authority
        // removal), then the Refounding read-cut (cryptographic severance).
        if community_id.len() == 64 {
            let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
            // A transient protocol-read failure must fail the ban loudly: flattened
            // to "not v2" it reroutes onto the v1 list no v2 reader folds — an Ok
            // that banned no one.
            if let Some(crate::community::ConcordProtocol::V2) = crate::db::community::community_protocol(&cid).map_err(VectorError::Other)? {
                return crate::community::v2::service::set_members_banned(&transport, &cid, &pks, banned)
                    .await
                    .map_err(VectorError::Other);
            }
        }
        // v1: same latest-wins mutation over the held list.
        let hexes: Vec<String> = pks.iter().map(|p| p.to_hex()).collect();
        let mut list = crate::db::community::get_community_banlist(community_id).map_err(VectorError::Other)?;
        list.retain(|h| !hexes.contains(h));
        if banned {
            list.extend(hexes);
        }
        let community = Self::load_community_hex(community_id)?;
        service::publish_banlist(&transport, &community, &list).await.map_err(VectorError::Other)
    }

    /// Repair a v2 community's roster cache: keep what is stored, but reset its
    /// PROVENANCE to zero so the next control fold is allowed to replace it.
    ///
    /// The completeness guard retains the cached roster whenever a stored entity is
    /// floored but folds no head — protection against a relay quietly stripping
    /// admins. If the cache itself is wrong, that same guard pins the wrong data in
    /// place: the fold can see the real roster and still be refused. Zeroing the
    /// provenance marks the cache a seed — a guess, not evidence — which the fold may
    /// overwrite. It publishes nothing and touches no key material; worst case the
    /// next fold writes what it can see, which is what a fresh join would get.
    pub async fn repair_community_roster(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::CommunityId;
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        if crate::db::community::load_community_v2(&cid).map_err(VectorError::Other)?.is_none() {
            return Err(VectorError::Other("not a held Concord v2 community".into()));
        }
        let before = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
        let before_at = crate::db::community::get_community_roles_at(community_id).map_err(VectorError::Other)?;
        crate::db::community::set_community_roles(community_id, &before, 0).map_err(VectorError::Other)?;
        // Fold immediately so the repair is one step, not two.
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(20));
        let community = crate::db::community::load_community_v2(&cid)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("community vanished mid-repair".into()))?;
        let follow = crate::community::v2::service::follow_control(&transport, &community).await;
        let after = crate::db::community::get_community_roles(community_id).map_err(VectorError::Other)?;
        Ok(serde_json::json!({
            "provenance_before": before_at,
            "provenance_after": crate::db::community::get_community_roles_at(community_id).unwrap_or(0),
            "roles_before": before.roles.len(),
            "grants_before": before.grants.len(),
            "roles_after": after.roles.len(),
            "grants_after": after.grants.len(),
            "fold_error": follow.as_ref().err().cloned(),
            "repaired": after.grants.len() > before.grants.len(),
        }))
    }

    /// Explain this account's epoch state for a v2 community: what it holds, what
    /// the next-epoch rekey plane offers, and why each rotation there was or was not
    /// adopted.
    ///
    /// This is the answer to "is this client stranded, and why" — a question that is
    /// otherwise unanswerable, because a stranded client sees only silence and
    /// silence looks exactly like a quiet community. Run it from two clients on one
    /// community to compare their views; the `held` block is where they diverge
    /// first. Read-only; returns public keys and verdicts, never key material.
    pub async fn explain_epoch_state(&self, community_id: &str) -> Result<serde_json::Value> {
        use crate::community::{transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        let community = crate::db::community::load_community_v2(&cid)
            .map_err(VectorError::Other)?
            .ok_or_else(|| VectorError::Other("not a held Concord v2 community".into()))?;
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(15));
        crate::community::v2::service::explain_epoch_state(&transport, &community)
            .await
            .map_err(VectorError::Other)
    }

    /// Contain a raid (v2 only): ban the named raiders, cut everyone who joined
    /// through the door during the raid window, and perform a SEVERING Refounding
    /// — the rotation that REVOKES the public invite links instead of carrying
    /// them into the new epoch.
    ///
    /// This is the raid lane. An ordinary ban keeps its shipped behaviour, where a
    /// Public community deliberately does not roll the root (the stable-URL refresh
    /// would undo it). Here the link IS the attack surface, so it closes.
    ///
    /// `window_start_secs` is the raid's opening edge (unix seconds); pass 0 to cut
    /// only the convicted. Returns an honest report — a failed refound is reported,
    /// never dressed up as a closed door.
    pub async fn contain_raid(
        &self,
        community_id: &str,
        npubs: &[&str],
        window_start_secs: u64,
        cut_also: &[&str],
    ) -> Result<crate::community::v2::service::ContainmentReport> {
        use crate::community::{transport::LiveTransport, CommunityId};
        if community_id.len() != 64 {
            return Err(VectorError::Other("malformed community id".into()));
        }
        let mut pks: Vec<nostr_sdk::prelude::PublicKey> = Vec::with_capacity(npubs.len());
        for n in npubs {
            let pk = nostr_sdk::prelude::PublicKey::parse(n).map_err(|_| VectorError::Other(format!("invalid npub: {n}")))?;
            if !pks.contains(&pk) {
                pks.push(pk);
            }
        }
        let mut extra: Vec<nostr_sdk::prelude::PublicKey> = Vec::with_capacity(cut_also.len());
        for n in cut_also {
            let pk = nostr_sdk::prelude::PublicKey::parse(n).map_err(|_| VectorError::Other(format!("invalid npub: {n}")))?;
            if !extra.contains(&pk) {
                extra.push(pk);
            }
        }
        let cid = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
        match crate::db::community::community_protocol(&cid).map_err(VectorError::Other)? {
            Some(crate::community::ConcordProtocol::V2) => {}
            // v1 has no severing rotation and no per-creator link custody; routing a
            // containment there would silently do something else entirely.
            _ => return Err(VectorError::Other("raid containment requires a Concord v2 community".into())),
        }
        // Unbound caller (SDK/command): a swap mid-containment would ban in one
        // account and rotate in another.
        let session = crate::db::current_session();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(20));
        let report = crate::community::v2::service::contain_raid(&transport, &cid, &pks, window_start_secs, &extra)
            .await
            .map_err(VectorError::Other)?;
        if !session.is_live() {
            return Err(VectorError::Other("account changed during raid containment".into()));
        }
        Ok(report)
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



    /// Leave a Community: announce a best-effort "left" presence (before dropping keys), then
    /// drop the held keys + local channel chats. You need a fresh invite to rejoin.
    pub async fn leave_community(&self, community_id: &str) -> Result<()> {
        crate::db::scoped(async move {
            use crate::community::{transport::LiveTransport, CommunityId};
            if community_id.len() != 64 {
                return Err(VectorError::Other("malformed community id".into()));
            }
            let id = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
            // v2: guestbook Leave + cross-device List tombstone + local delete, in the service.
            if let Some(v2) = Self::load_v2_if_v2(community_id)? {
                let channel_ids: Vec<String> =
                    v2.channels.iter().map(|ch| crate::simd::hex::bytes_to_hex_32(&ch.id.0)).collect();
                let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
                crate::community::v2::service::leave_community(&transport, &v2)
                    .await
                    .map_err(VectorError::Other)?;
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
        })
        .await
    }

    /// Resolve a channel id to its owning Community + the Channel (with its secret key).
    /// Refuse a WRITE into a sealed community (CORD-02 §9). Once dissolved, no honest
    /// peer accepts another event, so a send would sit pending forever with no reason
    /// given. v2 enforces this inside `chat_send_context`; v1 has no shared send gate,
    /// so each write path calls this.
    ///
    /// Reads are deliberately untouched — a dissolved community stays browsable.
    fn ensure_v1_writable(community: &crate::community::Community) -> Result<()> {
        if crate::db::community::get_community_dissolved(&community.id.to_hex()).unwrap_or(false) {
            return Err(VectorError::Other("this community has been dissolved".into()));
        }
        Ok(())
    }

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
        crate::db::scoped(async move {
            use futures_util::StreamExt;
            use nostr_sdk::prelude::*;

            let client = state::nostr_client()
                .ok_or(VectorError::Other("Not connected".into()))?;
            let my_pk = state::my_public_key()
                .ok_or(VectorError::Other("Not logged in".into()))?;

            // Load known wrapper IDs + timestamps for negentropy fingerprinting.
            // A windowed sync bounds the read in SQL: materialising all of
            // history to keep a few days of it is most of this function's cost
            // on an established account.
            let (items, filter) = if let Some(days) = since_days {
                let since_ts = Timestamp::now().as_secs().saturating_sub(days * 24 * 3600);
                let items = db::wrappers::load_negentropy_items_since(since_ts)
                    .unwrap_or_default();
                let filter = Filter::new()
                    .pubkey(my_pk)
                    .kind(Kind::GiftWrap)
                    .since(Timestamp::from_secs(since_ts));
                (items, filter)
            } else {
                let items = db::wrappers::load_negentropy_items().unwrap_or_default();
                let filter = Filter::new()
                    .pubkey(my_pk)
                    .kind(Kind::GiftWrap);
                (items, filter)
            };

            log_info!("[SyncDMs] {} negentropy items, since_days={:?}", items.len(), since_days);

            // Dry-run negentropy: exchange fingerprints to identify missing events
            let sync_opts = nostr_sdk::prelude::SyncOptions::new()
                .direction(nostr_sdk::prelude::SyncDirection::Down)
                .initial_timeout(std::time::Duration::from_secs(10))
                .dry_run();

            // Race all relays — first to reconcile drives the fetch. Relays with a
            // fresh no-NIP-77 verdict skip the doomed reconcile and get a bounded
            // REQ pass below instead.
            let relay_map = client.relays().await;
            let (all_relays, no_neg_relays): (Vec<(RelayUrl, Relay)>, Vec<(RelayUrl, Relay)>) =
                relay_map.iter()
                    .map(|(url, relay)| (url.clone(), relay.clone()))
                    .partition(|(url, _)| negentropy::neg_supported_cached(url.as_str()) != Some(false));
            drop(relay_map);
            let skipped_no_neg: Vec<String> = no_neg_relays.iter().map(|(u, _)| u.to_string()).collect();
            if !skipped_no_neg.is_empty() {
                log_info!("[SyncDMs] {} relay(s) on REQ path (no NIP-77)", skipped_no_neg.len());
            }

            // Tor-aware like the GUI path — a fixed clearnet budget over Tor makes
            // a healthy relay's slow first frame look like connected-silence and
            // earns it a false 24h no-NEG verdict in the shared account KV.
            let neg_budget = relay_request_timeout(std::time::Duration::from_secs(10));
            let neg_outer = neg_budget + std::time::Duration::from_secs(5);
            let connect_allowance = relay_request_timeout(std::time::Duration::from_secs(3))
                .min(neg_outer);
            let mut relay_futs = futures_util::stream::FuturesUnordered::new();
            for (url, relay) in &all_relays {
                let url = url.clone();
                let relay = relay.clone();
                let f = filter.clone();
                let i = items.clone();
                let o = sync_opts.clone();
                relay_futs.push(async move {
                    if !negentropy::wait_connected(&relay, connect_allowance).await {
                        return (url, None, false);
                    }
                    // Outer slack over the initial_timeout so the SDK's error
                    // (which distinguishes refusal from silence) surfaces first.
                    let result = tokio::time::timeout(
                        neg_outer,
                        relay.sync(f).items(i).opts(o),
                    ).await;
                    let connected = relay.status() == RelayStatus::Connected;
                    (url, Some(result), connected)
                });
            }

            // Collect missing IDs from all relays
            let cap_session = crate::db::current_session();
            let mut all_missing: std::collections::HashSet<EventId> = std::collections::HashSet::new();
            while let Some((url, result, connected)) = relay_futs.next().await {
                let Some(result) = result else {
                    log_warn!("[SyncDMs] {} skipped: not connected", url);
                    continue;
                };
                match result {
                    Ok(Ok(recon)) => {
                        let count = recon.remote.len();
                        all_missing.extend(recon.remote);
                        log_info!("[SyncDMs] {} reconciled: {} missing", url, count);
                        negentropy::record_neg_support(url.as_str(), true);
                    }
                    Ok(Err(e)) => {
                        log_warn!("[SyncDMs] {} failed: {}", url, e);
                        if cap_session.is_live()
                            && negentropy::classify_neg_sync_error(&e.to_string(), connected) == Some(false)
                        {
                            log_info!("[SyncDMs] {} marked no-NIP-77 for 24h", url);
                            negentropy::record_neg_support(url.as_str(), false);
                        }
                    }
                    Err(_) => log_warn!("[SyncDMs] {} timed out ({:?})", url, neg_outer),
                }
            }

            let mut total_events = 0u32;
            let mut new_messages = 0u32;

            // No-NIP-77 relays still contribute: one bounded REQ over the same
            // filter. The 500-event cap keeps a `since_days: None` call from
            // pulling a whole mailbox — deep history is negentropy's job on the
            // relays that speak it.
            if !skipped_no_neg.is_empty() {
                let req_filter = filter.clone().limit(500);
                match client
                    .stream_events(nostr_sdk::prelude::ReqTarget::manual(
                        skipped_no_neg.iter().cloned().map(|u| (u, vec![req_filter.clone()])),
                    ))
                    .timeout(std::time::Duration::from_secs(20))
                    .await
                {
                    Ok(stream) => {
                        let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
                        tokio::pin!(stream);
                        while let Some((_relay, res)) = stream.next().await {
                            let Ok(event) = res else { continue };
                            // Straddles the stream: a swap mid-drain must not push
                            // the old account's wrappers through the new account's
                            // pipeline (ErrorSkip would ledger them there).
                            if !seen.insert(event.id.to_bytes()) { continue; }
                            total_events += 1;
                            let prepared = event_handler::prepare_event(event, &client, my_pk).await;
                            if event_handler::commit_prepared_event(prepared, false, handler).await {
                                new_messages += 1;
                            }
                        }
                    }
                    Err(e) => log_warn!("[SyncDMs] REQ pass failed: {}", e),
                }
            }

            if all_missing.is_empty() {
                log_info!("[SyncDMs] No missing events");
                return Ok((total_events, new_messages));
            }

            // Fetch missing events in batches
            log_info!("[SyncDMs] Fetching {} missing events", all_missing.len());
            let ids: Vec<EventId> = all_missing.into_iter().collect();
            let relay_strs: Vec<String> = client.relays().await.keys()
                .map(|u| u.to_string()).collect();

            const BATCH_SIZE: usize = 500;

            for batch in ids.chunks(BATCH_SIZE) {
                // The #p is not redundant: Ditto refuses gift-wrap REQs that carry
                // neither authors nor #p, even authed — ids-only returns nothing.
                let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap).pubkey(my_pk);
                match client
                    .stream_events(nostr_sdk::prelude::ReqTarget::manual(
                        relay_strs.iter().cloned().map(|u| (u, vec![f.clone()])),
                    ))
                    .timeout(std::time::Duration::from_secs(30))
                    .await
                {
                    Ok(stream) => {
                        let client_clone = client.clone();
                        let prepared_stream = stream
                            .filter_map(|(_relay, res)| async move { res.ok() })
                            .map(move |event| {
                                let c = client_clone.clone();
                                db::spawn_bound(async move {
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
        })
        .await
    }

    // ========================================================================
    // Event Subscription
    // ========================================================================

    /// Subscribe to incoming DM events (NIP-17 GiftWraps).
    ///
    /// Returns the subscription ID for use in a custom notification loop.
    /// For a complete listen-and-process loop, use [`listen()`](Self::listen) instead.
    pub async fn subscribe_dms(&self) -> Result<nostr_sdk::prelude::SubscriptionId> {
        use nostr_sdk::prelude::*;
        let client = state::nostr_client()
            .ok_or(VectorError::Other("Not connected".into()))?;
        let my_pk = state::my_public_key()
            .ok_or(VectorError::Other("Not logged in".into()))?;

        let filter = Filter::new()
            .pubkey(my_pk)
            .kind(Kind::GiftWrap)
            .limit(0);

        let output = client.subscribe(filter).await
            .map_err(|e| VectorError::Nostr(e.to_string()))?;
        Ok(output.value)
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
    /// Returns each community's non-fatal follow warnings (rekey/control catch-up
    /// failures), prefixed with the id. A caller that discards them is blind to
    /// "the sync ran, but this account never adopted the rotation" — which reads
    /// exactly like a healthy quiet sync while the account sits on a dead epoch.
    pub async fn sync_communities(&self) -> Result<Vec<String>> {
        let mut all_warnings: Vec<String> = Vec::new();
        // Discover + rehydrate memberships from the Community List across devices (CORD-02 §8),
        // bootstrapping from the client's connected relays so even a fresh device that
        // holds no community yet can find them. Best-effort.
        {
            use crate::community::{transport::LiveTransport, v2::service as v2};
            let bootstrap: Vec<String> = match crate::state::nostr_client() {
                Some(client) => client.relays().await.keys().map(|r| r.to_string()).collect(),
                None => Vec::new(),
            };
            let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
            if let Ok(outcome) = v2::sync_community_list(&transport, &bootstrap).await {
                // Headless: core already dropped the rows; a GUI shell additionally clears the
                // chat rows + STATE via `removed` (see `ListSyncOutcome`).
                let joined = outcome.joined;
                for c in &joined {
                    if community::v2::realtime::follow_worker_running() {
                        community::v2::realtime::enqueue_follow(c.id());
                    } else {
                        let cid_hex = crate::simd::hex::bytes_to_hex_32(&c.id().0);
                        all_warnings.extend(
                            Self::v2_inline_follow(c.id()).await.into_iter().map(|w| format!("{}: {w}", &cid_hex[..8])),
                        );
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
                    let cid_hex = crate::simd::hex::bytes_to_hex_32(&id.0);
                    all_warnings.extend(
                        Self::v2_inline_follow(&id).await.into_iter().map(|w| format!("{}: {w}", &cid_hex[..8])),
                    );
                }
                // Back-fill each channel's chat, exactly as the v1 arm below does.
                // Without this a headless client only ever sees messages that arrive
                // LIVE: anything sent while it was down — or while it held no key for
                // a private channel — is never fetched, so a bot granted access reads
                // an empty room. Cheap when there is nothing new (id-deduped), and a
                // channel we cannot read simply yields nothing.
                if let Ok(Some(c)) = db::community::load_community_v2(&id) {
                    for ch in &c.channels {
                        let hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
                        let _ = Self::v2_backfill_channel(
                            &id, &hex, 50, 2, None, None,
                            crate::community::transport::Evidence::Fast, 12,
                        ).await;
                    }
                }
                continue;
            }
            if let Ok(Some(community)) = db::community::load_community(&id) {
                for ch in &community.channels {
                    let _ = self.sync_community_channel(&ch.id.to_hex(), 50).await;
                }
            }
        }
        Ok(all_warnings)
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

        // The subscriptions above are COMMITTED: live events flow from here. Say
        // so — the startup gauntlet (relay-connect wait + stream-auth priming)
        // can take a minute against a slow relay set, and without this signal a
        // bot cannot tell "still connecting" from "subscribed to nothing".
        // Fires even with 0 communities: that too is a completed, ready state.
        handler.on_subscription_ready(db::community::list_community_ids().map(|v| v.len()).unwrap_or(0));

        // Outage resilience via the relay Monitor — event-driven, not polling.
        //
        // (1) Reconnect-driven catch-up: a `limit(0)` realtime sub never replays what was published
        // while we were down, so a relay (re)connecting is exactly when we must catch up. On each
        // Connected transition we refold consensus + reconcile DMs (NIP-77 negentropy → only the
        // diff) and re-track the realtime sub at the current epochs. Idle when healthy. Stops on swap.
        if let Some(monitor) = client.monitor() {
            let mut rx = monitor.subscribe();
            db::spawn_bound(async move {
                // Debounce reconnect bursts: StatusChanged is per-relay, but one catch-up queries the
                // whole pool — so coalesce Connected transitions within a short window into one resync.
                let mut last_resync: Option<std::time::Instant> = None;
                while let Ok(notification) = rx.recv().await {
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
            db::spawn_bound(async move {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await; // warm-up
                loop {
                    for (url, relay) in client_health.relays().await {
                        match relay.status() {
                            RelayStatus::Connected => {
                                let probe = tokio::time::timeout(
                                    std::time::Duration::from_secs(10),
                                    client_health
                                        .fetch_events(nostr_sdk::prelude::ReqTarget::single(
                                            url.to_string(),
                                            [Filter::new().kind(Kind::Metadata).limit(1)],
                                        ))
                                        .timeout(std::time::Duration::from_secs(8)),
                                )
                                .await;
                                if !matches!(probe, Ok(Ok(_))) {
                                    let _ = relay.disconnect();
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                    let _ = relay.try_connect().timeout(crate::relay_connect_timeout(std::time::Duration::from_secs(10))).await;
                                }
                            }
                            RelayStatus::Terminated | RelayStatus::Disconnected => {
                                let _ = relay.try_connect().timeout(crate::relay_connect_timeout(std::time::Duration::from_secs(10))).await;
                            }
                            _ => {}
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            });
        }

        let client_for_closure = client.clone();

        // 0.45 removed `handle_notifications`; drive the stream directly. It ends when
        // the client shuts down, which is what stops this loop on `swap_session`.
        let mut notifications = client.notifications();
        while let Some(notification) = notifications.next().await {
            let handler = handler.clone();
            let c = client_for_closure.clone();
            let dm_sid = dm_sub_id.clone();
            {
                // Relay OKs feed the send pipeline: an OK that outlives the
                // per-attempt wait still confirms delivery, and can rescue a
                // message already marked Failed.
                if let nostr_sdk::prelude::ClientNotification::Message { message, .. } = &notification {
                    if let nostr_sdk::prelude::RelayMessage::Ok { event_id, status, .. } = &**message {
                        sending::note_relay_ok(event_id, *status);
                    }
                }
                if let nostr_sdk::prelude::ClientNotification::Event { event, subscription_id, .. } = notification {
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
                        community::realtime::dispatch_event(*event, handler.clone()).await;
                    } else if community::v2::realtime::subscription_id().await.as_ref() == Some(&subscription_id)
                        || community::v2::realtime::poolwide_subscription_id().await.as_ref() == Some(&subscription_id)
                    {
                        // Concord v2 plane events (authors-addressed kind-1059/21059).
                        community::v2::realtime::dispatch_event(*event, handler.clone()).await;
                    }
                }
            }
        }

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
    /// holding a `std::sync::Arc<crate::db::Session>` short-circuits before it can touch the next account's storage; shuts
    /// the client down (which ends any `listen()` notification loop bound to it, so the old account's
    /// events can't land in the new account's DB); closes the DB pool; and clears the key vaults plus
    /// all in-memory per-account state. Follow with `login()` to bind the next account, then re-attach
    /// `listen()`. (The app's `reset_session()` additionally clears Tauri-only caches it owns.)
    pub async fn swap_session(&self) {
        // FIRST — invalidate every captured guard before any teardown begins.
        state::clear_message_tombstones();

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

        // Everything else the account owned — chats, profiles, wrapper ids,
        // parked events, row-id caches, the sync queue, the relay caches, the
        // theme tags, the active chat — went with the session `close_database`
        // released. Only teardown that DOES something beyond forgetting is left.

        // Unsubscribing is an action: these hold live relay subscriptions keyed
        // to the prior account's plane pseudonyms.
        crate::community::realtime::clear().await;
        crate::community::v2::realtime::clear().await;
        // So is disconnecting: the pooled clients are AUTHENTICATED as the prior
        // account's plane secret keys, and must not be left holding sockets.
        crate::community::transport::clear_plane_pool();
    }
}

#[cfg(all(test, feature = "tor", not(target_arch = "wasm32")))]
mod transport_policy_tests {
    use std::time::Duration;

    /// ONE test covering proxy + budgets: the Tor preference is a process-global
    /// atomic, so separate `#[test]` fns would race under the parallel runner.
    #[test]
    fn tor_transport_policy() {
        let short = Duration::from_secs(5);
        let long = Duration::from_secs(300);

        // Tor off: connections may go direct, and every caller's clearnet budget
        // passes through untouched so the common path is never slowed down.
        crate::tor::set_tor_enabled_pref(false);
        assert_eq!(super::tor_proxy_target(), None);
        assert_eq!(super::relay_connect_timeout(short), short);
        assert_eq!(super::relay_request_timeout(short), short);

        // `RequiredButInactive` (Tor chosen, proxy not up yet) must raise the floor
        // just like `Active`: that window is when connects are slowest, and treating
        // it as clearnet is what tore relays down mid-handshake.
        crate::tor::set_tor_enabled_pref(true);
        assert!(matches!(
            crate::tor::transport_state(),
            crate::tor::TorTransportState::RequiredButInactive
        ));
        // THE leak invariant: `None` here means "connect direct". While Tor is the
        // chosen transport it must never be None — least of all during bootstrap,
        // which is exactly when a naive implementation falls through to direct.
        // Silent failure with an IP disclosure as the cost, so it gets a permanent
        // guard rather than a one-off manual check.
        assert_eq!(
            super::tor_proxy_target(),
            Some(crate::tor::blackhole_proxy_addr()),
            "Tor enabled but inactive must blackhole, never connect direct"
        );
        assert_eq!(super::relay_connect_timeout(short), super::TOR_RELAY_CONNECT_FLOOR);
        assert_eq!(super::relay_request_timeout(short), super::TOR_RELAY_REQUEST_FLOOR);

        // The floor only ever raises. A caller asking for longer than the floor has
        // a reason to, and shortening it would abort operations that used to finish.
        for tor in [true, false] {
            crate::tor::set_tor_enabled_pref(tor);
            assert_eq!(super::relay_connect_timeout(long), long, "connect, tor={tor}");
            assert_eq!(super::relay_request_timeout(long), long, "request, tor={tor}");
        }
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
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
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

#[cfg(test)]
mod history_paging_tests {
    use super::*;

    fn msg(at: u64, id_byte: u8, content: &str) -> Message {
        Message {
            id: format!("{:02x}", id_byte).repeat(32),
            content: content.to_string(),
            at,
            ..Default::default()
        }
    }

    /// The cursor is compared by VALUE: paging must step strictly through a
    /// same-millisecond wall (the id tiebreak) and must survive the cursored
    /// message being deleted — a persisted cursor must never wedge the walk.
    #[tokio::test]
    async fn history_pages_through_a_same_ms_wall_and_a_deleted_cursor() {
        let chat_id = "test-history-paging-wall";
        {
            let mut st = state::STATE.lock().await;
            st.ensure_community_chat(chat_id);
            // A wall of three messages in the SAME millisecond, plus one older.
            for m in [msg(500, 0x01, "old"), msg(900, 0xaa, "wall-a"), msg(900, 0xbb, "wall-b"), msg(900, 0xcc, "wall-c")] {
                st.add_message_to_chat(chat_id, &m);
            }
        }
        let core = VectorCore;

        // Newest 2 — the tail of the wall, chronological.
        let newest = core.get_messages_before(chat_id, None, 2).await;
        assert_eq!(newest.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["wall-b", "wall-c"]);

        // Page strictly before wall-b: its same-ms predecessor, then the older one.
        let cursor = (newest[0].at, newest[0].id.as_str().to_string());
        let page = core.get_messages_before(chat_id, Some((cursor.0, &cursor.1)), 10).await;
        assert_eq!(page.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["old", "wall-a"]);

        // The cursored message vanishes (delete / self-destruct): the SAME cursor
        // still resolves to the same page, because nothing resolves it to a row.
        {
            let mut st = state::STATE.lock().await;
            let chat = st.get_chat_mut(chat_id).unwrap();
            chat.messages.remove_by_hex_id(&cursor.1);
        }
        let page = core.get_messages_before(chat_id, Some((cursor.0, &cursor.1)), 10).await;
        assert_eq!(
            page.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["old", "wall-a"],
            "a deleted cursor pages identically"
        );

        // Unknown chat: empty, not an error.
        assert!(core.get_messages_before("test-history-paging-nochat", None, 5).await.is_empty());
    }
}
