//! Moderation authority: who may remove whose message (CORD-04 §3/§5).
//!
//! One algebra for both protocols. The wire differs — v1 proves its owner with a
//! signed attestation, v2's identity self-certifies against the community id — so
//! the owner lookup branches and everything downstream does not.

use super::roles::{CommunityRoles, Permissions};
use super::{CommunityId, ConcordProtocol};

/// The community's proven owner as lowercase hex, whichever protocol it speaks.
///
/// Resolving this through the v1 loader alone returns `None` for a v2 community
/// (v2 stores no attestation), and a `None` owner is not a safe default here: it
/// costs the owner their supremacy AND drops the protection that makes them an
/// invalid target, so an admin outranks the roleless owner in both directions.
pub fn owner_hex(community_id: &str) -> Option<String> {
    let id = CommunityId(crate::simd::hex::hex_to_bytes_32(community_id));
    match crate::db::community::community_protocol(&id).ok().flatten() {
        Some(ConcordProtocol::V2) => crate::db::community::load_community_v2(&id)
            .ok()
            .flatten()
            .and_then(|c| c.owner().ok())
            .map(|pk| pk.to_hex()),
        _ => crate::db::community::load_community(&id)
            .ok()
            .flatten()
            .and_then(|c| super::service::proven_owner_hex(&c)),
    }
}

/// May `actor_hex` remove a message authored by `author_hex`? The actor needs
/// `MANAGE_MESSAGES` and a strict outrank; the owner is supreme and is never a
/// valid target, so owner-protection falls out of the algebra with no carve-out.
///
/// Identities are normalized first: a message's stored npub is BECH32 while the
/// owner and the roster grants are keyed by lowercase HEX, and an unnormalized
/// author matches neither — it skips owner-protection and misses the roster
/// lookup, defaulting to the lowest rank.
pub fn can_hide(
    owner_hex: Option<&str>,
    roster: &CommunityRoles,
    actor_hex: &str,
    author_hex: &str,
) -> bool {
    let to_hex = |s: &str| {
        nostr_sdk::prelude::PublicKey::parse(s)
            .map(|pk| pk.to_hex())
            .unwrap_or_else(|_| s.to_string())
    };
    roster.can_act_on_member(
        &to_hex(actor_hex),
        owner_hex,
        &to_hex(author_hex),
        Permissions::MANAGE_MESSAGES,
    )
}

/// [`can_hide`] with the owner and roster resolved from the store. Callers
/// judging a whole page should resolve those once and call [`can_hide`] instead.
pub fn can_hide_in(community_id: &str, actor_hex: &str, author_hex: &str) -> bool {
    let roster = crate::db::community::get_community_roles(community_id).unwrap_or_default();
    can_hide(owner_hex(community_id).as_deref(), &roster, actor_hex, author_hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::roles::{MemberGrant, Role};

    fn roster(owner: &str, admin_a: &str, admin_b: &str, mod_hex: &str) -> CommunityRoles {
        let admin = Role::admin("a".repeat(64));
        let mut moderator = Role::admin("b".repeat(64));
        moderator.position = admin.position + 1;
        let grants = vec![
            MemberGrant { member: owner.to_string(), role_ids: vec![admin.role_id.clone()] },
            MemberGrant { member: admin_a.to_string(), role_ids: vec![admin.role_id.clone()] },
            MemberGrant { member: admin_b.to_string(), role_ids: vec![admin.role_id.clone()] },
            MemberGrant { member: mod_hex.to_string(), role_ids: vec![moderator.role_id.clone()] },
        ];
        CommunityRoles { grants, roles: vec![admin, moderator] }
    }

    #[test]
    fn the_owner_outranks_an_admin_even_holding_the_same_role() {
        let (owner, admin_a, admin_b, moderator) =
            ("11".repeat(32), "22".repeat(32), "33".repeat(32), "44".repeat(32));
        let r = roster(&owner, &admin_a, &admin_b, &moderator);

        // The owner is supreme: the shared Admin role puts them at the SAME
        // position as admin_a, and equal-cannot-act-on-equal must not apply.
        assert!(can_hide(Some(&owner), &r, &owner, &admin_a));
        assert!(can_hide(Some(&owner), &r, &owner, &moderator));
        // Peer admins still can't touch each other, and nobody touches the owner.
        assert!(!can_hide(Some(&owner), &r, &admin_a, &admin_b));
        assert!(!can_hide(Some(&owner), &r, &admin_a, &owner));
        assert!(can_hide(Some(&owner), &r, &admin_a, &moderator));
    }

    #[test]
    fn an_unresolved_owner_costs_supremacy_and_protection_both_ways() {
        // Why an unresolvable owner is never a safe default: position 0 is
        // implicit, so an owner holding no Role ranks LAST once `owner_hex` is
        // None — unable to moderate anyone, and outranked by their own admins.
        let (owner, admin, moderator) = ("11".repeat(32), "22".repeat(32), "44".repeat(32));
        let admin_role = Role::admin("a".repeat(64));
        let r = CommunityRoles {
            grants: vec![MemberGrant { member: admin.clone(), role_ids: vec![admin_role.role_id.clone()] }],
            roles: vec![admin_role],
        };

        assert!(!can_hide(None, &r, &owner, &admin), "the owner can't moderate anyone");
        assert!(!can_hide(None, &r, &owner, &moderator));
        assert!(can_hide(None, &r, &admin, &owner), "and is exposed to their own admins");

        // Resolved, the same roster behaves: supreme in one direction, untouchable in the other.
        assert!(can_hide(Some(&owner), &r, &owner, &admin));
        assert!(!can_hide(Some(&owner), &r, &admin, &owner));
    }

    #[test]
    fn a_bech32_author_resolves_to_the_same_verdict_as_hex() {
        use nostr_sdk::prelude::*;
        let owner_keys = Keys::generate();
        let admin_keys = Keys::generate();
        let owner = owner_keys.public_key().to_hex();
        let admin = admin_keys.public_key().to_hex();
        let r = roster(&owner, &admin, &"33".repeat(32), &"44".repeat(32));
        let admin_bech32 = admin_keys.public_key().to_bech32().unwrap();
        let owner_bech32 = owner_keys.public_key().to_bech32().unwrap();

        assert!(can_hide(Some(&owner), &r, &owner_bech32, &admin_bech32));
        // Owner-protection survives a bech32 target.
        assert!(!can_hide(Some(&owner), &r, &admin_bech32, &owner_bech32));
    }
}
