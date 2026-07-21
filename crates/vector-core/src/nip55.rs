//! NIP-55 "offline" signer — an on-device signer app (Amber) reached over
//! local Android IPC instead of a relay.
//!
//! This is the third signer mode alongside `Local` (nsec in the `MY_SECRET_KEY`
//! vault) and `Bunker` (NIP-46 over relays, see [`crate::signer`]). A NIP-55
//! account holds **nothing secret on this device** — not even the client
//! keypair a bunker account keeps. Every signing op is a local IPC hop to the
//! signer app; no network, works offline.
//!
//! Layering:
//! - [`Nip55Backend`] is a platform hook (mirrors [`crate::traits::EventEmitter`]):
//!   the Android shell registers the concrete JNI/ContentResolver/Intent impl at
//!   startup. vector-core stays Tauri- and Android-decoupled. When no backend is
//!   registered (desktop, CLI, tests), every op returns a clean runtime error —
//!   never a compile-time platform stub, so a stray shared-call-site reference
//!   can't break the desktop build.
//! - [`Nip55Signer`] implements `NostrSigner` on top of the hook, so every
//!   existing `client.signer()` call site (DM seals, Blossom auth, Concord v2
//!   identity ops) uses it agnostically with no changes.
//!
//! Wire-identity: NIP-55 `sign_event` / `nip44_*` produce byte-identical output
//! to the local path (same NIP-01 id computation, same NIP-44 conversation key),
//! which is why an Amber account and a local account share communities without
//! forking. Fail-closed: a signer returning an event authored by the wrong
//! identity is rejected here, not silently published.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, OnceLock};

use nostr_sdk::prelude::*;

// ============================================================================
// Nip55Error — hook failure taxonomy
// ============================================================================

/// Failure modes of a NIP-55 signing operation. The three variants map to
/// distinct observable states so the UI can tell "reopen and re-grant" apart
/// from "Amber is gone" apart from "transient hiccup".
#[derive(Debug, Clone)]
pub enum Nip55Error {
    /// Not pre-authorized: the background ContentResolver query returned a
    /// `rejected`/null result and no foreground Activity was available to
    /// prompt. Surfaces as [`Nip55State::NeedsAuth`]. MUST NEVER be read as a
    /// valid empty decrypt — a null cursor is an authorization signal, not data.
    NotAuthorized,
    /// No external signer resolvable — Amber uninstalled, or no backend
    /// registered on this platform. Surfaces as [`Nip55State::Missing`].
    Missing,
    /// Any other IPC / parse / transport failure. Transient — does not flip the
    /// observable state (the signer is presumed still paired).
    Ipc(String),
}

impl std::fmt::Display for Nip55Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Nip55Error::NotAuthorized => {
                write!(f, "external signer is not authorized for this operation")
            }
            Nip55Error::Missing => write!(f, "no external signer available"),
            Nip55Error::Ipc(msg) => write!(f, "external signer IPC error: {msg}"),
        }
    }
}

impl std::error::Error for Nip55Error {}

// ============================================================================
// Nip55Backend — the platform hook (registered by the Android shell)
// ============================================================================

