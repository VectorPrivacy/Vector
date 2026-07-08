//! The v2 in-memory community — the service/DB working type.
//!
//! Deliberately v2-native rather than reusing v1's [`crate::community::Community`]:
//! a v1 `Channel` assumes an independent per-channel key, but a v2 Public Channel
//! derives its key from the `community_root` (CORD-03), so the two don't share a
//! shape. Both protocols converge instead at the FACADE, where each produces the
//! same untyped JSON summary the SDK consumes — so dual-stack costs no SDK type
//! change (a `version` field is the only tell).

use nostr_sdk::prelude::PublicKey;

use super::super::{ChannelId, CommunityId, Epoch};
use super::control::{CommunityIdentity, Genesis};
use super::invite::CommunityInvite;

/// A channel as the v2 service holds it. A Public channel's secret is the
/// `community_root` (`key == None`); a Private channel carries its own
/// independent key at its own monotonic epoch.
#[derive(Debug, Clone)]
pub struct ChannelV2 {
    pub id: ChannelId,
    pub name: String,
    pub private: bool,
    /// The independent key of a Private channel; `None` for Public (derive from
    /// the community_root).
    pub key: Option<[u8; 32]>,
    pub epoch: Epoch,
}

/// A v2 community in memory: its self-certifying identity, base access key, and
/// channels. Persisted via [`crate::db::community`]'s v2 helpers into the shared
/// community tables (with the migration-65 columns).
#[derive(Debug, Clone)]
pub struct CommunityV2 {
    pub identity: CommunityIdentity,
    /// The base `@everyone` access key at `root_epoch` — holding it IS membership.
    pub community_root: [u8; 32],
    pub root_epoch: Epoch,
    pub name: String,
    pub description: Option<String>,
    pub relays: Vec<String>,
    pub channels: Vec<ChannelV2>,
    pub dissolved: bool,
    /// Local wall-clock of first acquisition (ms), for display ordering.
    pub created_at_ms: u64,
}

impl CommunityV2 {
    /// Build the owner's fresh community from a [`Genesis`] (the two genesis
    /// editions are the caller's to publish). The `#general` channel is Public.
    pub fn from_genesis(g: &Genesis, name: &str, description: Option<String>, relays: Vec<String>, created_at_ms: u64) -> CommunityV2 {
        CommunityV2 {
            identity: g.identity.clone(),
            community_root: g.community_root,
            root_epoch: Epoch(0),
            name: name.to_string(),
            description,
            relays,
            channels: vec![ChannelV2 {
                id: g.general_channel_id,
                name: "general".to_string(),
                private: false,
                key: None,
                epoch: Epoch(0),
            }],
            dissolved: false,
            created_at_ms,
        }
    }

    /// Reconstruct a member's community from an accepted invite bundle. The
    /// bundle's owner commitment MUST already have been verified
    /// ([`CommunityInvite::validate`]) — this re-checks it fail-closed anyway.
    /// A channel is treated as Private iff its bundle key differs from the
    /// community_root (a Public channel's "key" is the root; a Private one
    /// carries its own).
    pub fn from_bundle(bundle: &CommunityInvite, created_at_ms: u64) -> Result<CommunityV2, String> {
        let community_id = CommunityId(parse_hex32(&bundle.community_id, "community_id")?);
        let owner_xonly = parse_hex32(&bundle.owner, "owner")?;
        let owner_salt = parse_hex32(&bundle.owner_salt, "owner_salt")?;
        let identity = CommunityIdentity { community_id, owner_xonly, owner_salt };
        if !identity.verify() {
            return Err("invite bundle owner commitment does not reproduce the community_id".to_string());
        }
        let community_root = parse_hex32(&bundle.community_root, "community_root")?;

        let mut channels = Vec::with_capacity(bundle.channels.len());
        for g in &bundle.channels {
            let id = ChannelId(parse_hex32(&g.id, "channel id")?);
            let key = parse_hex32(&g.key, "channel key")?;
            let private = key != community_root;
            channels.push(ChannelV2 {
                id,
                name: g.name.clone(),
                private,
                key: private.then_some(key),
                epoch: Epoch(g.epoch),
            });
        }

        Ok(CommunityV2 {
            identity,
            community_root,
            root_epoch: Epoch(bundle.root_epoch),
            name: bundle.name.clone(),
            description: None,
            relays: bundle.relays.clone(),
            channels,
            dissolved: false,
            created_at_ms,
        })
    }

    /// The `community_id` this community is anchored on.
    pub fn id(&self) -> &CommunityId {
        &self.identity.community_id
    }

    /// The proven owner (the identity self-certifies at construction).
    pub fn owner(&self) -> Result<PublicKey, String> {
        self.identity.owner()
    }

    pub fn channel(&self, id: &ChannelId) -> Option<&ChannelV2> {
        self.channels.iter().find(|c| c.id.0 == id.0)
    }

