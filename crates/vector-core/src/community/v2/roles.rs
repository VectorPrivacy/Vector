//! CORD-04 Roles: v2 wire content for the Role (vsk 1), Grant (vsk 3), and Banlist
//! (vsk 4) control entities.
//!
//! The authority ALGEBRA is shared, not copied: these types convert to/from
//! [`crate::community::roles`]'s `Role`/`MemberGrant`/`Permissions`, and the fold
//! feeds the shared `authorize_delegation` + `is_authorized`. Only the SERIALIZATION
//! is v2-native, because CORD-04 §3 rides `permissions` as a decimal STRING (the
//! shared `Permissions` serializes as a bare `u64`), and a reader
//! MUST accept either form (a number from an older edition, a string henceforth) and
//! always write the string.

use serde::{Deserialize, Serialize};

use crate::community::roles::{MemberGrant, Permissions, Role, RoleScope};

/// A member holds at most this many Roles (CORD-04 §2); a Community folds at most
/// [`MAX_ROLES_PER_COMMUNITY`] Roles (the lowest role_ids win, the same deterministic
/// cap the member list uses). A Banlist edition holds at most [`MAX_BANLIST`] npubs
/// (the practical NIP-44-envelope ceiling, CORD-04 §4).
pub const MAX_ROLES_PER_MEMBER: usize = 64;
pub const MAX_ROLES_PER_COMMUNITY: usize = 100;
pub const MAX_BANLIST: usize = 500;
/// A role `name` shares the protocol-wide 64-byte cap (CORD-04 §2/§3).
pub const MAX_ROLE_NAME_BYTES: usize = super::control::MAX_NAME_BYTES;

/// CORD-04 §2 Role content (vsk 1), `eid == role_id`. `permissions` rides as a
/// decimal string (§3); unknown fields round-trip (CORD-02 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleContent {
    pub role_id: String,
    pub name: String,
    pub position: u32,
    #[serde(with = "perm_decimal_string")]
    pub permissions: u64,
    pub scope: RoleScope,
    #[serde(default)]
    pub color: u32,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl RoleContent {
    pub fn from_role(r: &Role) -> Self {
        RoleContent {
            role_id: r.role_id.clone(),
            name: r.name.clone(),
            position: r.position,
            permissions: r.permissions.0,
            scope: r.scope.clone(),
            color: r.color,
            extra: r.extra.clone(),
        }
    }
    pub fn into_role(self) -> Role {
        Role {
            role_id: self.role_id,
            name: self.name,
            position: self.position,
            permissions: Permissions(self.permissions),
            scope: self.scope,
            color: self.color,
            rank: None, display: false, mentionable: false,
            extra: self.extra,
        }
    }
}

/// CORD-04 §2 Grant content (vsk 3), `eid == grant_locator(community_id, member)`.
/// Empty `role_ids` is a revoke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantContent {
    pub member: String,
    #[serde(default)]
    pub role_ids: Vec<String>,
    /// The staff write key riding a staff-making Grant (CORD-04 §3): the
    /// current `control_root` NIP-44-encrypted under the granter↔member
    /// pairwise conversation key, base64 — delivery, never authority. Its
    /// plaintext is fixed-width `epoch_be[8] ‖ control_root[32]`, and the
    /// recipient adopts the secret only if it derives to exactly the
    /// `control_pk` they hold for the named epoch. Opaque pairwise ciphertext
    /// to every other reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_wrap: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Sanity bound on a carried `control_wrap` (a NIP-44 wrap of 40 bytes is
/// ~130 chars); anything over is dropped on read, never carried.
pub const MAX_CONTROL_WRAP_CHARS: usize = 1024;

impl GrantContent {
    pub fn from_grant(g: &MemberGrant) -> Self {
        GrantContent { member: g.member.clone(), role_ids: g.role_ids.clone(), control_wrap: None, extra: serde_json::Map::new() }
    }
    pub fn into_grant(self) -> MemberGrant {
        MemberGrant { member: self.member, role_ids: self.role_ids }
    }
}

/// Serialize a Role's content to its CORD-04 §2 wire JSON (permissions as a string).
pub fn role_content_json(r: &Role) -> Result<String, String> {
    serde_json::to_string(&RoleContent::from_role(r)).map_err(|e| e.to_string())
}

/// Parse a vsk-1 edition's content into a shared `Role`. Accepts permissions as a
/// string or a legacy number.
pub fn parse_role_content(content: &str) -> Option<Role> {
    serde_json::from_str::<RoleContent>(content).ok().map(RoleContent::into_role)
}

