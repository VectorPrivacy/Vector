//! Vector Community protocol (GROUP_PROTOCOL.md).
//!
//! A Community is the top-level container (Discord's "server", but Vector is
//! serverless so the name reflects that); it holds Channels. This module is the
//! cryptographic core: the frozen key-derivation convention and the message
//! envelope. It is pure, network-free, and DB-free — the riskiest unknowns
//! isolated for exhaustive unit testing before anything depends on them.

pub mod attachments;
pub mod cache;
pub mod cipher;
pub mod derive;
pub mod envelope;
pub mod inbound;
pub mod invite;
pub mod invite_list;
pub mod list;
pub mod metadata;
pub mod edition;
pub mod owner;
pub mod rekey;
pub mod roster;
pub mod version;
pub mod public_invite;
pub mod realtime;
pub mod roles;
pub mod send;
pub mod service;
pub mod transport;
pub mod v2;

use nostr_sdk::prelude::PublicKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A Community's stable identity = a random 32-byte opaque id (NOT a
/// timestamp-encoding snowflake, which would leak creation time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommunityId(pub [u8; 32]);

impl CommunityId {
    /// Lowercase hex — the addressable-event `d`-tag form.
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }
}

/// A Channel's stable identity within a Community. Same opaque-random rule as
/// [`CommunityId`]; doubles as the addressable metadata `d`-tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub [u8; 32]);

impl ChannelId {
    /// Lowercase hex, the form used in the inner channel-binding tag.
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }
}

/// The epoch counter — the read-access clock ("two clocks"). Bumps only on a
/// rekey; stamped explicitly even when it is 0, so multi-channel and rotation stay
/// additive (forward-compat hook #1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(pub u64);

/// A 32-byte symmetric channel secret — the raw NIP-44 v2 `ConversationKey`
/// material. Zeroized on drop; never logged.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ChannelKey(pub [u8; 32]);

impl ChannelKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// Deliberately no Debug: a channel secret must never reach a log line.
impl core::fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ChannelKey(<redacted>)")
    }
}

/// Per-epoch pseudonym = the value carried in the relay-filterable `z` tag.
/// Opaque 32 bytes; outsiders can't link it across epochs or to an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pseudonym(pub [u8; 32]);

impl Pseudonym {
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }
}

/// The server-root / `@everyone` key: always minted, always distinct from any
/// channel key. Gates metadata + roster + roleless channels. Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ServerRootKey(pub [u8; 32]);

impl ServerRootKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl core::fmt::Debug for ServerRootKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ServerRootKey(<redacted>)")
    }
}

/// The all-zero hex scope id for server-root-scoped epoch keys (`RekeyScope::ServerRoot`
/// uses the all-zero `id32` sentinel). A `ChannelId` is random-32, so it can never collide with
/// this — letting one `community_epoch_keys` table hold both channel keys and the base/server-root
/// key keyed by `(scope_id, epoch)`.
pub const SERVER_ROOT_SCOPE_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// 32 cryptographically-random bytes (OsRng).
pub(crate) fn random_32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// A reference to an encrypted image blob (community logo/banner), using the same
/// technique as NIP-17 file attachments: a fresh random AES-GCM key+nonce encrypts the
/// image, the ciphertext is uploaded to Blossom, and this reference travels inside
/// ServerRoot-sealed metadata. So possession of the server-root key (every member) gates
/// the image, and there is no key reuse across images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityImage {
    /// Blossom URL of the encrypted blob.
    pub url: String,
    /// Hex AES-GCM key (per-image, random).
    pub key: String,
    /// Hex AES-GCM nonce.
    pub nonce: String,
    /// SHA-256 of the plaintext image (integrity check + local cache key).
    pub hash: String,
    /// File extension hint (e.g. "png", "jpg").
    #[serde(default)]
    pub ext: String,
}

