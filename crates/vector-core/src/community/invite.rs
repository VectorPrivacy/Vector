//! Targeted invites (GROUP_PROTOCOL.md).
//!
//! An invite bundle is the key material a new member needs to join: the server-root
//! key, the granted channels' keys + ids + epochs + names, the relay set (the 
//! bootstrap), the owner attestation, and the Community id/name. It is delivered to the
//! invitee's npub over a NIP-17 gift-wrapped DM (the carrier; see the service/command
//! layer). `accept_invite` reconstructs a member-view Community (keyless — authority is
//! the owner-rooted roster, not a held key).
//!
//! Byte fields are hex strings so the bundle is plain JSON inside the DM rumor.

use serde::{Deserialize, Serialize};

use super::{Channel, ChannelId, ChannelKey, Community, CommunityId, Epoch, ServerRootKey};
use crate::stored_event::event_kind;
use nostr_sdk::prelude::{EventBuilder, Kind, PublicKey, UnsignedEvent};

/// A granted channel inside an invite bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteChannel {
    pub id: String,
    pub key: String,
    pub epoch: u64,
    pub name: String,
}

/// Everything a new member needs to join a Community.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityInvite {
    pub community_id: String,
    pub name: String,
    pub server_root_key: String,
    /// The server-root's current epoch, so a joiner adopts the right base read clock. `default`
    /// keeps older bundles parseable (they predate rotation, so epoch 0 is correct for them).
    #[serde(default)]
    pub server_root_epoch: u64,
    pub relays: Vec<String>,
    pub channels: Vec<InviteChannel>,
    /// Owner attestation (signed event JSON) so the joiner learns + verifies who the owner
    /// is. `serde(default)` keeps older bundles (pre-feature) parseable.
    #[serde(default)]
    pub owner_attestation: Option<String>,
}

/// Hard caps on a received bundle. A bundle arrives over an unauthenticated gift wrap
/// from an arbitrary sender, so a hostile one can declare an unbounded channel/relay
/// list to force mass allocation + per-channel DB writes + relay connections. Reject
/// anything past a sane MVP ceiling before allocating.
const MAX_INVITE_CHANNELS: usize = 256;
const MAX_INVITE_RELAYS: usize = 32;

impl CommunityInvite {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// Reject a bundle whose channel/relay counts exceed the MVP ceiling (DoS guard for
    /// inbound, attacker-controlled bundles).
    pub fn validate(&self) -> Result<(), String> {
        if self.channels.len() > MAX_INVITE_CHANNELS {
            return Err(format!("invite declares too many channels ({})", self.channels.len()));
        }
        if self.relays.len() > MAX_INVITE_RELAYS {
            return Err(format!("invite declares too many relays ({})", self.relays.len()));
        }
        Ok(())
    }
}

/// Decode a 64-char hex string to 32 bytes, rejecting malformed input (never
/// silently zero-fills — a corrupt invite must error, not fabricate keys).
fn hex32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", hex.len()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("invalid hex at byte {i}"))?;
    }
    Ok(out)
}

/// Build an invite bundle granting ALL of a Community's channels (the MVP grants the
/// full channel set; per-channel/role-scoped grants are a later feature). Only the
/// management *pubkey* is included — never the secret, so an invited member can't
/// write metadata.
pub fn build_invite(community: &Community) -> CommunityInvite {
    CommunityInvite {
        community_id: community.id.to_hex(),
        name: community.name.clone(),
        server_root_key: crate::simd::hex::bytes_to_hex_32(community.server_root_key.as_bytes()),
        server_root_epoch: community.server_root_epoch.0,
        relays: community.relays.clone(),
        channels: community
            .channels
            .iter()
            .map(|c| InviteChannel {
                id: c.id.to_hex(),
                key: crate::simd::hex::bytes_to_hex_32(c.key.as_bytes()),
                epoch: c.epoch.0,
                name: c.name.clone(),
            })
            .collect(),
        owner_attestation: community.owner_attestation.clone(),
    }
}

