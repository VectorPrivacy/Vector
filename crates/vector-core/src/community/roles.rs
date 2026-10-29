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
    /// Curate a Channel's Pin List (CORD-04 §7) — editorial power: a pin makes
    /// its message permanently attributable to every future member.
    pub const PIN_MESSAGES: u64 = 1 << 11;
    // Channel permissions (CORD-04 §3): what a member may do in a Channel, adjusted per
    // Channel by its overwrites.
    pub const VIEW_CHANNEL: u64 = 1 << 13;
    pub const SEND_MESSAGES: u64 = 1 << 14;
    pub const SEND_IN_THREADS: u64 = 1 << 15;
    pub const ATTACH_FILES: u64 = 1 << 16;
    pub const ADD_REACTIONS: u64 = 1 << 17;
    pub const READ_MESSAGE_HISTORY: u64 = 1 << 18;
    pub const CONNECT: u64 = 1 << 19;
    pub const SPEAK: u64 = 1 << 20;
    pub const VIDEO: u64 = 1 << 21;
    pub const PRIORITY_SPEAKER: u64 = 1 << 22;
    pub const MUTE_MEMBERS: u64 = 1 << 23;
    pub const DEAFEN_MEMBERS: u64 = 1 << 24;
    pub const MOVE_MEMBERS: u64 = 1 << 25;
    /// Timeouts (CORD-04 §6).
    pub const MODERATE_MEMBERS: u64 = 1 << 26;
    /// Name members beneath them (CORD-04 §10).
    pub const MANAGE_NICKNAMES: u64 = 1 << 28;
    /// Name themselves (CORD-04 §10).
    pub const CHANGE_NICKNAME: u64 = 1 << 29;
    // 1 << 27, 1 << 30 and 1 << 31 reserved: BYPASS_SLOWMODE, CREATE_PRIVATE_THREADS, MANAGE_THREADS.
    // Reserved / retired (claim the bit so it's never reassigned):
    // `1 << 7` was MANAGE_INVITES — RETIRED (per-creator ownership: no one can manage another's
    // links, so there is nothing to grant; `CREATE_INVITE` mints your own, `BAN` owns the revoking rekey).
    // MANAGE_EMOJI = 1 << 10, MANAGE_EVENTS = 1 << 12.

    /// The founding nine bits — the mask every Admin role published before
    /// PIN_MESSAGES existed carries. FROZEN FOREVER: this is the floor the
    /// admin-role finder tests with, so it must never gain a bit. Widening it
    /// would orphan every published Admin role and mint duplicates.
    pub const ADMIN_FOUNDING_MASK: u64 = Self::MANAGE_ROLES
        | Self::MANAGE_CHANNELS
        | Self::MANAGE_METADATA
        | Self::KICK
        | Self::BAN
        | Self::MANAGE_MESSAGES
        | Self::CREATE_INVITE
        | Self::VIEW_AUDIT_LOG
        | Self::MENTION_EVERYONE;

    /// Every management bit currently defined — what a newly minted "Admin"
    /// role holds. MAY widen as bits land; anything that *identifies* an
    /// existing admin role must test [`Self::ADMIN_FOUNDING_MASK`] instead.
    pub const ADMIN_ALL: u64 = Self::ADMIN_FOUNDING_MASK | Self::PIN_MESSAGES;

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
        | Self::VIEW_AUDIT_LOG
        | Self::PIN_MESSAGES;

    /// The STAFF bits (CORD-04 §3): the permissions whose authorized actions
    /// land as Control Plane editions. A member holding ANY of them — plus
    /// always the owner — is staff: the set that holds the `control_root`
    /// write key (CORD-02 §2). `KICK` writes to the Guestbook and
    /// `MANAGE_MESSAGES` to Chat planes; neither needs it. The list is
    /// normative — a future permission whose actions are Control editions
    /// joins it by explicit spec amendment, never by local judgment.
    pub const STAFF_MASK: u64 = Self::MANAGE_ROLES
        | Self::MANAGE_CHANNELS
        | Self::MANAGE_METADATA
        | Self::BAN
        | Self::CREATE_INVITE
        | Self::PIN_MESSAGES;

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
    /// The Roles v2 rank key (CORD-04 §3), from which `position` is assigned at fold time.
    /// `None` for a legacy Role, whose `position` is its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    /// Holders list under this Role's own heading.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub display: bool,
    /// Any member may @mention it (CORD-03 §3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mentionable: bool,
    /// Fields another client wrote that this one doesn't model. An edition replaces the
    /// whole entity, so they ride every republish untouched (CORD-02 §6): dropping them
    /// would erase another client's data for every member.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
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
    /// What every member holds before any Role (CORD-04 §2), once Roles v2 is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub everyone: Option<Permissions>,
    /// Channel permission overwrites (CORD-04 §8), on Roles v2.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overwrites: Vec<Overwrite>,
    /// Channels that take their Category's overwrites instead of their own: channel → category.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub syncs: std::collections::BTreeMap<String, String>,
    /// The Channels that stood before Roles v2 (CORD-04 §2), as the activating `@everyone`
    /// listed them: the only ones an unmarked Channel edition may still describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_channels: Option<std::collections::BTreeSet<String>>,
    /// Private Channels' creators, as each Channel's sealed metadata names them: channel → member.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub creators: std::collections::BTreeMap<String, String>,
}

