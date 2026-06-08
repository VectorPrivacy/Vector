//! Control-plane metadata CONTENT structs (GROUP_PROTOCOL.md).
//!
//! `CommunityMetadata` (GroupRoot, vsk=0) and `ChannelMetadata` (vsk=2) are the decrypted content
//! of real-npub-signed 3308 control editions, built by `roster::build_community_root_edition` /
//! `build_channel_metadata_edition` and folded by the per-entity version chain.

use serde::{Deserialize, Serialize};

use super::{Community, CommunityImage};

/// Community-level descriptor (the "GroupRoot" entity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityMetadata {
    pub name: String,
    /// Preferred relay set — also bootstrapped via the invite.
    #[serde(default)]
    pub relays: Vec<String>,
    /// Short description / topic. `serde(default)` so older roots stay readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Logo (encrypted blob ref — key rides in this ServerRoot-sealed content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<CommunityImage>,
    /// Banner (encrypted blob ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<CommunityImage>,
    /// Owner attestation (signed event JSON) — lets members verify who the owner is via the
    /// GroupRoot too (the invite bundle is the other carrier). `serde(default)` for old roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_attestation: Option<String>,
}

impl CommunityMetadata {
    /// The GroupRoot descriptor for a Community — the content of its vsk=0 control edition.
    pub fn of(community: &Community) -> Self {
        CommunityMetadata {
            name: community.name.clone(),
            relays: community.relays.clone(),
            description: community.description.clone(),
            icon: community.icon.clone(),
            banner: community.banner.clone(),
            owner_attestation: community.owner_attestation.clone(),
        }
    }
}

/// Channel-level descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMetadata {
    pub name: String,
}
