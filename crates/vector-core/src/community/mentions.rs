//! Role mentions (CORD-03 §3, CORD-04 §8): a `["role", "<role_id>"]` tag beside an `@Name`
//! in the text. The text stays readable on clients that don't know the tag.

use nostr_sdk::prelude::Tag;

use super::roles::{CommunityRoles, Permissions};

/// The most Role tags a message carries; more are dropped.
pub const MAX_ROLE_MENTIONS: usize = 16;

/// What a composer writes for a picked Role. Send swaps it for `@Name` and a tag.
pub const ROLE_TOKEN: &str = "@role:";

const TAG_ROLE: &str = "role";

pub fn is_role_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn collect<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values.filter(|v| is_role_id(v)) {
        if out.len() == MAX_ROLE_MENTIONS {
            break;
        }
        if !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
    }
    out
}

/// The Roles a rumor mentions: well-formed ids, once each, capped.
pub fn from_tags<'a, I: IntoIterator<Item = &'a Tag>>(tags: I) -> Vec<String> {
    collect(tags.into_iter().filter_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == TAG_ROLE).then(|| s[1].as_str())
    }))
}

/// [`from_tags`] over a stored event's tags.
pub fn from_stored(tags: &[Vec<String>]) -> Vec<String> {
    collect(tags.iter().filter(|t| t.len() >= 2 && t[0] == TAG_ROLE).map(|t| t[1].as_str()))
}

pub fn to_tags(ids: &[String]) -> impl Iterator<Item = Tag> + '_ {
    ids.iter().map(|id| Tag::custom(TAG_ROLE, [id.clone()]))
}

pub fn to_stored(ids: &[String]) -> impl Iterator<Item = Vec<String>> + '_ {
    ids.iter().map(|id| vec![TAG_ROLE.to_string(), id.clone()])
}