/// Reconstruct a **member-view** Community from an invite bundle: full read/post access via the
/// granted channel keys (keyless — write authority is the member's npub roster rank, not a held
/// key).
pub fn accept_invite(invite: &CommunityInvite) -> Result<Community, String> {
    invite.validate()?;
    let id = CommunityId(hex32(&invite.community_id)?);
    let server_root_key = ServerRootKey(hex32(&invite.server_root_key)?);

    // Keep the owner attestation ONLY if it verifies against this community's id (keyless — the
    // community_id is the sole binding; a bundle can't smuggle a bogus owner claim).
    let owner_attestation = invite.owner_attestation.as_ref().and_then(|att| {
        super::owner::verify_owner_attestation(att, &invite.community_id)
            .map(|_| att.clone())
    });

    let mut channels = Vec::with_capacity(invite.channels.len());
    for ic in &invite.channels {
        channels.push(Channel {
            id: ChannelId(hex32(&ic.id)?),
            key: ChannelKey(hex32(&ic.key)?),
            epoch: Epoch(ic.epoch),
            name: ic.name.clone(),
            // A fresh joiner starts with no local banlist; it arrives with the metadata fetch.
            banned: Vec::new(),
            protected: Vec::new(), roster: Default::default(),
            // Only the current key is conveyed on join; the archive fills as rekeys are caught up.
            epoch_keys: Vec::new(),
            dissolved: false,
        });
    }

    Ok(Community {
        id,
        server_root_key,
        server_root_epoch: Epoch(invite.server_root_epoch),
        name: invite.name.clone(),
        // The private bundle conveys join material only; display metadata
        // (description/icon/banner) arrives when the member syncs the GroupRoot, or via
        // a public-invite preview. Left None here.
        description: None,
        icon: None,
        banner: None,
        relays: invite.relays.clone(),
        channels,
        owner_attestation,
        // A fresh join starts alive; the first control fold detects + seals if a tombstone is present.
        dissolved: false,
    })
}

/// Build the gift-wrap rumor that carries an invite to an invitee (carrier). The
/// rumor is an unsigned NIP-59 inner event (kind 3304) whose content is the bundle
/// JSON; the caller gift-wraps it to the invitee's npub over NIP-17 (reusing Vector's
/// existing private-DM path). `my_pubkey` is the rumor author — irrelevant to the
/// bundle's trust (the owner attestation inside it anchors authority), it just
/// satisfies NIP-01 serialization.
pub fn build_invite_rumor(community: &Community, my_pubkey: PublicKey) -> Result<UnsignedEvent, String> {
    let json = build_invite(community).to_json()?;
    Ok(EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_INVITE_BUNDLE), json).build(my_pubkey))
}