/// The Android-side transport for a NIP-55 signer. Implemented in the Tauri
/// shell (JNI + ContentResolver + Intent-for-result); registered once at
/// startup via [`set_nip55_backend`].
///
/// All methods are blocking (JNI) — [`Nip55Signer`] wraps each in
/// `spawn_blocking` behind a concurrency semaphore, so implementors may block
/// freely (including parking on an Activity-result condvar for the foreground
/// pairing/fallback path).
///
/// Hex strings are lowercase x-only pubkey hex. `current_user_hex` is always
/// the paired identity; passing it on every op lets Amber route to the right
/// account even if the user has several.
pub trait Nip55Backend: Send + Sync + 'static {
    /// Whether an external signer app is installed and resolvable. Cheap; the
    /// login screen and boot path call it to avoid offering a dead button.
    fn is_installed(&self) -> Result<bool, Nip55Error>;

    /// Pairing handshake (foreground `get_public_key` Intent). The user picks
    /// their identity and grants the remembered permission set. Returns
    /// `(user_pubkey, signer_package)`; the package is pinned for every
    /// subsequent op. Called ONCE at login — never again while signed in.
    fn get_public_key_pairing(&self, perms_json: &str) -> Result<(String, String), Nip55Error>;

    /// Fast, silent background ContentResolver op. `method` is the uppercase
    /// content-authority suffix (`SIGN_EVENT`, `NIP44_DECRYPT`, ...); `data` is
    /// the payload (event JSON / plaintext / ciphertext); `counterparty` is the
    /// other party's pubkey hex (empty for `SIGN_EVENT`). Returns a TRI-STATE
    /// the caller must not collapse (see [`Nip55ResolverOutcome`]).
    fn resolver_op(
        &self,
        method: &str,
        data: &str,
        counterparty: &str,
        current_user: &str,
    ) -> Nip55ResolverOutcome;

    /// Foreground Intent fallback (may block up to the sign timeout on user approval).
    /// Returns `(result, event)` — `result` carries sig/ciphertext/plaintext,
    /// `event` the signed event JSON for `sign_event`. Only invoked when
    /// [`is_foreground`](Self::is_foreground) is true.
    fn intent_op(
        &self,
        intent_type: &str,
        data: &str,
        counterparty: &str,
        current_user: &str,
    ) -> Result<(Option<String>, Option<String>), Nip55Error>;

    /// Whether a foreground Activity exists to prompt the user. False in the
    /// Activity-less background service, where an un-remembered op fails soft.
    fn is_foreground(&self) -> bool;
}

/// Outcome of a background ContentResolver op — three distinct meanings the
/// caller must keep apart. A null/empty cursor is NEVER a valid empty decrypt.
pub enum Nip55ResolverOutcome {
    /// Success. `result` = sig/ciphertext/plaintext; `event` = signed JSON.
    Value {
        result: Option<String>,
        event: Option<String>,
    },
    /// Not remembered (null/empty cursor) — escalate to the foreground Intent.
    RequiresApproval,
    /// Remembered reject (a `rejected` column) — surface NeedsAuth, do NOT
    /// relaunch the signer.
    Rejected,
    /// IPC / parse failure. Transient — does not flip observable state.
    Error(String),
}

static NIP55_BACKEND: OnceLock<Box<dyn Nip55Backend>> = OnceLock::new();

/// Register the platform NIP-55 backend. Call once during app startup on
/// Android. No-op on desktop/CLI (nothing registers), so every op below returns
/// [`Nip55Error::Missing`] there — the runtime stub.
pub fn set_nip55_backend(backend: Box<dyn Nip55Backend>) {
    let _ = NIP55_BACKEND.set(backend);
}

/// The registered backend, if any. `None` on platforms without an external
/// signer.
#[inline]
pub fn nip55_backend() -> Option<&'static dyn Nip55Backend> {
    NIP55_BACKEND.get().map(|b| b.as_ref())
}

// ----------------------------------------------------------------------------
// Concurrency bound (review W4) — scoped to the fast path only (review W1)
// ----------------------------------------------------------------------------
//
// A deep-sync backfill decrypts one gift wrap per inbound message — thousands
// of ops. An unbounded `spawn_blocking` per op would park a large slice of
// tokio's blocking pool, so the fast ContentResolver query is gated behind this
// semaphore. The permit is released BEFORE the (rare, up-to-2-minute) foreground
// Intent fallback runs, so a parked signer prompt can't head-of-line-block an
// interactive send waiting for a permit. See `Nip55Signer::run`.

const NIP55_MAX_CONCURRENT_OPS: usize = 4;

static NIP55_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(NIP55_MAX_CONCURRENT_OPS));

/// Serializes the foreground-Intent fallback to ONE at a time. Amber shows a
/// single approval dialog anyway, so without this a burst of un-remembered ops
/// (e.g. a foreground backfill whose kinds weren't pre-granted) would each park
/// up to the sign timeout AND stack `startActivityForResult` launches — a
/// thundering herd. Size 1 turns that into an orderly queue.
static NIP55_INTENT_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(1));

