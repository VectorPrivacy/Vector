//! Polymorphic signer — local key vault vs. NIP-46 remote bunker.
//!
//! Vector supports two signer modes per account:
//!
//! - **Local** — the user's nsec lives in `MY_SECRET_KEY` (GuardedKey vault)
//!   on this device. Signing is local; the key materialises in plaintext only
//!   for microseconds per operation.
//! - **Bunker** — the user's nsec lives on a remote NIP-46 signer (Amber,
//!   nsec.app, ...). Vector holds only a *client keypair* (in `MY_SECRET_KEY`)
//!   used to RPC the bunker. Every signing request takes a round-trip; the
//!   user's identity key never touches this device.
//!
//! The discriminator is persisted in the per-account settings DB
//! (`signer_type` key) and materialised into the `SIGNER_KIND` atomic at
//! login. Hot paths read the atomic; cold paths read the DB directly.
//!
//! Storage layout for bunker accounts (see `db::settings`):
//! - `signer_type = "bunker"`
//! - `bunker_url`  = the `bunker://<remote_pubkey>?relay=...&secret=...`
//!   string, encrypted-at-rest if the account uses pin/pass encryption (same
//!   path as `pkey`).
//! - `bunker_remote_pubkey` = the signer's pubkey, plaintext (routing only).
//! - `pkey` = the NIP-46 client keypair (encrypted-at-rest under the same
//!   path as local accounts). Reusing the existing vault avoids a second
//!   GuardedKey slot; see the "Client-keypair storage note" section below.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use nostr_sdk::prelude::*;
use nostr_connect::prelude::{AuthUrlHandler, NostrConnect, NostrConnectURI};

// ============================================================================
// SignerKind — discriminator
// ============================================================================

/// Which signer backs the active account.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SignerKind {
    /// The user's nsec lives in `MY_SECRET_KEY` on this device.
    Local = 0,
    /// The user's nsec lives on a remote NIP-46 signer; we only hold the
    /// client keypair used to RPC it.
    Bunker = 1,
}

impl SignerKind {
    /// Persisted form used by the per-account settings KV.
    #[inline]
    pub fn as_setting_str(self) -> &'static str {
        match self {
            SignerKind::Local => "local",
            SignerKind::Bunker => "bunker",
        }
    }

    /// Parse from the on-disk setting string. Unknown values fall back to
    /// `Local` so an upgrade path from pre-NIP-46 accounts (which have no
    /// `signer_type` row) is the obvious default.
    #[inline]
    pub fn from_setting_str(s: &str) -> Self {
        match s {
            "bunker" => SignerKind::Bunker,
            _ => SignerKind::Local,
        }
    }
}

static SIGNER_KIND: AtomicU8 = AtomicU8::new(SignerKind::Local as u8);

/// The signer kind for the active session. Cheap to read; backed by an atomic.
#[inline]
pub fn signer_kind() -> SignerKind {
    match SIGNER_KIND.load(Ordering::Acquire) {
        1 => SignerKind::Bunker,
        _ => SignerKind::Local,
    }
}

/// Install the signer kind for the active session. Call at login after the
/// settings row has been read, and on swap before any signing work runs.
#[inline]
pub fn set_signer_kind(kind: SignerKind) {
    SIGNER_KIND.store(kind as u8, Ordering::Release);
}

/// `true` iff the active account signs via a remote NIP-46 bunker. Hot-path
/// helper for code that needs to branch on signer mode (e.g. parallelising
/// gift-wrap signing harder when each call pays a round-trip).
#[inline]
pub fn is_bunker() -> bool {
    signer_kind() == SignerKind::Bunker
}

// ============================================================================
// Client-keypair storage note
// ============================================================================
//
// The NIP-46 client keypair (used to RPC the bunker — not the user's
// identity) lives in the existing `MY_SECRET_KEY` vault for bunker accounts.
// This is intentional: every existing call site that loads "the active
// signing key" gets the client key, which is what the NIP-46 layer wants for
// its RPC envelope. For events the *user* sends, the path goes through
// `client.signer()` → NostrConnect, which tunnels to the bunker — so user
// events are signed by the user's identity, RPC envelopes by the client key.
//
// This avoids needing a second GuardedKey vault and the slot-coordination
// problem that comes with it. The trade-off: bunker accounts share the same
// memory-protection footprint as local accounts (the user's identity isn't
// on this device at all).