/// Parse an inbound rumor as a Community invite. Returns `None` unless the rumor is an
/// invite (kind 3304) carrying a well-formed bundle — a non-invite DM or corrupt
/// content yields `None`, never an error, so the inbound dispatcher can fall through.
pub fn parse_invite_rumor(kind: Kind, content: &str) -> Option<CommunityInvite> {
    if kind != Kind::Custom(event_kind::COMMUNITY_INVITE_BUNDLE) {
        return None;
    }
    CommunityInvite::from_json(content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::envelope::{open_message, seal_message};
    use nostr_sdk::prelude::Keys;

    #[test]
    fn invite_round_trips_to_member_view() {
        let owner = Community::create("HQ", "general", vec!["wss://r1".into(), "wss://r2".into()]);
        let json = build_invite(&owner).to_json().unwrap();
        let invite = CommunityInvite::from_json(&json).unwrap();
        let member = accept_invite(&invite).unwrap();

        // Same Community identity + read material (keyless — the bundle conveys read material only).
        assert_eq!(member.id, owner.id);
        assert_eq!(member.name, "HQ");
        assert_eq!(member.relays, owner.relays);
        assert_eq!(member.server_root_key.as_bytes(), owner.server_root_key.as_bytes());
        assert_eq!(member.channels.len(), 1);
        assert_eq!(member.channels[0].id, owner.channels[0].id);
        assert_eq!(member.channels[0].key.as_bytes(), owner.channels[0].key.as_bytes());
        assert_eq!(member.channels[0].name, "general");
    }

    #[test]
    fn invited_member_can_read_owner_messages() {
        // The killer property: the keys conveyed by the invite actually WORK. Owner
        // seals a message under the channel key; the invited member, reconstructed
        // purely from the bundle, opens it and recovers the author.
        let owner = Community::create("HQ", "general", vec![]);
        let owner_author = Keys::generate();
        let chan = &owner.channels[0];
        let sealed =
            seal_message(&owner_author, &chan.key, &chan.id, chan.epoch, "welcome!", 1).unwrap();

        let invite = CommunityInvite::from_json(&build_invite(&owner).to_json().unwrap()).unwrap();
        let member = accept_invite(&invite).unwrap();
        let mc = &member.channels[0];
        let opened = open_message(&sealed, &mc.key, &mc.id, mc.epoch).unwrap();
        assert_eq!(opened.content, "welcome!");
        assert_eq!(opened.author, owner_author.public_key());
    }

    #[test]
    fn malformed_invite_errors() {
        let owner = Community::create("HQ", "general", vec![]);
        let mut invite = build_invite(&owner);
        invite.server_root_key = "zz".into(); // too short + non-hex
        assert!(accept_invite(&invite).is_err());
    }

    #[test]
    fn accept_rejects_a_bundle_exceeding_the_caps() {
        // DoS guard: an attacker-controlled bundle declaring a huge relay/channel list is rejected
        // before any allocation/connection — never a connect-loop or mass-write.
        let owner = Community::create("HQ", "general", vec![]);
        let mut over_relays = build_invite(&owner);
        over_relays.relays = (0..100).map(|i| format!("wss://r{i}")).collect();
        assert!(accept_invite(&over_relays).is_err(), "too many relays → rejected");

        let mut over_channels = build_invite(&owner);
        over_channels.channels = (0..500)
            .map(|i| InviteChannel { id: "aa".repeat(32), key: "bb".repeat(32), epoch: 0, name: format!("c{i}") })
            .collect();
        assert!(accept_invite(&over_channels).is_err(), "too many channels → rejected");
    }

    #[test]
    fn accept_rejects_malformed_community_id() {
        // A corrupt id must error, never silently zero-fill into a fabricated community.
        let owner = Community::create("HQ", "general", vec![]);
        let mut bad = build_invite(&owner);
        bad.community_id = "not-64-hex".into();
        assert!(accept_invite(&bad).is_err());
    }

    #[test]
    fn accept_drops_a_bogus_owner_attestation_but_still_joins() {
        // The bundle can carry an owner_attestation; an unverifiable one is DROPPED (no spoofed crown),
        // but the join still succeeds — graceful, no panic, no false owner.
        let owner = Community::create("HQ", "general", vec![]);
        let mut inv = build_invite(&owner);
        inv.owner_attestation = Some("not even an event".to_string());
        let member = accept_invite(&inv).unwrap();
        assert!(member.owner_attestation.is_none(), "an unverifiable attestation is dropped, not trusted");
    }

    #[test]
    fn accept_an_empty_channel_bundle_is_graceful() {
        let owner = Community::create("HQ", "general", vec![]);
        let mut inv = build_invite(&owner);
        inv.channels.clear();
        let member = accept_invite(&inv).unwrap();
        assert!(member.channels.is_empty(), "a 0-channel bundle accepts without panic");
    }

    #[test]
    fn bundle_carries_read_keys() {
        // The invite conveys the read keys (server-root + channel) a member needs to read/post.
        let owner = Community::create("HQ", "general", vec![]);
        let json = build_invite(&owner).to_json().unwrap();
        assert!(json.contains(&crate::simd::hex::bytes_to_hex_32(owner.server_root_key.as_bytes())));
        assert!(json.contains(&crate::simd::hex::bytes_to_hex_32(owner.channels[0].key.as_bytes())));
    }

    #[test]
    fn invite_rumor_round_trips() {
        let owner = Community::create("HQ", "general", vec!["wss://r1".into()]);
        let author = Keys::generate();
        let rumor = build_invite_rumor(&owner, author.public_key()).unwrap();

        assert_eq!(rumor.kind, Kind::Custom(event_kind::COMMUNITY_INVITE_BUNDLE));
        let parsed = parse_invite_rumor(rumor.kind, &rumor.content).expect("parses");
        let member = accept_invite(&parsed).unwrap();
        assert_eq!(member.id, owner.id);
        assert_eq!(member.channels[0].key.as_bytes(), owner.channels[0].key.as_bytes());
    }

    #[test]
    fn parse_invite_rumor_rejects_wrong_kind() {
        // A normal text DM (kind 14) must never parse as an invite, even if its content
        // happened to be valid bundle JSON.
        let owner = Community::create("HQ", "general", vec![]);
        let json = build_invite(&owner).to_json().unwrap();
        assert!(parse_invite_rumor(Kind::Custom(14), &json).is_none());
        // Right kind but garbage content → None, not panic.
        assert!(parse_invite_rumor(Kind::Custom(event_kind::COMMUNITY_INVITE_BUNDLE), "not json").is_none());
    }

    #[tokio::test]
    async fn full_carrier_chain_invitee_reads_owner_message() {
        // End-to-end (sans the gift-wrap transport, which is src-tauri): owner builds an
        // invite rumor → invitee parses it → accepts → opens a real channel message the
        // owner sealed. Proves the rumor faithfully conveys working keys.
        use crate::community::transport::memory::MemoryRelay;
        use crate::community::send::{fetch_channel_messages, publish_message};

        let owner = Community::create("HQ", "general", vec!["r1".into()]);
        let author = Keys::generate();
        let rumor = build_invite_rumor(&owner, author.public_key()).unwrap();

        // The invitee only ever sees the rumor content.
        let invite = parse_invite_rumor(rumor.kind, &rumor.content).expect("invite");
        let member = accept_invite(&invite).unwrap();

        // Owner posts a message; the member (reconstructed from the bundle) reads it.
        let relay = MemoryRelay::new();
        let owner_author = Keys::generate();
        publish_message(&relay, &owner, &owner.channels[0], &owner_author, "welcome aboard", 1)
            .await
            .unwrap();

        let msgs = fetch_channel_messages(&relay, &member, &member.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "welcome aboard");
        assert_eq!(msgs[0].author, owner_author.public_key());
    }
}