// ============================================================================
// Permissions requested at pairing
// ============================================================================

/// Encryption/decryption permissions Vector requests at pairing. These are
/// granted blanket (no `kind`) because Amber only drops a `kind`-less
/// permission when its type is `sign_event`/`nip` — nip04/nip44 with a null
/// kind are kept.
pub const VECTOR_NIP55_ENCRYPT_TYPES: &[&str] = &[
    "nip44_encrypt",
    "nip44_decrypt",
    "nip04_encrypt",
    "nip04_decrypt",
];

/// Event kinds Vector signs, requested per-kind at pairing.
///
/// CRITICAL: Amber drops a bare `{"type":"sign_event"}` (kind-less) permission
/// during pairing, so a blanket sign grant silently pre-authorizes NOTHING and
/// every outbound op would need a foreground tap. We must enumerate every kind
/// we sign. A kind missed here isn't a hard failure — the first sign of it
/// prompts once and the user can "remember" — but a background sign of an
/// un-remembered kind fails soft (no Activity to prompt). Keep this in sync
/// with the kinds Vector actually signs.
pub const VECTOR_NIP55_SIGN_KINDS: &[u16] = &[
    0,     // profile metadata
    3,     // contacts
    5,     // deletion
    7,     // reaction
    13,    // NIP-17 seal (DM hot path)
    14,    // NIP-17 chat rumor
    1059,  // NIP-17 gift wrap
    8,     // NIP-58 badge award
    30008, // NIP-58 profile badges
    30009, // NIP-58 badge definition
    10030, // emoji pack list
    10050, // NIP-17 DM relay list
    10063, // blossom server list
    22242, // NIP-42 relay auth
    24242, // blossom auth
    30078, // app-specific data (invite acceptance)
    // Concord / Communities (v1 3300-3311, v2 added 3312/3313).
    3300, 3301, 3302, 3303, 3304, 3305, 3306,
    3307, 3308, 3309, 3310, 3311, 3312, 3313,
];

/// Render the pairing `permissions` JSON: blanket encrypt/decrypt plus one
/// `{"type":"sign_event","kind":K}` per signed kind. Never includes
/// `get_private_key` — same policy as [`crate::signer::VECTOR_NIP46_PERMS`];
/// the whole point of an external signer is that the identity nsec never leaves
/// it.
pub fn nip55_perms_json() -> String {
    let mut arr: Vec<serde_json::Value> = VECTOR_NIP55_ENCRYPT_TYPES
        .iter()
        .map(|t| serde_json::json!({ "type": t }))
        .collect();
    for &kind in VECTOR_NIP55_SIGN_KINDS {
        arr.push(serde_json::json!({ "type": "sign_event", "kind": kind }));
    }
    serde_json::Value::Array(arr).to_string()
}

// ============================================================================
// Nip55State — observable pairing lifecycle (mirror BunkerConnectionState)
// ============================================================================

/// Observable state of the NIP-55 pairing. Backed by an atomic for hot-path
/// reads; transitions fan out to the frontend as `nip55_state` events.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Nip55State {
    /// No active NIP-55 session (local/bunker account, or between login and
    /// first op).
    Idle = 0,
    /// Signer reachable and pre-authorized; ops succeed silently.
    Ready = 1,
    /// A background op came back `rejected` — the user must reopen the signer
    /// and re-grant. Inbound decryption defers to a foreground prompt.
    NeedsAuth = 2,
    /// The signer app is gone (uninstalled). The account can't sign until it's
    /// reinstalled and re-paired.
    Missing = 3,
}

impl Nip55State {
    /// User-facing label, mirrored to the frontend in `nip55_state` events.
    pub fn as_label(self) -> &'static str {
        match self {
            Nip55State::Idle => "idle",
            Nip55State::Ready => "ready",
            Nip55State::NeedsAuth => "needs_auth",
            Nip55State::Missing => "missing",
        }
    }
}