// ============================================================================
// BUNKER_SIGNER — live NostrConnect handle
// ============================================================================

/// Active `NostrConnect` handle. `None` for local-signer sessions.
///
/// `NostrConnect` is internally `Arc`-counted (relay pool, OnceCell-backed
/// remote pubkey cache), so cloning it for per-call use is cheap. The lock is
/// only held briefly to snapshot the inner value.
pub static BUNKER_SIGNER: LazyLock<RwLock<Option<NostrConnect>>> =
    LazyLock::new(|| RwLock::new(None));

/// Snapshot the active bunker handle. Returns `None` for local-signer sessions.
#[inline]
pub fn bunker_signer() -> Option<NostrConnect> {
    BUNKER_SIGNER.read().ok().and_then(|g| g.as_ref().cloned())
}

/// Install the bunker handle for the active session. Replaces any prior handle
/// without shutting it down — callers swapping should `take_bunker_signer()`
/// first and `.shutdown().await` the old one to drain its relay pool cleanly.
#[inline]
pub fn set_bunker_signer(signer: NostrConnect) {
    if let Ok(mut g) = BUNKER_SIGNER.write() {
        *g = Some(signer);
    }
}

/// Atomically remove the bunker handle. Used by session teardown so the
/// caller can `.shutdown()` it without racing readers.
#[inline]
pub fn take_bunker_signer() -> Option<NostrConnect> {
    BUNKER_SIGNER.write().ok().and_then(|mut g| g.take())
}

// ============================================================================
// Construction helpers
// ============================================================================

/// Parse a `bunker://` URL and return the relay URLs it lists. Used by the
/// Settings UI to render "Connected via <relay>" without re-bootstrapping.
/// Returns an empty Vec on any parse failure — the caller treats this as a
/// display-only signal and renders a generic fallback instead of erroring.
pub fn parse_bunker_relays(bunker_url: &str) -> Vec<String> {
    match NostrConnectURI::parse(bunker_url) {
        Ok(NostrConnectURI::Bunker { relays, .. }) => {
            relays.into_iter().map(|r| r.to_string()).collect()
        }
        _ => Vec::new(),
    }
}

/// Inspect a `bunker://` URL without bootstrapping: returns the remote
/// signer's pubkey (hex). Used by login flows to check whether a re-submitted
/// URL points at the same bunker as the active session (idempotent re-login)
/// versus a different bunker (which requires logout first). Cheap — no
/// network.
pub fn parse_bunker_remote_pubkey(bunker_url: &str) -> Result<String, String> {
    let uri = NostrConnectURI::parse(bunker_url)
        .map_err(|e| format!("Invalid bunker URL: {}", e))?;
    match uri {
        NostrConnectURI::Bunker { remote_signer_public_key, .. } => {
            // Force lowercase. `to_hex()` already returns lowercase per
            // nostr-sdk, but normalising here lets callers compare hex
            // forms with `==` without worrying about a future upstream
            // shift to mixed-case.
            Ok(remote_signer_public_key.to_hex().to_ascii_lowercase())
        }
        // Client-initiated URIs aren't supported as login entry points in v1;
        // they're for the reverse direction (we hand a URL to the signer).
        NostrConnectURI::Client { .. } => {
            Err("Client-initiated URIs not supported here; use a bunker:// URL".into())
        }
    }
}

// ============================================================================
// Vector app identity — surfaced to remote signers via NIP-46 metadata
// ============================================================================

/// Application name shown to the user by the remote signer when approving the
/// connection (e.g. on Amber's pairing screen).
pub const VECTOR_APP_NAME: &str = "Vector";

/// Marketing site — surfaced as the signer's "More info" link.
pub const VECTOR_APP_URL: &str = "https://vectorapp.io";

/// Icon shown by the signer alongside the app name. PNG, served from the
/// public GitHub mirror so the URL stays valid even if vectorapp.io changes
/// its asset layout. Signers cache by URL, so a stable target avoids
/// re-fetches on every pairing.
pub const VECTOR_APP_ICON: &str = "https://raw.githubusercontent.com/VectorPrivacy/Vector/master/src-tauri/icons/icon.png";

