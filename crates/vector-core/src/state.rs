//! Global state management — ChatState, globals, processing gate.
//!
//! All Tauri-specific globals (TAURI_APP) have been removed. Event emission
//! uses the `EventEmitter` trait via `crate::traits::emit_event`.

use nostr_sdk::prelude::*;
use std::sync::RwLock;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

use crate::chat::{Chat, ChatType};
use crate::compact::{CompactMessage, CompactAttachment, NpubInterner, NO_NPUB};
use crate::profile::{Profile, SlimProfile};
use crate::types::{Message, Reaction};
use crate::traits::emit_event;

// ============================================================================
// WrapperIdCache — Hybrid duplicate detection during sync
// ============================================================================

/// Outside a sync the ledger dedups; only a catch-up batch whose ledger writes are still
/// buffered needs remembering.
const IDLE_PENDING_CAP: usize = 512;

pub struct WrapperIdCache {
    historical: Vec<[u8; 32]>,
    pending: HashSet<[u8; 32]>,
    /// Between `load` and `clear`: every insert is kept until the dump.
    syncing: bool,
    /// Idle inserts, oldest first, so `pending` stays at [`IDLE_PENDING_CAP`].
    recent: std::collections::VecDeque<[u8; 32]>,
}

impl WrapperIdCache {
    pub fn new() -> Self {
        Self { historical: Vec::new(), pending: HashSet::new(), syncing: false, recent: std::collections::VecDeque::new() }
    }

    pub fn load(&mut self, mut ids: Vec<[u8; 32]>) {
        ids.sort_unstable();
        self.historical = ids;
        self.pending.clear();
        self.recent = std::collections::VecDeque::new();
        self.syncing = true;
    }

    #[inline]
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        self.historical.binary_search(id).is_ok() || self.pending.contains(id)
    }

    #[inline]
    pub fn insert(&mut self, id: [u8; 32]) {
        if !self.pending.insert(id) || self.syncing {
            return;
        }
        if self.recent.len() == IDLE_PENDING_CAP {
            if let Some(old) = self.recent.pop_front() {
                self.pending.remove(&old);
            }
        }
        self.recent.push_back(id);
    }

    pub fn clear(&mut self) {
        self.historical.clear();
        self.historical.shrink_to_fit();
        self.pending.clear();
        self.pending.shrink_to_fit();
        self.recent = std::collections::VecDeque::new();
        self.syncing = false;
    }

    pub fn len(&self) -> usize { self.historical.len() + self.pending.len() }

    pub fn is_empty(&self) -> bool { self.historical.is_empty() && self.pending.is_empty() }
}

impl Default for WrapperIdCache {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Globals
// ============================================================================

pub static TRUSTED_RELAYS: &[&str] = &[
    "wss://jskitty.com/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://nostr.computingcache.com",
    // Also listed in DISCOVERY_READ_ONLY_RELAYS: Ditto once ack'd stranger
    // kind-10050 writes with OK true and silently dropped them, so it must
    // never count toward the relay-list freshness anchor. General traffic
    // (DMs, communities) is unrestricted per its NIP-11.
    "wss://relay.ditto.pub",
];

/// Discovery Relays: widely-used indexers queried/written for relay-list
/// events (kind 10050) ONLY — never DMs, profiles, or communities. They give
/// list syncs and publishes a rendezvous point that doesn't depend on the
/// user's own relay set overlapping anyone else's. Every entry must be
/// verified to ACCEPT AND SERVE kind 10050 from unknown pubkeys (write +
/// readback); accept-then-silently-drop policy relays reduce coverage
/// invisibly.
pub static DISCOVERY_RELAYS: &[&str] = &[
    "wss://purplepag.es",
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://relay.snort.social",
];

/// Read-only Discovery Relays: queried when SYNCING relay lists but never
/// published to. Ditto acks stranger kind-10050 writes with OK true and then
/// silently drops them, so counting it as a publish landing would falsely
/// advance the list-freshness anchor — but it serves ESTABLISHED accounts'
/// lists honestly, and those are exactly the users whose existing list a
/// fresh Vector login must find before it dares bootstrap-publish.
pub static DISCOVERY_READ_ONLY_RELAYS: &[&str] = &["wss://relay.ditto.pub"];

/// Every discovery relay url (writable + read-only) — the fetch/pool/identity
/// set. Publish copies use `DISCOVERY_RELAYS` alone.
pub fn discovery_relay_iter() -> impl Iterator<Item = &'static str> {
    DISCOVERY_RELAYS
        .iter()
        .chain(DISCOVERY_READ_ONLY_RELAYS.iter())
        .copied()
}

pub async fn active_trusted_relays() -> Vec<&'static str> {
    let Some(client) = nostr_client() else { return Vec::new() };
    let pool_relays = client.relays().await;
    TRUSTED_RELAYS.iter().copied()
        .filter(|url| {
            let normalized = url.trim_end_matches('/');
            pool_relays.keys().any(|r| r.as_str().trim_end_matches('/') == normalized)
        })
        .collect()
}

/// Blossom media servers with failover: this account's resolved list (defaults
/// minus its disabled ones, plus its enabled customs).
///
/// Per-account, and consequential — it decides where THIS account's
/// attachments are uploaded, so account A's self-hosted destination must never
/// still be selected under account B. Resolved from the account's settings on first
/// use, and replaced by `blossom_servers::refresh_cache` after an edit.
#[derive(Default)]
struct BlossomServers(std::sync::Mutex<Option<Vec<String>>>);

pub fn init_blossom_servers() -> Vec<String> {
    crate::blossom_servers::DEFAULT_BLOSSOM_SERVERS
        .iter().map(|s| s.to_string()).collect()
}

pub fn get_blossom_servers() -> Vec<String> {
    let owner = crate::db::current_session().scoped::<BlossomServers, BlossomServers>();
    if let Some(servers) = owner.0.lock().unwrap().clone() {
        return servers;
    }
    // Resolved outside the lock. Until the account's database opens there is nothing to
    // resolve from, so the defaults stand in without being kept.
    match crate::blossom_servers::resolve_if_open() {
        Some(servers) => owner.0.lock().unwrap().get_or_insert(servers).clone(),
        None => init_blossom_servers(),
    }
}

/// Install this account's resolved server list.
pub fn set_blossom_servers(servers: Vec<String>) {
    let owner = crate::db::current_session().scoped::<BlossomServers, BlossomServers>();
    *owner.0.lock().unwrap() = Some(servers);
}

pub static MNEMONIC_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub static PENDING_NSEC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Staged bunker metadata between `connect_bunker` / `start_nostrconnect_session`
/// and the subsequent encryption-flow commit. URL is wrapped in `Zeroizing`
/// because it contains the NIP-46 single-use pairing secret; the pubkey hex
/// is non-secret. `setup_encryption` / `skip_encryption` reads this when
/// `signer_kind() == Bunker` to write the right settings rows via
/// `commit_bunker_account_setup`.
///
/// Tuple order: (bunker_url, remote_pubkey_hex).
pub static PENDING_BUNKER_SETUP:
    std::sync::Mutex<Option<(zeroize::Zeroizing<String>, String)>> =
    std::sync::Mutex::new(None);

#[inline]
pub fn set_pending_bunker_setup(url: String, remote_pk_hex: String) {
    *PENDING_BUNKER_SETUP.lock().unwrap() =
        Some((zeroize::Zeroizing::new(url), remote_pk_hex));
}

#[inline]
pub fn pending_bunker_setup() -> Option<(String, String)> {
    PENDING_BUNKER_SETUP.lock().unwrap()
        .as_ref()
        .map(|(z, pk)| (String::clone(&**z), pk.clone()))
}

#[inline]
pub fn clear_pending_bunker_setup() {
    *PENDING_BUNKER_SETUP.lock().unwrap() = None;
}

/// Staged NIP-55 pairing result between `login_with_nip55` and the subsequent
/// encryption-flow commit. Both fields are public material (identity pubkey +
/// signer package name), so no `Zeroizing` — unlike bunker, a NIP-55 account
/// stages nothing secret. `setup_encryption` / `skip_encryption` reads this
/// when `signer_kind() == Nip55` to write the right settings rows via
/// `commit_nip55_account_setup`.
///
/// Tuple order: (user_pubkey_hex, signer_package).
pub static PENDING_NIP55_SETUP: std::sync::Mutex<Option<(String, String)>> =
    std::sync::Mutex::new(None);

#[inline]
pub fn set_pending_nip55_setup(user_pubkey_hex: String, signer_package: String) {
    *PENDING_NIP55_SETUP.lock().unwrap() = Some((user_pubkey_hex, signer_package));
}

#[inline]
pub fn pending_nip55_setup() -> Option<(String, String)> {
    PENDING_NIP55_SETUP.lock().unwrap().clone()
}

#[inline]
pub fn clear_pending_nip55_setup() {
    *PENDING_NIP55_SETUP.lock().unwrap() = None;
}

pub static ENCRYPTION_KEY: crate::crypto::GuardedKey = crate::crypto::GuardedKey::empty();

/// The at-rest key a PIN change just replaced, kept openable for a few seconds so a read that
/// straddled the change still opens. Empty at every other time.
pub static PREVIOUS_AT_REST_KEY: crate::crypto::GuardedKey = crate::crypto::GuardedKey::empty();

pub static ENCRYPTION_ENABLED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_encryption_enabled_fast() -> bool { ENCRYPTION_ENABLED.load(Ordering::Acquire) }

#[inline]
pub fn set_encryption_enabled(enabled: bool) { ENCRYPTION_ENABLED.store(enabled, Ordering::Release); }

/// Resolve "is this account encrypted?" from raw DB settings. Single
/// source of truth — every caller (crypto::is_encryption_enabled,
/// init_encryption_enabled, Android bg-sync) delegates here, so the
/// answer is consistent regardless of code path.
///
/// Rules:
///   * `encryption_enabled = "false"` → not encrypted (explicit opt-out)
///   * `encryption_enabled = "true"`  → encrypted (explicit opt-in)
///   * row missing                    → encrypted iff `security_type` exists
///
/// The `security_type` fallback handles pre-multi-account installs that
/// wrote `security_type` without `encryption_enabled`. A brand-new
/// account has neither row, so the answer is "not encrypted".
pub fn resolve_encryption_enabled(
    encryption_enabled_row: Option<&str>,
    security_type_row: Option<&str>,
) -> bool {
    match encryption_enabled_row {
        Some("false") => false,
        Some(_) => true,
        None => security_type_row.is_some(),
    }
}

/// Resolve from the current account's DB via the global settings helper.
/// Returns `false` if the DB is not yet open.
pub fn resolve_encryption_enabled_from_db() -> bool {
    let enc = crate::db::get_sql_setting("encryption_enabled".to_string()).ok().flatten();
    let sec = crate::db::get_sql_setting("security_type".to_string()).ok().flatten();
    resolve_encryption_enabled(enc.as_deref(), sec.as_deref())
}

pub fn init_encryption_enabled() {
    let enabled = resolve_encryption_enabled_from_db();
    set_encryption_enabled(enabled);
}

#[cfg(test)]
mod resolve_encryption_enabled_tests {
    use super::*;

    #[test]
    fn explicit_false_wins_even_with_security_type() {
        assert!(!resolve_encryption_enabled(Some("false"), Some("password")));
    }

    #[test]
    fn explicit_true_is_encrypted() {
        assert!(resolve_encryption_enabled(Some("true"), None));
    }

    #[test]
    fn missing_row_defaults_to_security_type_presence() {
        // Pre-multi-account install that wrote security_type but not the
        // encryption_enabled flag — must be treated as encrypted.
        assert!(resolve_encryption_enabled(None, Some("password")));
        // Fresh account with no rows — must be treated as unencrypted.
        assert!(!resolve_encryption_enabled(None, None));
    }

    #[test]
    fn explicit_non_false_value_is_encrypted() {
        // Anything other than the literal "false" string is treated as
        // encrypted, matching the previous behaviour of `v != "false"`.
        assert!(resolve_encryption_enabled(Some("1"), None));
        assert!(resolve_encryption_enabled(Some(""), None));
    }
}

