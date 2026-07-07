//! Concord v2 (the upstream CORD specs — `github.com/concord-protocol/concord`).
//!
//! Vector's second-generation community protocol, carried ALONGSIDE v1 (the
//! sibling `community::` modules) for a migration window. v2 keeps v1's control
//! plane engine verbatim — the edition hash, fold, roster, and authority algebra
//! are shared by import, not copied — and replaces the envelope (CORD-01 Private
//! Streams: group-key-signed kind-1059 wraps addressed by `authors`), the owner
//! anchor (a self-certifying `community_id`), the invite system (CORD-05), and
//! adds the Guestbook plane (CORD-02 §5).
//!
//! Nothing in here touches v1 state: a `Community` is one protocol or the other
//! (`protocol` discriminator), and the two stacks share only pure logic.
//!
//! Frozen wire constants live in two places: the derivation labels in
//! [`derive`], and the kind registry below (CORD-02 Appendix B). A retired
//! number is burned forever, never reused.

pub mod chat;
pub mod control;
pub mod derive;
pub mod dissolution;
pub mod guestbook;
pub mod invite;
pub mod list;
pub mod rekey;
pub mod stream;

/// Inner rumor kinds (CORD-02 Appendix B). The *outer* event is always a wrap
/// ([`stream::KIND_WRAP`] / [`stream::KIND_WRAP_EPHEMERAL`]); these ride inside.
pub mod kind {
    /// Chat message (NIP-C7 shape: content = text, replies via `q` tag).
    pub const MESSAGE: u16 = 9;
    /// Reaction (NIP-25 shape: `e`/`p`/`k` tags name the target).
    pub const REACTION: u16 = 7;
    /// Delete (NIP-09 shape: `e` names the author's own rumor id).
    pub const DELETE: u16 = 5;
    /// Message edit (fields unpinned upstream; Vector: `e` = own message rumor id,
    /// content = replacement text).
    pub const EDIT: u16 = 3302;
    /// Rekey blobs (CORD-06).
    pub const REKEY: u16 = 3303;
    /// Guestbook join/leave (content = the verb).
    pub const JOIN_LEAVE: u16 = 3306;
    /// Control edition, sub-kinded by `vsk` (shared grammar with v1 — `community::edition`).
    pub const CONTROL: u16 = 3308;
    /// Guestbook kick (admin-signed, `p` target, `vac` citation).
    pub const KICK: u16 = 3309;
    /// WebXDC peer signal (payload opaque to the protocol).
    pub const WEBXDC: u16 = 3310;
    /// Guestbook snapshot (refounder-signed, chunked 400/event).
    pub const SNAPSHOT: u16 = 3312;
    /// Direct invite rumor — rides a STANDARD NIP-59 giftwrap to a person,
    /// never a stream wrap (CORD-05 §6).
    pub const DIRECT_INVITE: u16 = 3313;
    /// Typing indicator (ephemeral tier — 21059 wrap, never stored).
    pub const TYPING: u16 = 23311;
    /// Voice presence heartbeat (ephemeral tier; CORD-07 — deferred in Vector,
    /// constant reserved so nothing else claims it).
    pub const VOICE_PRESENCE: u16 = 23313;
    /// Public invite bundle — bare addressable event signed by the per-link
    /// keypair at an empty `d` (CORD-05 §2). Outside the wrap.
    pub const INVITE_BUNDLE: u16 = 33301;
    /// A member's self-encrypted Community List (replaceable). Outside the wrap.
    pub const COMMUNITY_LIST: u16 = 13302;
    /// A creator's self-encrypted Invite List (replaceable). Outside the wrap.
    pub const INVITE_LIST: u16 = 13303;
}

/// Control-plane `vsk` sub-kinds (CORD-02 Appendix B). Identical numbering to
/// v1 (`community::roster`) — that is deliberate, the registry is shared. 7 is
/// retired upstream (the v1 owner attestation, obsoleted by the self-certifying
/// community id); 11 is claimed by Vector for the v1-side migration pointer and
/// never appears on the v2 wire.
pub mod vsk {
    pub const COMMUNITY_METADATA: &str = "0";
    pub const ROLE: &str = "1";
    pub const CHANNEL_METADATA: &str = "2";
    pub const GRANT: &str = "3";
    pub const BANLIST: &str = "4";
    // "5" reserved (role ordering), "6"/"9" claimed by the 33301 invite marker,
    // "7" retired, "8" = invite-link registry, "10" = dissolved tombstone.
    pub const INVITE_LINKS: &str = "8";
    pub const INVITE_LIVE: &str = "6";
    pub const INVITE_REVOKED: &str = "9";
    pub const DISSOLVED: &str = "10";
}