/// Serialize a Grant's content to its CORD-04 §2 wire JSON.
pub fn grant_content_json(g: &MemberGrant) -> Result<String, String> {
    serde_json::to_string(&GrantContent::from_grant(g)).map_err(|e| e.to_string())
}

/// [`grant_content_json`] carrying the staff write key (CORD-04 §3).
pub fn grant_content_json_with_wrap(g: &MemberGrant, control_wrap: Option<String>) -> Result<String, String> {
    let mut content = GrantContent::from_grant(g);
    content.control_wrap = control_wrap;
    serde_json::to_string(&content).map_err(|e| e.to_string())
}

/// Parse a vsk-3 edition's content into a shared `MemberGrant`.
pub fn parse_grant_content(content: &str) -> Option<MemberGrant> {
    serde_json::from_str::<GrantContent>(content).ok().map(GrantContent::into_grant)
}

/// Parse a vsk-3 edition's `control_wrap`, if one rides within bounds
/// (CORD-04 §3). Only the grant's own member can open it — every other
/// reader treats it as opaque bytes.
pub fn parse_grant_control_wrap(content: &str) -> Option<String> {
    serde_json::from_str::<GrantContent>(content)
        .ok()?
        .control_wrap
        .filter(|w| !w.is_empty() && w.len() <= MAX_CONTROL_WRAP_CHARS)
}

/// Serialize a banlist to its CORD-04 §4 wire JSON (a flat array of lowercase-hex
/// npubs, replaced entire on every edit).
pub fn banlist_content_json(banned: &[String]) -> Result<String, String> {
    serde_json::to_string(banned).map_err(|e| e.to_string())
}

/// Parse a vsk-4 edition's content into the banned set (lowercase hex). Non-array or
/// malformed content yields `None` (dropped, never a partial ban).
pub fn parse_banlist_content(content: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(content).ok()
}

/// A Role's content byte-fits its cap discipline: name ≤ 64 bytes. (The 100-per-
/// community and 64-per-member caps are fold-side, applied on read.)
pub fn validate_role(r: &Role) -> Result<(), String> {
    if r.name.len() > MAX_ROLE_NAME_BYTES {
        return Err("role name over 64 bytes".to_string());
    }
    // Position 0 is the owner's alone; no Role may claim it (CORD-04 §3).
    if r.position == 0 {
        return Err("position 0 is reserved to the owner".to_string());
    }
    Ok(())
}

/// A Banlist edition fits its ceiling (CORD-04 §4): refuse an over-cap edit rather
/// than publish one a strict reader drops.
pub fn validate_banlist(banned: &[String]) -> Result<(), String> {
    if banned.len() > MAX_BANLIST {
        return Err("banlist over the 500-npub ceiling".to_string());
    }
    Ok(())
}


/// CORD-04 §2 Role content (vsk 13), `eid == role_id`: a rank key in place of a position,
/// no scope, and a terminal deleted form `{ role_id, deleted: true }`. Every field the
/// live form carries is written even at its default; readers default what's absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleV2Content {
    pub role_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    #[serde(default, with = "perm_decimal_string_opt", skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentionable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One vsk-13 edition, read.
#[derive(Debug, Clone, PartialEq)]
pub enum RoleV2 {
    Live(Role),
    /// Terminal: the Role leaves every holder and its slot frees.
    Deleted { role_id: String },
}

/// `@everyone`'s content (vsk 13 at `everyone_locator`): its permissions alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EveryoneContent {
    #[serde(with = "perm_decimal_string")]
    pub permissions: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// What `@everyone` starts with (CORD-04 §3): the chat and voice basics.
pub const EVERYONE_DEFAULT: u64 = Permissions::VIEW_CHANNEL
    | Permissions::SEND_MESSAGES
    | Permissions::SEND_IN_THREADS
    | Permissions::ATTACH_FILES
    | Permissions::ADD_REACTIONS
    | Permissions::READ_MESSAGE_HISTORY
    | Permissions::CONNECT
    | Permissions::SPEAK
    | Permissions::VIDEO;