// ============================================================================
// Per-session globals — must be resettable for inline account swap
// ============================================================================
//
// `NOSTR_CLIENT` and `MY_PUBLIC_KEY` are RwLock<Option<_>> rather than OnceLock
// so `reset_session()` can swap them atomically — mobile cannot rely on
// `app.restart()`. Callers should prefer the helpers below over locking directly.

/// The relay client and identity this account signs and publishes with.
///
/// On the session, not global: a task that outlived a swap would otherwise
/// reach the NEW account's client and publish the previous account's payload
/// under the wrong identity. A bound task reaches the client it started with,
/// which the swap has already shut down, so its send fails instead.
///
/// A login fills these before its database exists — creating an account
/// installs the keys and client, and only reaches `init_database` once the
/// user has chosen a PIN — which is why binding a session to a database keeps
/// what it holds rather than replacing it.
struct ActiveClient;
struct ActiveIdentity;

fn active_client() -> std::sync::Arc<RwLock<Option<Client>>> {
    crate::db::current_session().scoped::<ActiveClient, _>()
}

fn active_identity() -> std::sync::Arc<RwLock<Option<PublicKey>>> {
    crate::db::current_session().scoped::<ActiveIdentity, _>()
}

pub static MY_SECRET_KEY: crate::crypto::GuardedKey = crate::crypto::GuardedKey::empty();

/// Get a clone of the active Nostr client. The clone is cheap — `Client`
/// is internally `Arc`-counted, so all clones share connections, signers,
/// and subscription state. Returns `None` when no session is active.
#[inline]
pub fn nostr_client() -> Option<Client> {
    active_client().read().unwrap().as_ref().cloned()
}

/// Returns `true` when there is an active session (client + pubkey set).
#[inline]
pub fn has_active_session() -> bool {
    active_client().read().unwrap().is_some()
}

/// Get the active user's public key. `PublicKey` is `Copy`, so this is by-value.
#[inline]
pub fn my_public_key() -> Option<PublicKey> {
    *active_identity().read().unwrap()
}

/// Install the Nostr client for the current session. Replaces any prior client
/// without shutting it down — `reset_session()` is responsible for orderly
/// teardown of the outgoing client.
#[inline]
pub fn set_nostr_client(client: Client) {
    *active_client().write().unwrap() = Some(client);
}

/// Install `client` only if this session has none yet, reporting whether it
/// took. Logins race on Android, where a service-only background sync can
/// install a client before the Activity's login reaches this point; the loser
/// must drop its own rather than replace a live one.
#[inline]
pub fn set_nostr_client_if_absent(client: Client) -> bool {
    let owner = active_client();
    let mut slot = owner.write().unwrap();
    if slot.is_some() {
        return false;
    }
    *slot = Some(client);
    true
}

/// Install the active user's public key for the current session.
#[inline]
pub fn set_my_public_key(pk: PublicKey) {
    *active_identity().write().unwrap() = Some(pk);
    crate::transport::prelogin::note_identity(&pk);
}

/// Atomically take the current Nostr client out of global state.
/// Used by `reset_session()` so the post-take shutdown call doesn't race
/// with new readers.
#[inline]
pub fn take_nostr_client() -> Option<Client> {
    active_client().write().unwrap().take()
}

/// Clear `MY_PUBLIC_KEY`. The Nostr client is taken separately via
/// `take_nostr_client()` so the caller can shut it down before this clear.
#[inline]
pub fn clear_my_public_key() {
    *active_identity().write().unwrap() = None;
}

#[derive(Clone)]
pub struct PendingInviteAcceptance {
    pub invite_code: String,
    pub inviter_pubkey: PublicKey,
}

// Per-session: tracks an invite captured during account-creation that should
// be broadcast to relays once login finishes. Must reset across accounts so a
// pending invite captured for account A doesn't auto-execute on account B.
/// Per-account: an invite captured while creating account A must never
/// auto-execute once account B is live.
struct PendingInvite;

fn pending_invite_slot() -> std::sync::Arc<RwLock<Option<PendingInviteAcceptance>>> {
    crate::db::current_session().scoped::<PendingInvite, _>()
}

#[inline]
pub fn pending_invite() -> Option<PendingInviteAcceptance> {
    pending_invite_slot().read().unwrap().clone()
}

#[inline]
pub fn set_pending_invite(invite: PendingInviteAcceptance) {
    *pending_invite_slot().write().unwrap() = Some(invite);
}

#[inline]
pub fn clear_pending_invite() {
    *pending_invite_slot().write().unwrap() = None;
}

// ============================================================================
// Session generation — defends background tasks against account swaps
// ============================================================================
//

/// Session-scoped tombstones for NIP-09-deleted DM messages. The batching flush consults
/// this POSITIVE deletion signal — never STATE absence, which also means LRU-evicted or
/// expired (an evicted archive message must still persist). A deletion whose target sat
/// unflushed in a batch buffer no-ops its `delete_event`; the tombstone stops the flush
/// from resurrecting the row. Cleared on session bump — ids are meaningless across accounts.
/// Event ids are meaningless across accounts, so this is per-account.
struct DeletedMessageTombstones;

/// Event ids as their 32 bytes; anything that isn't lowercase 64-hex is kept verbatim, so
/// membership is exactly the strings' own.
#[derive(Default)]
struct Tombstones {
    ids: HashSet<[u8; 32]>,
    other: HashSet<Box<str>>,
}

impl Tombstones {
    fn key(id: &str) -> Option<[u8; 32]> {
        crate::community::id32_from_hex(id)
    }

    fn insert(&mut self, id: &str) {
        match Self::key(id) {
            Some(bytes) => self.ids.insert(bytes),
            None => self.other.insert(id.into()),
        };
    }

    fn contains(&self, id: &str) -> bool {
        match Self::key(id) {
            Some(bytes) => self.ids.contains(&bytes),
            None => self.other.contains(id),
        }
    }
}

fn deleted_message_tombstones() -> std::sync::Arc<std::sync::Mutex<Tombstones>> {
    crate::db::current_session().scoped::<DeletedMessageTombstones, _>()
}

/// Record that `message_id` was deleted this session (called where the deletion is decided).
pub fn note_message_deleted(message_id: &str) {
    if let Ok(mut set) = deleted_message_tombstones().lock() {
        set.insert(message_id);
    }
}

/// Seed the tombstone set from the account's durable `deleted_messages` rows,
/// making `was_message_deleted` survive restarts and account swaps. Called at
/// account DB init (the set was cleared by the session bump on swap).
pub fn seed_message_tombstones<S: AsRef<str>>(ids: impl IntoIterator<Item = S>) {
    // In chunks: the scan feeding this can be long, and ingest reads the set meanwhile.
    let tombstones = deleted_message_tombstones();
    let mut ids = ids.into_iter().peekable();
    while ids.peek().is_some() {
        let chunk: Vec<S> = ids.by_ref().take(1024).collect();
        let Ok(mut set) = tombstones.lock() else { return };
        for id in &chunk {
            set.insert(id.as_ref());
        }
    }
}

/// Whether `message_id` was deleted this session.
pub fn was_message_deleted(message_id: &str) -> bool {
    deleted_message_tombstones().lock().map(|s| s.contains(message_id)).unwrap_or(false)
}

/// Drop the delete-tombstone set on an account switch.
///
/// All that survives of the old session-generation counter. Everything else it
/// invalidated now lives on the session and goes when that does; these are
/// keyed by event id, which means nothing across accounts either way.
#[inline]
pub fn clear_message_tombstones() {
    if let Ok(mut set) = deleted_message_tombstones().lock() {
        *set = Tombstones::default();
    }
}

#[cfg(test)]
mod tombstone_tests {
    use super::Tombstones;

    #[test]
    fn hex_ids_are_held_as_bytes_and_match_only_themselves() {
        let mut t = Tombstones::default();
        let id = "ab".repeat(32);
        t.insert(&id);
        assert!(t.contains(&id));
        assert_eq!((t.ids.len(), t.other.len()), (1, 0));
        assert!(!t.contains(&"ab".repeat(31)));
        assert!(!t.contains(&"AB".repeat(32)), "an uppercase spelling is a different string");
    }

    #[test]
    fn anything_else_is_kept_verbatim() {
        let mut t = Tombstones::default();
        let (upper, not_hex, long) = ("AB".repeat(32), "zz".repeat(32), "ab".repeat(33));
        for id in ["moderated_evt", "", upper.as_str(), not_hex.as_str(), long.as_str()] {
            t.insert(id);
            assert!(t.contains(id), "{id:?}");
        }
        assert_eq!(t.ids.len(), 0);
        assert!(!t.contains(&"ab".repeat(32)), "the lowercase twin of an uppercase id was never deleted");
        assert!(!t.contains("moderated"));
    }
}

#[cfg(test)]
mod session_identity_tests {

    /// A session held across an account switch knows it is no longer the one on
    /// screen. This is the whole of what the generation counter used to do, and
    /// it now falls out of the session's own identity.
    #[test]
    fn a_held_session_stops_being_live_once_the_account_changes() {
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let held = crate::db::current_session();
        assert!(held.is_live(), "it is the account on screen to begin with");

        crate::db::close_database();

        assert!(!held.is_live(), "and is not, once the account has switched");
        assert!(crate::db::current_session().is_live(), "whoever came next is");
        assert_ne!(held.id(), crate::db::current_session_id(), "they are different sessions");
    }
}

#[cfg(test)]
mod session_globals_tests {
    use super::*;

    /// Single combined test so the global statics aren't raced by parallel
    /// runners. cargo runs each `#[test]` on its own thread, so anything
    /// touching `MY_PUBLIC_KEY`/`PENDING_INVITE` lives here.
    #[test]
    fn session_helpers_round_trip_and_clear() {
        // It swaps the live session, which every transport test reads.
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // Defensive cleanup: a previous test panic could have left global
        // state behind. Start every run from a known-empty baseline so the
        // assertions below aren't fooled by leftover values.
        clear_my_public_key();
        clear_pending_invite();

        // PublicKey: set → get → clear.
        // Use a deterministic key from a hex seed; nostr_sdk::prelude::Keys::generate()
        // would also work but the deterministic form makes failures easier
        // to reproduce.
        let keys = Keys::parse(
            "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5",
        ).expect("parse test nsec");
        let pk = keys.public_key();

        assert_eq!(my_public_key(), None, "starts as None");

        set_my_public_key(pk);
        assert_eq!(my_public_key(), Some(pk));

        clear_my_public_key();
        assert_eq!(my_public_key(), None, "cleared returns None");

        // PendingInvite: set → get → clear.
        let invite = PendingInviteAcceptance {
            invite_code: "abc123".to_string(),
            inviter_pubkey: pk,
        };

        assert!(pending_invite().is_none(), "starts as None");

        set_pending_invite(invite.clone());
        let got = pending_invite().expect("set then read");
        assert_eq!(got.invite_code, invite.invite_code);
        assert_eq!(got.inviter_pubkey, invite.inviter_pubkey);

        clear_pending_invite();
        assert!(pending_invite().is_none(), "cleared returns None");

        // Take semantics for nostr_client: with no client installed,
        // `take_nostr_client()` returns None and is_active is false.
        assert!(!has_active_session(), "no client installed");
        assert!(take_nostr_client().is_none(), "take from empty returns None");
        assert!(!has_active_session(), "still none after take-of-empty");

        // PendingBunkerSetup: set → peek → clear. The URL slot is wrapped in
        // Zeroizing<String> because it carries the NIP-46 single-use pairing
        // secret. The accessors return plain String clones so callers don't
        // accidentally consume the protected buffer.
        clear_pending_bunker_setup();
        assert!(pending_bunker_setup().is_none(), "starts as None");

        let url = "bunker://0123456789abcdef?relay=wss%3A%2F%2Frelay.example&secret=topsecret".to_string();
        let pk_hex = "0123456789abcdef".to_string();
        set_pending_bunker_setup(url.clone(), pk_hex.clone());

        // Peek twice — the underlying Zeroizing<String> must NOT be consumed
        // by either read, so successive reads return identical contents.
        let first = pending_bunker_setup().expect("first peek");
        let second = pending_bunker_setup().expect("second peek");
        assert_eq!(first.0, url, "url survives clone-out from Zeroizing");
        assert_eq!(first.0, second.0, "successive peeks return same data");
        assert_eq!(first.1, pk_hex);

        // Overwrite — replacing the slot scrubs the prior Zeroizing<String>
        // on Drop. We can't assert heap-residue from a test, but we CAN
        // assert the new value is what's exposed.
        let url2 = "bunker://feedface?relay=wss%3A%2F%2Falt".to_string();
        set_pending_bunker_setup(url2.clone(), "feedface".to_string());
        let after = pending_bunker_setup().expect("overwritten read");
        assert_eq!(after.0, url2);
        assert_eq!(after.1, "feedface");

        clear_pending_bunker_setup();
        assert!(pending_bunker_setup().is_none(), "cleared returns None");

        // PendingNip55Setup: set → peek → clear. Both fields are public
        // material (identity pubkey + signer package), so no Zeroizing — a
        // NIP-55 account stages nothing secret.
        clear_pending_nip55_setup();
        assert!(pending_nip55_setup().is_none(), "starts as None");

        let user_pk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
        let package = "com.greenart7c3.nostrsigner".to_string();
        set_pending_nip55_setup(user_pk.clone(), package.clone());
        let peek1 = pending_nip55_setup().expect("first peek");
        let peek2 = pending_nip55_setup().expect("second peek");
        assert_eq!(peek1.0, user_pk);
        assert_eq!(peek1.1, package);
        assert_eq!(peek1, peek2, "successive peeks return same data");

        // Overwrite replaces cleanly.
        set_pending_nip55_setup("beef".to_string(), "org.other.signer".to_string());
        let after55 = pending_nip55_setup().expect("overwritten read");
        assert_eq!(after55.0, "beef");
        assert_eq!(after55.1, "org.other.signer");

        clear_pending_nip55_setup();
        assert!(pending_nip55_setup().is_none(), "cleared returns None");

        // A session held across the switch knows it is no longer current.
        let held = crate::db::current_session();
        assert!(held.is_live(), "the account on screen to begin with");
        crate::db::close_database();
        assert!(!held.is_live(), "and not, once the account has switched");
    }
}