/// A Channel inside a Community: its own independent key, current epoch, and name.
///
/// MULTI-EPOCH READ: `key`/`epoch` are the channel's CURRENT (head) epoch (what SENDS use), while
/// `epoch_keys` carries EVERY epoch key the member retains across rekeys. The read paths
/// (`fetch_channel_*`, `open_message_multi`) query `"#z":[<pseudonym per held epoch>]` and select the
/// decryption key by the wire event's `z` pseudonym tag — so messages a removed-from-the-future member
/// posted under an older epoch aren't stranded after a catch-up across one or more rekeys. Non-ratcheted
/// per-epoch keys make this random-access: any retained epoch's plane decrypts directly, no replay.
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: ChannelId,
    pub key: ChannelKey,
    pub epoch: Epoch,
    pub name: String,
    /// The owning Community's banlist (the "anti-memberlist"), denormalized onto each channel
    /// so the inbound path (which holds a `&Channel`) can drop events from banned authors
    /// without a separate lookup. Populated only by `db::load_community`; empty everywhere a
    /// channel is built for sending or in tests. See [`crate::community::inbound`].
    pub banned: Vec<PublicKey>,
    /// The protected set — the proven owner only (implicit roster position 0), denormalized so the
    /// inbound path enforces the invariant that the owner is NEVER effectively banned or hidden.
    /// Admins are NOT in this set: the owner outranks them and may ban/hide them; everyone else is
    /// ranked through the roster, not here. `db::load_community` filters the owner out of `banned`
    /// and populates this; empty for send-built channels and tests.
    pub protected: Vec<PublicKey>,
    /// The Community's AUTHORIZED roster (roles + grants, post delegation check), denormalized so
    /// the inbound delete path can verify a moderation-hide the keyless way — the hider's real npub must
    /// hold `MANAGE_MESSAGES` and outrank the target's author, resolved against this roster.
    /// `db::load_community` populates it from the cached authorized roster; empty for send-built channels
    /// and tests (an empty roster authorizes only the owner, via `protected`).
    pub roster: roles::CommunityRoles,
    /// EVERY epoch key the member retains for this channel (`(epoch, key)`, from the multi-held archive),
    /// so the read path can fetch + decrypt across rekeys. Populated only by `db::load_community`; empty
    /// for send-built channels and tests, where reads fall back to the single head epoch (see
    /// [`Self::read_epoch_keys`]).
    pub epoch_keys: Vec<(Epoch, ChannelKey)>,
    /// The owning Community's dissolution seal, denormalized onto each channel so the inbound path
    /// (which holds a `&Channel`) drops EVERY subsequent event without a separate lookup — any kind, any
    /// author, any time. Populated only by `db::load_community`; `false` for send-built channels + tests.
    pub dissolved: bool,
}

impl Channel {
    /// The `(epoch, key)` set the read path queries + decrypts against: every retained epoch when
    /// loaded from the DB, else just the head (send-built channels / tests). Newest epoch first, so a
    /// backward page walk and the per-event key lookup both see the current epoch before older ones.
    pub fn read_epoch_keys(&self) -> Vec<(Epoch, ChannelKey)> {
        let mut keys = if self.epoch_keys.is_empty() {
            vec![(self.epoch, self.key.clone())]
        } else {
            self.epoch_keys.clone()
        };
        keys.sort_by(|a, b| b.0.0.cmp(&a.0.0));
        keys
    }
}