/// Parse a vsk-13 Role edition. A live Role needs a well-formed rank key; the position is
/// assigned at fold time from the keys in play (`rank::dense_ranks`).
pub fn parse_role_v2_content(content: &str) -> Option<RoleV2> {
    let c: RoleV2Content = serde_json::from_str(content).ok()?;
    if c.deleted == Some(true) {
        return Some(RoleV2::Deleted { role_id: c.role_id });
    }
    let rank = c.rank.filter(|k| crate::community::rank::is_rank_key(k))?;
    Some(RoleV2::Live(Role {
        role_id: c.role_id,
        name: c.name.unwrap_or_default(),
        position: u32::MAX,
        permissions: Permissions(c.permissions.unwrap_or(0)),
        scope: RoleScope::Server,
        color: c.color.unwrap_or(0),
        rank: Some(rank),
        display: c.display.unwrap_or(false),
        mentionable: c.mentionable.unwrap_or(false),
        extra: c.extra,
    }))
}

/// Serialize a live Role as a vsk-13 edition; `rank` must be set.
pub fn role_v2_content_json(r: &Role) -> Result<String, String> {
    let rank = r.rank.clone().filter(|k| crate::community::rank::is_rank_key(k)).ok_or("a Role needs a rank key")?;
    let c = RoleV2Content {
        role_id: r.role_id.clone(),
        name: Some(r.name.clone()),
        rank: Some(rank),
        permissions: Some(r.permissions.0),
        color: Some(r.color),
        display: Some(r.display),
        mentionable: Some(r.mentionable),
        deleted: None,
        extra: r.extra.clone(),
    };
    serde_json::to_string(&c).map_err(|e| e.to_string())
}

/// The terminal deletion edition of a Role.
pub fn role_v2_deleted_json(role_id: &str) -> String {
    serde_json::json!({ "role_id": role_id, "deleted": true }).to_string()
}

/// Parse `@everyone`'s content.
pub fn parse_everyone_content(content: &str) -> Option<u64> {
    serde_json::from_str::<EveryoneContent>(content).ok().map(|c| c.permissions)
}

pub fn everyone_content_json(permissions: u64) -> String {
    serde_json::to_string(&EveryoneContent { permissions, extra: Default::default() }).expect("plain struct")
}

mod perm_decimal_string_opt {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => super::perm_decimal_string::serialize(v, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        super::perm_decimal_string::deserialize(d).map(Some)
    }
}

/// One sealed standing in a Tenure (CORD-04 §9): the listed editions are judged as if their
/// author held `permissions` through a Role at `rank`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenureEntry {
    pub rank: String,
    #[serde(with = "perm_decimal_string")]
    pub permissions: u64,
    #[serde(default)]
    pub heads: Vec<String>,
}

/// A member's Tenure (vsk 16 at `tenure_locator(community_id, member)`), replaced entire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenureContent {
    pub member: String,
    #[serde(default)]
    pub sealed: Vec<TenureEntry>,
}

/// The most entries a Tenure carries, and heads an entry lists: bounded by a member's own
/// standing heads in practice, capped so a hostile writer can't bloat every fold.
pub const MAX_TENURE_ENTRIES: usize = 16;
pub const MAX_TENURE_HEADS: usize = 512;

/// Parse a Tenure; entries with a malformed rank key are dropped, and oversize lists cut.
pub fn parse_tenure_content(content: &str) -> Option<TenureContent> {
    let mut t: TenureContent = serde_json::from_str(content).ok()?;
    t.sealed.retain(|e| crate::community::rank::is_rank_key(&e.rank));
    t.sealed.truncate(MAX_TENURE_ENTRIES);
    for e in &mut t.sealed {
        e.heads.retain(|h| crate::simd::hex::hex_to_bytes_32_checked(h).is_some());
        e.heads.truncate(MAX_TENURE_HEADS);
    }
    Some(t)
}

/// The most heads a writer puts in one Tenure, across its entries: about 35 KB of hashes,
/// inside what relays take once the edition is sealed and wrapped.
pub const TENURE_HEAD_BUDGET: usize = 512;

/// A Tenure's entries once `entry` is added, or `None` when it adds nothing. Older entries
/// keep only the heads that are still the member's (`current`), since a superseded edition
/// no longer decides anything; the oldest go first when the budget runs out.
pub fn tenure_with(prior: Vec<TenureEntry>, entry: TenureEntry, current: &[[u8; 32]]) -> Option<Vec<TenureEntry>> {
    let current: std::collections::HashSet<String> = current.iter().map(crate::simd::hex::bytes_to_hex_32).collect();
    let mut sealed: Vec<TenureEntry> = prior
        .into_iter()
        .map(|mut e| {
            e.heads.retain(|h| current.contains(h));
            e
        })
        .filter(|e| !e.heads.is_empty())
        .collect();
    if sealed.iter().any(|e| e.rank == entry.rank && e.permissions & entry.permissions == entry.permissions && entry.heads.iter().all(|h| e.heads.contains(h))) {
        return None;
    }
    sealed.push(entry);
    let mut total: usize = sealed.iter().map(|e| e.heads.len()).sum();
    while sealed.len() > MAX_TENURE_ENTRIES || (total > TENURE_HEAD_BUDGET && sealed.len() > 1) {
        total -= sealed.remove(0).heads.len();
    }
    Some(sealed)
}