/// Which wrapper events this account has already ingested.
///
/// Per-account: the ids come from its relays and gate writes to its database.
/// Same handle shape as [`STATE`], and for the same reason.
pub struct WrapperIdCacheHandle;

pub static WRAPPER_ID_CACHE: WrapperIdCacheHandle = WrapperIdCacheHandle;

impl WrapperIdCacheHandle {
    pub async fn lock(&self) -> tokio::sync::OwnedMutexGuard<WrapperIdCache> {
        crate::db::current_session().scoped::<Self, Mutex<WrapperIdCache>>().lock_owned().await
    }
}

/// The account's chats and profiles in memory.
///
/// Locking resolves the session THIS work belongs to, so a task bound by
/// [`crate::db::spawn_bound`] reaches the state of the account it started
/// under — for its whole life, whoever logs in meanwhile. A swap installs a
/// fresh session, and with it a fresh state; the previous account's is dropped
/// with the session rather than cleared field by field.
///
/// Deliberately a handle rather than the `Mutex` itself: `STATE.lock().await`
/// reads the same at every call site, and cannot be hoisted into a `static` or
/// a long-lived struct where it would freeze one account's state in place.
pub struct ChatStateHandle;

pub static STATE: ChatStateHandle = ChatStateHandle;

impl ChatStateHandle {
    pub async fn lock(&self) -> tokio::sync::OwnedMutexGuard<ChatState> {
        crate::db::current_session().chat_state().lock_owned().await
    }

    /// For callers that must not block — notably Android's WebView threads,
    /// which have no tokio runtime.
    pub fn try_lock(&self) -> Result<tokio::sync::OwnedMutexGuard<ChatState>, tokio::sync::TryLockError> {
        crate::db::current_session().chat_state().try_lock_owned()
    }
}

/// Chat id currently visible to the user with auto-mark eligibility — set by
/// the frontend when the chat is open AND pinned to bottom AND the window is
/// active. Used by the inbound event handler to mark new messages read on
/// arrival, so the dock badge never bumps for messages the user is actively
/// watching. Cleared when any of those conditions flips.
/// Per-account: the id is one of this account's chats, and a contact shared
/// with another account would otherwise let A's open chat mark B's messages read.
struct ActiveChat;

fn active_chat() -> std::sync::Arc<RwLock<Option<String>>> {
    crate::db::current_session().scoped::<ActiveChat, _>()
}

pub fn set_active_chat(chat_id: Option<String>) {
    if let Ok(mut guard) = active_chat().write() {
        *guard = chat_id;
    }
}

pub fn get_active_chat() -> Option<String> {
    active_chat().read().ok().and_then(|g| g.clone())
}

/// Chats whose messages the UI holds (a page was served, not since evicted). The rest keep
/// only a short tail in memory: their pages are read from the database when opened.
struct UiHeldChats;

fn ui_held_chats() -> std::sync::Arc<RwLock<std::collections::HashSet<String>>> {
    crate::db::current_session().scoped::<UiHeldChats, _>()
}

pub fn note_chat_held(chat_id: &str) {
    let held = ui_held_chats();
    if held.read().is_ok_and(|g| g.contains(chat_id)) {
        return;
    }
    if let Ok(mut g) = held.write() {
        g.insert(chat_id.to_string());
    };
}

pub fn note_chat_released(chat_id: &str) {
    if let Ok(mut g) = ui_held_chats().write() {
        g.remove(chat_id);
    };
}

/// Messages a chat the UI doesn't hold keeps in memory: enough for its preview and unread run.
pub const UNHELD_CHAT_TAIL: usize = 20;

// ============================================================================
// Processing Gate — Controls event processing during encryption migration
// ============================================================================

pub static PROCESSING_GATE: AtomicBool = AtomicBool::new(true);
/// Events parked while the processing gate is closed (encryption migration).
/// Per-account: they are this account's inbound traffic, awaiting its key.
pub struct PendingEventsHandle;

pub static PENDING_EVENTS: PendingEventsHandle = PendingEventsHandle;

impl PendingEventsHandle {
    pub async fn lock(&self) -> tokio::sync::OwnedMutexGuard<Vec<(Event, bool)>> {
        crate::db::current_session().scoped::<Self, Mutex<Vec<(Event, bool)>>>().lock_owned().await
    }
}

#[inline]
pub fn is_processing_allowed() -> bool { PROCESSING_GATE.load(Ordering::Acquire) }
pub fn close_processing_gate() { PROCESSING_GATE.store(false, Ordering::Release); }
pub fn open_processing_gate() { PROCESSING_GATE.store(true, Ordering::Release); }

// ============================================================================
// ChatState
// ============================================================================

#[derive(Clone, Debug)]
pub struct ChatState {
    pub profiles: Vec<Profile>,
    pub chats: Vec<Chat>,
    pub interner: NpubInterner,
    pub is_syncing: bool,
    pub db_loaded: bool,
    /// Authoritative per-chat unread counts (chat_identifier → count), so the badge recount is an
    /// in-RAM fold instead of a whole-DB scan. Seeded once per account from `db::unread_counts` (see
    /// `unread_seeded`), then kept current per-chat: cleared on read, reconciled from the DB
    /// (`db::unread_count_for_chat`) on inbound / delete / mark-unread. SENDER-level muted/blocked
    /// filtering is baked into the DB-sourced values (mute/block toggles reseed); CHAT-level
    /// filtering happens at fold time, matching `sum_unread_from`. Cleared on account reset.
    pub unread_cache: std::collections::HashMap<String, u32>,
    /// False until `unread_cache` has been seeded from the DB for this account. Guards the one-time
    /// seed so the full-scan query runs once per login, never per message.
    pub unread_seeded: bool,
    #[cfg(debug_assertions)]
    pub cache_stats: crate::stats::CacheStats,
}