    /// The encryption secret + epoch that address a channel's Chat Plane: the
    /// `community_root` at `root_epoch` for a Public channel, the channel's own
    /// key at its own epoch for a Private one (CORD-03 §1).
    pub fn channel_secret(&self, ch: &ChannelV2) -> ([u8; 32], Epoch) {
        match ch.key {
            Some(k) if ch.private => (k, ch.epoch),
            _ => (self.community_root, self.root_epoch),
        }
    }

    /// Every `(secret, epoch)` pair to query for a channel's history — for the
    /// first cut this is the single current head; the multi-epoch archive
    /// (across rekeys) layers on once rotation lands in the service.
    pub fn channel_read_coords(&self, ch: &ChannelV2) -> Vec<([u8; 32], Epoch)> {
        vec![self.channel_secret(ch)]
    }

    /// The untyped summary the facade hands the SDK (protocol-agnostic shape +
    /// a `version` tell). Kept deliberately close to v1's summary keys so a
    /// consumer treats both uniformly.
    pub fn to_summary_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": crate::simd::hex::bytes_to_hex_32(&self.identity.community_id.0),
            "version": 2,
            "name": self.name,
            "description": self.description,
            "relays": self.relays,
            "owner": crate::simd::hex::bytes_to_hex_32(&self.identity.owner_xonly),
            "dissolved": self.dissolved,
            "channels": self.channels.iter().map(|c| serde_json::json!({
                "id": crate::simd::hex::bytes_to_hex_32(&c.id.0),
                "name": c.name,
                "private": c.private,
            })).collect::<Vec<_>>(),
        })
    }
}

fn parse_hex32(hex: &str, field: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{field} is not 32-byte hex"));
    }
    Ok(crate::simd::hex::hex_to_bytes_32(hex))
}

#[cfg(test)]
mod tests {
    use super::super::invite::ChannelGrant;
    use super::*;
    use nostr_sdk::prelude::Keys;

    #[test]
    fn genesis_yields_a_public_general_channel() {
        let owner = Keys::generate();
        let meta = super::super::control::CommunityMetadata { name: "Test".into(), ..Default::default() };
        let g = super::super::control::genesis(&owner, meta, 1_000).unwrap();
        let c = CommunityV2::from_genesis(&g, "Test", None, vec!["wss://r".into()], 42);

        assert!(c.identity.verify());
        assert_eq!(c.owner().unwrap(), owner.public_key());
        assert_eq!(c.channels.len(), 1);
        let ch = &c.channels[0];
        assert!(!ch.private);
        assert_eq!(ch.key, None, "a public channel stores no key");
        // A public channel's secret is the community_root at the root epoch.
        assert_eq!(c.channel_secret(ch), (c.community_root, Epoch(0)));
    }

    #[test]
    fn from_bundle_verifies_owner_and_classifies_channels() {
        let owner = Keys::generate();
        let identity = CommunityIdentity::mint(&owner.public_key());
        let root = [0x11u8; 32];
        let hex = crate::simd::hex::bytes_to_hex_32;

        let priv_key = [0x22u8; 32];
        let bundle = CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&identity.owner_xonly),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&root),
            root_epoch: 0,
            channels: vec![
                // Public: key == root.
                ChannelGrant { id: hex(&[0xa1; 32]), key: hex(&root), epoch: 0, name: "general".into() },
                // Private: key != root.
                ChannelGrant { id: hex(&[0xa2; 32]), key: hex(&priv_key), epoch: 1, name: "mods".into() },
            ],
            relays: vec!["wss://r".into()],
            name: "Test".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };

        let c = CommunityV2::from_bundle(&bundle, 99).unwrap();
        assert_eq!(c.owner().unwrap(), owner.public_key());
        assert!(!c.channels[0].private);
        assert!(c.channels[1].private);
        assert_eq!(c.channels[1].key, Some(priv_key));
        // Public channel reads under the root; private under its own key/epoch.
        assert_eq!(c.channel_secret(&c.channels[0]), (root, Epoch(0)));
        assert_eq!(c.channel_secret(&c.channels[1]), (priv_key, Epoch(1)));
    }

    #[test]
    fn from_bundle_rejects_a_forged_owner_commitment() {
        let owner = Keys::generate();
        let attacker = Keys::generate();
        let identity = CommunityIdentity::mint(&owner.public_key());
        let hex = crate::simd::hex::bytes_to_hex_32;
        // Claim the real id but the attacker's key — the commitment won't reproduce it.
        let bundle = CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&attacker.public_key().to_bytes()),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&[0x11; 32]),
            root_epoch: 0,
            channels: vec![],
            relays: vec![],
            name: "X".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        assert!(CommunityV2::from_bundle(&bundle, 0).is_err());
    }
}