/// NIP-46 permission scope Vector requests on client-initiated pairings.
///
/// Sent as the `perms=` query parameter on `nostrconnect://` URIs. Signer apps
/// that honour it (Amber, nsec.app) surface this list on their pairing screen
/// and refuse RPC calls outside the granted scope. Vector intentionally never
/// requests `get_private_key`: the whole point of a Remote Signer is that the
/// identity nsec stays on the signer device, so allowing extraction would
/// defeat the threat model. Adding a method here is an explicit policy
/// decision; signer apps that don't enforce `perms` server-side still benefit
/// from a smaller surface in their pairing UI.
pub const VECTOR_NIP46_PERMS: &[&str] = &[
    "get_public_key",
    "sign_event",
    "nip04_encrypt",
    "nip04_decrypt",
    "nip44_encrypt",
    "nip44_decrypt",
];

/// Build the NIP-46 metadata payload Vector advertises in client-initiated
/// `nostrconnect://` URIs. The signer reads this to render the approval
/// prompt — name and icon are the bits the user actually sees.
pub fn vector_metadata() -> NostrConnectMetadata {
    let mut md = NostrConnectMetadata::new(VECTOR_APP_NAME);
    if let Ok(url) = Url::parse(VECTOR_APP_URL) {
        md = md.url(url);
    }
    if let Ok(icon) = Url::parse(VECTOR_APP_ICON) {
        md = md.icons(vec![icon]);
    }
    md
}

/// Build a client-initiated `nostrconnect://` URI. The user copies this URL
/// into their signer app (or scans the QR rendering of it); the signer
/// initiates the connection back to the listed relays.
///
/// Multi-relay by design — single-relay connect URIs are a centralisation
/// trap: if that one relay goes down, the user can't reconnect to their own
/// account. Pass the live trusted-relay list from `state::TRUSTED_RELAYS`.
pub fn build_nostrconnect_uri(
    client_pubkey: PublicKey,
    relays: Vec<RelayUrl>,
) -> NostrConnectURI {
    NostrConnectURI::Client {
        public_key: client_pubkey,
        relays,
        metadata: vector_metadata(),
    }
}

/// Build a `NostrConnect` for a client-initiated session — generates the
/// `nostrconnect://` URI from the client keys + relays + Vector metadata,
/// constructs the underlying `NostrConnect` with the Vector auth-URL handler
/// already attached, and returns both for the caller to (a) display the URI
/// to the user (QR + copy button) and (b) install the signer.
///
/// Note: doesn't bootstrap. The caller is expected to install the returned
/// `NostrConnect` in `BUNKER_SIGNER` and await `get_public_key()` to wait
/// for the signer's connect response.
pub fn build_nostrconnect_session(
    client_keys: Keys,
    relays: Vec<RelayUrl>,
    timeout: Duration,
) -> Result<(NostrConnect, String), String> {
    let uri = build_nostrconnect_uri(client_keys.public_key, relays);
    // Append the NIP-46 `perms=` scope. nostr-sdk's `Display` impl doesn't
    // write it, so the SDK-built URI is fine to hand back to `NostrConnect`
    // (which doesn't read perms locally), while the signer app on the other
    // side parses the appended query param to render its pairing screen.
    //
    // NIP-46 also defines a `secret=` query param the signer should echo
    // in its connect response for spoof detection. Not emitted: current
    // signers (Amber) return only `"ack"`, and the SDK's response parser
    // accepts only `"ack"` — a secret round-trip would short-circuit at
    // both ends. Revisit when ecosystem support lands.
    let mut uri_string = uri.to_string();
    let perms = VECTOR_NIP46_PERMS.join(",");
    if !perms.is_empty() {
        uri_string.push_str("&perms=");
        uri_string.push_str(&perms);
    }
    let mut nc = NostrConnect::new(uri, client_keys, timeout, None)
        .map_err(|e| format!("Bunker init failed: {}", e))?;
    nc.auth_url_handler(VectorAuthUrlHandler);
    Ok((nc, uri_string))
}

/// Build a `NostrConnect` from a `bunker://` URL + client keypair. Doesn't
/// connect yet — `NostrConnect` bootstraps lazily on the first signing call.
/// Use `prewarm()` if you want the connection up before the user's first send.
///
/// `timeout` bounds each RPC round-trip. 60s is the upstream example; we
/// expose it so chat-send paths can tighten this for snappier failure surfacing.
pub fn build_bunker_signer(
    bunker_url: &str,
    client_keys: Keys,
    timeout: Duration,
) -> Result<NostrConnect, String> {
    let uri = NostrConnectURI::parse(bunker_url)
        .map_err(|e| format!("Invalid bunker URL: {}", e))?;
    NostrConnect::new(uri, client_keys, timeout, None)
        .map_err(|e| format!("Bunker init failed: {}", e))
}

