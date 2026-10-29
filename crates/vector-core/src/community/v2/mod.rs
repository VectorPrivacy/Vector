//! Concord v2 (the upstream CORD specs — `github.com/concord-protocol/concord`).
//!
//! Vector's community protocol. The control-plane engine (the edition hash, fold,
//! roster, and authority algebra) lives in the sibling `community::` modules; this
//! one carries the envelope (CORD-01 Private Streams: group-key-signed kind-1059
//! wraps addressed by `authors`), the owner anchor (a self-certifying
//! `community_id`), the invite system (CORD-05), and the Guestbook plane (CORD-02 §5).
//!
//! Frozen wire constants live in two places: the derivation labels in
//! [`derive`], and the kind registry below (CORD-02 Appendix B). A retired
//! number is burned forever, never reused.

#[cfg(feature = "bench")]
pub mod bench;
pub mod chat;
pub mod community;
pub mod control;
pub mod derive;
pub mod dissolution;
pub mod guestbook;
pub mod inbound;
pub mod invite;
pub mod list;
pub mod list_frag;
pub mod pin_keys;
pub mod pins;
pub mod realtime;
pub mod volley;
pub mod voice;
pub mod rekey;
pub mod roles;
pub mod service;
pub mod stream;
pub mod streamauth;
pub mod writers;

/// Inner rumor kinds (CORD-02 Appendix B). The *outer* event is always a wrap
/// ([`stream::KIND_WRAP`] / [`stream::KIND_WRAP_EPHEMERAL`]); these ride inside.
pub mod kind {
    /// Chat message (NIP-C7 shape: content = text; a `q` tag = an INLINE quote).
    pub const MESSAGE: u16 = 9;
    /// Threaded reply (NIP-22 comment, CORD-03 §3): uppercase `K`/`E`/`P` pin the
    /// thread root, lowercase `k`/`e`/`p` the immediate parent — all rumor ids.
    /// Deliberately distinct from a kind-9 inline quote.
    pub const COMMENT: u16 = 1111;
    /// Reaction (NIP-25 shape: `e`/`p`/`k` tags name the target — `k` is the
    /// target's kind, `9` or `1111`).
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
    /// Control edition, sub-kinded by `vsk` (grammar in `community::edition`).
    pub const CONTROL: u16 = 3308;
    /// Guestbook kick (admin-signed, `p` target, `vac` citation).
    pub const KICK: u16 = 3309;
    /// WebXDC peer signal (payload opaque to the protocol).
    pub const WEBXDC: u16 = 3310;
    /// Guestbook snapshot (refounder-signed, chunked 400/event).
    pub const SNAPSHOT: u16 = 3312;
    /// A Timeout (CORD-04 §8): `["p", target]`, `["until", "<unix seconds>"]`, the signer's `vac`.
    pub const TIMEOUT: u16 = 3314;
    /// A Nickname (CORD-04 §10): `["p", target]`, the nickname as content, `""` clearing it.
    pub const NICKNAME: u16 = 3315;
    /// Direct invite rumor — rides a STANDARD NIP-59 giftwrap to a person,
    /// never a stream wrap (CORD-05 §6).
    pub const DIRECT_INVITE: u16 = 3313;
    /// Typing indicator (ephemeral tier — 21059 wrap, never stored).
    pub const TYPING: u16 = 23311;
    /// Voice presence heartbeat (ephemeral tier; CORD-07 §4).
    pub const VOICE_PRESENCE: u16 = 23313;
    /// A moderator's voice directive (ephemeral tier; CORD-07 §7): `["p", target]`, `["do", …]`.
    pub const VOICE_DIRECTIVE: u16 = 23314;
    /// Public invite bundle — bare addressable event signed by the per-link
    /// keypair at an empty `d` (CORD-05 §2). Outside the wrap.
    pub const INVITE_BUNDLE: u16 = 33301;
    /// A member's self-encrypted Community List (replaceable). Outside the wrap.
    /// RETIRED by the fragmented form below — a replaceable kind holds one event
    /// per pubkey, so it cannot shard, and the List outgrew what relays accept.
    pub const COMMUNITY_LIST: u16 = 13302;
    /// The fragmented Community List (CORD-02 §8): addressable, one event per
    /// fragment at `d` = the fragment index in decimal. Outside the wrap.
    pub const COMMUNITY_LIST_FRAG: u16 = 33302;
    /// A creator's self-encrypted Invite List (replaceable). Outside the wrap.
    pub const INVITE_LIST: u16 = 13303;
}

/// Control-plane `vsk` sub-kinds (CORD-02 Appendix B). 7 is retired upstream (an
/// owner attestation, obsoleted by the self-certifying community id).
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
    /// One Pin List per Channel at `pins_locator` (CORD-04 §7).
    pub const PINS: &str = "11";
    /// Roles v2 and `@everyone` (CORD-04 §2), replacing vsk 1 once a Community activates it.
    pub const ROLE_V2: &str = "13";
    /// A Channel or Category permission overwrite (CORD-04 §8).
    pub const CHANNEL_ACL: &str = "14";
    /// A Category (CORD-03 §4).
    pub const CATEGORY: &str = "15";
    /// A member's Tenure (CORD-04 §9).
    pub const TENURE: &str = "16";
    /// A Channel's sync to its Category's overwrites (CORD-04 §8).
    pub const CHANNEL_SYNC: &str = "17";
    /// A restricted Channel's Writers ledger (CORD-03 §1).
    pub const WRITERS: &str = "18";
}
