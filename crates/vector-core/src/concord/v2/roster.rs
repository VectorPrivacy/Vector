//! CORD-04: the owner-rooted Roster — Roles, Grants, permissions, position.
//!
//! Authority is *rejection, not prevention*: anyone can publish an action;
//! everyone else drops the ones that don't map to a qualifying rank. Rank is
//! owner-rooted — the owner is proven by the `community_id` itself, occupies
//! position 0, and is supreme; every Role and Grant must trace to them.

use std::collections::{BTreeMap, HashMap};

use nostr_sdk::prelude::PublicKey;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{perm, ChannelId, RoleId, MAX_ROLES, MAX_ROLES_PER_MEMBER, NAME_MAX_BYTES};

/// A Role's reach: server-wide, or one Channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RoleScope {
    Server,
    Channel {
        #[serde(with = "hex32_serde")]
        channel_id: [u8; 32],
    },
}

/// A Role: a named bundle of permission bits at a position (vsk 1). It mints
/// no key — granting it hands *rank*, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    #[serde(with = "hex32_serde")]
    pub role_id: [u8; 32],
    pub name: String,
    /// Lower is higher; 0 is the owner and never mintable.
    pub position: u32,
    /// Rides the wire as a decimal string (a JSON number is a float in
    /// JavaScript and corrupts past 2^53); a reader accepts either form.
    #[serde(with = "perm_string")]
    pub permissions: u64,
    pub scope: RoleScope,
    /// Cosmetic badge tint; 0 = theme default.
    #[serde(default)]
    pub color: u32,
}

impl Role {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.len() > NAME_MAX_BYTES {
            return Err(format!("role name exceeds {NAME_MAX_BYTES} bytes"));
        }
        if self.position == 0 {
            return Err("position 0 is the owner's and never mintable".into());
        }
        Ok(())
    }
}

/// A Grant: a member's npub mapped to their Roles (vsk 3). Empty `role_ids`
/// is a revoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    #[serde(with = "hex32_serde")]
    pub member: [u8; 32],
    #[serde(with = "hex32_vec_serde")]
    pub role_ids: Vec<[u8; 32]>,
}

/// A member's authority rank. Ordering: `Owner` above every position, lower
/// position above higher, any position above `Roleless`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    Owner,
    Position(u32),
    Roleless,
}

impl Rank {
    /// Strictly outranks — equal cannot act on equal (CORD-04 §3).
    pub fn outranks(&self, other: &Rank) -> bool {
        match (self, other) {
            (Rank::Owner, Rank::Owner) => false,
            (Rank::Owner, _) => true,
            (_, Rank::Owner) => false,
            (Rank::Position(a), Rank::Position(b)) => a < b,
            (Rank::Position(_), Rank::Roleless) => true,
            (Rank::Roleless, _) => false,
        }
    }
}

/// The folded Roster: the owner plus every honored Role and Grant.
#[derive(Debug, Clone)]
pub struct Roster {
    pub owner: PublicKey,
    /// Keyed (and capped) by `role_id`: a Community carries at most 100
    /// Roles — fold the 100 lowest ids, ignore the rest, deterministically.
    pub roles: BTreeMap<RoleId, Role>,
    pub grants: HashMap<PublicKey, Vec<RoleId>>,
}

impl Roster {
    pub fn new(owner: PublicKey) -> Self {
        Roster { owner, roles: BTreeMap::new(), grants: HashMap::new() }
    }

    /// Insert a Role under the deterministic 100-lowest-ids cap.
    pub fn insert_role(&mut self, role: Role) {
        self.roles.insert(RoleId(role.role_id), role);
        while self.roles.len() > MAX_ROLES {
            // BTreeMap orders by id: evict the highest.
            let last = *self.roles.keys().next_back().expect("non-empty");
            self.roles.remove(&last);
        }
    }

    /// Apply a Grant, truncating to the 64-role member cap deterministically
    /// (lowest role ids kept).
    pub fn apply_grant(&mut self, member: PublicKey, mut role_ids: Vec<RoleId>) {
        role_ids.sort();
        role_ids.dedup();
        role_ids.truncate(MAX_ROLES_PER_MEMBER);
        if role_ids.is_empty() {
            self.grants.remove(&member);
        } else {
            self.grants.insert(member, role_ids);
        }
    }

