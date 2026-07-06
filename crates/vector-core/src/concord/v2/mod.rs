//! Concord v2 — the CORD protocol (see the CORD-01..06 documents).
//!
//! This module is the pure protocol core: identities, frozen derivations, the
//! stream envelope, Control Plane editions and their convergent fold, the
//! Guestbook coalesce, invites, and rekeys/refoundings. It is network-free and
//! DB-free; transports and persistence layer on top.
//!
//! Wire universe: every plane is a Private Stream (CORD-01) — kind 1059 wraps
//! signed by a derived stream key and fetched by `authors` filter. This is a
//! different address space from v1's `#z`-tag pseudonyms; the two protocols
//! never collide on the wire.

pub mod community;
pub mod community_list;
pub mod control;
pub mod db;
pub mod derive;
pub mod edition;
pub mod guestbook;
pub mod invite;
pub mod rekey;
pub mod roster;
pub mod service;
pub mod stream;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

// ============================================================================
// Identities & keys
// ============================================================================

/// A Community's permanent identity: a self-certifying SHA-256 commitment to
/// the owner's key (CORD-02 §1). Never on the wire itself — every coordinate
/// derives from it one-way — but it travels inside invites so any member can
/// verify who founded the Community.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommunityId(pub [u8; 32]);

impl CommunityId {
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        crate::simd::hex::hex_to_bytes_32_checked(hex).map(Self)
    }
}

/// The salt minted alongside a Community so one owner can run many (CORD-02
/// §1). Not secret — it travels inside invites for owner verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerSalt(pub [u8; 32]);

/// The Community's private access key: holding the current root *is*
/// membership (CORD-02 §2). Deliberately independent of the `community_id` so
/// access rotates while identity stays fixed.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct CommunityRoot(pub [u8; 32]);

impl CommunityRoot {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// Deliberately no Debug: the root must never reach a log line.
impl core::fmt::Debug for CommunityRoot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CommunityRoot(<redacted>)")
    }
}

/// A Channel's stable identity within a Community: random 32 bytes, doubling
/// as its ChannelMetadata edition coordinate (CORD-03 §2). Never reused, even
/// after deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub [u8; 32]);

impl ChannelId {
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        crate::simd::hex::hex_to_bytes_32_checked(hex).map(Self)
    }
}

/// A Private Channel's independent 32-byte symmetric secret (CORD-03 §1).
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct ChannelKey(pub [u8; 32]);

impl ChannelKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl core::fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ChannelKey(<redacted>)")
    }
}

/// The read-access clock: bumps only on a Rekey (CORD-02 §3). Public Channels
/// ride the root's epoch; Private Channels carry their own, monotonic and
/// never resetting across public/private conversions (CORD-03 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct Epoch(pub u64);

/// A role's stable identity: random 32 bytes minted at creation (CORD-04 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoleId(pub [u8; 32]);

impl RoleId {
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        crate::simd::hex::hex_to_bytes_32_checked(hex).map(Self)
    }
}

/// The all-zero 32-byte sentinel: the rekey scope id for a base rotation
/// (CORD-06 §1) and the `id` for zero-id derivations (CORD-02 A.6).
pub const ZERO_ID: [u8; 32] = [0u8; 32];

// ============================================================================
// Kind registry (CORD-02 Appendix B) — FROZEN
// ============================================================================

/// Wire kinds. A retired number is never reused; its meaning is burned.
pub mod kind {
    /// Durable gift wrap — every durable plane event.
    pub const WRAP: u16 = 1059;
    /// Ephemeral gift wrap — relays MUST NOT store (typing indicator only).
    pub const WRAP_EPHEMERAL: u16 = 21059;
    /// Encrypted seal (Chat, Guestbook, rekey planes).
    pub const SEAL_ENCRYPTED: u16 = 20013;
    /// Plaintext seal (Control Plane only — survives compaction re-wraps).
    pub const SEAL_PLAINTEXT: u16 = 20014;
    /// Standard NIP-59 seal (Direct Invites only).
    pub const SEAL_NIP59: u16 = 13;