static NIP55_STATE: AtomicU8 = AtomicU8::new(Nip55State::Idle as u8);

/// Read the live NIP-55 pairing state. Backed by an atomic; cheap to call.
#[inline]
pub fn nip55_state() -> Nip55State {
    match NIP55_STATE.load(Ordering::Acquire) {
        1 => Nip55State::Ready,
        2 => Nip55State::NeedsAuth,
        3 => Nip55State::Missing,
        _ => Nip55State::Idle,
    }
}

/// Install a new state and fan out a `nip55_state` event. No-op if unchanged,
/// so per-op confirmation of an already-known state doesn't spam the UI.
pub fn set_nip55_state(new_state: Nip55State) {
    let prev = NIP55_STATE.swap(new_state as u8, Ordering::AcqRel);
    if prev == new_state as u8 {
        return;
    }
    crate::traits::emit_event_json(
        "nip55_state",
        serde_json::json!({ "state": new_state.as_label() }),
    );
}

/// Reset NIP-55 observable state to Idle. Called by `reset_session()` on swap
/// so a stale state from the previous account doesn't leak onto the new one.
/// (The Android shell separately cancels any stranded Intent waiters.)
pub fn drain_nip55_state() {
    set_nip55_state(Nip55State::Idle);
}

// ============================================================================
// Nip55Signer — NostrSigner over the platform hook
// ============================================================================

/// A `NostrSigner` that routes every identity op to an external NIP-55 signer
/// over the platform hook. Cheap to clone (just a pubkey + a session
/// generation snapshot).
///
/// Captures a [`SessionGuard`](crate::state::SessionGuard) at construction:
/// state flips after an account swap are suppressed so an in-flight op
/// resolving against the previous account can't leak `nip55_state` onto the new
/// one. Wrong-key is impossible regardless — every op is bound to
/// `user_pubkey` and passes it as `current_user`, so a stale op still signs as
/// the correct (old) identity; it just must not narrate onto the new session.
#[derive(Debug, Clone)]
pub struct Nip55Signer {
    user_pubkey: PublicKey,
    session: crate::state::SessionGuard,
}

impl Nip55Signer {
    /// Build a signer for the paired identity. Capture happens now so the guard
    /// is bound to the session that installed this signer.
    pub fn new(user_pubkey: PublicKey) -> Self {
        Self {
            user_pubkey,
            session: crate::state::SessionGuard::capture(),
        }
    }

    /// The paired identity pubkey.
    #[inline]
    pub fn user_pubkey(&self) -> PublicKey {
        self.user_pubkey
    }

    /// Flip observable state only while the captured session is still active.
    #[inline]
    fn flip(&self, state: Nip55State) {
        if self.session.is_valid() {
            set_nip55_state(state);
        }
    }