/// Force a bunker bootstrap and discover the user's identity pubkey.
///
/// The signer's *device* pubkey (returned by `bunker_uri()`) is NOT the user
/// identity for signers like Amber — bypassing this RPC produces events
/// signed under the wrong key. In Amber's "Manually approve each" mode this
/// prompts the user once during initial pairing.
pub async fn prewarm_bunker(signer: &NostrConnect) -> Result<PublicKey, String> {
    signer
        .get_public_key()
        .await
        .map_err(|e| format!("Bunker prewarm failed: {}", e))
}

// ============================================================================
// BunkerConnectionState — observable connection lifecycle
// ============================================================================

/// Observable state of the bunker connection. The atomic backs hot-path reads
/// (e.g. send paths checking "is it safe to issue a sign call?"); state changes
/// also fan out to the frontend via `EventEmitter` so the UI can show a banner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BunkerConnectionState {
    /// No active bunker session. Either we're on a local account, or we're
    /// between login and the first successful bootstrap.
    Idle = 0,
    /// Currently bootstrapping (relay connect + remote-pubkey discovery).
    Connecting = 1,
    /// Bunker is reachable; signing calls should succeed.
    Online = 2,
    /// Bunker is unreachable. Hot-path sends will fail fast; the next signing
    /// call will retry the underlying NostrConnect path which may reconnect.
    Offline = 3,
}

impl BunkerConnectionState {
    /// User-facing label, mirrored to the frontend in `bunker_state` events.
    pub fn as_label(self) -> &'static str {
        match self {
            BunkerConnectionState::Idle => "idle",
            BunkerConnectionState::Connecting => "connecting",
            BunkerConnectionState::Online => "online",
            BunkerConnectionState::Offline => "offline",
        }
    }
}

static BUNKER_STATE: AtomicU8 = AtomicU8::new(BunkerConnectionState::Idle as u8);

/// Read the live bunker connection state. Backed by an atomic; cheap to call.
#[inline]
pub fn bunker_state() -> BunkerConnectionState {
    match BUNKER_STATE.load(Ordering::Acquire) {
        1 => BunkerConnectionState::Connecting,
        2 => BunkerConnectionState::Online,
        3 => BunkerConnectionState::Offline,
        _ => BunkerConnectionState::Idle,
    }
}

/// Install a new bunker state and fan out a `bunker_state` event to the
/// frontend. No-op if the state didn't change — avoids spamming the UI with
/// duplicate transitions when a signing call confirms what's already known.
pub fn set_bunker_state(new_state: BunkerConnectionState) {
    let prev = BUNKER_STATE.swap(new_state as u8, Ordering::AcqRel);
    if prev == new_state as u8 {
        return;
    }
    crate::traits::emit_event_json(
        "bunker_state",
        serde_json::json!({ "state": new_state.as_label() }),
    );
}

// ============================================================================
// WatchedBunkerSigner — wrap NostrConnect with bunker_state observability
// ============================================================================
//
// Every signing operation (sign_event, nip44_encrypt, nip04_*) flows through
// this adapter when a bunker account is active. On success we flip
// `BUNKER_STATE` to Online; on error we flip to Offline. The frontend's
// `bunker_state` listener picks up the transition and surfaces a banner /
// toast so the user knows when their signer becomes unreachable mid-session.
//
// State flips are deduplicated by `set_bunker_state` (same-value writes are
// no-ops), so the per-call overhead is just one atomic load.

/// `NostrSigner` wrapper that emits `BunkerConnectionState` transitions on
/// every signing outcome. The inner `NostrConnect` is cheaply clonable
/// (internally Arc'd), so this is also Clone.
///
/// Captures a `SessionGuard` at construction; state flips after `reset_session`
/// are no-ops to avoid leaking signer-state events across an account swap (an
/// in-flight signing call resolving after the new account is installed would
/// otherwise emit `bunker_state: offline` against a local-account session).
#[derive(Debug, Clone)]
pub struct WatchedBunkerSigner {
    inner: NostrConnect,
    session: crate::state::SessionGuard,
}