    /// Chat message (NIP-C7 shape).
    pub const MESSAGE: u16 = 9;
    /// Reaction (NIP-25 shape).
    pub const REACTION: u16 = 7;
    /// Delete (NIP-09 shape).
    pub const DELETE: u16 = 5;
    /// Edit.
    pub const EDIT: u16 = 3302;
    /// Rekey blobs (CORD-06).
    pub const REKEY: u16 = 3303;
    /// Guestbook Join / Leave.
    pub const JOIN_LEAVE: u16 = 3306;
    /// Control edition (sub-kinded by `vsk`).
    pub const CONTROL_EDITION: u16 = 3308;
    /// Guestbook Kick.
    pub const KICK: u16 = 3309;
    /// WebXDC peer signal.
    pub const WEBXDC: u16 = 3310;
    /// Guestbook snapshot (refounder-signed, chunked).
    pub const SNAPSHOT: u16 = 3312;
    /// Direct invite (standard NIP-59 wrap, `k`-tagged).
    pub const DIRECT_INVITE: u16 = 3313;
    /// Typing indicator (ephemeral).
    pub const TYPING: u16 = 23311;
    /// Public invite bundle (addressable, bare on relays).
    pub const PUBLIC_INVITE: u16 = 33301;
    /// Community List (replaceable, self-encrypted).
    pub const COMMUNITY_LIST: u16 = 13302;
    /// Invite List (replaceable, self-encrypted).
    pub const INVITE_LIST: u16 = 13303;
}

/// Control-edition sub-kinds (`vsk` tag).
pub mod vsk {
    pub const COMMUNITY_METADATA: u8 = 0;
    pub const ROLE: u8 = 1;
    pub const CHANNEL_METADATA: u8 = 2;
    pub const GRANT: u8 = 3;
    pub const BANLIST: u8 = 4;
    /// Public invite bundle marker: live.
    pub const INVITE_LIVE: u8 = 6;
    /// Invite-link registry.
    pub const INVITE_REGISTRY: u8 = 8;
    /// Public invite bundle marker: revocation tombstone.
    pub const INVITE_REVOKED: u8 = 9;
    /// Dissolved tombstone (chainless, exempt from version discipline).
    pub const DISSOLVED: u8 = 10;
}

// ============================================================================
// Permission bits (CORD-04 §3) — FROZEN
// ============================================================================

/// Permission bits. Positions are frozen: a new permission claims the next
/// free bit, a retired one is burned, never renumbered or reused. There is no
/// all-powerful bit.
pub mod perm {
    pub const MANAGE_ROLES: u64 = 1 << 0;
    pub const MANAGE_CHANNELS: u64 = 1 << 1;
    pub const MANAGE_METADATA: u64 = 1 << 2;
    pub const KICK: u64 = 1 << 3;
    pub const BAN: u64 = 1 << 4;
    pub const MANAGE_MESSAGES: u64 = 1 << 5;
    pub const CREATE_INVITE: u64 = 1 << 6;
    // 1 << 7 retired (was MANAGE_INVITES)
    pub const VIEW_AUDIT_LOG: u64 = 1 << 8;
    pub const MENTION_EVERYONE: u64 = 1 << 9;
    // 1 << 10..=12 reserved (MANAGE_EMOJI, PIN_MESSAGES, MANAGE_EVENTS)
}

// ============================================================================
// Protocol caps & constants
// ============================================================================

/// NIP-44 hard plaintext cap — enforced at every nesting layer ourselves;
/// libraries are lenient and a lenient publisher mints events strict readers
/// cannot decrypt (CORD-02 Appendix B).
pub const NIP44_MAX_PLAINTEXT: usize = 65_535;