    /// Resolver-first op with a foreground-Intent fallback. The semaphore bounds
    /// ONLY the fast ContentResolver query; the permit is released before the
    /// (rare, up-to-2-minute) Intent runs so a parked prompt can't head-of-line-block
    /// interactive signing (review W1). Returns `(result, event)`.
    async fn run(
        &self,
        method: &'static str,
        intent_type: &'static str,
        data: String,
        counterparty: String,
        current_user: String,
    ) -> Result<(Option<String>, Option<String>), SignerError> {
        let backend = match nip55_backend() {
            Some(b) => b,
            None => {
                self.flip(Nip55State::Missing);
                return Err(SignerError::backend(Nip55Error::Missing));
            }
        };

        // Fast ContentResolver path, permit-bounded. Permit drops with this block.
        let outcome = {
            let _permit = NIP55_SEMAPHORE.acquire().await.map_err(|_| {
                SignerError::backend(Nip55Error::Ipc("nip55 semaphore closed".to_string()))
            })?;
            let (d, cp, cu) = (data.clone(), counterparty.clone(), current_user.clone());
            match tokio::task::spawn_blocking(move || backend.resolver_op(method, &d, &cp, &cu)).await {
                Ok(o) => o,
                Err(e) => {
                    return Err(SignerError::backend(Nip55Error::Ipc(format!(
                        "nip55 worker join error: {e}"
                    ))))
                }
            }
        };

        match outcome {
            Nip55ResolverOutcome::Value { result, event } => {
                self.flip(Nip55State::Ready);
                Ok((result, event))
            }
            Nip55ResolverOutcome::Rejected => {
                self.flip(Nip55State::NeedsAuth);
                Err(SignerError::backend(Nip55Error::NotAuthorized))
            }
            Nip55ResolverOutcome::Error(e) => Err(SignerError::backend(Nip55Error::Ipc(e))),
            Nip55ResolverOutcome::RequiresApproval => {
                // Not remembered — prompt only if the user is actually in the app.
                if !backend.is_foreground() {
                    self.flip(Nip55State::NeedsAuth);
                    return Err(SignerError::backend(Nip55Error::NotAuthorized));
                }
                // Serialize to one live prompt (Amber shows one dialog anyway).
                // Held across the intent, but NOT the fast-path permit, so it
                // can't head-of-line-block silent signing.
                let _intent_permit = NIP55_INTENT_SEMAPHORE.acquire().await.map_err(|_| {
                    SignerError::backend(Nip55Error::Ipc("nip55 intent semaphore closed".to_string()))
                })?;
                let res = match tokio::task::spawn_blocking(move || {
                    backend.intent_op(intent_type, &data, &counterparty, &current_user)
                })
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(SignerError::backend(Nip55Error::Ipc(format!(
                            "nip55 worker join error: {e}"
                        ))))
                    }
                };
                match res {
                    Ok((result, event)) => {
                        self.flip(Nip55State::Ready);
                        Ok((result, event))
                    }
                    Err(e @ Nip55Error::NotAuthorized) => {
                        self.flip(Nip55State::NeedsAuth);
                        Err(SignerError::backend(e))
                    }
                    Err(e @ Nip55Error::Missing) => {
                        self.flip(Nip55State::Missing);
                        Err(SignerError::backend(e))
                    }
                    Err(e) => Err(SignerError::backend(e)),
                }
            }
        }
    }

    /// Test-only view onto the captured guard so a test can assert the wrapper
    /// is bound to the session generation at construction.
    #[cfg(test)]
    pub(crate) fn session_generation_for_test(&self) -> u64 {
        self.session.generation()
    }
}

impl NostrSigner for Nip55Signer {
    fn backend(&self) -> SignerBackend<'_> {
        SignerBackend::Custom(std::borrow::Cow::Borrowed("nip55"))
    }

    fn get_public_key<'a>(&'a self) -> BoxedFuture<'a, Result<PublicKey, SignerError>> {
        // Cached (review W6): NIP-55 `get_public_key` is a pairing-time Intent,
        // never a per-call hop. `my_pk` is resolved constantly on the hot path;
        // an IPC round-trip here would be catastrophic.
        Box::pin(async move { Ok(self.user_pubkey) })
    }

    fn sign_event<'a>(&'a self, unsigned: UnsignedEvent) -> BoxedFuture<'a, Result<Event, SignerError>> {
        Box::pin(async move {
            let event_json = unsigned.as_json();
            let user_hex = self.user_pubkey.to_hex();
            let (_result, signed) = self
                .run("SIGN_EVENT", "sign_event", event_json, String::new(), user_hex)
                .await?;
            let signed_json = signed.ok_or_else(|| {
                SignerError::backend(Nip55Error::Ipc("signer returned no signed event".to_string()))
            })?;
            let event = Event::from_json(&signed_json).map_err(SignerError::backend)?;
            // Fail closed: the signer must return an event authored by OUR
            // identity. A mismatch (wrong Amber account, corrupted IPC) would
            // otherwise fork the wire under a foreign key.
            if event.pubkey != self.user_pubkey {
                return Err(SignerError::backend(Nip55Error::Ipc(format!(
                    "signer returned event authored by {} (expected {})",
                    event.pubkey.to_hex(),
                    self.user_pubkey.to_hex()
                ))));
            }
            event.verify().map_err(SignerError::backend)?;
            Ok(event)
        })
    }

    fn nip04_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            let (result, _event) = self
                .run("NIP04_ENCRYPT", "nip04_encrypt", content.to_string(), public_key.to_hex(), self.user_pubkey.to_hex())
                .await?;
            result.ok_or_else(|| SignerError::backend(Nip55Error::Ipc("signer returned no result".to_string())))
        })
    }

    fn nip04_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            let (result, _event) = self
                .run("NIP04_DECRYPT", "nip04_decrypt", content.to_string(), public_key.to_hex(), self.user_pubkey.to_hex())
                .await?;
            result.ok_or_else(|| SignerError::backend(Nip55Error::Ipc("signer returned no result".to_string())))
        })
    }

    fn nip44_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            let (result, _event) = self
                .run("NIP44_ENCRYPT", "nip44_encrypt", content.to_string(), public_key.to_hex(), self.user_pubkey.to_hex())
                .await?;
            result.ok_or_else(|| SignerError::backend(Nip55Error::Ipc("signer returned no result".to_string())))
        })
    }

    fn nip44_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        Box::pin(async move {
            let (result, _event) = self
                .run("NIP44_DECRYPT", "nip44_decrypt", content.to_string(), public_key.to_hex(), self.user_pubkey.to_hex())
                .await?;
            result.ok_or_else(|| SignerError::backend(Nip55Error::Ipc("signer returned no result".to_string())))
        })
    }
}