    fn member_roles(&self, member: &PublicKey) -> impl Iterator<Item = &Role> {
        self.grants
            .get(member)
            .into_iter()
            .flatten()
            .filter_map(|rid| self.roles.get(rid))
    }

    /// A member's rank: the lowest position among their Roles; the owner is
    /// position 0 by identity, a roleless member effectively last.
    pub fn rank(&self, member: &PublicKey) -> Rank {
        if *member == self.owner {
            return Rank::Owner;
        }
        self.member_roles(member)
            .map(|r| r.position)
            .min()
            .map(Rank::Position)
            .unwrap_or(Rank::Roleless)
    }

    /// Effective *server-wide* permissions: the union of server-scoped Roles'
    /// bits. The owner is supreme (all bits).
    pub fn permissions(&self, member: &PublicKey) -> u64 {
        if *member == self.owner {
            return u64::MAX;
        }
        self.member_roles(member)
            .filter(|r| matches!(r.scope, RoleScope::Server))
            .fold(0u64, |acc, r| acc | r.permissions)
    }

    /// Effective permissions *within one Channel*: server-wide bits plus
    /// channel-scoped Roles matching it.
    pub fn permissions_in_channel(&self, member: &PublicKey, channel: &ChannelId) -> u64 {
        if *member == self.owner {
            return u64::MAX;
        }
        self.member_roles(member)
            .filter(|r| match r.scope {
                RoleScope::Server => true,
                RoleScope::Channel { channel_id } => channel_id == channel.0,
            })
            .fold(0u64, |acc, r| acc | r.permissions)
    }

    /// The one hard rule (CORD-04 §3): the actor holds the required bit AND
    /// strictly outranks the target.
    pub fn can_act_on(&self, actor: &PublicKey, required_bit: u64, target: &PublicKey) -> bool {
        self.permissions(actor) & required_bit != 0 && self.rank(actor).outranks(&self.rank(target))
    }

    /// Whether `actor` may publish a Role edition claiming `position`: holds
    /// `MANAGE_ROLES` and the position sits strictly *below* them — nobody
    /// promotes toward the top, and the top itself is unmintable. Binds the
    /// owner too: position 0 is refused even from position 0.
    pub fn may_place_role(&self, actor: &PublicKey, position: u32) -> bool {
        if self.permissions(actor) & perm::MANAGE_ROLES == 0 || position == 0 {
            return false;
        }
        self.rank(actor).outranks(&Rank::Position(position))
    }

    /// Whether `actor` may hand out this exact role set to `member`: outranks
    /// every Role handed out AND the target member (a revoke included).
    pub fn may_grant(&self, actor: &PublicKey, member: &PublicKey, role_ids: &[RoleId]) -> bool {
        if self.permissions(actor) & perm::MANAGE_ROLES == 0 {
            return false;
        }
        let actor_rank = self.rank(actor);
        if !actor_rank.outranks(&self.rank(member)) {
            return false;
        }
        role_ids.iter().all(|rid| match self.roles.get(rid) {
            Some(role) => actor_rank.outranks(&Rank::Position(role.position)),
            // Granting a Role the fold doesn't hold can't be rank-checked.
            None => false,
        })
    }
}

// --- serde helpers ---

mod hex32_serde {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::simd::hex::bytes_to_hex_32(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        crate::simd::hex::hex_to_bytes_32_checked(&s)
            .ok_or_else(|| D::Error::custom("expected 64-char lowercase hex"))
    }
}

mod hex32_vec_serde {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[[u8; 32]], s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(v.len()))?;
        for item in v {
            seq.serialize_element(&crate::simd::hex::bytes_to_hex_32(item))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<[u8; 32]>, D::Error> {
        let strings = Vec::<String>::deserialize(d)?;
        strings
            .into_iter()
            .map(|s| {
                crate::simd::hex::hex_to_bytes_32_checked(&s)
                    .ok_or_else(|| D::Error::custom("expected 64-char lowercase hex"))
            })
            .collect()
    }
}