/// Uniform name cap: Community, Channel, and Role names alike (UTF-8 bytes).
pub const NAME_MAX_BYTES: usize = 64;

/// Community description cap (UTF-8 bytes).
pub const DESCRIPTION_MAX_BYTES: usize = 10_000;

/// Recommended relay-set ceiling; a longer set MAY be truncated (CORD-02 §6).
pub const RELAYS_RECOMMENDED_MAX: usize = 5;

/// The Community List membership cap (CORD-02 §8) — a protocol constant, not
/// client taste; the byte cap (`NIP44_MAX_PLAINTEXT`) is the law.
pub const COMMUNITY_LIST_MAX_MEMBERSHIPS: usize = 50;

/// Guestbook snapshot chunk size: present members per kind 3312 event.
pub const SNAPSHOT_CHUNK_MEMBERS: usize = 400;

/// Rekey blob fan-out: recipients per kind 3303 event (CORD-06 §1).
pub const REKEY_RECIPIENTS_PER_EVENT: usize = 120;

/// Bundle bound: a hostile invite is an allocation vector, so channels are
/// capped before any allocation (CORD-05 §1).
pub const INVITE_MAX_CHANNELS: usize = 256;

/// Bootstrap relays a link fragment may carry (CORD-05 §3).
pub const FRAGMENT_MAX_RELAYS: usize = 3;

/// Community role ceiling: fold the 100 lowest `role_id`s, ignore the rest.
pub const MAX_ROLES: usize = 100;

/// A member holds at most 64 roles.
pub const MAX_ROLES_PER_MEMBER: usize = 64;

/// Practical banlist ceiling before the NIP-44 envelope refuses the edit.
pub const BANLIST_PRACTICAL_MAX: usize = 500;

/// Guestbook fold rule: entries dated more than one hour ahead of the
/// receiver's clock are dropped outright (CORD-02 §5).
pub const MAX_FUTURE_SKEW_MS: u64 = 60 * 60 * 1000;

// ============================================================================
// Millisecond ordering (CORD-02 §4)
// ============================================================================

/// Split epoch-milliseconds the way every Concord rumor carries time:
/// `created_at` seconds plus an `["ms", 0..999]` remainder tag.
pub fn split_ms(ms: u64) -> (u64, u16) {
    (ms / 1000, (ms % 1000) as u16)
}

/// Reconstruct the true millisecond time from `created_at` and a parsed `ms`
/// remainder. Returns `None` for a malformed remainder (outside 0..=999) —
/// malformed is *dropped, never interpreted*, or the excess would smuggle
/// arbitrary "future" past the clock check (CORD-02 §5).
pub fn combine_ms(created_at_secs: u64, ms_remainder: u64) -> Option<u64> {
    if ms_remainder > 999 {
        return None;
    }
    created_at_secs.checked_mul(1000)?.checked_add(ms_remainder)
}

/// The current wall clock in epoch milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_combine_roundtrip() {
        let ms = 1_686_840_217_417u64;
        let (secs, rem) = split_ms(ms);
        assert_eq!(secs, 1_686_840_217);
        assert_eq!(rem, 417);
        assert_eq!(combine_ms(secs, rem as u64), Some(ms));
    }

    #[test]
    fn malformed_ms_remainder_is_dropped_not_interpreted() {
        assert_eq!(combine_ms(1_686_840_217, 1000), None);
        assert_eq!(combine_ms(1_686_840_217, u64::MAX), None);
        assert_eq!(combine_ms(1_686_840_217, 999), Some(1_686_840_217_999));
    }

    #[test]
    fn keys_never_debug_print() {
        let root = CommunityRoot([0xAA; 32]);
        let ck = ChannelKey([0xBB; 32]);
        assert_eq!(format!("{root:?}"), "CommunityRoot(<redacted>)");
        assert_eq!(format!("{ck:?}"), "ChannelKey(<redacted>)");
    }
}