/// `content` with each `@role:<id>` naming a Role of `roster` written as `@Name`, and the
/// ids it mentions. A token for no known Role stays as typed.
pub fn resolve_tokens(content: &str, roster: &CommunityRoles) -> (String, Vec<String>) {
    if !content.contains(ROLE_TOKEN) {
        return (content.to_string(), Vec::new());
    }
    let mut out = String::with_capacity(content.len());
    let mut ids: Vec<String> = Vec::new();
    let mut rest = content;
    while let Some(at) = rest.find(ROLE_TOKEN) {
        let after = &rest[at + ROLE_TOKEN.len()..];
        let role = after.get(..64).filter(|id| is_role_id(id)).and_then(|id| roster.role(id));
        out.push_str(&rest[..at]);
        match role {
            Some(r) => {
                out.push('@');
                out.push_str(&r.name);
                if ids.len() < MAX_ROLE_MENTIONS && !ids.contains(&r.role_id) {
                    ids.push(r.role_id.clone());
                }
                rest = &after[64..];
            }
            None => {
                out.push_str(ROLE_TOKEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, ids)
}

/// [`resolve_tokens`] against a community's Roster as this device holds it.
pub fn resolve_in(community_id: &str, content: &str) -> (String, Vec<String>) {
    if !content.contains(ROLE_TOKEN) {
        return (content.to_string(), Vec::new());
    }
    match crate::db::community::get_community_roles(community_id) {
        Ok(roster) => resolve_tokens(content, &roster),
        Err(_) => (content.to_string(), Vec::new()),
    }
}

/// Whether `author`'s mention of `role_id` stands: the Role is mentionable, or the author
/// may mention everyone. One that doesn't renders as plain text and pings nobody.
pub fn stands(roster: &CommunityRoles, owner_hex: Option<&str>, author_hex: &str, role_id: &str) -> bool {
    roster.role(role_id).is_some_and(|r| {
        r.mentionable || roster.is_authorized(author_hex, owner_hex, Permissions::MENTION_EVERYONE)
    })
}

/// Whether a message's Role mentions ping `reader_hex`: one names a Role they hold, in its
/// text as well as its tags, and stands.
pub fn pings(
    roster: &CommunityRoles,
    owner_hex: Option<&str>,
    author_hex: &str,
    reader_hex: &str,
    content: &str,
    role_ids: &[String],
) -> bool {
    role_ids.iter().any(|id| {
        roster.roles_of(reader_hex).any(|r| r.role_id == *id && content.contains(&format!("@{}", r.name)))
            && stands(roster, owner_hex, author_hex, id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::roles::{MemberGrant, Role, RoleScope};

    fn role(id: &str, name: &str, mentionable: bool, permissions: u64) -> Role {
        Role {
            role_id: id.into(),
            name: name.into(),
            position: 1,
            permissions: Permissions(permissions),
            scope: RoleScope::Server,
            color: 0,
            rank: None,
            display: false,
            mentionable,
            extra: Default::default(),
        }
    }

    fn roster() -> CommunityRoles {
        CommunityRoles {
            roles: vec![
                role(&"a".repeat(64), "Raid Team", true, 0),
                role(&"b".repeat(64), "Staff", false, 0),
                role(&"c".repeat(64), "Pingers", false, Permissions::MENTION_EVERYONE),
            ],
            grants: vec![
                MemberGrant { member: "reader".into(), role_ids: vec!["a".repeat(64), "b".repeat(64)] },
                MemberGrant { member: "pinger".into(), role_ids: vec!["c".repeat(64)] },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn tokens_resolve_to_names_and_ids_once_each() {
        let r = roster();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let text = format!("{ROLE_TOKEN}{a} and {ROLE_TOKEN}{b}, again {ROLE_TOKEN}{a}!");
        assert_eq!(resolve_tokens(&text, &r), ("@Raid Team and @Staff, again @Raid Team!".into(), vec![a.clone(), b]));
        let unknown = format!("hi {ROLE_TOKEN}{} {ROLE_TOKEN}short", "d".repeat(64));
        assert_eq!(resolve_tokens(&unknown, &r), (unknown.clone(), vec![]), "an unknown or short token stays as typed");
        let upper = format!("{ROLE_TOKEN}{}", "A".repeat(64));
        assert_eq!(resolve_tokens(&upper, &r).1, Vec::<String>::new());
        assert_eq!(resolve_tokens("plain 🎉 text", &r), ("plain 🎉 text".into(), vec![]));
        // A multi-byte char where an id would end is not a slice boundary.
        let cut = format!("{ROLE_TOKEN}{}é", "a".repeat(63));
        assert_eq!(resolve_tokens(&cut, &r), (cut.clone(), vec![]));
    }

    #[test]
    fn tags_round_trip_deduped_and_capped() {
        let ids: Vec<String> = (0..20).map(|i| format!("{i:064x}")).collect();
        let tags: Vec<Tag> = to_tags(&ids).chain(to_tags(&ids[..2])).collect();
        let back = from_tags(tags.iter());
        assert_eq!(back, ids[..MAX_ROLE_MENTIONS].to_vec());
        let stored: Vec<Vec<String>> = to_stored(&ids[..3]).chain([vec!["role".into(), "nothex".into()]]).collect();
        assert_eq!(from_stored(&stored), ids[..3].to_vec());
    }

    #[test]
    fn a_mention_pings_holders_when_mentionable_or_its_author_may_ping_everyone() {
        let r = roster();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let text = "@Raid Team and @Staff";
        assert!(pings(&r, Some("owner"), "member", "reader", text, &[a.clone()]), "mentionable");
        assert!(!pings(&r, Some("owner"), "member", "reader", "hi", &[a.clone()]), "a tag the text doesn't name");
        assert!(!pings(&r, Some("owner"), "member", "reader", text, &[b.clone()]), "not mentionable, plain author");
        assert!(pings(&r, Some("owner"), "pinger", "reader", text, &[b.clone()]), "MENTION_EVERYONE reaches any Role");
        assert!(pings(&r, Some("owner"), "owner", "reader", text, &[b.clone()]), "the owner may mention anything");
        assert!(!pings(&r, Some("owner"), "pinger", "member", text, &[a.clone(), b.clone()]), "a Role the reader doesn't hold");
        assert!(!pings(&r, Some("owner"), "pinger", "reader", text, &["d".repeat(64)]), "a Role that doesn't exist");
        let mut open = r.clone();
        open.everyone = Some(Permissions(Permissions::MENTION_EVERYONE));
        assert!(pings(&open, Some("owner"), "member", "reader", text, &[b]), "@everyone holding the bit lets anyone");
    }
}
