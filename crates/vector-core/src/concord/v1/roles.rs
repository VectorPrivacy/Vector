//! Concord role graph (GROUP_PROTOCOL.md).
//!
//! The MVP exposes a single auto-generated "Admin" role, but this is the FULL graph:
//! roles are data (not a hardcoded `is_admin` flag), enforcement is capability-based
//! (effective-permission bits + position), and the engine reads an arbitrary set of
//! roles + grants. Mod roles, per-channel mods, and custom roles later are just more
//! `Role` records flowing through the same code — additive, no enforcement changes.
//!
//! WIRE MODEL (per-entity): roles and grants do NOT live in the GroupRoot, and they
//! are NOT one consolidated blob. Each role is its own addressable RoleMetadata event
//! (vsk=1, d-tag = role_id) and each member's grants are their own Grant event
//! (vsk=3, d-tag = an opaque per-member locator). So two managers editing *different*
//! roles or *different* members never clobber each other — only same-coordinate edits
//! converge (authority-first). `CommunityRoles` here is the in-memory AGGREGATION a
//! client builds from those fetched per-entity events, not an on-wire document.

use serde::{Deserialize, Serialize};

/// Management/moderation permission bits. Access (read/post a channel) is NOT
/// here — that is key possession (the two-mechanism split). Bit positions are part of
/// the wire format and are FROZEN: append a reserved bit, never renumber or reuse one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Permissions(pub u64);

impl Permissions {
    pub const MANAGE_ROLES: u64 = 1 << 0;
    pub const MANAGE_CHANNELS: u64 = 1 << 1;
    pub const MANAGE_METADATA: u64 = 1 << 2;
    pub const KICK: u64 = 1 << 3;
    pub const BAN: u64 = 1 << 4;
    pub const MANAGE_MESSAGES: u64 = 1 << 5;
    pub const CREATE_INVITE: u64 = 1 << 6;
    pub const VIEW_AUDIT_LOG: u64 = 1 << 8;
    pub const MENTION_EVERYONE: u64 = 1 << 9;
    // Reserved / retired (claim the bit so it's never reassigned):
    // `1 << 7` was MANAGE_INVITES — RETIRED (per-creator ownership: no one can manage another's
    // links, so there is nothing to grant; `CREATE_INVITE` mints your own, `BAN` owns the revoking rekey).
    // MANAGE_EMOJI = 1 << 10, PIN_MESSAGES = 1 << 11, MANAGE_EVENTS = 1 << 12.

    /// Every management bit currently defined — what the MVP "Admin" role holds.
    pub const ADMIN_ALL: u64 = Self::MANAGE_ROLES
        | Self::MANAGE_CHANNELS
        | Self::MANAGE_METADATA
        | Self::KICK
        | Self::BAN
        | Self::MANAGE_MESSAGES
        | Self::CREATE_INVITE
        | Self::VIEW_AUDIT_LOG
        | Self::MENTION_EVERYONE;

    /// Control-plane bits: exercising any of these signs a control/metadata edition (keyless model —
    /// the actor's own npub signature IS the authority, re-verified against the roster). Every
    /// management bit EXCEPT purely-social ones (`MENTION_EVERYONE` acts at the message layer, not the
    /// control plane). A role with any of these is a "management role" — its holder shows the admin crown.
    pub const MANAGEMENT_MASK: u64 = Self::MANAGE_ROLES
        | Self::MANAGE_CHANNELS
        | Self::MANAGE_METADATA
        | Self::KICK
        | Self::BAN
        | Self::MANAGE_MESSAGES
        | Self::CREATE_INVITE
        | Self::VIEW_AUDIT_LOG;

    pub fn empty() -> Self {
        Permissions(0)
    }
    pub fn admin() -> Self {
        Permissions(Self::ADMIN_ALL)
    }
    /// True iff this role carries any management permission (vs. a purely-social role) — i.e. its
    /// holder counts as an admin.
    pub fn is_management(self) -> bool {
        self.0 & Self::MANAGEMENT_MASK != 0
    }
    /// True iff every bit in `bits` is set.
    pub fn contains(self, bits: u64) -> bool {
        self.0 & bits == bits
    }
    pub fn union(self, other: Permissions) -> Permissions {
        Permissions(self.0 | other.0)
    }
}

/// Discord's "any channel" vs "this channel". Server-scope acts everywhere;
/// channel-scope is rejected against any other channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "channel_id")]
pub enum RoleScope {
    Server,
    /// Channel id, lowercase hex.
    Channel(String),
}