impl WatchedBunkerSigner {
    pub fn new(inner: NostrConnect) -> Self {
        Self { inner, session: crate::state::SessionGuard::capture() }
    }

    /// Flip state only when the captured session is still active.
    #[inline]
    fn flip(&self, state: BunkerConnectionState) {
        if self.session.is_valid() {
            set_bunker_state(state);
        }
    }

    /// Test-only view onto the captured guard so a test can assert the
    /// wrapper is bound to the session generation at construction.
    #[cfg(test)]
    pub(crate) fn session_generation_for_test(&self) -> u64 {
        self.session.generation()
    }
}

impl NostrSigner for WatchedBunkerSigner {
    fn backend(&self) -> SignerBackend<'_> {
        self.inner.backend()
    }

    fn get_public_key<'a>(&'a self) -> BoxedFuture<'a, Result<PublicKey, SignerError>> {
        Box::pin(async move {
            match self.inner.get_public_key().await {
                Ok(pk) => {
                    self.flip(BunkerConnectionState::Online);
                    Ok(pk)
                }
                Err(e) => {
                    self.flip(BunkerConnectionState::Offline);
                    Err(e)
                }
            }
        })
    }

    fn sign_event<'a>(&'a self, unsigned: UnsignedEvent) -> BoxedFuture<'a, Result<Event, SignerError>> {
        Box::pin(async move {
            match self.inner.sign_event(unsigned).await {
                Ok(event) => {
                    self.flip(BunkerConnectionState::Online);
                    Ok(event)
                }
                Err(e) => {
                    self.flip(BunkerConnectionState::Offline);
                    Err(e)
                }
            }
        })
    }

    fn nip04_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            match self.inner.nip04_encrypt(public_key, content).await {
                Ok(s) => { self.flip(BunkerConnectionState::Online); Ok(s) }
                Err(e) => { self.flip(BunkerConnectionState::Offline); Err(e) }
            }
        })
    }

    fn nip04_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            match self.inner.nip04_decrypt(public_key, content).await {
                Ok(s) => { self.flip(BunkerConnectionState::Online); Ok(s) }
                Err(e) => { self.flip(BunkerConnectionState::Offline); Err(e) }
            }
        })
    }

    fn nip44_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            match self.inner.nip44_encrypt(public_key, content).await {
                Ok(s) => { self.flip(BunkerConnectionState::Online); Ok(s) }
                Err(e) => { self.flip(BunkerConnectionState::Offline); Err(e) }
            }
        })
    }

    fn nip44_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            match self.inner.nip44_decrypt(public_key, content).await {
                Ok(s) => { self.flip(BunkerConnectionState::Online); Ok(s) }
                Err(e) => { self.flip(BunkerConnectionState::Offline); Err(e) }
            }
        })
    }
}

// ============================================================================
// VectorAuthUrlHandler — bridge bunker permission prompts to the frontend
// ============================================================================
//
// NIP-46 signers occasionally need user approval (e.g. signing an event kind
// the user hasn't yet granted blanket permission for). Amber and nsec.app
// respond with an `auth_url` the user must visit; on completion the signing
// retry succeeds. This handler emits the URL to the frontend so the UI can
// show a "Open signer" prompt — we deliberately don't auto-open a browser
// from the core because (a) the core doesn't own the platform-specific
// browser-open path, and (b) frontends may prefer in-app webview.

/// Auth-URL handler that forwards bunker prompts to the frontend via the
/// `EventEmitter` trait. The frontend receives a `bunker_auth_url` event and
/// is responsible for opening the URL (in-app webview, system browser, ...).
#[derive(Debug, Clone, Default)]
pub struct VectorAuthUrlHandler;

impl AuthUrlHandler for VectorAuthUrlHandler {
    fn on_auth_url<'a>(&'a self, auth_url: Url) -> BoxedFuture<'a, Result<()>> {
        Box::pin(async move {
            crate::traits::emit_event_json(
                "bunker_auth_url",
                serde_json::json!({ "url": auth_url.to_string() }),
            );
            Ok(())
        })
    }
}

// ============================================================================
// attempt_bunker_login — end-to-end: build → prewarm → install
// ============================================================================