pub fn tenure_content_json(t: &TenureContent) -> Result<String, String> {
    serde_json::to_string(t).map_err(|e| e.to_string())
}

/// CORD-04 §3 permissions: a decimal string on the wire, accepting either a string or
/// a legacy bare number on read, and always writing the string. Digit-only (a leading
/// `+`/`-` or whitespace is rejected, matching the strict `ms`/`ev` discipline).
mod perm_decimal_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrNumber {
            S(String),
            N(u64),
        }
        match StringOrNumber::deserialize(d)? {
            StringOrNumber::N(n) => Ok(n),
            StringOrNumber::S(s) => {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(serde::de::Error::custom("permissions must be a decimal string"));
                }
                s.parse::<u64>().map_err(serde::de::Error::custom)
            }
        }
    }
}

#[cfg(test)]
mod tests {

    fn te(rank: &str, bits: u64, heads: &[u8]) -> TenureEntry {
        TenureEntry { rank: rank.into(), permissions: bits, heads: heads.iter().map(|b| crate::simd::hex::bytes_to_hex_32(&[*b; 32])).collect() }
    }

    #[test]
    fn a_tenure_keeps_live_heads_and_skips_what_it_already_seals() {
        let live: Vec<[u8; 32]> = [1u8, 2, 3].iter().map(|b| [*b; 32]).collect();
        // A superseded head (9) leaves the older entry; an emptied entry goes.
        let got = tenure_with(vec![te("C", 7, &[1, 9]), te("D", 1, &[9])], te("F", 3, &[2, 3]), &live).unwrap();
        assert_eq!(got, vec![te("C", 7, &[1]), te("F", 3, &[2, 3])]);
        // Already sealed at that rank with those bits: nothing to publish.
        assert_eq!(tenure_with(got.clone(), te("F", 1, &[2]), &live), None);
        assert!(tenure_with(got, te("F", 8, &[2]), &live).is_some(), "a bit not yet sealed is new");
    }

    #[test]
    fn a_tenure_drops_its_oldest_entries_past_the_caps() {
        let live: Vec<[u8; 32]> = (0..=255u8).map(|b| [b; 32]).collect();
        let prior: Vec<TenureEntry> = (0..MAX_TENURE_ENTRIES as u8).map(|i| te("C", 1 << i, &[i])).collect();
        let got = tenure_with(prior, te("F", 1, &[200]), &live).unwrap();
        assert_eq!(got.len(), MAX_TENURE_ENTRIES);
        assert_eq!((got[0].permissions, got.last().unwrap().rank.as_str()), (2, "F"), "the oldest went");
        let wide = TenureEntry { rank: "C".into(), permissions: 1, heads: (0..TENURE_HEAD_BUDGET).map(|i| format!("{i:064x}")).collect() };
        let got = tenure_with(vec![te("B", 1, &[1])], wide, &[[1; 32]]).unwrap();
        assert_eq!(got.len(), 1, "an entry past the head budget pushes the older one out");
    }


    #[test]
    fn a_role_republished_here_keeps_what_another_client_wrote() {
        // Armada's hoist flag and a field nobody has invented yet: neither is modelled
        // here, and an edit must not erase either for every member (CORD-02 §6).
        let wire = r#"{"role_id":"ab","name":"Mods","position":3,"permissions":"24","scope":{"kind":"server"},"color":0,"display":true,"future":{"x":1}}"#;
        let mut role = parse_role_content(wire).expect("parses");
        role.name = "Moderators".into();
        role.color = 0x59fcb3;
        let back: serde_json::Value = serde_json::from_str(&role_content_json(&role).unwrap()).unwrap();
        assert_eq!(back["display"], serde_json::json!(true), "the hoist flag survives the edit");
        assert_eq!(back["future"], serde_json::json!({"x": 1}), "so does anything else unknown");
        assert_eq!(back["name"], "Moderators");
        assert_eq!(back["permissions"], "24", "still a decimal string");
    }