/// A role: its own addressable RoleMetadata event (vsk=1). Fully general; the MVP
/// auto-creates exactly one (`Role::admin`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    /// Random opaque 32-byte hex id, stable across renames; also the event's d-tag.
    pub role_id: String,
    pub name: String,
    /// Lower = higher authority. Owner is the implicit top (position 0, never a Role).
    /// Authoritative ordering will move to a single RoleOrder entity when
    /// drag-reorder ships; until then this declared value is the position (no reordering
    /// to race in the owner-only MVP).
    pub position: u32,
    pub permissions: Permissions,
    pub scope: RoleScope,
    /// UI badge color (e.g. the Admin crown); 0 = theme default. Cosmetic.
    #[serde(default)]
    pub color: u32,
}

impl Role {
    /// The MVP's auto-created server-scope Admin role: all management bits, position 1
    /// (just below the owner). `role_id` is a fresh random opaque id minted at creation.
    pub fn admin(role_id: String) -> Self {
        Role {
            role_id,
            name: "Admin".to_string(),
            position: 1,
            permissions: Permissions::admin(),
            scope: RoleScope::Server,
            color: 0,
        }
    }
}

/// One member's role grants (vsk=3) — its own addressable event so granting Alice
/// never clobbers Bob's grants. `member` is the grantee's pubkey, lowercase hex (same form
/// as the banlist), and `role_ids` are the roles they hold (a member can have several).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberGrant {
    pub member: String,
    #[serde(default)]
    pub role_ids: Vec<String>,
}

/// The role graph a client AGGREGATES from the fetched per-entity events (RoleMetadata +
/// per-member Grant). Not an on-wire document — the enforcement engine queries this.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommunityRoles {
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub grants: Vec<MemberGrant>,
}