/// Whether a Community chat is one the user can actually reach.
///
/// Widescreen gives every LISTED channel a row, so a sibling's unread counts
/// like any other. Two kinds of row still have nowhere to go: a bare
/// persistence anchor with no community stamp, and a channel its community
/// stopped listing — a tombstoned or no-longer-readable one keeps its chat row
/// and history on disk, so its unread could never be seen or cleared.
fn chat_has_a_row(chat: &crate::chat::Chat) -> bool {
    if chat.metadata.community_id().is_none() {
        return false;
    }
    chat.is_surfaced_community_channel() || crate::notify::channel_is_listed(&chat.id)
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            chats: Vec::new(),
            interner: NpubInterner::new(),
            is_syncing: false,
            db_loaded: false,
            unread_cache: std::collections::HashMap::new(),
            unread_seeded: false,
            #[cfg(debug_assertions)]
            cache_stats: crate::stats::CacheStats::new(),
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    /// Drop the pictures every blocked profile still holds, for blocks made before a block
    /// erased them. Returns each changed profile to persist and the files to delete
    /// (`profile::sync::persist_forgotten_imagery`).
    pub fn forget_blocked_imagery(&mut self) -> (Vec<SlimProfile>, Vec<crate::profile::CachedImage>) {
        let ids: Vec<u16> = self.profiles.iter().filter(|p| p.flags.is_blocked() && p.has_imagery()).map(|p| p.id).collect();
        let mut slims = Vec::with_capacity(ids.len());
        let mut images = Vec::new();
        for id in ids {
            let held = self.get_profile_mut_by_id(id).map(|p| p.forget_imagery()).unwrap_or_default();
            images.extend(crate::profile::sync::unshared(self, id, held));
            slims.extend(self.serialize_profile(id));
        }
        (slims, images)
    }

    pub fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>, my_npub: &str) {
        self.profiles.reserve(slim_profiles.len());
        self.interner.reserve(slim_profiles.len());
        for slim in slim_profiles {
            let mut full_profile = slim.to_profile();
            full_profile.flags.set_mine(slim.id == my_npub);
            self.insert_or_replace_profile(&slim.id, full_profile);
        }
    }

    /// Drop the growth slack in the profile list and interner once the DB load is in.
    pub fn shrink_after_load(&mut self) {
        self.profiles.shrink_to_fit();
        self.interner.shrink_to_fit();
    }

    pub fn insert_or_replace_profile(&mut self, npub: &str, mut profile: Profile) {
        let id = self.interner.intern(npub);
        profile.id = id;
        match self.profiles.binary_search_by(|p| p.id.cmp(&id)) {
            Ok(idx) => self.profiles[idx] = profile,
            Err(idx) => self.profiles.insert(idx, profile),
        }
    }

    /// The people whose DM requests wait on us. Their pictures wait too, unless a Community
    /// counts them: see `requests::held_pictures`.
    pub fn request_senders(&self) -> std::collections::HashSet<&str> {
        self.chats.iter().filter(|c| c.is_request()).map(|c| c.id.as_str()).collect()
    }

    /// Whether this person's DM request waits on us. Whether their pictures do is
    /// `requests::pictures_held`.
    pub fn sent_request(&self, npub: &str) -> bool {
        self.chats.iter().any(|c| c.id == npub && c.is_request())
    }

    /// Whether what this person sends waits for an answer or an accept: someone we've
    /// never written to, including someone with no chat here yet.
    pub fn dm_unanswered(&self, npub: &str) -> bool {
        self.chats.iter().find(|c| c.id == npub).is_none_or(|c| c.is_unanswered())
    }

    pub fn get_profile(&self, npub: &str) -> Option<&Profile> {
        self.interner.lookup(npub).and_then(|id| self.get_profile_by_id(id))
    }

    pub fn get_profile_mut(&mut self, npub: &str) -> Option<&mut Profile> {
        self.interner.lookup(npub).and_then(move |id| self.get_profile_mut_by_id(id))
    }

    #[inline]
    pub fn get_profile_by_id(&self, id: u16) -> Option<&Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id)).ok().map(|idx| &self.profiles[idx])
    }

    #[inline]
    pub fn get_profile_mut_by_id(&mut self, id: u16) -> Option<&mut Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id)).ok().map(move |idx| &mut self.profiles[idx])
    }

    pub fn serialize_profile(&self, id: u16) -> Option<SlimProfile> {
        self.get_profile_by_id(id).map(|p| SlimProfile::from_profile(p, &self.interner))
    }

    // ========================================================================
    // Chat Management
    // ========================================================================

    pub fn get_chat(&self, id: &str) -> Option<&Chat> { self.chats.iter().find(|c| c.id == id) }
    pub fn get_chat_mut(&mut self, id: &str) -> Option<&mut Chat> { self.chats.iter_mut().find(|c| c.id == id) }

    pub fn create_dm_chat(&mut self, their_npub: &str) -> String {
        if self.get_chat(their_npub).is_none() {
            let chat = Chat::new_dm(their_npub.to_string(), &mut self.interner);
            self.chats.push(chat);
        }
        their_npub.to_string()
    }

    // ========================================================================
    // Message Management
    // ========================================================================

    /// Ensure a Community channel chat exists, created as `ChatType::Community`.
    pub fn ensure_community_chat(&mut self, channel_id: &str) {
        // An empty id would make a chat nothing can open or delete.
        if channel_id.is_empty() {
            return;
        }
        if !self.chats.iter().any(|c| c.id == channel_id) {
            let chat =
                Chat::new_community_channel(channel_id.to_string(), Vec::new(), &mut self.interner);
            self.chats.push(chat);
        }
    }

    /// Create-or-update a Community channel chat with its display metadata, so the chat
    /// row carries name/description/owning-community directly (and persists + loads like
    /// any DM — no separate hydrate). `is_owner`/`has_icon` read as "true"/"1"
    /// strings in `custom_fields`. The caller persists the row (`save_slim_chat`).
    /// Sibling rows of one community share one header.
    ///
    /// `name` is the COMMUNITY's name (every channel row carries it, so the chat list can
    /// label a community from any one of its rows); `channel_name` is this channel's own.
    /// `primary_channel` is the community's primary channel id — equal to `channel_id` on
    /// that row, and what the UI uses to render one list row per community while still
    /// holding a chat per channel.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_community_chat(
        &mut self,
        channel_id: &str,
        name: &str,
        description: &str,
        community_id: &str,
        is_owner: bool,
        has_icon: bool,
        owner_npub: Option<&str>,
        created_at_ms: Option<u64>,
        dissolved: bool,
        channel_name: &str,
        primary_channel: &str,
    ) {
        self.ensure_community_chat(channel_id);
        let Some(idx) = self.chats.iter().position(|c| c.id == channel_id) else { return };
        let reg = crate::chat::CommunityRegistration {
            community_id,
            name,
            description,
            primary_channel,
            // The PROVEN owner npub (verified upstream) — for the crown/hoist + in-chat tag.
            owner_npub,
            // Join time — sorts a not-yet-active community by when we joined, not to the bottom.
            created_at_ms,
            is_owner,
            // Owner-dissolution seal — the GUI reads this to lock the composer + show the end divider.
            dissolved,
            has_icon,
        };
        let header = self.chats[idx].metadata.registration_header(&reg);
        let header = self
            .chats
            .iter()
            .filter_map(|c| c.metadata.community_header())
            .find(|h| ***h == header)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(header));
        self.chats[idx].metadata.stamp_community(header, channel_name, created_at_ms.is_some());
    }

    pub fn add_message_to_chat(&mut self, chat_id: &str, message: &Message) -> bool {
        let compact = CompactMessage::from_message(message, &mut self.interner);

        let (is_msg_added, chat_idx) = if let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) {
            let added = self.chats[idx].add_compact_message(compact);
            (added, idx)
        } else {
            let mut chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string(), &mut self.interner)
            } else {
                Chat::new(chat_id.to_string(), ChatType::Community, vec![])
            };
            let was_added = chat.add_compact_message(compact);
            self.chats.push(chat);
            (was_added, self.chats.len() - 1)
        };

        if is_msg_added && chat_idx > 0 {
            let this_time = self.chats[chat_idx].last_message_time();
            let target = self.chats[..chat_idx].iter()
                .position(|c| c.last_message_time() <= this_time)
                .unwrap_or(chat_idx);
            if target < chat_idx {
                self.chats[target..=chat_idx].rotate_right(1);
            }
        }

        is_msg_added
    }

    /// Trim every chat the UI doesn't hold down to its tail. Amortised: a chat is only cut
    /// once it holds twice the tail, so one arriving message never triggers a trim.
    pub fn trim_unheld_chats(&mut self) -> usize {
        let held = ui_held_chats();
        let Ok(held) = held.read() else { return 0 };
        self.chats
            .iter_mut()
            .filter(|c| c.messages.len() > UNHELD_CHAT_TAIL * 2 && !held.contains(&c.id))
            .map(|c| c.messages.retain_newest(UNHELD_CHAT_TAIL))
            .sum()
    }

    pub fn add_messages_to_chat_batch(&mut self, chat_id: &str, messages: Vec<Message>) -> usize {
        if messages.is_empty() { return 0; }

        let compact_messages: Vec<_> = messages.into_iter()
            .map(|msg| CompactMessage::from_message_owned(msg, &mut self.interner))
            .collect();

        let chat_idx = if let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) {
            idx
        } else {
            let chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string(), &mut self.interner)
            } else {
                Chat::new(chat_id.to_string(), ChatType::Community, vec![])
            };
            self.chats.push(chat);
            self.chats.len() - 1
        };

        let old_last_time = self.chats[chat_idx].messages.last_timestamp();
        self.chats[chat_idx].replied |= compact_messages.iter().any(|m| m.flags.is_mine());
        let added = self.chats[chat_idx].messages.insert_batch(compact_messages);

        if added > 0 && self.chats[chat_idx].messages.last_timestamp() != old_last_time && chat_idx > 0 {
            let this_time = self.chats[chat_idx].last_message_time();
            let target = self.chats[..chat_idx].iter()
                .position(|c| c.last_message_time() <= this_time)
                .unwrap_or(chat_idx);
            if target < chat_idx {
                self.chats[target..=chat_idx].rotate_right(1);
            }
        }

        added
    }

    /// Add a message to a participant's DM chat. Creates profile if missing.
    ///
    /// Unlike the src-tauri version, emitting `profile_update` is the caller's responsibility.
    pub fn add_message_to_participant(&mut self, their_npub: &str, message: &Message) -> bool {
        let id = self.interner.intern(their_npub);
        if self.get_profile_by_id(id).is_none() {
            let profile = Profile::new();
            self.insert_or_replace_profile(their_npub, profile);

            // Emit profile update via EventEmitter trait (replaces TAURI_APP.emit)
            if let Some(slim) = self.serialize_profile(id) {
                emit_event("profile_update", &slim);
            }
        }

        let chat_id = self.create_dm_chat(their_npub);
        self.add_message_to_chat(&chat_id, message)
    }

    // ========================================================================
    // Message Lookup
    // ========================================================================

    /// Index of the chat holding `message_id`, decoding the id once rather than per chat.
    fn chat_index_of_message(&self, message_id: &str) -> Option<usize> {
        if message_id.is_empty() { return None; }
        let id = crate::compact::encode_message_id(message_id);
        self.chats.iter().position(|chat| chat.messages.contains_id(&id))
    }

    pub fn find_message(&self, message_id: &str) -> Option<(&Chat, Message)> {
        if message_id.is_empty() { return None; }
        let id = crate::compact::encode_message_id(message_id);
        self.chats.iter().find_map(|chat| chat.messages.find_by_id(&id).map(|m| (chat, m.to_message(&self.interner))))
    }

    pub fn find_chat_for_message(&self, message_id: &str) -> Option<(usize, String)> {
        let idx = self.chat_index_of_message(message_id)?;
        Some((idx, self.chats[idx].id.clone()))
    }

    pub fn update_message<F>(&mut self, message_id: &str, f: F) -> Option<(String, Message)>
    where F: FnOnce(&mut CompactMessage)
    {
        self.update_message_with(message_id, |m, _| f(m))
    }

    /// [`Self::update_message`] for a change that interns (an edit's emoji).
    pub fn update_message_with<F>(&mut self, message_id: &str, f: F) -> Option<(String, Message)>
    where F: FnOnce(&mut CompactMessage, &mut NpubInterner)
    {
        if message_id.is_empty() { return None; }
        let id = crate::compact::encode_message_id(message_id);
        let chat = self.chats.iter_mut().find(|c| c.messages.contains_id(&id))?;
        let msg = chat.messages.find_by_id_mut(&id)?;
        f(msg, &mut self.interner);
        Some((chat.id.clone(), msg.to_message(&self.interner)))
    }

    pub fn update_message_in_chat<F>(&mut self, chat_id: &str, message_id: &str, f: F) -> Option<Message>
    where F: FnOnce(&mut CompactMessage)
    {
        self.update_message_in_chat_with(chat_id, message_id, |m, _| f(m))
    }

    /// [`Self::update_message_in_chat`] for a change that interns (an edit's emoji).
    pub fn update_message_in_chat_with<F>(&mut self, chat_id: &str, message_id: &str, f: F) -> Option<Message>
    where F: FnOnce(&mut CompactMessage, &mut NpubInterner)
    {
        let chat = self.chats.iter_mut().find(|c| c.id == chat_id)?;
        let msg = chat.messages.find_by_hex_id_mut(message_id)?;
        f(msg, &mut self.interner);
        Some(msg.to_message(&self.interner))
    }

    pub fn finalize_pending_message(&mut self, chat_id: &str, pending_id: &str, real_id: &str) -> Option<(String, Message)> {
        let chat_idx = self.chats.iter().position(|c| c.id == chat_id)?;
        let messages = &mut self.chats[chat_idx].messages;
        let pending = crate::compact::encode_message_id(pending_id);
        let real = crate::simd::hex::hex_to_bytes_32(real_id);
        if let Some(msg) = messages.find_by_id_mut(&pending) {
            msg.set_pending(false);
        }
        // The real id can already be held (an echo landed first): that copy stands, the
        // pending one goes rather than leaving two rows under one id.
        if !messages.rekey(&pending, real) && messages.contains_id(&real) {
            messages.remove_by_id(&pending);
        }
        self.chats[chat_idx].get_compact_message(real_id)
            .map(|m| (pending_id.to_string(), m.to_message(&self.interner)))
    }

    pub fn update_attachment<F>(&mut self, chat_hint: &str, msg_id: &str, attachment_id: &str, f: F) -> bool
    where F: FnOnce(&mut CompactAttachment)
    {
        for chat in &mut self.chats {
            let is_target = match &chat.chat_type {
                // Community channels are addressed by their id.
                ChatType::Community => chat.id == chat_hint,
                ChatType::DirectMessage => chat.has_participant(chat_hint, &self.interner),
            };
            if is_target {
                if let Some(msg) = chat.messages.find_by_hex_id_mut(msg_id) {
                    if let Some(att) = msg.attachments.iter_mut().find(|a| a.id_eq(attachment_id)) {
                        f(att);
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn add_attachment_to_message(&mut self, chat_id: &str, msg_id: &str, attachment: CompactAttachment) -> bool {
        let chat_idx = match self.chats.iter().position(|c| c.id == chat_id || c.has_participant(chat_id, &self.interner)) {
            Some(idx) => idx,
            None => return false,
        };
        if let Some(msg) = self.chats[chat_idx].messages.find_by_hex_id_mut(msg_id) {
            msg.attachments.push(attachment);
            true
        } else { false }
    }

    pub fn add_reaction_to_message(&mut self, message_id: &str, reaction: Reaction) -> Option<(String, bool)> {
        if message_id.is_empty() { return None; }
        let chat_idx = self.chat_index_of_message(message_id)?;
        let chat_id = self.chats[chat_idx].id.clone();
        let msg = self.chats[chat_idx].get_compact_message_mut(message_id)?;
        let added = msg.add_reaction(reaction, &mut self.interner);
        Some((chat_id, added))
    }

    /// Locate a reaction by its event id across all chats.
    /// Returns `(chat_id, parent_message_id, author_npub, is_community)`.
    pub fn find_reaction(&self, reaction_id: &str) -> Option<(String, String, String, bool)> {
        if reaction_id.is_empty() { return None; }
        let target = crate::simd::hex::hex_to_bytes_32(reaction_id);
        for chat in &self.chats {
            for msg in chat.iter_compact() {
                if let Some(r) = msg.reactions.iter().find(|r| r.id == target) {
                    let author = self.interner.resolve(r.author_idx).unwrap_or("").to_string();
                    return Some((chat.id.clone(), msg.id_hex(), author, chat.is_community()));
                }
            }
        }
        None
    }

    /// Remove a reaction from its parent message. Returns `(chat_id, updated Message)`
    /// for the UI refresh, or `None` if the reaction wasn't present.
    pub fn remove_reaction_from_message(&mut self, message_id: &str, reaction_id: &str) -> Option<(String, Message)> {
        if message_id.is_empty() { return None; }
        let chat_idx = self.chat_index_of_message(message_id)?;
        let removed = self.chats[chat_idx]
            .get_compact_message_mut(message_id)
            .map(|m| m.remove_reaction(reaction_id))
            .unwrap_or(false);
        if !removed { return None; }
        let chat_id = self.chats[chat_idx].id.clone();
        self.chats[chat_idx]
            .get_compact_message(message_id)
            .map(|m| (chat_id, m.to_message(&self.interner)))
    }

    pub fn remove_message(&mut self, message_id: &str) -> Option<(String, Message)> {
        if message_id.is_empty() { return None; }
        for chat in &mut self.chats {
            if let Some(compact) = chat.messages.find_by_hex_id(message_id) {
                let msg = compact.to_message(&self.interner);
                let removed_id = *compact.id();
                let removed_at = compact.at;
                let chat_id = chat.id.clone();
                let was_marker = chat.last_read == removed_id;
                let was_unread_from = chat.unread_from == removed_id;
                chat.messages.remove_by_hex_id(message_id);
                // A "Mark as Unread" from it moves on to the next message, or ends with none left.
                if was_unread_from {
                    chat.unread_from = chat.messages.iter()
                        .find(|m| m.at >= removed_at)
                        .map(|m| *m.id())
                        .unwrap_or([0u8; 32]);
                }
                // A deleted read marker leaves `last_read` dangling and collapses the unread anchor
                // (badge stuck at 99+); retreat it to the newest surviving contact message before the
                // deleted one, or clear it. Mirrors the DB retreat in `db::events::delete_event`.
                if was_marker {
                    chat.last_read = chat.messages.iter().rev()
                        .find(|m| m.at <= removed_at && !m.flags.is_mine())
                        .map(|m| *m.id())
                        .unwrap_or([0u8; 32]);
                }
                return Some((chat_id, msg));
            }
        }
        None
    }

    pub fn message_exists(&self, message_id: &str) -> bool {
        self.chat_index_of_message(message_id).is_some()
    }

    // ========================================================================
    // Unread Count
    // ========================================================================

    /// Sum DB-computed per-chat unread counts, applying the same muted/blocked filters as
    /// [`count_unread_messages`] but sourcing each COUNT from `counts` (chat_identifier → unread)
    /// rather than walking in-memory messages — so it's correct even when only the last message per
    /// chat is in RAM (the boot state). Muted chats and blocked-DM contacts contribute 0.
    pub fn sum_unread_from(&self, counts: &std::collections::HashMap<String, u32>) -> u32 {
        let mut total = 0u32;
        for chat in &self.chats {
            // Only a room that rings for everything pushes the OS badge. At Mentions or
            // Nothing its traffic is the row's own business, and the row shows pings.
            if crate::notify::ring_for_chat(chat) != crate::notify::NotifyLevel::All {
                continue;
            }
            if !chat.is_community() {
                if let Some(id) = self.interner.lookup(&chat.id) {
                    if self.get_profile_by_id(id).is_some_and(|p| p.flags.is_blocked()) {
                        continue;
                    }
                }
            } else if !chat_has_a_row(chat) {
                continue;
            }
            total += counts.get(&chat.id).copied().unwrap_or(0);
        }
        total
    }

    // ------------------------------------------------------------------------
    // Unread cache — in-RAM per-chat counts, so the per-message badge recount
    // never re-scans the DB. See the `unread_cache` field doc.
    // ------------------------------------------------------------------------

    /// Seed the whole cache from a DB `unread_counts()` result and mark it seeded. Idempotent per
    /// login; a later call replaces the map wholesale (used only if a reseed is ever forced).
    pub fn unread_seed(&mut self, counts: std::collections::HashMap<String, u32>) {
        self.unread_cache = counts;
        self.unread_seeded = true;
    }

    /// The chat was read (opened / marked): zero its unread.
    pub fn unread_clear(&mut self, chat_id: &str) {
        self.unread_cache.remove(chat_id);
    }

    /// A live message landed: it is newer than any read point, so it counts here without a DB
    /// recount. An author the user blocked or muted counts for nothing, as in the seed.
    pub fn unread_bump(&mut self, chat_id: &str, author: Option<&str>) {
        if let Some(npub) = author {
            if self.get_profile(npub).is_some_and(|p| p.flags.is_blocked()) || self.get_chat(npub).is_some_and(|c| c.muted) {
                return;
            }
        }
        *self.unread_cache.entry(chat_id.to_string()).or_insert(0) += 1;
    }

    /// Reconcile a chat to an exact DB-computed count (delete / retreat / backfill). A zero drops the
    /// entry so the map stays small and `unwrap_or(0)` reads it as caught-up.
    pub fn unread_set(&mut self, chat_id: &str, count: u32) {
        if count == 0 {
            self.unread_cache.remove(chat_id);
        } else {
            self.unread_cache.insert(chat_id.to_string(), count);
        }
    }

    /// Total unread for the badge, from the cache, applying the same muted/blocked filters as
    /// [`sum_unread_from`].
    pub fn sum_unread(&self) -> u32 {
        self.sum_unread_from(&self.unread_cache)
    }

    /// A snapshot of the raw per-chat counts, for the frontend's boot badges.
    pub fn unread_snapshot(&self) -> std::collections::HashMap<String, u32> {
        self.unread_cache.clone()
    }

    pub fn count_unread_messages(&self) -> u32 {
        // Sender-level mutes: a muted DM silences that person's community messages too.
        let muted_senders: std::collections::HashSet<u16> = self
            .chats
            .iter()
            .filter(|c| !c.is_community() && crate::notify::muted_for_chat(c))
            .filter_map(|c| self.interner.lookup(&c.id))
            .collect();
        let mut total_unread = 0;
        for chat in &self.chats {
            if crate::notify::ring_for_chat(chat) != crate::notify::NotifyLevel::All { continue; }
            let is_group = chat.is_community();
            if !is_group {
                if let Some(id) = self.interner.lookup(&chat.id) {
                    if self.get_profile_by_id(id).is_some_and(|p| p.flags.is_blocked()) { continue; }
                }
            } else if !chat_has_a_row(chat) {
                continue;
            }
            let mut unread_count = 0u32;
            // A "Mark as Unread" from a chosen message counts back to it, past own replies.
            let from = (chat.unread_from != [0u8; 32]).then_some(chat.unread_from);
            for msg in chat.iter_compact().rev() {
                if let Some(from) = from {
                    let reached = *msg.id() == from;
                    if !msg.flags.is_mine() && !(is_group && msg.npub_idx != NO_NPUB
                        && (muted_senders.contains(&msg.npub_idx)
                            || self.get_profile_by_id(msg.npub_idx).is_some_and(|p| p.flags.is_blocked())))
                    {
                        unread_count += 1;
                    }
                    if reached { break; }
                    continue;
                }
                if msg.flags.is_mine() { break; }
                if chat.last_read != [0u8; 32] && *msg.id() == chat.last_read { break; }
                if is_group && msg.npub_idx != NO_NPUB {
                    if muted_senders.contains(&msg.npub_idx) { continue; }
                    if self.get_profile_by_id(msg.npub_idx).is_some_and(|p| p.flags.is_blocked()) { continue; }
                }
                unread_count += 1;
            }
            // Debug: log which chat has unread messages
            #[cfg(debug_assertions)]
            if unread_count > 0 {
                let last_read_hex = crate::compact::decode_message_id(&chat.last_read);
                let last_msg_hex = chat.messages.last().map(|m| crate::compact::decode_message_id(m.id())).unwrap_or_default();
                let msg_count = chat.message_count();
                eprintln!("[Unread] chat={} unread={} msgs_in_memory={} last_read={} last_msg={}",
                    &chat.id[..20.min(chat.id.len())], unread_count, msg_count,
                    &last_read_hex[..16.min(last_read_hex.len())], &last_msg_hex[..16.min(last_msg_hex.len())]);
            }
            total_unread += unread_count;
        }
        total_unread
    }

    // ========================================================================
    // Typing Indicators
    // ========================================================================

    pub fn update_typing_and_get_active(&mut self, chat_id: &str, npub: &str, expires_at: u64) -> Vec<String> {
        let handle = self.interner.intern(npub);
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.update_typing_participant(handle, expires_at);
            chat.get_active_typers(&self.interner)
        } else {
            Vec::new()
        }
    }
}

impl Default for ChatState {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use crate::profile::{Profile, SlimProfile, Status};
    use crate::simd::hex::bytes_to_hex_32;

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Create a deterministic 64-char hex ID from a u8 seed.
    /// First byte is always >= 0x10 to avoid the pending ID marker (0x01).
    fn make_hex_id(seed: u8) -> String {
        let mut bytes = [seed; 32];
        bytes[0] = seed.wrapping_add(0x10) | 0x10; // never 0x00 or 0x01
        bytes[1] = seed.wrapping_mul(37);
        bytes_to_hex_32(&bytes)
    }

    /// One channel registration, as `upsert_community_chat` takes it.
    struct Reg<'a> {
        name: &'a str,
        description: &'a str,
        community_id: &'a str,
        is_owner: bool,
        has_icon: bool,
        owner_npub: Option<&'a str>,
        created_at_ms: Option<u64>,
        dissolved: bool,
        channel_name: &'a str,
        primary_channel: &'a str,
    }

    impl Reg<'_> {
        fn apply(&self, state: &mut ChatState, channel_id: &str) {
            state.upsert_community_chat(
                channel_id, self.name, self.description, self.community_id, self.is_owner, self.has_icon,
                self.owner_npub, self.created_at_ms, self.dissolved, self.channel_name, self.primary_channel,
            );
        }

        /// The registration as a plain string map holds it.
        fn apply_plain(&self, cf: &mut std::collections::BTreeMap<String, String>) {
            let mut put = |k: &str, v: String| cf.insert(k.to_string(), v);
            put("name", self.name.into());
            put("channel_name", self.channel_name.into());
            put("primary_channel", self.primary_channel.into());
            put("description", self.description.into());
            put("community_id", self.community_id.into());
            put("is_owner", self.is_owner.to_string());
            put("dissolved", self.dissolved.to_string());
            if let Some(ms) = self.created_at_ms {
                put("created_at", ms.to_string());
            }
            match self.owner_npub {
                Some(n) => { cf.insert("owner_npub".into(), n.into()); }
                None => { cf.remove("owner_npub"); }
            }
            if self.has_icon {
                cf.insert("icon".into(), "1".into());
            } else {
                cf.remove("icon");
            }
        }
    }

    #[test]
    fn a_registration_stamps_exactly_what_the_plain_map_upsert_did() {
        let (cid, primary, sibling) = (make_hex_id(0x61), make_hex_id(0x62), make_hex_id(0x63));
        let first = Reg {
            name: "Club", description: "about", community_id: &cid, is_owner: true, has_icon: true,
            owner_npub: Some("npub1owner"), created_at_ms: Some(1_700_000_000_123), dissolved: false,
            channel_name: "general", primary_channel: &primary,
        };
        let later = Reg {
            name: "Club 2", description: "", has_icon: false, owner_npub: None, created_at_ms: None,
            dissolved: true, ..first
        };
        // Rows as they might have been stored: empty, keys outside the typed set, and typed
        // keys holding values only a plain pair can carry.
        let priors: [&[(&str, &str)]; 3] = [
            &[],
            &[("member_count", "9"), ("proto_version", "2")],
            &[("is_owner", "yes"), ("icon", "true"), ("created_at", "0042"), ("zz", "kept")],
        ];
        for prior in priors {
            for regs in [&[&first][..], &[&first, &later][..], &[&later][..]] {
                let mut state = ChatState::new();
                let mut cf: std::collections::BTreeMap<String, String> =
                    prior.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                state.ensure_community_chat(&sibling);
                state.chats[0].metadata = crate::chat::ChatMetadata::from_map(cf.clone().into_iter().collect());
                for reg in regs {
                    reg.apply(&mut state, &sibling);
                    reg.apply_plain(&mut cf);
                }
                let got: std::collections::BTreeMap<String, String> = state.chats[0]
                    .metadata
                    .entries()
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.into_owned()))
                    .collect();
                assert_eq!(got, cf, "prior {prior:?}");
            }
        }
    }

    #[test]
    fn sibling_channels_share_their_communitys_header() {
        let (cid, other, primary) = (make_hex_id(0x71), make_hex_id(0x72), make_hex_id(0x73));
        let reg = Reg {
            name: "Club", description: "", community_id: &cid, is_owner: false, has_icon: false,
            owner_npub: None, created_at_ms: Some(5), dissolved: false, channel_name: "general",
            primary_channel: &primary,
        };
        let mut state = ChatState::new();
        reg.apply(&mut state, &primary);
        Reg { channel_name: "random", ..reg }.apply(&mut state, &make_hex_id(0x74));
        Reg { community_id: &other, ..reg }.apply(&mut state, &make_hex_id(0x75));
        let header = |state: &ChatState, i: usize| state.chats[i].metadata.community_header().unwrap().clone();
        assert!(std::sync::Arc::ptr_eq(&header(&state, 0), &header(&state, 1)), "one community, one header");
        assert!(!std::sync::Arc::ptr_eq(&header(&state, 0), &header(&state, 2)), "another community keeps its own");
        assert_eq!(state.chats[1].metadata.get("channel_name").as_deref(), Some("random"));
        // A rename re-stamps every row onto a new shared header.
        let before = header(&state, 0);
        Reg { name: "Renamed", ..reg }.apply(&mut state, &primary);
        Reg { name: "Renamed", channel_name: "random", ..reg }.apply(&mut state, &make_hex_id(0x74));
        assert!(std::sync::Arc::ptr_eq(&header(&state, 0), &header(&state, 1)));
        assert!(!std::sync::Arc::ptr_eq(&header(&state, 0), &before));
        assert_eq!(state.chats[1].metadata.get_name(), Some("Renamed"));
    }

    /// Build a test Message with the given parameters.
    fn make_message(id_seed: u8, content: &str, timestamp_ms: u64, mine: bool) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine,
            ..Default::default()
        }
    }

    /// Build a message with an npub sender.
    fn make_message_from(id_seed: u8, content: &str, timestamp_ms: u64, npub: &str) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine: false,
            npub: Some(npub.to_string()),
            ..Default::default()
        }
    }

    /// Build a SlimProfile for testing.
    fn make_slim_profile(id: &str, name: &str) -> SlimProfile {
        SlimProfile {
            id: id.to_string(),
            name: name.to_string(),
            display_name: String::new(),
            nickname: String::new(),
            lud06: String::new(),
            lud16: String::new(),
            banner: String::new(),
            avatar: String::new(),
            about: String::new(),
            website: String::new(),
            nip05: String::new(),
            status: Status::new(),
            last_updated: 0,
            mine: false,
            bot: false,
            is_blocked: false,
            avatar_cached: String::new(),
            banner_cached: String::new(),
            stream_consent: false,
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    #[test]
    fn insert_or_replace_profile_creates_new() {
        let mut state = ChatState::new();
        let profile = Profile::new();
        state.insert_or_replace_profile("npub1alice", profile);

        assert!(
            state.get_profile("npub1alice").is_some(),
            "newly inserted profile should be retrievable"
        );
        assert_eq!(state.profiles.len(), 1, "should have exactly one profile");
    }

    #[test]
    fn insert_or_replace_profile_updates_existing() {
        let mut state = ChatState::new();
        let mut p1 = Profile::new();
        p1.set_name("Alice");
        state.insert_or_replace_profile("npub1alice", p1);

        let mut p2 = Profile::new();
        p2.set_name("Alice Updated");
        state.insert_or_replace_profile("npub1alice", p2);

        let fetched = state.get_profile("npub1alice").expect("profile should exist");
        assert_eq!(
            fetched.name(), "Alice Updated",
            "profile name should be updated after replace"
        );
        assert_eq!(state.profiles.len(), 1, "should still be one profile, not duplicated");
    }

    #[test]
    fn get_profile_by_npub() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.set_name("Bob");
        state.insert_or_replace_profile("npub1bob", profile);

        let fetched = state.get_profile("npub1bob").expect("profile should be found");
        assert_eq!(fetched.name(), "Bob", "fetched profile name should match");
    }

    #[test]
    fn get_profile_returns_none_for_unknown() {
        let state = ChatState::new();
        assert!(
            state.get_profile("npub1unknown").is_none(),
            "unknown npub should return None"
        );
    }

    #[test]
    fn get_profile_by_id_works() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.set_name("Charlie");
        state.insert_or_replace_profile("npub1charlie", profile);

        let id = state.interner.lookup("npub1charlie").expect("npub should be interned");
        let fetched = state.get_profile_by_id(id).expect("profile should be found by id");
        assert_eq!(fetched.name(), "Charlie", "profile looked up by id should match");
    }

    #[test]
    fn get_profile_by_id_returns_none_for_invalid() {
        let state = ChatState::new();
        assert!(
            state.get_profile_by_id(9999).is_none(),
            "invalid interner id should return None"
        );
    }

    #[test]
    fn merge_db_profiles_sets_mine_flag() {
        let mut state = ChatState::new();
        let slim_mine = make_slim_profile("npub1me", "Me");
        let slim_other = make_slim_profile("npub1other", "Other");

        state.merge_db_profiles(vec![slim_mine, slim_other], "npub1me");

        let me = state.get_profile("npub1me").expect("my profile should exist");
        assert!(me.flags.is_mine(), "my profile should have mine flag set");

        let other = state.get_profile("npub1other").expect("other profile should exist");
        assert!(!other.flags.is_mine(), "other profile should not have mine flag");
    }

    #[test]
    fn serialize_profile_roundtrip() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.set_name("Roundtrip");
        profile.set_about("Test about");
        profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1round", profile);

        let id = state.interner.lookup("npub1round").unwrap();
        let slim = state.serialize_profile(id).expect("serialization should succeed");

        assert_eq!(slim.id, "npub1round", "serialized id should match");
        assert_eq!(slim.name, "Roundtrip", "serialized name should match");
        assert_eq!(slim.about, "Test about", "serialized about should match");
        assert!(slim.is_blocked, "serialized blocked flag should be true");

        // Convert back to profile and re-insert
        let restored = slim.to_profile();
        assert_eq!(restored.name(), "Roundtrip", "restored name should match");
        assert!(restored.flags.is_blocked(), "restored blocked flag should be true");
    }

    #[test]
    fn binary_search_maintains_sorted_order_with_100_profiles() {
        let mut state = ChatState::new();

        // Insert 100 profiles in random-ish order
        let npubs: Vec<String> = (0..100).map(|i| format!("npub1user{:04}", i)).collect();
        let mut shuffled = npubs.clone();
        // Simple deterministic shuffle
        for i in (1..shuffled.len()).rev() {
            let j = (i * 37 + 13) % (i + 1);
            shuffled.swap(i, j);
        }

        for npub in &shuffled {
            let mut profile = Profile::new();
            profile.set_name(&npub);
            state.insert_or_replace_profile(npub, profile);
        }

        // All should be findable
        for npub in &npubs {
            assert!(
                state.get_profile(npub).is_some(),
                "profile {} should be retrievable after bulk insert",
                npub
            );
        }

        // Internal profiles vec should be sorted by id
        for window in state.profiles.windows(2) {
            assert!(
                window[0].id < window[1].id,
                "profiles should be sorted by interner id"
            );
        }

        assert_eq!(state.profiles.len(), 100, "should have exactly 100 profiles");
    }

    #[test]
    fn insert_same_npub_twice_updates_not_duplicates() {
        let mut state = ChatState::new();

        for i in 0..5 {
            let mut profile = Profile::new();
            profile.set_name(&format!("version_{}", i));
            state.insert_or_replace_profile("npub1repeated", profile);
        }

        assert_eq!(state.profiles.len(), 1, "repeated inserts should not create duplicates");
        let p = state.get_profile("npub1repeated").unwrap();
        assert_eq!(p.name(), "version_4", "should retain the last update");
    }

    #[test]
    fn get_profile_mut_modifies_in_place() {
        let mut state = ChatState::new();
        let profile = Profile::new();
        state.insert_or_replace_profile("npub1mutable", profile);

        let p = state.get_profile_mut("npub1mutable").expect("profile should exist");
        p.set_name("Mutated");

        let fetched = state.get_profile("npub1mutable").unwrap();
        assert_eq!(fetched.name(), "Mutated", "mutation should persist");
    }

    // ========================================================================
    // Chat Management
    // ========================================================================

    #[test]
    fn create_dm_chat_creates_new() {
        let mut state = ChatState::new();
        let id = state.create_dm_chat("npub1peer");

        assert_eq!(id, "npub1peer", "returned id should match the npub");
        assert!(state.get_chat("npub1peer").is_some(), "chat should be created");
        assert_eq!(state.chats.len(), 1, "should have exactly one chat");
    }

    #[test]
    fn create_dm_chat_is_idempotent() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");
        state.create_dm_chat("npub1peer");
        state.create_dm_chat("npub1peer");

        assert_eq!(state.chats.len(), 1, "repeated creates should not duplicate");
    }

    #[test]
    fn ensure_community_chat_idempotent() {
        let mut state = ChatState::new();
        state.ensure_community_chat("grp1");
        state.ensure_community_chat("grp1");

        assert_eq!(state.chats.len(), 1, "second call should not create a duplicate");
        let chat = state.get_chat("grp1").expect("community chat should exist");
        assert!(chat.is_community(), "should be a Community chat");
    }

    #[test]
    fn get_chat_by_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1x");

        let chat = state.get_chat("npub1x").expect("chat should exist");
        assert_eq!(chat.id, "npub1x", "chat id should match");
    }

    #[test]
    fn get_chat_returns_none_for_missing() {
        let state = ChatState::new();
        assert!(state.get_chat("nonexistent").is_none(), "missing chat should return None");
    }

    #[test]
    fn get_chat_mut_modifies_in_place() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1editable");

        let chat = state.get_chat_mut("npub1editable").expect("chat should exist");
        chat.muted = true;

        let refetched = state.get_chat("npub1editable").unwrap();
        assert!(refetched.muted, "muted flag should persist after mutation");
    }

    #[test]
    fn multiple_different_chats() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1alice");
        state.create_dm_chat("npub1bob");
        state.ensure_community_chat("grp1");

        assert_eq!(state.chats.len(), 3, "should have three distinct chats");
    }

    // ========================================================================
    // Message Management
    // ========================================================================

    #[test]
    fn add_message_to_chat_single() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "hello", 1700000000000, false);
        let added = state.add_message_to_chat("npub1peer", &msg);

        assert!(added, "first message should be added successfully");
        let chat = state.get_chat("npub1peer").unwrap();
        assert_eq!(chat.message_count(), 1, "chat should have one message");
    }

    #[test]
    fn add_message_to_chat_dedup_rejects_same_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg1 = make_message(1, "hello", 1700000000000, false);
        let msg2 = make_message(1, "duplicate", 1700000001000, false);

        let added1 = state.add_message_to_chat("npub1peer", &msg1);
        let added2 = state.add_message_to_chat("npub1peer", &msg2);

        assert!(added1, "first insert should succeed");
        assert!(!added2, "duplicate ID should be rejected");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 1,
            "should still have only one message"
        );
    }

    #[test]
    fn add_messages_to_chat_batch_works() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msgs: Vec<Message> = (0..10).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();

        let added = state.add_messages_to_chat_batch("npub1peer", msgs);
        assert_eq!(added, 10, "all 10 messages should be added");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 10,
            "chat should have 10 messages"
        );
    }

    #[test]
    fn add_messages_to_chat_batch_dedup() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Add first batch
        let msgs1: Vec<Message> = (0..5).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();
        state.add_messages_to_chat_batch("npub1peer", msgs1);

        // Add overlapping batch (IDs 3,4 overlap, 5,6,7 are new)
        let msgs2: Vec<Message> = (3..8).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();
        let added = state.add_messages_to_chat_batch("npub1peer", msgs2);

        assert_eq!(added, 3, "only 3 new messages should be added (5, 6, 7)");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 8,
            "total should be 8 unique messages"
        );
    }

    #[test]
    fn add_message_to_participant_creates_profile_and_chat() {
        let mut state = ChatState::new();

        let msg = make_message(1, "hi there", 1700000000000, false);
        let added = state.add_message_to_participant("npub1stranger", &msg);

        assert!(added, "message should be added");
        assert!(
            state.get_profile("npub1stranger").is_some(),
            "profile should be auto-created for unknown participant"
        );
        assert!(
            state.get_chat("npub1stranger").is_some(),
            "DM chat should be auto-created"
        );
    }

    #[test]
    fn add_message_to_participant_uses_existing_profile() {
        let mut state = ChatState::new();

        // Pre-create profile
        let mut profile = Profile::new();
        profile.set_name("Known User");
        state.insert_or_replace_profile("npub1known", profile);

        let msg = make_message(1, "hello", 1700000000000, false);
        state.add_message_to_participant("npub1known", &msg);

        // Profile should not be replaced
        let p = state.get_profile("npub1known").unwrap();
        assert_eq!(p.name(), "Known User", "existing profile should not be overwritten");
    }

    #[test]
    fn find_message_across_chats() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1a");
        state.create_dm_chat("npub1b");

        let msg_a = make_message(1, "in chat a", 1700000000000, false);
        let msg_b = make_message(2, "in chat b", 1700000001000, false);
        let msg_id_b = msg_b.id.clone();

        state.add_message_to_chat("npub1a", &msg_a);
        state.add_message_to_chat("npub1b", &msg_b);

        let (chat, found_msg) = state.find_message(&msg_id_b).expect("message should be found");
        assert_eq!(chat.id, "npub1b", "should find in correct chat");
        assert_eq!(found_msg.content, "in chat b", "content should match");
    }

    #[test]
    fn find_message_returns_none_for_unknown() {
        let state = ChatState::new();
        assert!(
            state.find_message(&make_hex_id(99)).is_none(),
            "unknown message id should return None"
        );
    }

    #[test]
    fn find_message_empty_id_returns_none() {
        let state = ChatState::new();
        assert!(state.find_message("").is_none(), "empty id should return None");
    }

    #[test]
    fn update_message_mutates_and_returns() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "original", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", &msg);

        let result = state.update_message(&msg_id, |cm| {
            cm.at += 1;
        });

        let (chat_id, updated) = result.expect("update should return Some");
        assert_eq!(chat_id, "npub1peer", "should return correct chat id");
        assert_eq!(updated.at, 1700000000001, "the update should land");
    }

    #[test]
    fn update_message_returns_none_for_missing() {
        let mut state = ChatState::new();
        let result = state.update_message(&make_hex_id(99), |_cm| {});
        assert!(result.is_none(), "updating nonexistent message should return None");
    }

    #[test]
    fn finalize_pending_message_changes_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let mut msg = make_message(1, "pending msg", 1700000000000, true);
        msg.pending = true;
        let pending_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", &msg);

        let real_id = make_hex_id(2);
        let result = state.finalize_pending_message("npub1peer", &pending_id, &real_id);

        let (old_id, finalized) = result.expect("finalize should succeed");
        assert_eq!(old_id, pending_id, "should return old pending id");
        assert_eq!(finalized.id, real_id, "message id should now be the real id");
        assert!(!finalized.pending, "message should no longer be pending");

        // Old ID should no longer be findable
        assert!(
            state.find_message(&pending_id).is_none(),
            "pending id should no longer resolve"
        );
        // New ID should be findable
        assert!(
            state.find_message(&real_id).is_some(),
            "real id should now resolve"
        );
    }

    #[test]
    fn remove_message_works() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "deleteme", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", &msg);

        let result = state.remove_message(&msg_id);
        assert!(result.is_some(), "remove should return the removed message");

        let (chat_id, removed) = result.unwrap();
        assert_eq!(chat_id, "npub1peer", "should return correct chat id");
        assert_eq!(removed.content, "deleteme", "content should match");

        assert!(
            state.find_message(&msg_id).is_none(),
            "removed message should no longer be findable"
        );
    }

    #[test]
    fn remove_message_retreats_last_read_marker() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");
        let m1 = make_message(1, "one", 1_700_000_000_000, false);
        let m2 = make_message(2, "two", 1_700_000_001_000, false);
        let (m1_id, m2_id) = (m1.id.clone(), m2.id.clone());
        state.add_message_to_chat("npub1peer", &m1);
        state.add_message_to_chat("npub1peer", &m2);

        // Read up to the newest — m2 is the marker.
        state.chats.iter_mut().find(|c| c.id == "npub1peer").unwrap().last_read =
            crate::compact::encode_message_id(&m2_id);

        // Deleting the marker retreats it to the prior survivor, never leaves it dangling.
        state.remove_message(&m2_id);
        assert_eq!(state.get_chat("npub1peer").unwrap().last_read,
            crate::compact::encode_message_id(&m1_id), "marker retreats to m1");

        // Deleting the last survivor clears the marker (no predecessor).
        state.remove_message(&m1_id);
        assert_eq!(state.get_chat("npub1peer").unwrap().last_read, [0u8; 32],
            "no predecessor → marker clears");
    }

    #[test]
    fn remove_message_returns_none_for_missing() {
        let mut state = ChatState::new();
        assert!(
            state.remove_message(&make_hex_id(99)).is_none(),
            "removing nonexistent message should return None"
        );
    }

    #[test]
    fn message_exists_check() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "exists", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", &msg);

        assert!(state.message_exists(&msg_id), "added message should exist");
        assert!(!state.message_exists(&make_hex_id(99)), "unknown id should not exist");
        assert!(!state.message_exists(""), "empty id should not exist");
    }

    #[test]
    fn chat_reordering_newest_first_after_message_add() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1old");
        state.create_dm_chat("npub1new");

        // Add an old message to the first chat
        let old_msg = make_message(1, "old", 1700000000000, false);
        state.add_message_to_chat("npub1old", &old_msg);

        // Add a newer message to the second chat
        let new_msg = make_message(2, "new", 1700000002000, false);
        state.add_message_to_chat("npub1new", &new_msg);

        assert_eq!(
            state.chats[0].id, "npub1new",
            "chat with newest message should be first"
        );
        assert_eq!(
            state.chats[1].id, "npub1old",
            "chat with older message should be second"
        );
    }

    #[test]
    fn batch_add_does_not_reorder_for_old_messages() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1active");
        state.create_dm_chat("npub1history");

        // Give active chat a recent message
        let recent = make_message(1, "recent", 1700000010000, false);
        state.add_message_to_chat("npub1active", &recent);

        // Batch-add old messages to history chat (pagination loading)
        let old_msgs: Vec<Message> = (10..15).map(|i| {
            make_message(i, &format!("old {}", i), 1700000000000 + i as u64 * 100, false)
        }).collect();
        state.add_messages_to_chat_batch("npub1history", old_msgs);

        assert_eq!(
            state.chats[0].id, "npub1active",
            "active chat should remain first when batch has only old messages"
        );
    }

    #[test]
    fn stress_test_50_messages_in_5_chats() {
        let mut state = ChatState::new();

        for i in 0..5 {
            state.create_dm_chat(&format!("npub1chat{}", i));
        }

        let mut total_added = 0;
        for i in 0..50u8 {
            let chat_idx = i as usize % 5;
            let chat_id = format!("npub1chat{}", chat_idx);
            let msg = make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, i % 3 == 0);
            if state.add_message_to_chat(&chat_id, &msg) {
                total_added += 1;
            }
        }

        assert_eq!(total_added, 50, "all 50 unique messages should be added");

        let total_in_chats: usize = state.chats.iter().map(|c| c.message_count()).sum();
        assert_eq!(total_in_chats, 50, "total messages across all chats should be 50");

        // Each chat should have 10 messages
        for i in 0..5 {
            let chat = state.get_chat(&format!("npub1chat{}", i)).unwrap();
            assert_eq!(
                chat.message_count(), 10,
                "chat {} should have 10 messages",
                i
            );
        }

        // All messages should be findable
        for i in 0..50u8 {
            assert!(
                state.message_exists(&make_hex_id(i)),
                "message {} should exist",
                i
            );
        }
    }

    #[test]
    fn add_message_auto_creates_dm_chat() {
        let mut state = ChatState::new();

        // Add message to a chat that doesn't exist yet (npub-style ID)
        let msg = make_message(1, "auto create", 1700000000000, false);
        let added = state.add_message_to_chat("npub1auto", &msg);

        assert!(added, "message should be added");
        assert!(state.get_chat("npub1auto").is_some(), "DM chat should be auto-created");
    }

    #[test]
    fn add_message_auto_creates_community_chat() {
        let mut state = ChatState::new();

        // Add message to a non-npub ID (should create a Community chat)
        let msg = make_message(1, "group msg", 1700000000000, false);
        let added = state.add_message_to_chat("group_abc123", &msg);

        assert!(added, "message should be added");
        let chat = state.get_chat("group_abc123").expect("community chat should be auto-created");
        assert!(chat.is_community(), "auto-created non-npub chat should be a Community chat");
    }

    // ========================================================================
    // Unread Count
    // ========================================================================

    #[test]
    fn count_unread_messages_basic() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        for i in 0..5u8 {
            let msg = make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false);
            state.add_message_to_chat("npub1peer", &msg);
        }

        assert_eq!(state.count_unread_messages(), 5, "all 5 non-mine messages should be unread");
    }

    #[test]
    fn count_unread_muted_chat_skipped() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1muted");

        let msg = make_message(1, "muted msg", 1700000000000, false);
        state.add_message_to_chat("npub1muted", &msg);

        state.get_chat_mut("npub1muted").unwrap().muted = true;

        assert_eq!(state.count_unread_messages(), 0, "muted chat should not count toward unread");
    }

    #[test]
    fn count_unread_blocked_user_skipped() {
        let mut state = ChatState::new();

        let mut profile = Profile::new();
        profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blocked", profile);
        state.create_dm_chat("npub1blocked");

        let msg = make_message(1, "blocked msg", 1700000000000, false);
        state.add_message_to_chat("npub1blocked", &msg);

        assert_eq!(state.count_unread_messages(), 0, "blocked user DM should not count");
    }

    #[test]
    fn blocked_pictures_are_forgotten_but_a_file_another_profile_shows_is_kept() {
        let mut state = ChatState::new();
        let mut shown = |npub: &str, blocked: bool, avatar: &str| {
            let mut p = Profile::new();
            p.set_avatar(&format!("https://x/{avatar}"));
            p.set_avatar_cached(&format!("/cache/avatars/{avatar}"));
            p.flags.set_blocked(blocked);
            state.insert_or_replace_profile(npub, p);
        };
        shown("npub1blocked", true, "own.png");
        shown("npub1alsoblocked", true, "shared.png");
        shown("npub1friend", false, "shared.png");

        let (slims, files) = state.forget_blocked_imagery();
        let mut ids: Vec<&str> = slims.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["npub1alsoblocked", "npub1blocked"]);
        assert!(slims.iter().all(|s| s.avatar.is_empty() && s.avatar_cached.is_empty()));
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["/cache/avatars/own.png"], "the friend still shows shared.png");
        assert!(state.get_profile("npub1friend").unwrap().has_avatar_cached());
        assert!(state.forget_blocked_imagery().0.is_empty(), "a second sweep finds nothing");
    }

    #[test]
    fn a_live_message_counts_in_ram_unless_its_author_is_blocked_or_muted() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1room");
        let mut blocked = Profile::new();
        blocked.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blocked", blocked);
        state.create_dm_chat("npub1muted");
        state.get_chat_mut("npub1muted").unwrap().muted = true;

        state.unread_bump("npub1room", Some("npub1friend"));
        state.unread_bump("npub1room", None);
        state.unread_bump("npub1room", Some("npub1blocked"));
        state.unread_bump("npub1room", Some("npub1muted"));
        assert_eq!(state.unread_cache.get("npub1room"), Some(&2));
    }

    #[test]
    fn unread_cache_seed_clear_set_and_sum() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1a");
        state.create_dm_chat("npub1b");

        let mut seed = std::collections::HashMap::new();
        seed.insert("npub1a".to_string(), 3u32);
        seed.insert("npub1b".to_string(), 2u32);
        state.unread_seed(seed);
        assert!(state.unread_seeded);
        assert_eq!(state.sum_unread(), 5);

        state.unread_clear("npub1a");
        assert_eq!(state.sum_unread(), 2, "clear drops a's 3");

        state.unread_set("npub1b", 4);
        assert_eq!(state.sum_unread(), 4, "reconcile b to exact 4");
        state.unread_set("npub1b", 0);
        assert_eq!(state.sum_unread(), 0);
        assert!(!state.unread_cache.contains_key("npub1b"), "a zero count drops the entry");
    }

    #[test]
    fn unread_cache_sum_honours_muted_and_blocked() {
        let mut state = ChatState::new();
        let mut blocked = Profile::new();
        blocked.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blk", blocked);
        state.create_dm_chat("npub1blk");
        state.create_dm_chat("npub1mut");
        state.get_chat_mut("npub1mut").unwrap().muted = true;
        state.create_dm_chat("npub1ok");

        let mut seed = std::collections::HashMap::new();
        seed.insert("npub1blk".to_string(), 5u32);
        seed.insert("npub1mut".to_string(), 7u32);
        seed.insert("npub1ok".to_string(), 2u32);
        state.unread_seed(seed);

        // Blocked + muted chats are filtered exactly like sum_unread_from; only the normal chat counts.
        assert_eq!(state.sum_unread(), 2);
    }

    #[test]
    fn count_unread_own_messages_break_count() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // 3 from them, then 1 from me, then 2 from them
        let msg1 = make_message(1, "them 1", 1700000001000, false);
        let msg2 = make_message(2, "them 2", 1700000002000, false);
        let msg3 = make_message(3, "them 3", 1700000003000, false);
        let msg_mine = make_message(4, "me", 1700000004000, true);
        let msg5 = make_message(5, "them 4", 1700000005000, false);
        let msg6 = make_message(6, "them 5", 1700000006000, false);

        for m in [msg1, msg2, msg3, msg_mine, msg5, msg6] {
            state.add_message_to_chat("npub1peer", &m);
        }

        // Counting from the end: msg6 (unread), msg5 (unread), then msg_mine breaks
        assert_eq!(
            state.count_unread_messages(), 2,
            "only messages after last 'mine' should count as unread"
        );
    }

    #[test]
    fn count_unread_from_counts_past_own_messages() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");
        for m in [
            make_message(1, "them 1", 1700000001000, false),
            make_message(2, "them 2", 1700000002000, false),
            make_message(3, "me", 1700000003000, true),
            make_message(4, "them 3", 1700000004000, false),
            make_message(5, "me", 1700000005000, true),
        ] {
            state.add_message_to_chat("npub1peer", &m);
        }
        assert_eq!(state.count_unread_messages(), 0, "our newest reply reads everything");

        let chat = state.chats.iter_mut().find(|c| c.id == "npub1peer").unwrap();
        chat.unread_from = crate::compact::encode_message_id(&make_hex_id(2));
        assert_eq!(state.count_unread_messages(), 2, "them 2 and them 3, past our replies");

        let chat = state.chats.iter_mut().find(|c| c.id == "npub1peer").unwrap();
        assert!(chat.set_as_read());
        assert_eq!(chat.unread_from, [0u8; 32], "a read clears the mark");
        assert_eq!(state.count_unread_messages(), 0);
    }

    #[test]
    fn count_unread_last_read_marker_breaks_count() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg1 = make_message(1, "old", 1700000001000, false);
        let msg2 = make_message(2, "read up to here", 1700000002000, false);
        let msg3 = make_message(3, "new 1", 1700000003000, false);
        let msg4 = make_message(4, "new 2", 1700000004000, false);
        let read_marker_id = msg2.id.clone();

        for m in [msg1, msg2, msg3, msg4] {
            state.add_message_to_chat("npub1peer", &m);
        }

        // Set last_read to msg2's ID
        let chat = state.get_chat_mut("npub1peer").unwrap();
        chat.last_read = crate::simd::hex::hex_to_bytes_32(&read_marker_id);

        assert_eq!(
            state.count_unread_messages(), 2,
            "only messages after last_read marker should count"
        );
    }

    #[test]
    fn count_unread_empty_chats_is_zero() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1empty1");
        state.create_dm_chat("npub1empty2");

        assert_eq!(state.count_unread_messages(), 0, "empty chats should have zero unread");
    }

    #[test]
    fn count_unread_blocked_group_member_messages_skipped() {
        let mut state = ChatState::new();

        // Create a blocked profile
        let mut blocked_profile = Profile::new();
        blocked_profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blockedmember", blocked_profile);

        // Create a normal profile
        let normal_profile = Profile::new();
        state.insert_or_replace_profile("npub1normal", normal_profile);

        // A SURFACED community row (carries its owning community_id) — a bare
        // anchor row is excluded from unread totals by design.
        state.ensure_community_chat("grp1");
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == "grp1") {
            chat.metadata.insert("community_id", "c".repeat(64));
        }

        // Message from blocked member
        let msg_blocked = make_message_from(1, "blocked says hi", 1700000001000, "npub1blockedmember");
        state.add_message_to_chat("grp1", &msg_blocked);

        // Message from normal member
        let msg_normal = make_message_from(2, "normal says hi", 1700000002000, "npub1normal");
        state.add_message_to_chat("grp1", &msg_normal);

        assert_eq!(
            state.count_unread_messages(), 1,
            "only the non-blocked member's message should count"
        );
    }

    #[test]
    fn count_unread_muted_sender_group_messages_skipped() {
        let mut state = ChatState::new();
        state.insert_or_replace_profile("npub1mutedmember", Profile::new());
        state.insert_or_replace_profile("npub1normal", Profile::new());

        // Muting a person = muting their (possibly message-less) DM row.
        state.create_dm_chat("npub1mutedmember");
        state.get_chat_mut("npub1mutedmember").unwrap().muted = true;

        state.ensure_community_chat("grp1");
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == "grp1") {
            chat.metadata.insert("community_id", "c".repeat(64));
        }

        let msg_muted = make_message_from(1, "muted says hi", 1700000001000, "npub1mutedmember");
        state.add_message_to_chat("grp1", &msg_muted);
        let msg_normal = make_message_from(2, "normal says hi", 1700000002000, "npub1normal");
        state.add_message_to_chat("grp1", &msg_normal);

        assert_eq!(
            state.count_unread_messages(), 1,
            "a muted sender's community messages should not count"
        );
    }

    #[test]
    fn count_unread_multiple_chats_summed() {
        let mut state = ChatState::new();

        for i in 0..3 {
            let npub = format!("npub1chat{}", i);
            state.create_dm_chat(&npub);
            for j in 0..3u8 {
                let msg = make_message(
                    i * 10 + j,
                    &format!("msg {}-{}", i, j),
                    1700000000000 + j as u64 * 1000,
                    false,
                );
                state.add_message_to_chat(&npub, &msg);
            }
        }

        assert_eq!(
            state.count_unread_messages(), 9,
            "3 chats x 3 unread each = 9 total"
        );
    }

    // ========================================================================
    // Typing Indicators
    // ========================================================================

    #[test]
    fn update_typing_and_get_active_basic() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Set a far-future expiry so it's definitely active
        let far_future = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs() + 300;

        let active = state.update_typing_and_get_active("npub1peer", "npub1typer", far_future);
        assert_eq!(active.len(), 1, "should have one active typer");
        assert_eq!(active[0], "npub1typer", "typer npub should match");
    }

    #[test]
    fn update_typing_expired_typers_filtered() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Expired timestamp (in the past)
        let expired = 1000;
        let active = state.update_typing_and_get_active("npub1peer", "npub1expired", expired);

        assert!(active.is_empty(), "expired typer should be filtered out");
    }

    #[test]
    fn update_typing_multiple_typers() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let far_future = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs() + 300;

        state.update_typing_and_get_active("npub1peer", "npub1typer1", far_future);
        let active = state.update_typing_and_get_active("npub1peer", "npub1typer2", far_future);

        assert_eq!(active.len(), 2, "should have two active typers");
    }

    #[test]
    fn update_typing_unknown_chat_returns_empty() {
        let mut state = ChatState::new();
        let far_future = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs() + 300;

        let active = state.update_typing_and_get_active("npub1nonexistent", "npub1typer", far_future);
        assert!(active.is_empty(), "unknown chat should return empty typers");
    }

    #[test]
    fn update_typing_refreshes_existing_typer() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let far_future = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs() + 300;

        state.update_typing_and_get_active("npub1peer", "npub1typer", far_future);
        // Update the same typer with a new expiry
        let active = state.update_typing_and_get_active("npub1peer", "npub1typer", far_future + 100);

        assert_eq!(active.len(), 1, "should still have only one typer entry after refresh");
    }

    // ========================================================================
    // WrapperIdCache
    // ========================================================================

    #[test]
    fn wrapper_id_cache_historical_and_pending() {
        let mut cache = WrapperIdCache::new();

        let id1 = [1u8; 32];
        let id2 = [2u8; 32];
        let id3 = [3u8; 32];

        cache.load(vec![id1, id2]);
        cache.insert(id3);

        assert!(cache.contains(&id1), "historical id should be found");
        assert!(cache.contains(&id2), "historical id should be found");
        assert!(cache.contains(&id3), "pending id should be found");
        assert!(!cache.contains(&[4u8; 32]), "unknown id should not be found");
        assert_eq!(cache.len(), 3, "total count should be 3");
    }

    #[test]
    fn an_idle_wrapper_cache_keeps_only_the_newest_inserts() {
        let mut cache = WrapperIdCache::new();
        let id = |n: u32| {
            let mut b = [0u8; 32];
            b[..4].copy_from_slice(&n.to_le_bytes());
            b
        };
        let extra = 5u32;
        for n in 0..IDLE_PENDING_CAP as u32 + extra {
            cache.insert(id(n));
        }
        cache.insert(id(IDLE_PENDING_CAP as u32 + extra - 1));
        assert_eq!(cache.len(), IDLE_PENDING_CAP);
        for n in 0..extra {
            assert!(!cache.contains(&id(n)), "id {n} was among the oldest");
        }
        assert!(cache.contains(&id(extra)), "the oldest kept");
        assert!(cache.contains(&id(IDLE_PENDING_CAP as u32 + extra - 1)), "the newest kept");

        cache.load(vec![]);
        for n in 0..IDLE_PENDING_CAP as u32 * 2 {
            cache.insert(id(n));
        }
        assert_eq!(cache.len(), IDLE_PENDING_CAP * 2, "a sync keeps everything until the dump");
        assert!(cache.contains(&id(0)));

        cache.clear();
        for n in 0..IDLE_PENDING_CAP as u32 + 1 {
            cache.insert(id(n));
        }
        assert_eq!(cache.len(), IDLE_PENDING_CAP, "idle again after the dump");
        assert!(!cache.contains(&id(0)) && cache.contains(&id(1)));
    }

    #[test]
    fn wrapper_id_cache_clear() {
        let mut cache = WrapperIdCache::new();
        cache.load(vec![[1u8; 32]]);
        cache.insert([2u8; 32]);

        cache.clear();

        assert_eq!(cache.len(), 0, "cache should be empty after clear");
        assert!(!cache.contains(&[1u8; 32]), "cleared historical should not be found");
        assert!(!cache.contains(&[2u8; 32]), "cleared pending should not be found");
    }
}