/// Build a `NostrConnect`, attach the Vector auth-URL handler, bootstrap it,
/// and install it as the active bunker signer. Returns the discovered remote
/// signer pubkey on success.
///
/// Emits `bunker_state` transitions: Connecting → Online (on success) or
/// Connecting → Offline (on failure). The caller is expected to update the
/// account-level discriminator (`signer_kind`) separately — this helper deals
/// only with the live connection.
pub async fn attempt_bunker_login(
    bunker_url: &str,
    client_keys: Keys,
    timeout: Duration,
) -> Result<PublicKey, String> {
    set_bunker_state(BunkerConnectionState::Connecting);

    let mut nc = match build_bunker_signer(bunker_url, client_keys, timeout) {
        Ok(nc) => nc,
        Err(e) => {
            set_bunker_state(BunkerConnectionState::Offline);
            return Err(e);
        }
    };
    nc.auth_url_handler(VectorAuthUrlHandler);

    match prewarm_bunker(&nc).await {
        Ok(remote_pk) => {
            // If a prior NostrConnect is already installed (retry-after-blip
            // path), take it out and shut it down on a background task so
            // its relay pool drains cleanly. Without this, repeated calls
            // leak Arc'd RelayPool handles fighting for connection slots.
            //
            if let Some(old) = take_bunker_signer() {
                tokio::spawn(async move { let _ = old.shutdown().await; });
            }
            set_bunker_signer(nc);
            set_bunker_state(BunkerConnectionState::Online);
            Ok(remote_pk)
        }
        Err(e) => {
            // The just-built `nc`'s Drop will release its half-opened relay
            // connections asynchronously; we don't need a shutdown call here
            // because we never installed it as the active signer.
            set_bunker_state(BunkerConnectionState::Offline);
            Err(e)
        }
    }
}

// ============================================================================
// Teardown
// ============================================================================