impl CommunityRoles {
    /// Look up a role definition by id.
    pub fn role(&self, role_id: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.role_id == role_id)
    }

    /// The roles granted to `member_hex` (resolved through the grant list).
    pub fn roles_of<'a>(&'a self, member_hex: &'a str) -> impl Iterator<Item = &'a Role> + 'a {
        self.grants
            .iter()
            .filter(move |g| g.member == member_hex)
            .flat_map(move |g| g.role_ids.iter())
            .filter_map(move |rid| self.role(rid))
    }

    /// Effective permissions for a member = union of their granted roles' bits.
    pub fn effective_permissions(&self, member_hex: &str) -> Permissions {
        self.roles_of(member_hex)
            .fold(Permissions::empty(), |acc, r| acc.union(r.permissions))
    }

    /// True iff the member's effective permissions include every bit in `bits`.
    pub fn has_permission(&self, member_hex: &str, bits: u64) -> bool {
        self.effective_permissions(member_hex).contains(bits)
    }

    /// Highest authority (lowest position index) among the member's roles; `None` if
    /// they hold no role. The owner (implicit position 0) is handled by the caller.
    pub fn highest_position(&self, member_hex: &str) -> Option<u32> {
        self.roles_of(member_hex).map(|r| r.position).min()
    }

    /// True iff the member holds at least one role (i.e. is privileged below the owner).
    pub fn is_privileged(&self, member_hex: &str) -> bool {
        self.roles_of(member_hex).next().is_some()
    }

    /// True iff the member holds a role with management permissions — an "admin" (vs. a member who
    /// holds only a non-management/social role). Drives the member-list crown.
    pub fn is_admin(&self, member_hex: &str) -> bool {
        self.roles_of(member_hex).any(|r| r.permissions.is_management())
    }

    /// Is `actor_hex` authorized for an action requiring `permission`? The **owner** (the
    /// proven owner npub, if known) is supreme and always authorized; otherwise the actor must hold
    /// a role granting `permission`. This is the grant-set check the inner-author-proof gates on: a
    /// demoted member is no longer in the grant set, so their actions stop being honored.
    pub fn is_authorized(&self, actor_hex: &str, owner_hex: Option<&str>, permission: u64) -> bool {
        if owner_hex == Some(actor_hex) {
            return true;
        }
        self.has_permission(actor_hex, permission)
    }

    /// escalation defense — may `actor_hex` manage something sitting at `target_position`
    /// (grant/revoke/edit/reorder a role)? The actor must **strictly outrank** it (their highest
    /// authority is a *lower* position number) AND hold `MANAGE_ROLES`. The owner is supreme
    /// (implicit position 0, above every role) and always may. Equal cannot act on equal: an admin
    /// can never grant/revoke a peer admin at the same position — only someone strictly above can.
    /// This is what stops an admin granting the Admin role (`pos == pos`, refused) while still
    /// letting them manage any role beneath them.
    pub fn can_manage_position(&self, actor_hex: &str, owner_hex: Option<&str>, target_position: u32) -> bool {
        self.can_act_on_position(actor_hex, owner_hex, target_position, Permissions::MANAGE_ROLES)
    }

    /// May `actor_hex` act on MEMBER `target_hex` for a role change (add/remove a role)? Resolves the
    /// target's highest authority and applies the `MANAGE_ROLES` position rule. The **owner is never a
    /// valid target** (supreme, unremovable — the sole hardcoded exception).
    pub fn can_manage_member(&self, actor_hex: &str, owner_hex: Option<&str>, target_hex: &str) -> bool {
        self.can_act_on_member(actor_hex, owner_hex, target_hex, Permissions::MANAGE_ROLES)
    }

    /// The pure position test (no permission bit): does `actor_hex` **strictly outrank**
    /// `target_position`? The owner (implicit position 0) outranks everything; a roleless actor
    /// outranks nothing. This is the position half of every authority check — callers AND it with the
    /// specific permission the action needs (`BAN`, `MANAGE_MESSAGES`, `MANAGE_ROLES`, ...).
    pub fn outranks(&self, actor_hex: &str, owner_hex: Option<&str>, target_position: u32) -> bool {
        if owner_hex == Some(actor_hex) {
            return true;
        }
        match self.highest_position(actor_hex) {
            Some(p) => p < target_position,
            None => false,
        }
    }

    /// Generalized authority test: may `actor_hex` perform an action requiring `permission` against a
    /// target at `target_position`? Owner is supreme; otherwise the actor must hold `permission` AND
    /// strictly outrank the target. (`can_manage_position` is this with `MANAGE_ROLES`; bans pass
    /// `BAN`, moderation-hides pass `MANAGE_MESSAGES`.)
    pub fn can_act_on_position(&self, actor_hex: &str, owner_hex: Option<&str>, target_position: u32, permission: u64) -> bool {
        if owner_hex == Some(actor_hex) {
            return true;
        }
        self.has_permission(actor_hex, permission) && self.outranks(actor_hex, owner_hex, target_position)
    }

    /// Generalized member-targeting authority test (ban / kick / hide / role-change). The **owner is
    /// never a valid target**; a roleless member sits below everyone. The actor needs `permission`
    /// plus a strict outrank of the target's highest role.
    pub fn can_act_on_member(&self, actor_hex: &str, owner_hex: Option<&str>, target_hex: &str, permission: u64) -> bool {
        if owner_hex == Some(target_hex) {
            return false;
        }
        let target_position = self.highest_position(target_hex).unwrap_or(u32::MAX);
        self.can_act_on_position(actor_hex, owner_hex, target_position, permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin_roster(member: &str) -> CommunityRoles {
        let role = Role::admin("a".repeat(64));
        CommunityRoles {
            grants: vec![MemberGrant {
                member: member.to_string(),
                role_ids: vec![role.role_id.clone()],
            }],
            roles: vec![role],
        }
    }

    #[test]
    fn admin_role_holds_every_management_bit() {
        let p = Permissions::admin();
        for bit in [
            Permissions::MANAGE_ROLES,
            Permissions::MANAGE_CHANNELS,
            Permissions::MANAGE_METADATA,
            Permissions::KICK,
            Permissions::BAN,
            Permissions::MANAGE_MESSAGES,
            Permissions::CREATE_INVITE,
            Permissions::VIEW_AUDIT_LOG,
            Permissions::MENTION_EVERYONE,
        ] {
            assert!(p.contains(bit), "admin must hold bit {bit}");
        }
    }

    #[test]
    fn social_only_role_is_not_management_nor_admin() {
        // A role holding ONLY a social bit (MENTION_EVERYONE) is not "management" and its holder is
        // NOT an admin — so it gets no crown and no management-secret delivery. Guards the
        // MANAGEMENT_MASK invariant against a future fat-finger that folds a social bit in.
        let social = Permissions(Permissions::MENTION_EVERYONE);
        assert!(!social.is_management(), "a purely-social permission is not management");
        let role = Role {
            role_id: "c".repeat(64),
            name: "Hype".into(),
            position: 5,
            permissions: social,
            scope: RoleScope::Server,
            color: 0,
        };
        let alice = "aa".repeat(32);
        let r = CommunityRoles {
            grants: vec![MemberGrant { member: alice.clone(), role_ids: vec![role.role_id.clone()] }],
            roles: vec![role],
        };
        assert!(!r.is_admin(&alice), "a social-only role holder is not an admin");
        assert!(r.is_privileged(&alice), "though they do hold a role");
    }

    #[test]
    fn hierarchy_only_strictly_higher_can_manage() {
        // owner (implicit pos 0) > admin (pos 1) > mod (pos 2); a plain member holds no role.
        let owner = "00".repeat(32);
        let admin = "aa".repeat(32);
        let moderator = "bb".repeat(32);
        let member = "cc".repeat(32);
        let admin_role = Role::admin("a".repeat(64));
        let mod_role = Role {
            role_id: "b".repeat(64),
            name: "Mod".into(),
            position: 2,
            permissions: Permissions(Permissions::MANAGE_ROLES | Permissions::KICK),
            scope: RoleScope::Server,
            color: 0,
        };
        let admin_pos = admin_role.position; // 1
        let mod_pos = mod_role.position; // 2
        let r = CommunityRoles {
            grants: vec![
                MemberGrant { member: admin.clone(), role_ids: vec![admin_role.role_id.clone()] },
                MemberGrant { member: moderator.clone(), role_ids: vec![mod_role.role_id.clone()] },
            ],
            roles: vec![admin_role, mod_role],
        };
        let owner_ref = Some(owner.as_str());

        // Owner is supreme — outranks every position and every member, can't be targeted.
        assert!(r.can_manage_position(&owner, owner_ref, admin_pos));
        assert!(r.can_manage_member(&owner, owner_ref, &admin));
        assert!(!r.can_manage_member(&admin, owner_ref, &owner), "owner is never a valid target");

        // Equal cannot act on equal: an admin can NOT manage the Admin position (closes self/peer
        // escalation — granting the Admin role), but CAN manage everything strictly below.
        assert!(!r.can_manage_position(&admin, owner_ref, admin_pos), "admin can't grant a peer-rank role");
        assert!(r.can_manage_position(&admin, owner_ref, mod_pos), "admin outranks Mod");
        assert!(r.can_manage_member(&admin, owner_ref, &moderator), "admin outranks the mod member");
        assert!(r.can_manage_member(&admin, owner_ref, &member), "admin outranks a roleless member");
        assert!(!r.can_manage_member(&admin, owner_ref, &admin), "admin can't manage a peer admin");

        // A mod can't reach up to the Admin position, and a roleless member can manage nothing.
        assert!(!r.can_manage_position(&moderator, owner_ref, admin_pos));
        assert!(!r.can_manage_position(&member, owner_ref, mod_pos), "no role, no MANAGE_ROLES → nothing");
    }

    #[test]
    fn can_act_on_member_gates_on_permission_and_rank() {
        // owner (pos 0), a BAN-capable admin (pos 1), a Mod with KICK-only (pos 2, NO ban), a plain
        // member (no role). Verifies the generalized gate requires BOTH the right permission AND a
        // strict outrank — the rule ban/hide use, distinct from MANAGE_ROLES.
        let owner = "00".repeat(32);
        let admin = "aa".repeat(32);
        let kicker = "bb".repeat(32);
        let member = "cc".repeat(32);
        let admin_role = Role::admin("a".repeat(64)); // pos 1, ADMIN_ALL (incl. BAN + MANAGE_MESSAGES)
        let kick_role = Role {
            role_id: "b".repeat(64),
            name: "Mod".into(),
            position: 2,
            permissions: Permissions(Permissions::KICK), // KICK only — NO ban, NO manage-messages
            scope: RoleScope::Server,
            color: 0,
        };
        let r = CommunityRoles {
            grants: vec![
                MemberGrant { member: admin.clone(), role_ids: vec![admin_role.role_id.clone()] },
                MemberGrant { member: kicker.clone(), role_ids: vec![kick_role.role_id.clone()] },
            ],
            roles: vec![admin_role, kick_role],
        };
        let o = Some(owner.as_str());
        use Permissions as P;

        // The BAN-capable admin: bans the Mod and a plain member, but NOT a peer admin, NOT the owner.
        assert!(r.can_act_on_member(&admin, o, &kicker, P::BAN));
        assert!(r.can_act_on_member(&admin, o, &member, P::BAN));
        assert!(!r.can_act_on_member(&admin, o, &admin, P::BAN), "no banning a peer admin (equal rank)");
        assert!(!r.can_act_on_member(&admin, o, &owner, P::BAN), "the owner is never a valid target");
        assert!(r.can_act_on_member(&admin, o, &member, P::MANAGE_MESSAGES), "admin can hide a member's msg");

        // The Mod has KICK but NOT BAN/MANAGE_MESSAGES → the permission gate refuses even a target it outranks.
        assert!(!r.can_act_on_member(&kicker, o, &member, P::BAN), "no BAN permission → can't ban");
        assert!(!r.can_act_on_member(&kicker, o, &member, P::MANAGE_MESSAGES), "no MANAGE_MESSAGES → can't hide");
        assert!(r.can_act_on_member(&kicker, o, &member, P::KICK), "but it CAN kick a plain member");

        // The owner is supreme for any permission, against anyone.
        assert!(r.can_act_on_member(&owner, o, &admin, P::BAN));
        // A plain member can do nothing.
        assert!(!r.can_act_on_member(&member, o, &kicker, P::BAN));
    }

    #[test]
    fn effective_permissions_union_and_position() {
        let alice = "aa".repeat(32);
        let r = admin_roster(&alice);
        assert!(r.has_permission(&alice, Permissions::BAN));
        assert!(r.has_permission(&alice, Permissions::MANAGE_ROLES));
        assert_eq!(r.highest_position(&alice), Some(1));
        assert!(r.is_privileged(&alice));

        // A member with no grant has no permissions and no position.
        let bob = "bb".repeat(32);
        assert!(!r.has_permission(&bob, Permissions::BAN));
        assert_eq!(r.highest_position(&bob), None);
        assert!(!r.is_privileged(&bob));
    }

    #[test]
    fn multiple_roles_union_perms_and_take_highest_position() {
        let alice = "aa".repeat(32);
        let mod_role = Role {
            role_id: "b".repeat(64),
            name: "Mod".into(),
            position: 2,
            permissions: Permissions(Permissions::MANAGE_MESSAGES | Permissions::KICK),
            scope: RoleScope::Server,
            color: 0,
        };
        let admin = Role::admin("a".repeat(64));
        let r = CommunityRoles {
            grants: vec![MemberGrant {
                member: alice.clone(),
                role_ids: vec![admin.role_id.clone(), mod_role.role_id.clone()],
            }],
            roles: vec![admin, mod_role],
        };
        // Union of both roles' bits, and the *highest* (lowest index) position wins.
        assert!(r.has_permission(&alice, Permissions::BAN)); // from Admin
        assert!(r.has_permission(&alice, Permissions::MANAGE_MESSAGES)); // from both
        assert_eq!(r.highest_position(&alice), Some(1));
    }

    #[test]
    fn round_trips_json() {
        let alice = "aa".repeat(32);
        let r = admin_roster(&alice);
        let json = serde_json::to_string(&r).unwrap();
        let back: CommunityRoles = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
        assert!(json.contains("\"kind\":\"server\""));
    }

    #[test]
    fn is_authorized_owner_supreme_admin_by_permission_demoted_rejected() {
        let owner = "00".repeat(32);
        let alice = "aa".repeat(32);
        let r = admin_roster(&alice); // alice granted Admin (has BAN)
        // Owner is always authorized, even without a role.
        assert!(r.is_authorized(&owner, Some(&owner), Permissions::BAN));
        // Admin Alice is authorized for BAN via her role.
        assert!(r.is_authorized(&alice, Some(&owner), Permissions::BAN));
        // A member with no role (Bob) is not — and neither is Alice once demoted (no grant).
        let bob = "bb".repeat(32);
        assert!(!r.is_authorized(&bob, Some(&owner), Permissions::BAN));
        let demoted = CommunityRoles { roles: r.roles.clone(), grants: vec![] };
        assert!(!demoted.is_authorized(&alice, Some(&owner), Permissions::BAN));
        // ...but the owner stays authorized even with an empty grant set.
        assert!(demoted.is_authorized(&owner, Some(&owner), Permissions::BAN));
    }

    #[test]
    fn channel_scope_round_trips() {
        let scope = RoleScope::Channel("cc".repeat(32));
        let json = serde_json::to_string(&scope).unwrap();
        let back: RoleScope = serde_json::from_str(&json).unwrap();
        assert_eq!(scope, back);
    }
}