/// The bits an overwrite may touch (CORD-04 §8): the channel permissions and
/// `MANAGE_MESSAGES`. Everything else, staff bits above all, means the same in every Channel.
pub const OVERWRITABLE: u64 = Permissions::MANAGE_MESSAGES
    | Permissions::VIEW_CHANNEL
    | Permissions::SEND_MESSAGES
    | Permissions::SEND_IN_THREADS
    | Permissions::ATTACH_FILES
    | Permissions::ADD_REACTIONS
    | Permissions::READ_MESSAGE_HISTORY
    | Permissions::CONNECT
    | Permissions::SPEAK
    | Permissions::VIDEO
    | Permissions::PRIORITY_SPEAKER
    | Permissions::MUTE_MEMBERS
    | Permissions::DEAFEN_MEMBERS
    | Permissions::MOVE_MEMBERS
    | Permissions::MODERATE_MEMBERS;

/// Who an overwrite applies to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum OverwriteTarget {
    Everyone,
    Role(String),
    Member(String),
}

/// One overwrite in one Channel or Category: bits allowed and bits denied on top of what
/// the target holds server-wide (CORD-04 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overwrite {
    /// The Channel or Category id, lowercase hex.
    pub scope: String,
    pub target: OverwriteTarget,
    pub allow: Permissions,
    pub deny: Permissions,
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

    /// A member's permissions in one Channel (CORD-04 §8), holding `role_ids` (their Grant,
    /// or one about to replace it): server permissions, then the Channel's `@everyone`
    /// overwrite, its Roles' denies then allows, and their own overwrite last. Without
    /// VIEW_CHANNEL they hold nothing there. The owner holds everything. A private Channel
    /// is closed to `@everyone` whatever its overwrites say: only a Role's or a member's
    /// allow opens it, so no overwrite a manager can clear is all that keeps it private.
    pub fn channel_permissions_holding(&self, owner_hex: Option<&str>, member_hex: &str, channel_hex: &str, role_ids: &[String], private: bool) -> Permissions {
        if owner_hex == Some(member_hex) {
            return Permissions(u64::MAX);
        }
        let roles: Vec<&Role> = role_ids.iter().filter_map(|id| self.role(id)).collect();
        let mut p = roles.iter().fold(self.everyone.unwrap_or_default(), |acc, r| acc.union(r.permissions)).0;
        let channel = channel_hex.to_ascii_lowercase();
        // A Private Channel's creator is let in while they manage channels: nobody's own overwrite
        // can admit them, since none outranks itself (CORD-04 §8).
        let created = p & Permissions::MANAGE_CHANNELS != 0 && self.creators.get(&channel).is_some_and(|c| c == member_hex);
        // A synced Channel resolves against its Category's overwrites, ignoring its own.
        let scope = self.syncs.get(&channel).cloned().unwrap_or(channel);
        let here = || self.overwrites.iter().filter(|o| o.scope == scope);
        if let Some(o) = here().find(|o| o.target == OverwriteTarget::Everyone) {
            p = (p & !o.deny.0) | o.allow.0;
        }
        if private {
            p &= !Permissions::VIEW_CHANNEL;
        }
        let (mut deny, mut allow) = (0u64, 0u64);
        for o in here() {
            if let OverwriteTarget::Role(id) = &o.target {
                if roles.iter().any(|r| r.role_id == *id) {
                    deny |= o.deny.0;
                    allow |= o.allow.0;
                }
            }
        }
        p = (p & !deny) | allow;
        if created {
            p |= Permissions::VIEW_CHANNEL;
        }
        if let Some(o) = here().find(|o| matches!(&o.target, OverwriteTarget::Member(m) if m == member_hex)) {
            p = (p & !o.deny.0) | o.allow.0;
        }
        if p & Permissions::VIEW_CHANNEL == 0 {
            return Permissions::empty();
        }
        Permissions(p)
    }

    /// Whether `@everyone`, resolved up to the View Channel step, may post in a Channel: one
    /// where it can't is restricted (CORD-03 §1). A legacy Roster restricts nothing.
    pub fn everyone_sends(&self, channel_hex: &str) -> bool {
        let Some(everyone) = self.everyone else { return true };
        let channel = channel_hex.to_ascii_lowercase();
        let scope = self.syncs.get(&channel).cloned().unwrap_or(channel);
        let row = self.overwrites.iter().find(|o| o.scope == scope && o.target == OverwriteTarget::Everyone);
        let p = row.map_or(everyone.0, |o| (everyone.0 & !o.deny.0) | o.allow.0);
        p & Permissions::SEND_MESSAGES != 0
    }

    /// [`Self::channel_permissions_holding`] for the Roles the member holds now.
    pub fn channel_permissions(&self, owner_hex: Option<&str>, member_hex: &str, channel_hex: &str, private: bool) -> Permissions {
        let held: Vec<String> = self.roles_of(member_hex).map(|r| r.role_id.clone()).collect();
        self.channel_permissions_holding(owner_hex, member_hex, channel_hex, &held, private)
    }

    /// Effective permissions for a member: `@everyone` and every granted Role's bits.
    pub fn effective_permissions(&self, member_hex: &str) -> Permissions {
        self.roles_of(member_hex)
            .fold(self.everyone.unwrap_or_default(), |acc, r| acc.union(r.permissions))
    }

    /// The permissions a member holds through Roles positioned strictly above `bound`: the only
    /// standing an action on something at `bound` may draw on (CORD-04 §3). Authority then flows
    /// strictly downward, so no Role can prop up the authority that admits it.
    pub fn permissions_above(&self, member_hex: &str, bound: u32) -> Permissions {
        self.roles_of(member_hex)
            .filter(|r| r.position < bound)
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

    /// Whether a member is STAFF (CORD-04 §3): the owner, or any holder of a
    /// Control-writing bit ([`Permissions::STAFF_MASK`]) — the set entitled to
    /// the `control_root` (CORD-02 §2).
    pub fn is_staff(&self, member_hex: &str, owner_hex: Option<&str>) -> bool {
        if owner_hex == Some(member_hex) {
            return true;
        }
        self.effective_permissions(member_hex).0 & Permissions::STAFF_MASK != 0
    }

    /// Whether a grant's role set leaves its member staff, judged against this
    /// roster's role definitions — the granter-side trigger for delivering the
    /// `control_root` inside the Grant itself (CORD-04 §3). An id with no known
    /// definition contributes nothing (never a reason to hand out the secret).
    pub fn roles_make_staff(&self, role_ids: &[String]) -> bool {
        role_ids
            .iter()
            .filter_map(|rid| self.role(rid))
            .any(|r| r.permissions.0 & Permissions::STAFF_MASK != 0)
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

    /// Every grantee [`is_authorized`](Self::is_authorized) for `permission`, found in one pass
    /// over the grants rather than one scan of them per member.
    pub fn authorized_grantees(&self, owner_hex: Option<&str>, permission: u64) -> std::collections::HashSet<&str> {
        let mut bits: std::collections::HashMap<&str, Permissions> = std::collections::HashMap::new();
        for r in &self.roles {
            // `role` finds the first of a repeated id.
            bits.entry(r.role_id.as_str()).or_insert(r.permissions);
        }
        let mut held: std::collections::HashMap<&str, Permissions> = std::collections::HashMap::new();
        for g in &self.grants {
            let member = held.entry(g.member.as_str()).or_insert(self.everyone.unwrap_or_default());
            for rid in &g.role_ids {
                if let Some(b) = bits.get(rid.as_str()) {
                    *member = member.union(*b);
                }
            }
        }
        held.into_iter().filter(|(m, p)| owner_hex == Some(*m) || p.contains(permission)).map(|(m, _)| m).collect()
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

    /// May `actor_hex` set `target_hex`'s Nickname (CORD-04 §10)? Their own takes `CHANGE_NICKNAME`,
    /// `@everyone`'s included, and a legacy Roster restricts nobody's; anyone else's takes
    /// `MANAGE_NICKNAMES` and a strict outrank.
    pub fn may_name(&self, actor_hex: &str, owner_hex: Option<&str>, target_hex: &str) -> bool {
        if actor_hex != target_hex {
            return self.can_act_on_member(actor_hex, owner_hex, target_hex, Permissions::MANAGE_NICKNAMES);
        }
        owner_hex == Some(actor_hex) || self.everyone.is_none() || self.has_permission(actor_hex, Permissions::CHANGE_NICKNAME)
    }

    // ── Private-Channel entitlement (CORD-03/04/06) ──────────────────────────
    //
    // CORD-03 defines a Private Channel as "readable only by granted
    // role-holders", its key "delivered on grant and rekeyed on removal"; the
    // binding that names those role-holders is CORD-04 §2's
    // `scope: {"kind":"channel","channel_id":...}`. So **the roles scoped to a
    // channel are its access list**.
    //
    // Read access is enforced by key possession alone — nothing here grants it.
    // This is the routing that decides who a key is delivered TO on grant, and
    // who a rekey keeps on revoke.

    /// The roles conferring read access to `channel_hex`, in display order
    /// (highest authority first).
    ///
    /// A Private Channel with none is readable by nobody but the owner and
    /// whoever already holds the key — degenerate rather than "open", so
    /// clients shouldn't create it, but one arriving that way from elsewhere
    /// still reads correctly and its key holders keep reading it.
    pub fn channel_roles(&self, channel_hex: &str) -> Vec<&Role> {
        let wanted = channel_hex.to_ascii_lowercase();
        let mut out: Vec<&Role> = self
            .roles
            .iter()
            .filter(|r| matches!(&r.scope, RoleScope::Channel(c) if c.eq_ignore_ascii_case(&wanted)))
            .collect();
        out.sort_by_key(|r| r.position);
        out
    }

    /// [`channel_roles`], by id.
    pub fn channel_role_ids(&self, channel_hex: &str) -> Vec<String> {
        self.channel_roles(channel_hex).into_iter().map(|r| r.role_id.clone()).collect()
    }

    /// Is `member_hex` entitled to `channel_hex`'s key? The owner always is
    /// (position 0, supreme and unremovable).
    ///
    /// `with`/`without` overlay a Grant that was JUST published, so a caller can
    /// settle key custody against the change it just made rather than against a
    /// fold that lags its own publish.
    pub fn is_entitled(
        &self,
        owner_hex: Option<&str>,
        member_hex: &str,
        channel_hex: &str,
        with: &[String],
        without: &[String],
    ) -> bool {
        if owner_hex == Some(member_hex) {
            return true;
        }
        let held = self.held_with(member_hex, with, without);
        // On Roles v2 the key follows View Channel as the Channel resolves it (CORD-03 §5).
        if self.everyone.is_some() {
            return self.channel_permissions_holding(owner_hex, member_hex, channel_hex, &held, true).contains(Permissions::VIEW_CHANNEL);
        }
        self.channel_role_ids(channel_hex).iter().any(|id| held.contains(id))
    }

    /// Whether `member_hex` may read `channel_hex`'s history from before their access began,
    /// under the same overlay as [`Self::is_entitled`]. Legacy rosters have no such bit.
    pub fn reads_history(&self, owner_hex: Option<&str>, member_hex: &str, channel_hex: &str, with: &[String], without: &[String]) -> bool {
        self.everyone.is_none()
            || self.channel_permissions_holding(owner_hex, member_hex, channel_hex, &self.held_with(member_hex, with, without), true).contains(Permissions::READ_MESSAGE_HISTORY)
    }

    fn held_with(&self, member_hex: &str, with: &[String], without: &[String]) -> Vec<String> {
        let mut held: Vec<String> = self.roles_of(member_hex).map(|r| r.role_id.clone()).filter(|id| !without.contains(id)).collect();
        for id in with.iter().filter(|id| !without.contains(id)) {
            if !held.contains(id) {
                held.push(id.clone());
            }
        }
        held
    }

    /// Effective permissions for an action TARGETING one channel: server-scope
    /// roles plus roles scoped to that channel.
    ///
    /// ⚠️ **Offer-side only.** The fold stays scope-agnostic in every
    /// implementation (CORD-04 §3, and [`effective_permissions`] is what judges
    /// inbound authority) — this narrows only what THIS client offers its own
    /// user, never what it honors from others. Tightening the honor path would
    /// retroactively invalidate actions shipped clients already folded.
    pub fn effective_permissions_in(&self, member_hex: &str, channel_hex: &str) -> Permissions {
        let wanted = channel_hex.to_ascii_lowercase();
        self.roles_of(member_hex)
            .filter(|r| match &r.scope {
                RoleScope::Server => true,
                RoleScope::Channel(c) => c.eq_ignore_ascii_case(&wanted),
            })
            .fold(Permissions::empty(), |acc, r| acc.union(r.permissions))
    }

    /// [`is_authorized`](Self::is_authorized), judged against one channel per
    /// [`effective_permissions_in`](Self::effective_permissions_in). Offer-side only.
    pub fn is_authorized_in(&self, actor_hex: &str, owner_hex: Option<&str>, channel_hex: &str, permission: u64) -> bool {
        if owner_hex == Some(actor_hex) {
            return true;
        }
        self.effective_permissions_in(actor_hex, channel_hex).contains(permission)
    }
}

/// One roster's per-member questions answered from each member's Roles gathered once: what
/// [`CommunityRoles`] answers by scanning every grant per question. A fold asks for each
/// candidate it judges, so over a large roster the scans dominate.
pub struct RoleIndex<'a> {
    everyone: Permissions,
    by_id: std::collections::HashMap<&'a str, &'a Role>,
    held: std::collections::HashMap<&'a str, Vec<&'a Role>>,
}

impl<'a> RoleIndex<'a> {
    pub fn of(roster: &'a CommunityRoles) -> Self {
        let mut by_id: std::collections::HashMap<&'a str, &'a Role> = std::collections::HashMap::new();
        for r in &roster.roles {
            // `role` finds the first of a repeated id.
            by_id.entry(r.role_id.as_str()).or_insert(r);
        }
        let mut held: std::collections::HashMap<&str, Vec<&Role>> = std::collections::HashMap::new();
        for g in &roster.grants {
            let roles = held.entry(g.member.as_str()).or_default();
            roles.extend(g.role_ids.iter().filter_map(|rid| by_id.get(rid.as_str()).copied()));
        }
        RoleIndex { everyone: roster.everyone.unwrap_or_default(), by_id, held }
    }

    /// [`CommunityRoles::role`].
    pub fn role(&self, role_id: &str) -> Option<&'a Role> {
        self.by_id.get(role_id).copied()
    }

    fn roles_of(&self, member_hex: &str) -> &[&'a Role] {
        self.held.get(member_hex).map_or(&[], Vec::as_slice)
    }

    /// [`CommunityRoles::permissions_above`].
    pub fn permissions_above(&self, member_hex: &str, bound: u32) -> Permissions {
        self.roles_of(member_hex).iter().filter(|r| r.position < bound).fold(Permissions::empty(), |acc, r| acc.union(r.permissions))
    }

    /// [`CommunityRoles::is_authorized`].
    pub fn is_authorized(&self, actor_hex: &str, owner_hex: Option<&str>, permission: u64) -> bool {
        owner_hex == Some(actor_hex) || self.roles_of(actor_hex).iter().fold(self.everyone, |acc, r| acc.union(r.permissions)).contains(permission)
    }

    /// [`CommunityRoles::highest_position`].
    pub fn highest_position(&self, member_hex: &str) -> Option<u32> {
        self.roles_of(member_hex).iter().map(|r| r.position).min()
    }

    /// The highest of the member's Role positions strictly above `bound`.
    pub fn highest_position_above(&self, member_hex: &str, bound: u32) -> Option<u32> {
        self.roles_of(member_hex).iter().map(|r| r.position).filter(|p| *p < bound).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_grantees_agrees_with_asking_each_member() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..200 {
            let ids: Vec<String> = (0..6).map(|i| format!("{i:064x}")).collect();
            let members: Vec<String> = (0..8).map(|i| format!("{:064x}", 100 + i)).collect();
            // Repeated role ids and members, roles granted that don't exist, @everyone or not.
            let roles = (0..next(7)).map(|_| Role { role_id: ids[next(6) as usize].clone(), position: 1 + next(5) as u32, permissions: Permissions(next(16)), ..Role::admin(String::new()) }).collect();
            let grants = (0..next(12)).map(|_| MemberGrant { member: members[next(8) as usize].clone(), role_ids: (0..next(4)).map(|_| ids[next(6) as usize].clone()).collect() }).collect();
            let r = CommunityRoles { roles, grants, everyone: (next(2) == 0).then(|| Permissions(next(16))), ..Default::default() };
            let owner = (round % 3 == 0).then(|| members[0].as_str());
            let index = RoleIndex::of(&r);
            for bits in 1..16u64 {
                let fast = r.authorized_grantees(owner, bits);
                for g in &r.grants {
                    assert_eq!(fast.contains(g.member.as_str()), r.is_authorized(&g.member, owner, bits), "round {round} bits {bits}");
                }
                for m in members.iter().map(String::as_str).chain(["nobody"]) {
                    assert_eq!(index.is_authorized(m, owner, bits), r.is_authorized(m, owner, bits), "round {round}");
                }
            }
            for m in members.iter().map(String::as_str).chain(["nobody"]) {
                assert_eq!(index.highest_position(m), r.highest_position(m), "round {round}");
                for bound in 0..8 {
                    assert_eq!(index.permissions_above(m, bound), r.permissions_above(m, bound), "round {round} bound {bound}");
                    assert_eq!(index.highest_position_above(m, bound), r.roles_of(m).map(|r| r.position).filter(|p| *p < bound).min(), "round {round} bound {bound}");
                }
            }
        }
    }

    #[test]
    fn a_channel_resolves_everyone_then_roles_then_the_member() {
        let (r1, r2) = ("1".repeat(64), "2".repeat(64));
        let (m, n) = ("a".repeat(64), "b".repeat(64));
        let role = |id: &str, bits: u64| Role { role_id: id.into(), position: 1, permissions: Permissions(bits), ..Role::admin(id.into()) };
        let ch = "c".repeat(64);
        let ow = |target, allow: u64, deny: u64| Overwrite { scope: ch.clone(), target, allow: Permissions(allow), deny: Permissions(deny) };
        let (view, send) = (Permissions::VIEW_CHANNEL, Permissions::SEND_MESSAGES);
        let mut r = CommunityRoles {
            roles: vec![role(&r1, 0), role(&r2, 0)],
            grants: vec![MemberGrant { member: m.clone(), role_ids: vec![r1.clone(), r2.clone()] }],
            everyone: Some(Permissions(view | send)),
            overwrites: vec![ow(OverwriteTarget::Everyone, 0, view | send), ow(OverwriteTarget::Role(r1.clone()), view, 0), ow(OverwriteTarget::Role(r2.clone()), 0, view)],
            syncs: Default::default(), legacy_channels: None, creators: Default::default(),
        };
        let at = |r: &CommunityRoles, who: &str| r.channel_permissions(Some("o"), who, &ch, false).0;
        assert_eq!(at(&r, &m), view, "one role's allow beats another's deny; @everyone's deny of Send stands");
        assert_eq!(at(&r, &n), 0, "without View a member holds nothing there");
        assert_eq!(at(&r, "o"), u64::MAX, "the owner holds everything");
        r.overwrites.push(ow(OverwriteTarget::Member(m.clone()), send, view));
        assert_eq!(at(&r, &m), 0, "the member's own overwrite has the last word");
        assert!(!r.is_entitled(Some("o"), &m, &ch, &[], &[]), "and the key follows View");
        r.overwrites.pop();
        assert!(r.is_entitled(Some("o"), &m, &ch, &[], &[]));
        assert!(!r.is_entitled(Some("o"), &m, &ch, &[], std::slice::from_ref(&r1)), "losing the allowing role loses the key");
        let cat = "d".repeat(64);
        r.overwrites.push(Overwrite { scope: cat.clone(), target: OverwriteTarget::Role(r2.clone()), allow: Permissions(0), deny: Permissions(send) });
        r.syncs.insert(ch.clone(), cat);
        assert_eq!(at(&r, &n), view | send, "a synced channel takes its Category's overwrites, not its own");
        assert_eq!(at(&r, &m), view, "including the Category's denies");
    }

    fn admin_roster(member: &str) -> CommunityRoles {
        let role = Role::admin("a".repeat(64));
        CommunityRoles {
            grants: vec![MemberGrant {
                member: member.to_string(),
                role_ids: vec![role.role_id.clone()],
            }],
            roles: vec![role],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
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
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let alice = "aa".repeat(32);
        let r = CommunityRoles {
            grants: vec![MemberGrant { member: alice.clone(), role_ids: vec![role.role_id.clone()] }],
            roles: vec![role],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
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
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let admin_pos = admin_role.position; // 1
        let mod_pos = mod_role.position; // 2
        let r = CommunityRoles {
            grants: vec![
                MemberGrant { member: admin.clone(), role_ids: vec![admin_role.role_id.clone()] },
                MemberGrant { member: moderator.clone(), role_ids: vec![mod_role.role_id.clone()] },
            ],
            roles: vec![admin_role, mod_role],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
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
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let r = CommunityRoles {
            grants: vec![
                MemberGrant { member: admin.clone(), role_ids: vec![admin_role.role_id.clone()] },
                MemberGrant { member: kicker.clone(), role_ids: vec![kick_role.role_id.clone()] },
            ],
            roles: vec![admin_role, kick_role],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
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
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let admin = Role::admin("a".repeat(64));
        let r = CommunityRoles {
            grants: vec![MemberGrant {
                member: alice.clone(),
                role_ids: vec![admin.role_id.clone(), mod_role.role_id.clone()],
            }],
            roles: vec![admin, mod_role],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
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
        let demoted = CommunityRoles { roles: r.roles.clone(), grants: vec![], everyone: None, overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default() };
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

    // ── Private-Channel entitlement ──────────────────────────────────────────

    /// A roster with one channel-scoped role (`pos` 5) over `chan`, granted to
    /// `member`, plus a server-scope Admin held by `admin`.
    fn scoped_roster(chan: &str, member: &str, admin: &str) -> CommunityRoles {
        let scoped = Role {
            role_id: "11".repeat(32),
            name: "Testers".into(),
            position: 5,
            permissions: Permissions(Permissions::MANAGE_MESSAGES),
            scope: RoleScope::Channel(chan.to_string()),
            color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let server = Role::admin("22".repeat(32));
        CommunityRoles {
            grants: vec![
                MemberGrant { member: member.to_string(), role_ids: vec![scoped.role_id.clone()] },
                MemberGrant { member: admin.to_string(), role_ids: vec![server.role_id.clone()] },
            ],
            roles: vec![scoped, server],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
        }
    }

    #[test]
    fn a_channels_scoped_roles_are_its_access_list() {
        let chan = "cc".repeat(32);
        let other = "dd".repeat(32);
        let owner = "00".repeat(32);
        let member = "aa".repeat(32);
        let admin = "bb".repeat(32);
        let r = scoped_roster(&chan, &member, &admin);

        assert_eq!(r.channel_roles(&chan).len(), 1, "the scoped role is the access list");
        assert!(r.channel_roles(&other).is_empty(), "another channel gets none of it");

        assert!(r.is_entitled(Some(&owner), &member, &chan, &[], &[]), "the granted holder is entitled");
        assert!(r.is_entitled(Some(&owner), &owner, &chan, &[], &[]), "the owner is always entitled");
        // A SERVER-scope Admin is not automatically entitled: entitlement is the
        // channel's scoped roles, never rank (CORD-03 — key possession, not authority).
        assert!(!r.is_entitled(Some(&owner), &admin, &chan, &[], &[]), "rank alone is not entitlement");
        assert!(!r.is_entitled(Some(&owner), &member, &other, &[], &[]), "entitlement does not spill to another channel");
    }

    #[test]
    fn the_grant_overlay_settles_against_a_publish_the_fold_has_not_caught() {
        let chan = "cc".repeat(32);
        let owner = "00".repeat(32);
        let member = "aa".repeat(32);
        let stranger = "ee".repeat(32);
        let r = scoped_roster(&chan, &member, &"bb".repeat(32));
        let scoped_id = r.channel_role_ids(&chan)[0].clone();

        assert!(
            r.is_entitled(Some(&owner), &stranger, &chan, std::slice::from_ref(&scoped_id), &[]),
            "a just-published grant settles as entitled before the fold catches up"
        );
        assert!(
            !r.is_entitled(Some(&owner), &member, &chan, &[], std::slice::from_ref(&scoped_id)),
            "a just-published revoke settles as unentitled before the fold catches up"
        );
    }

    #[test]
    fn channel_narrowing_is_offer_side_only() {
        let chan = "cc".repeat(32);
        let other = "dd".repeat(32);
        let owner = "00".repeat(32);
        let member = "aa".repeat(32);
        let r = scoped_roster(&chan, &member, &"bb".repeat(32));

        // Narrowed: the scoped role's bits apply in ITS channel and nowhere else.
        assert!(r.is_authorized_in(&member, Some(&owner), &chan, Permissions::MANAGE_MESSAGES));
        assert!(!r.is_authorized_in(&member, Some(&owner), &other, Permissions::MANAGE_MESSAGES));

        // The HONOR path stays scope-agnostic — every implementation folds the
        // same union (CORD-04 §3). Tightening this would retroactively invalidate
        // actions shipped clients already folded.
        assert!(
            r.is_authorized(&member, Some(&owner), Permissions::MANAGE_MESSAGES),
            "the scope-blind honor path is unchanged"
        );
    }

    /// The founding mask is the admin-FINDER's floor and is frozen forever: a
    /// role minted under the ORIGINAL nine bits must always satisfy it, or
    /// every published Admin role orphans and the finder mints duplicates.
    #[test]
    fn founding_mask_is_frozen_at_the_original_nine_bits() {
        const ORIGINAL_NINE: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4)
            | (1 << 5) | (1 << 6) | (1 << 8) | (1 << 9);
        assert_eq!(Permissions::ADMIN_FOUNDING_MASK, ORIGINAL_NINE);
        // A legacy admin role (pre-PIN_MESSAGES) still passes the finder's test...
        assert!(Permissions(ORIGINAL_NINE).contains(Permissions::ADMIN_FOUNDING_MASK));
        // ...and a newly minted one does too (ADMIN_ALL may widen, and has).
        assert!(Permissions::admin().contains(Permissions::ADMIN_FOUNDING_MASK));
        assert!(Permissions::admin().contains(Permissions::PIN_MESSAGES));
        // But a legacy role does NOT contain the widened ADMIN_ALL — the exact
        // reason nothing may identify admins with it.
        assert!(!Permissions(ORIGINAL_NINE).contains(Permissions::ADMIN_ALL));
    }
}
