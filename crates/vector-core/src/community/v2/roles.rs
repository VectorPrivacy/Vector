//! CORD-04 Roles: v2 wire content for the Role (vsk 1), Grant (vsk 3), and Banlist
//! (vsk 4) control entities.
//!
//! The authority ALGEBRA is shared, not copied: these types convert to/from
//! [`crate::community::roles`]'s `Role`/`MemberGrant`/`Permissions`, and the fold
//! feeds the shared `authorize_delegation` + `is_authorized`. Only the SERIALIZATION
//! is v2-native, because CORD-04 §3 rides `permissions` as a decimal STRING (the
//! shared `Permissions` serializes as a bare `u64` for v1's storage), and a reader
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
            extra: serde_json::Map::new(),
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
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl GrantContent {
    pub fn from_grant(g: &MemberGrant) -> Self {
        GrantContent { member: g.member.clone(), role_ids: g.role_ids.clone(), extra: serde_json::Map::new() }
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

/// Parse a vsk-3 edition's content into a shared `MemberGrant`.
pub fn parse_grant_content(content: &str) -> Option<MemberGrant> {
    serde_json::from_str::<GrantContent>(content).ok().map(GrantContent::into_grant)
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
    use super::*;

    fn role(perms: u64, pos: u32) -> Role {
        Role {
            role_id: "aa".repeat(32),
            name: "Moderator".into(),
            position: pos,
            permissions: Permissions(perms),
            scope: RoleScope::Server,
            color: 15158332,
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