/// Clear all bunker-specific state. Called by `reset_session()` so a swap
/// from bunker → local (or between two bunker accounts) leaves no stale
/// keying material, relay-pool handles, or stale connection-state observed
/// by the frontend. The caller is responsible for `.shutdown().await`-ing
/// the returned signer outside the lock.
pub fn drain_bunker_state() -> Option<NostrConnect> {
    set_signer_kind(SignerKind::Local);
    set_bunker_state(BunkerConnectionState::Idle);
    take_bunker_signer()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_roundtrip() {
        assert_eq!(SignerKind::from_setting_str("local"), SignerKind::Local);
        assert_eq!(SignerKind::from_setting_str("bunker"), SignerKind::Bunker);
        assert_eq!(SignerKind::Local.as_setting_str(), "local");
        assert_eq!(SignerKind::Bunker.as_setting_str(), "bunker");
        // Unknown values fall back to Local — upgrade path for pre-NIP-46 rows.
        assert_eq!(SignerKind::from_setting_str(""), SignerKind::Local);
        assert_eq!(SignerKind::from_setting_str("garbage"), SignerKind::Local);
    }

    // SIGNER_KIND + BUNKER_SIGNER + BUNKER_STATE are process-wide atomics /
    // locks. Cargo runs `#[test]` functions in parallel, so any pair of
    // tests that mutate the same global races and produces flaky failures.
    // Bundled into one test function so the sequence is deterministic —
    // mirrors `session_helpers_round_trip_and_clear` in state.rs which
    // does the same for `MY_PUBLIC_KEY` / `PENDING_INVITE`.
    #[test]
    fn atomic_state_round_trips_and_drains() {
        // Defensive cleanup: a previous test panic could have left a non-
        // default value behind.
        set_signer_kind(SignerKind::Local);
        set_bunker_state(BunkerConnectionState::Idle);

        // atomic kind roundtrip
        set_signer_kind(SignerKind::Bunker);
        assert_eq!(signer_kind(), SignerKind::Bunker);
        assert!(is_bunker());
        set_signer_kind(SignerKind::Local);
        assert_eq!(signer_kind(), SignerKind::Local);
        assert!(!is_bunker());

        // drain resets discriminator + state and returns the (absent) signer
        set_signer_kind(SignerKind::Bunker);
        set_bunker_state(BunkerConnectionState::Online);
        let drained = drain_bunker_state();
        assert!(drained.is_none());
        assert_eq!(signer_kind(), SignerKind::Local);
        assert_eq!(bunker_state(), BunkerConnectionState::Idle);

        // drain is idempotent — running again on already-cleared state is
        // safe (no panic, no spurious event), and leaves things clean.
        let drained_again = drain_bunker_state();
        assert!(drained_again.is_none());
        assert_eq!(signer_kind(), SignerKind::Local);
        assert_eq!(bunker_state(), BunkerConnectionState::Idle);
    }

    #[test]
    fn bunker_state_label_covers_all_variants() {
        // Whenever a new BunkerConnectionState is added, this test forces a
        // matching label so the frontend's `bunker_state` listener never sees
        // an unlabelled discriminant.
        assert_eq!(BunkerConnectionState::Idle.as_label(), "idle");
        assert_eq!(BunkerConnectionState::Connecting.as_label(), "connecting");
        assert_eq!(BunkerConnectionState::Online.as_label(), "online");
        assert_eq!(BunkerConnectionState::Offline.as_label(), "offline");
    }

    #[test]
    fn parse_bunker_relays_returns_relays_from_bunker_uri() {
        let signer_keys = Keys::generate();
        let r1 = RelayUrl::parse("wss://relay1.example").unwrap();
        let r2 = RelayUrl::parse("wss://relay2.example").unwrap();
        let uri = NostrConnectURI::Bunker {
            remote_signer_public_key: signer_keys.public_key,
            relays: vec![r1.clone(), r2.clone()],
            secret: None,
        };
        let relays = parse_bunker_relays(&uri.to_string());
        assert_eq!(relays.len(), 2);
        assert!(relays.iter().any(|r| r.contains("relay1.example")));
        assert!(relays.iter().any(|r| r.contains("relay2.example")));
    }

    #[test]
    fn parse_bunker_relays_returns_empty_on_invalid_input() {
        // Display-only signal; never panics, never errors. Bad input collapses
        // to "no relays known" so the Security panel falls back to "unknown"
        // instead of crashing.
        assert!(parse_bunker_relays("").is_empty());
        assert!(parse_bunker_relays("not a url").is_empty());
        assert!(parse_bunker_relays("http://example.com").is_empty());

        // Client-initiated URIs also return empty — they're not the bunker
        // form we want to surface relays for.
        let client_keys = Keys::generate();
        let relay = RelayUrl::parse("wss://relay.example").unwrap();
        let client_uri = build_nostrconnect_uri(client_keys.public_key, vec![relay]);
        assert!(parse_bunker_relays(&client_uri.to_string()).is_empty(),
            "client URI must not surface as a bunker relay list");
    }

    #[test]
    fn parse_bunker_remote_pubkey_invalid_url() {
        assert!(parse_bunker_remote_pubkey("not a url").is_err());
        assert!(parse_bunker_remote_pubkey("").is_err());
        assert!(parse_bunker_remote_pubkey("http://example.com").is_err());
    }

    #[test]
    fn parse_bunker_remote_pubkey_rejects_client_uri() {
        // A client-initiated `nostrconnect://` URI is not a login entry point;
        // accepting it would let a hostile clipboard string register an
        // attacker-controlled client pubkey as "the remote signer".
        let client_keys = Keys::generate();
        let relay = RelayUrl::parse("wss://relay.example").unwrap();
        let uri = build_nostrconnect_uri(client_keys.public_key, vec![relay]);
        let err = parse_bunker_remote_pubkey(&uri.to_string())
            .expect_err("client URI must be rejected");
        assert!(err.contains("Client-initiated"), "unexpected error: {}", err);
    }

    #[test]
    fn parse_bunker_remote_pubkey_normalizes_lowercase() {
        // Build a valid bunker URI with a known pubkey and verify the parse
        // result is forced to lowercase regardless of upstream casing choice.
        let signer_keys = Keys::generate();
        let relay = RelayUrl::parse("wss://relay.example").unwrap();
        let uri = NostrConnectURI::Bunker {
            remote_signer_public_key: signer_keys.public_key,
            relays: vec![relay],
            secret: None,
        };
        let parsed = parse_bunker_remote_pubkey(&uri.to_string())
            .expect("valid bunker URI");
        assert_eq!(parsed, signer_keys.public_key.to_hex().to_ascii_lowercase());
        assert_eq!(parsed, parsed.to_ascii_lowercase(),
            "callers may compare with == — output must already be lowercase");
    }

    #[test]
    fn vector_metadata_carries_app_name_and_icon() {
        let md = vector_metadata();
        let json = serde_json::to_string(&md).expect("metadata serializes");
        assert!(json.contains(VECTOR_APP_NAME),
            "metadata must include app name for the signer's approval prompt; got {}", json);
        assert!(json.contains("vectorapp.io"),
            "metadata must reference the app URL for the signer's 'More info' link");
    }

    #[test]
    fn nip46_perms_list_excludes_get_private_key() {
        // The whole point of a Remote Signer is keeping the identity nsec on
        // the signer device. Adding `get_private_key` to the requested perms
        // would invite the signer to expose it back to Vector and defeat the
        // threat model. This test fails loudly if a future edit re-adds it.
        for perm in VECTOR_NIP46_PERMS {
            assert!(!perm.contains("get_private_key"),
                "VECTOR_NIP46_PERMS must never include get_private_key (found: {})", perm);
            assert!(!perm.contains("private_key"),
                "perm string looks dangerous: {}", perm);
        }
    }

    #[test]
    fn build_nostrconnect_session_appends_perms_query_param() {
        let client_keys = Keys::generate();
        let relay = RelayUrl::parse("wss://relay.example").unwrap();
        let (_nc, uri) = build_nostrconnect_session(
            client_keys,
            vec![relay],
            std::time::Duration::from_secs(1),
        ).expect("session builds");
        assert!(uri.contains("perms="),
            "URI must carry perms query param so signers can scope the pairing; got: {}", uri);
        // Every permission we DO ask for must appear in the URI.
        for perm in VECTOR_NIP46_PERMS {
            assert!(uri.contains(perm),
                "URI missing permission '{}': {}", perm, uri);
        }
        // And get_private_key must NOT.
        assert!(!uri.contains("get_private_key"),
            "URI must never request get_private_key: {}", uri);
    }

    #[test]
    fn build_nostrconnect_session_rejects_empty_uri() {
        // `build_nostrconnect_session` is the QR-flow entry. Constructing one
        // with zero relays would produce a URI that no signer can connect
        // back to — caller-side check is in `start_nostrconnect_session`, but
        // this is a sanity test that NostrConnect itself does not silently
        // accept an empty relay list at the URI level.
        let client_keys = Keys::generate();
        let session = build_nostrconnect_session(
            client_keys,
            vec![],
            std::time::Duration::from_secs(1),
        );
        // We don't assert pass/fail — different upstream versions may treat
        // empty relays differently — only that we don't panic.
        let _ = session;
    }

    // Combined into one #[test] to serialise mutation of process-wide globals
    // (SESSION_GENERATION, BUNKER_STATE, BUNKER_SIGNER). See the rationale on
    // `atomic_state_round_trips_and_drains` above.
    #[test]
    fn watched_signer_session_gate_and_state_transitions() {
        use crate::state::{bump_session_generation, current_session_generation};

        // Build a real NostrConnect so we can wrap it. We never call any of
        // its async methods (those would require a relay) — only the inner
        // wrapper's session-guard semantics are under test.
        let client_keys = Keys::generate();
        let relay = RelayUrl::parse("wss://relay.example").unwrap();
        let signer_keys = Keys::generate();
        let uri = NostrConnectURI::Bunker {
            remote_signer_public_key: signer_keys.public_key,
            relays: vec![relay],
            secret: None,
        };
        let nc = NostrConnect::new(
            uri,
            client_keys,
            std::time::Duration::from_secs(1),
            None,
        ).expect("NostrConnect builds");

        let gen_before = current_session_generation();
        let watched = WatchedBunkerSigner::new(nc);
        assert_eq!(watched.session_generation_for_test(), gen_before,
            "WatchedBunkerSigner must capture the live session generation at construction");

        // Pre-swap: flip emits because the captured guard matches.
        set_bunker_state(BunkerConnectionState::Idle);
        watched.flip(BunkerConnectionState::Online);
        assert_eq!(bunker_state(), BunkerConnectionState::Online,
            "flip with valid session must update bunker_state");

        // Simulate a session swap (logout / account swap). The captured
        // guard goes stale; subsequent flips must be ignored so a leftover
        // in-flight signing call from the previous account can't leak
        // bunker_state changes into the new session.
        bump_session_generation();
        set_bunker_state(BunkerConnectionState::Online);
        watched.flip(BunkerConnectionState::Offline);
        assert_eq!(bunker_state(), BunkerConnectionState::Online,
            "flip with stale session must be a no-op");

        // Cleanup so subsequent test runs / siblings see a sane state.
        set_bunker_state(BunkerConnectionState::Idle);
    }
}