/// Wire form of the permission field: always written as a decimal string,
/// read as either a string or a (legacy) number.
mod perm_string {
    use super::*;

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Str(String),
            Num(u64),
        }
        match Either::deserialize(d)? {
            Either::Str(s) => s.parse().map_err(|_| D::Error::custom("permissions not a decimal u64")),
            Either::Num(n) => Ok(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    fn pk() -> PublicKey {
        Keys::generate().public_key()
    }

    fn role(id: u8, position: u32, permissions: u64) -> Role {
        Role {
            role_id: [id; 32],
            name: "r".into(),
            position,
            permissions,
            scope: RoleScope::Server,
            color: 0,
        }
    }

    fn roster_with(owner: PublicKey, roles: Vec<Role>, grants: Vec<(PublicKey, Vec<u8>)>) -> Roster {
        let mut r = Roster::new(owner);
        for role in roles {
            r.insert_role(role);
        }
        for (member, ids) in grants {
            r.apply_grant(member, ids.into_iter().map(|i| RoleId([i; 32])).collect());
        }
        r
    }

    #[test]
    fn role_wire_shape_permissions_as_decimal_string() {
        let r = role(1, 2, perm::KICK | perm::MANAGE_MESSAGES);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["permissions"], serde_json::json!("40"));
        assert_eq!(json["scope"], serde_json::json!({"kind": "server"}));
        // Reader accepts both forms.
        let from_str: Role = serde_json::from_value(json).unwrap();
        assert_eq!(from_str.permissions, 40);
        let legacy = serde_json::json!({
            "role_id": crate::simd::hex::bytes_to_hex_32(&[1; 32]),
            "name": "r", "position": 2, "permissions": 40,
            "scope": {"kind": "server"}, "color": 0
        });
        let from_num: Role = serde_json::from_value(legacy).unwrap();
        assert_eq!(from_num.permissions, 40);
        // All 64 bits survive the string form exactly.
        let full = role(1, 2, u64::MAX);
        let round: Role = serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(round.permissions, u64::MAX);
    }

    #[test]
    fn channel_scope_wire_shape() {
        let r = Role { scope: RoleScope::Channel { channel_id: [9; 32] }, ..role(1, 2, 1) };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["scope"]["kind"], "channel");
        let back: Role = serde_json::from_value(json).unwrap();
        assert_eq!(back.scope, RoleScope::Channel { channel_id: [9; 32] });
    }

    #[test]
    fn rank_is_lowest_position_owner_supreme_roleless_last() {
        let owner = pk();
        let admin = pk();
        let nobody = pk();
        let r = roster_with(owner, vec![role(1, 5, 0), role(2, 2, 0)], vec![(admin, vec![1, 2])]);
        assert_eq!(r.rank(&owner), Rank::Owner);
        assert_eq!(r.rank(&admin), Rank::Position(2));
        assert_eq!(r.rank(&nobody), Rank::Roleless);
        assert!(r.rank(&owner).outranks(&r.rank(&admin)));
        assert!(r.rank(&admin).outranks(&r.rank(&nobody)));
        assert!(!r.rank(&nobody).outranks(&r.rank(&nobody)), "equal cannot act on equal");
    }

    #[test]
    fn permissions_are_the_union_of_role_bits() {
        let owner = pk();
        let member = pk();
        let r = roster_with(
            owner,
            vec![role(1, 3, perm::KICK), role(2, 4, perm::BAN)],
            vec![(member, vec![1, 2])],
        );
        assert_eq!(r.permissions(&member), perm::KICK | perm::BAN);
        assert_eq!(r.permissions(&owner), u64::MAX, "owner is supreme");
        assert_eq!(r.permissions(&pk()), 0);
    }

    #[test]
    fn channel_scoped_roles_apply_only_in_their_channel() {
        let owner = pk();
        let modder = pk();
        let chan = ChannelId([0x77; 32]);
        let mut scoped = role(1, 3, perm::MANAGE_MESSAGES);
        scoped.scope = RoleScope::Channel { channel_id: chan.0 };
        let r = roster_with(owner, vec![scoped], vec![(modder, vec![1])]);
        assert_eq!(r.permissions(&modder), 0, "channel-scoped bits don't leak server-wide");
        assert_eq!(r.permissions_in_channel(&modder, &chan), perm::MANAGE_MESSAGES);
        assert_eq!(r.permissions_in_channel(&modder, &ChannelId([0x78; 32])), 0);
    }

    #[test]
    fn equal_rank_cannot_act_on_equal() {
        let owner = pk();
        let a = pk();
        let b = pk();
        let r = roster_with(owner, vec![role(1, 2, perm::BAN)], vec![(a, vec![1]), (b, vec![1])]);
        assert!(!r.can_act_on(&a, perm::BAN, &b), "an admin cannot ban a peer admin");
        assert!(r.can_act_on(&owner, perm::BAN, &a));
    }

    #[test]
    fn role_placement_rules() {
        let owner = pk();
        let admin = pk();
        let r = roster_with(owner, vec![role(1, 2, perm::MANAGE_ROLES)], vec![(admin, vec![1])]);
        // Below the actor: fine.
        assert!(r.may_place_role(&admin, 3));
        // At or above the actor: refused — no self-promotion toward the top.
        assert!(!r.may_place_role(&admin, 2));
        assert!(!r.may_place_role(&admin, 1));
        // Position 0 is not mintable, the owner included.
        assert!(!r.may_place_role(&owner, 0));
        assert!(r.may_place_role(&owner, 1));
        // No MANAGE_ROLES bit → no placement at all.
        assert!(!r.may_place_role(&pk(), 5));
    }

    #[test]
    fn grant_rules_outrank_both_roles_and_target() {
        let owner = pk();
        let admin = pk();
        let peer = pk();
        let member = pk();
        let r = roster_with(
            owner,
            vec![role(1, 2, perm::MANAGE_ROLES), role(2, 5, 0), role(3, 1, 0)],
            vec![(admin, vec![1]), (peer, vec![1])],
        );
        // Handing out a lower role to a roleless member: fine.
        assert!(r.may_grant(&admin, &member, &[RoleId([2; 32])]));
        // Handing out a role above the actor: refused.
        assert!(!r.may_grant(&admin, &member, &[RoleId([3; 32])]));
        // Acting on a peer of equal rank: refused (revoke included).
        assert!(!r.may_grant(&admin, &peer, &[]));
        // A role the fold doesn't hold can't be rank-checked: refused.
        assert!(!r.may_grant(&admin, &member, &[RoleId([9; 32])]));
        // The owner outranks everyone.
        assert!(r.may_grant(&owner, &admin, &[RoleId([2; 32])]));
    }

    #[test]
    fn caps_are_deterministic() {
        let owner = pk();
        let mut r = Roster::new(owner);
        for i in 0..=(MAX_ROLES as u8) {
            r.insert_role(role(i, 5, 0));
        }
        assert_eq!(r.roles.len(), MAX_ROLES, "fold the 100 lowest role ids");
        assert!(r.roles.contains_key(&RoleId([0; 32])));
        assert!(!r.roles.contains_key(&RoleId([MAX_ROLES as u8; 32])), "highest id evicted");

        let member = pk();
        let many: Vec<RoleId> = (0..70u8).map(|i| RoleId([i; 32])).collect();
        r.apply_grant(member, many);
        assert_eq!(r.grants.get(&member).unwrap().len(), MAX_ROLES_PER_MEMBER);
    }

    #[test]
    fn grant_wire_roundtrip_and_empty_is_revoke() {
        let g = Grant { member: [0xAA; 32], role_ids: vec![[1; 32], [2; 32]] };
        let json = serde_json::to_string(&g).unwrap();
        let back: Grant = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);

        let owner = pk();
        let member = pk();
        let mut r = roster_with(owner, vec![role(1, 2, 0)], vec![(member, vec![1])]);
        assert_eq!(r.rank(&member), Rank::Position(2));
        r.apply_grant(member, vec![]);
        assert_eq!(r.rank(&member), Rank::Roleless, "empty role_ids is a revoke");
    }

    #[test]
    fn role_validation() {
        assert!(role(1, 1, 0).validate().is_ok());
        assert!(role(1, 0, 0).validate().is_err(), "position 0 unmintable");
        let mut long = role(1, 1, 0);
        long.name = "x".repeat(NAME_MAX_BYTES + 1);
        assert!(long.validate().is_err());
    }
}