/// A Community (Discord's "server").
///
/// Keyless authority model: there is no shared signing key. READ access = key possession
/// (`server_root_key` + the granted channel keys let any member read/post); WRITE authority = the
/// member's npub rank in the owner-rooted roster, and the OWNER is derived by verifying the
/// `owner_attestation` (see `service::is_proven_owner`). `ChannelKey`/`ServerRootKey` secrets are not
/// serialized here; persistence is a separate, vault-backed concern.
#[derive(Debug, Clone)]
pub struct Community {
    pub id: CommunityId,
    /// @everyone base key — at `server_root_epoch`.
    pub server_root_key: ServerRootKey,
    /// The server-root's current epoch — the base/`@everyone` read clock. Bumps only on a
    /// base rotation (a Private-community removal, or re-founding); stays 0 in a Public community and
    /// at MVP. The per-epoch server-root pseudonym is derived from this, so it is the G1 seam the
    /// control-plane fetch widens against once re-anchoring + multi-epoch fetch ship.
    pub server_root_epoch: Epoch,
    pub name: String,
    /// Short description / topic (server-root-gated metadata; shown in invite previews).
    pub description: Option<String>,
    /// Logo (encrypted blob ref — see [`CommunityImage`]).
    pub icon: Option<CommunityImage>,
    /// Banner (encrypted blob ref).
    pub banner: Option<CommunityImage>,
    /// Preferred relay set for all of this Community's events.
    pub relays: Vec<String>,
    pub channels: Vec<Channel>,
    /// The owner's identity attestation (a signed event JSON, see [`owner`]) binding this
    /// community's id to the owner's npub. The proven owner is DERIVED by verifying it, never stored
    /// as a bare claim. `None` until the creator signs it. Travels in the GroupRoot + invite bundle.
    pub owner_attestation: Option<String>,
    /// The owner-dissolution SEAL. `true` once a folded GroupDissolved tombstone was verified against
    /// the proven owner — PERMANENT and irreversible (there is no un-dissolve; the way forward is a fresh
    /// community). Once set, the control fold stops advancing and the inbound path drops every subsequent
    /// event (any kind/author/time — the seal is this flag, NOT a timestamp). `false` for a live community.
    pub dissolved: bool,
}

/// Protocol cap on a Community's relay set (§ transport). More relays are needless and
/// amplify resource + metadata-exposure cost; 5 gives redundancy without centralisation.
/// Enforced by truncate-on-read at every Community/CommunityInvite construction boundary, so a
/// hostile or legacy bundle degrades to ≤5 distinct relays rather than being honored or rejected.
pub const MAX_COMMUNITY_RELAYS: usize = 5;

/// Dedupe (order-preserving) + truncate a relay set to [`MAX_COMMUNITY_RELAYS`]. Dedup first so the
/// cap means "up to 5 DISTINCT relays" — a bundle padding one relay 5× can't waste the budget.
pub fn cap_relays(relays: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(relays.len().min(MAX_COMMUNITY_RELAYS));
    for r in relays {
        if out.len() >= MAX_COMMUNITY_RELAYS {
            break;
        }
        if seen.insert(r.clone()) {
            out.push(r);
        }
    }
    out
}

impl Community {
    /// Mint a brand-new Community with one default channel, owned by the creator.
    /// All ids are random opaque 32-byte values (NOT timestamp snowflakes), and the
    /// server-root + channel keys are independently generated (hook #3). The owner attestation
    /// is signed separately by `service::create_community` (it needs the owner's identity signer).
    pub fn create(
        name: impl Into<String>,
        default_channel_name: impl Into<String>,
        relays: Vec<String>,
    ) -> Self {
        let channel = Channel {
            id: ChannelId(random_32()),
            key: ChannelKey(random_32()),
            epoch: Epoch(0),
            name: default_channel_name.into(),
            banned: Vec::new(),
            protected: Vec::new(), roster: Default::default(),
            epoch_keys: Vec::new(),
            dissolved: false,
        };
        Community {
            id: CommunityId(random_32()),
            server_root_key: ServerRootKey(random_32()),
            server_root_epoch: Epoch(0),
            name: name.into(),
            description: None,
            icon: None,
            banner: None,
            relays: cap_relays(relays),
            channels: vec![channel],
            // Signed asynchronously by `service::create_community` (needs the owner's identity
            // signer); a freshly-minted in-memory Community has none yet.
            owner_attestation: None,
            dissolved: false,
        }
    }
}