// ============================================================================
// Pairing + availability helpers (login-flow entry points)
// ============================================================================

/// Whether an external signer is installed. Returns `Ok(false)` on platforms
/// without a registered backend (desktop) rather than erroring — the login
/// screen treats "not installed" and "not supported" the same (hide the
/// button).
pub fn nip55_is_installed() -> Result<bool, String> {
    match nip55_backend() {
        Some(b) => b.is_installed().map_err(|e| e.to_string()),
        None => Ok(false),
    }
}

/// Run the pairing handshake: fire the `get_public_key` Intent with Vector's
/// remembered permission set and return the discovered identity pubkey plus the
/// signer's package name. Blocking (Activity round-trip) — wrapped in
/// `spawn_blocking`.
pub async fn nip55_pair() -> Result<(PublicKey, String), String> {
    let perms = nip55_perms_json();
    let backend = nip55_backend().ok_or("no external signer available on this platform")?;
    let (pk_str, package) =
        tokio::task::spawn_blocking(move || backend.get_public_key_pairing(&perms))
            .await
            .map_err(|e| format!("pairing worker join error: {e}"))?
            .map_err(|e| e.to_string())?;
    // Amber returns the identity as npub (bech32); `parse` also accepts hex.
    let pk = PublicKey::parse(&pk_str)
        .map_err(|e| format!("external signer returned an invalid pubkey: {e}"))?;
    Ok((pk, package))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perms_exclude_private_key_and_enumerate_sign_kinds() {
        let json = nip55_perms_json();
        // The identity nsec must never leave the signer.
        assert!(!json.contains("private_key"), "perms leaked private-key access: {json}");
        // Blanket encrypt/decrypt (kind-less, which Amber keeps).
        for t in VECTOR_NIP55_ENCRYPT_TYPES {
            assert!(json.contains(t), "perms JSON missing '{t}': {json}");
        }
        // The DM-seal hot-path kind must be pre-granted.
        assert!(json.contains("\"kind\":13"), "seal kind 13 must be pre-granted: {json}");

        // EVERY sign_event permission must carry an explicit kind — Amber drops
        // a bare (kind-less) sign_event at pairing, so a blanket grant would
        // silently pre-authorize nothing.
        let v: serde_json::Value = serde_json::from_str(&json).expect("perms json parses");
        let mut saw_sign = false;
        for entry in v.as_array().expect("perms is an array") {
            if entry.get("type").and_then(|t| t.as_str()) == Some("sign_event") {
                saw_sign = true;
                assert!(
                    entry.get("kind").and_then(|k| k.as_u64()).is_some(),
                    "bare sign_event perm would be dropped by Amber: {entry}"
                );
            }
        }
        assert!(saw_sign, "perms must request sign_event for at least one kind: {json}");
    }

    #[test]
    fn state_label_covers_all_variants() {
        // A new Nip55State without a label would ship an unlabelled event to
        // the frontend listener. Force the match here.
        assert_eq!(Nip55State::Idle.as_label(), "idle");
        assert_eq!(Nip55State::Ready.as_label(), "ready");
        assert_eq!(Nip55State::NeedsAuth.as_label(), "needs_auth");
        assert_eq!(Nip55State::Missing.as_label(), "missing");
    }

    #[tokio::test]
    async fn get_public_key_is_cached_and_needs_no_backend() {
        // No backend is registered in pure vector-core tests, yet get_public_key
        // must still resolve — it returns the cached identity, never an IPC hop.
        let keys = Keys::generate();
        let signer = Nip55Signer::new(keys.public_key());
        let pk = signer.get_public_key().await.expect("cached pubkey resolves");
        assert_eq!(pk, keys.public_key());
    }

    // NIP55_STATE and SESSION_GENERATION are process-wide. Cargo runs #[test]
    // functions in parallel, so every mutation of NIP55_STATE is bundled into
    // THIS single test to keep the sequence deterministic — mirrors
    // `atomic_state_round_trips_and_drains` / `watched_signer_session_gate...`
    // in signer.rs.
    #[tokio::test]
    async fn global_state_session_gate_and_missing_backend() {
        use crate::state::{bump_session_generation, current_session_generation};

        // Defensive reset (a prior panic could have left state dirty).
        set_nip55_state(Nip55State::Idle);

        // State roundtrip.
        set_nip55_state(Nip55State::Ready);
        assert_eq!(nip55_state(), Nip55State::Ready);
        set_nip55_state(Nip55State::NeedsAuth);
        assert_eq!(nip55_state(), Nip55State::NeedsAuth);
        set_nip55_state(Nip55State::Missing);
        assert_eq!(nip55_state(), Nip55State::Missing);
        drain_nip55_state();
        assert_eq!(nip55_state(), Nip55State::Idle);

        // Session gate: a signer flips state only while its captured generation
        // is live.
        let keys = Keys::generate();
        let gen_before = current_session_generation();
        let signer = Nip55Signer::new(keys.public_key());
        assert_eq!(
            signer.session_generation_for_test(),
            gen_before,
            "signer must capture the live session generation at construction"
        );
        // Valid session flips (tolerate a concurrent bump from a sibling test).
        if signer.session_generation_for_test() == current_session_generation() {
            signer.flip(Nip55State::Ready);
            assert_eq!(nip55_state(), Nip55State::Ready, "valid-session flip must apply");
        }
        // After a swap the guard is stale; flips are no-ops so a leftover op
        // can't leak state onto the new account.
        set_nip55_state(Nip55State::Ready);
        bump_session_generation();
        signer.flip(Nip55State::Missing);
        assert_eq!(nip55_state(), Nip55State::Ready, "stale-session flip must be a no-op");

        // Missing backend: a FRESH signer (guard valid post-bump) signing with
        // no registered backend fails `Missing` and flips the state.
        set_nip55_state(Nip55State::Idle);
        let fresh = Nip55Signer::new(keys.public_key());
        let unsigned = EventBuilder::text_note("hi")
            .build(keys.public_key());
        let err = fresh.sign_event(unsigned).await;
        assert!(err.is_err(), "no backend registered => sign must fail");
        assert_eq!(
            nip55_state(),
            Nip55State::Missing,
            "missing-backend op must surface Missing state"
        );

        // nip55_is_installed with no backend registered is a clean false.
        assert_eq!(nip55_is_installed().unwrap(), false);

        // Cleanup for sibling tests / reruns.
        drain_nip55_state();
    }
}