    #[test]
    fn a_role_with_nothing_extra_serializes_as_before() {
        let role = parse_role_content(r#"{"role_id":"ab","name":"A","position":1,"permissions":"1","scope":{"kind":"server"},"color":0}"#).unwrap();
        assert!(role.extra.is_empty());
        let stored = serde_json::to_string(&role).unwrap();
        assert!(!stored.contains("extra"), "stored rosters stay byte-identical when nothing is carried");
    }

    use super::*;

    fn role(perms: u64, pos: u32) -> Role {
        Role {
            role_id: "aa".repeat(32),
            name: "Moderator".into(),
            position: pos,
            permissions: Permissions(perms),
            scope: RoleScope::Server,
            color: 15158332, rank: None, display: false, mentionable: false, extra: Default::default(),
        }
    }

    #[test]
    fn role_permissions_ride_as_a_decimal_string() {
        // CORD-04 §3 canonical example: 1<<3 KICK | 1<<5 MANAGE_MESSAGES = 40.
        let json = role_content_json(&role(40, 2)).unwrap();
        assert!(json.contains("\"permissions\":\"40\""), "permissions is a decimal string, not a number: {json}");
        assert!(!json.contains("\"permissions\":40"), "must not emit a bare number");
    }

    #[test]
    fn role_round_trips_through_the_wire_form() {
        let r = role(Permissions::ADMIN_ALL, 1);
        let back = parse_role_content(&role_content_json(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn a_legacy_numeric_permissions_is_still_read() {
        // A reader MUST accept a bare number from an older edition (§3).
        let legacy = r#"{"role_id":"bb","name":"X","position":3,"permissions":40,"scope":{"kind":"server"},"color":0}"#;
        assert_eq!(parse_role_content(legacy).unwrap().permissions, Permissions(40));
    }

    #[test]
    fn a_non_digit_permissions_string_is_rejected() {
        for bad in ["\"+40\"", "\"4a\"", "\"\"", "\" 40\""] {
            let j = format!(r#"{{"role_id":"cc","name":"X","position":2,"permissions":{bad},"scope":{{"kind":"server"}}}}"#);
            assert!(parse_role_content(&j).is_none(), "rejected non-digit permissions {bad}");
        }
    }

    #[test]
    fn channel_scope_round_trips() {
        let mut r = role(8, 2);
        r.scope = RoleScope::Channel("cc".repeat(32));
        let json = role_content_json(&r).unwrap();
        assert!(json.contains(r#""scope":{"kind":"channel","channel_id":""#), "{json}");
        assert_eq!(parse_role_content(&json).unwrap().scope, r.scope);
    }

    #[test]
    fn grant_round_trips_and_empty_is_a_revoke() {
        let g = MemberGrant { member: "dd".repeat(32), role_ids: vec!["aa".repeat(32)] };
        assert_eq!(parse_grant_content(&grant_content_json(&g).unwrap()).unwrap(), g);
        let revoke = MemberGrant { member: "ee".repeat(32), role_ids: vec![] };
        let back = parse_grant_content(&grant_content_json(&revoke).unwrap()).unwrap();
        assert!(back.role_ids.is_empty());
    }

    #[test]
    fn banlist_round_trips() {
        let banned = vec!["11".repeat(32), "22".repeat(32)];
        assert_eq!(parse_banlist_content(&banlist_content_json(&banned).unwrap()).unwrap(), banned);
        assert!(parse_banlist_content("not-an-array").is_none());
    }

    #[test]
    fn unknown_role_fields_round_trip() {
        let j = r#"{"role_id":"ab","name":"X","position":2,"permissions":"8","scope":{"kind":"server"},"color":0,"future":"keep"}"#;
        let parsed: RoleContent = serde_json::from_str(j).unwrap();
        let reser = serde_json::to_string(&parsed).unwrap();
        assert!(reser.contains("\"future\":\"keep\""), "unknown fields survive: {reser}");
    }

    #[test]
    fn validate_rejects_pos_zero_and_long_name() {
        assert!(validate_role(&role(8, 0)).is_err(), "position 0 reserved to owner");
        let mut long = role(8, 2);
        long.name = "x".repeat(65);
        assert!(validate_role(&long).is_err());
        assert!(validate_banlist(&vec!["z".into(); MAX_BANLIST + 1]).is_err());
    }
}
