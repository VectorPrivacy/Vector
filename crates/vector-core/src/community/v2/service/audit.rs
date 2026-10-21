//! The Audit Log: every Control chain this device accepted, read back as who did what and when
//! (CORD-04 §1), beside the Guestbook's moderation and the acts this device saw that leave no
//! edition behind.

use super::*;
use crate::community::roles::{OverwriteTarget, Permissions};
use nostr_sdk::prelude::ToBech32;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// One act in the log.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub id: String,
    /// Milliseconds, as its author dated it.
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Value>,
    /// What stood before this act was compacted away, so what it changed can't be told.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct AuditLog {
    /// Newest first.
    pub entries: Vec<AuditEntry>,
    /// History from before a key rotation this device didn't live through is missing.
    pub compacted: bool,
}

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

fn npub(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

fn npub_of(hex: &str) -> String {
    PublicKey::from_hex(hex).map(|p| npub(&p)).unwrap_or_else(|_| hex.to_string())
}

fn member(hex: &str) -> Value {
    json!({ "kind": "member", "npub": npub_of(hex) })
}

/// Bit positions set in `v`.
fn bits(v: u64) -> Vec<u32> {
    (0..64).filter(|i| v >> i & 1 == 1).collect()
}

fn bit_change(field: &str, old: u64, new: u64) -> Option<Value> {
    let (added, removed) = (new & !old, old & !new);
    (added | removed != 0).then(|| json!({ "field": field, "added": bits(added), "removed": bits(removed) }))
}

fn text_change(field: &str, old: Option<&str>, new: Option<&str>) -> Option<Value> {
    (old != new).then(|| json!({ "field": field, "old": old, "new": new }))
}

/// What each id was last called, gathered from the chains themselves so a deleted Role or
/// Channel keeps its name in the log.
#[derive(Default)]
struct Names {
    roles: HashMap<String, String>,
    channels: HashMap<String, (String, String)>,
    categories: HashMap<String, String>,
    everyone: String,
}

impl Names {
    fn role(&self, id: &str) -> Value {
        if id == self.everyone {
            return json!({ "kind": "everyone" });
        }
        json!({ "kind": "role", "id": id, "name": self.roles.get(id) })
    }

    fn channel(&self, id: &str) -> Value {
        let (name, view) = self.channels.get(id).cloned().unwrap_or_default();
        json!({ "kind": "channel", "id": id, "name": (!name.is_empty()).then_some(name), "view": view })
    }

    fn category(&self, id: &str) -> Value {
        json!({ "kind": "category", "id": id, "name": self.categories.get(id) })
    }

    /// A Channel or a Category: an overwrite or a sync names either.
    fn scope(&self, id: &str) -> Value {
        if self.categories.contains_key(id) {
            self.category(id)
        } else {
            self.channel(id)
        }
    }

    fn overwrite_target(&self, t: &OverwriteTarget) -> Value {
        match t {
            OverwriteTarget::Everyone => json!({ "kind": "everyone" }),
            OverwriteTarget::Role(id) => self.role(id),
            OverwriteTarget::Member(id) => member(id),
        }
    }
}

/// How this device reads one edition's content, abstracted so the log builds without keys.
pub(crate) struct Readers<'a> {
    /// A Channel edition's metadata, sealed or plain; `None` when it can't be opened.
    pub channel: &'a dyn Fn(&ParsedEdition) -> Option<control::ChannelMetadata>,
    /// A Pin List edition's pins as `(message id, author hex)`; `None` when it can't be opened.
    pub pins: &'a dyn Fn(&ParsedEdition, &str) -> Option<Vec<(String, String)>>,
}

/// Each accepted entity's chain, oldest first: walked back from the head this device settled
/// on, so a fork or an unauthorized edition never reads as something that happened.
fn chains<'e>(editions: &'e [ParsedEdition], heads: &HashMap<[u8; 32], [u8; 32]>) -> Vec<Vec<&'e ParsedEdition>> {
    let by_hash: HashMap<[u8; 32], &ParsedEdition> = editions.iter().map(|e| (e.self_hash, e)).collect();
    let mut out = Vec::new();
    for (entity, head) in heads {
        let Some(mut cur) = by_hash.get(head).copied().filter(|e| e.entity_id == *entity) else { continue };
        let mut chain = vec![cur];
        while let Some(prev) = cur.prev_hash.and_then(|p| by_hash.get(&p).copied()) {
            if prev.entity_id != cur.entity_id || prev.version >= cur.version || chain.len() > 4096 {
                break;
            }
            chain.push(prev);
            cur = prev;
        }
        chain.reverse();
        out.push(chain);
    }
    out
}

fn str_extra<'m>(m: &'m serde_json::Map<String, Value>, key: &str) -> Option<&'m str> {
    m.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn view_of(m: &control::ChannelMetadata) -> String {
    match str_extra(&m.extra, "view") {
        Some("voice") => "voice",
        Some("forum") => "forum",
        Some(_) => "chat",
        None if m.voice == Some(true) => "voice",
        None => "chat",
    }
    .to_string()
}

fn relay_list(m: &control::CommunityMetadata, voice: bool) -> Vec<String> {
    if voice {
        return m.extra.get("voice_relays").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|u| u.as_str().map(str::to_string)).collect()).unwrap_or_default();
    }
    m.relays.clone()
}

fn list_change(field: &str, old: &[String], new: &[String]) -> Option<Value> {
    let added: Vec<&String> = new.iter().filter(|r| !old.contains(r)).collect();
    let removed: Vec<&String> = old.iter().filter(|r| !new.contains(r)).collect();
    (!added.is_empty() || !removed.is_empty()).then(|| json!({ "field": field, "added": added, "removed": removed }))
}

fn image_change(field: &str, old: Option<&control::ImageRef>, new: Option<&control::ImageRef>) -> Option<Value> {
    let how = match (old, new) {
        (None, Some(_)) => "added",
        (Some(_), None) => "removed",
        (Some(a), Some(b)) if a.hash != b.hash || a.url != b.url => "changed",
        _ => return None,
    };
    Some(json!({ "field": field, "new": how }))
}

/// The log's Control half: one entry per accepted edition.
pub(crate) fn control_entries(
    community_id: &crate::community::CommunityId,
    editions: &[ParsedEdition],
    heads: &HashMap<[u8; 32], [u8; 32]>,
    known_channels: &[String],
    readers: &Readers,
) -> (Vec<AuditEntry>, NamesView) {
    let chains = chains(editions, heads);
    let everyone = super::super::derive::everyone_locator(community_id);
    let mut names = Names { everyone: hex(&everyone), ..Default::default() };

    // Names first, so an entry made before a rename reads with the name it carries now.
    for chain in &chains {
        for e in chain {
            match e.vsk.as_str() {
                vsk::ROLE_V2 if e.entity_id != everyone => {
                    if let Some(super::super::roles::RoleV2::Live(r)) = super::super::roles::parse_role_v2_content(&e.content) {
                        names.roles.insert(hex(&e.entity_id), r.name);
                    }
                }
                vsk::ROLE => {
                    if let Some(r) = super::super::roles::parse_role_content(&e.content) {
                        names.roles.insert(r.role_id.to_ascii_lowercase(), r.name);
                    }
                }
                vsk::CHANNEL_METADATA => {
                    if let Some(m) = (readers.channel)(e).filter(|m| !m.name.is_empty()) {
                        names.channels.insert(hex(&e.entity_id), (m.name.clone(), view_of(&m)));
                    }
                }
                vsk::CATEGORY => {
                    if let Ok(m) = serde_json::from_str::<control::CategoryMetadata>(&e.content) {
                        if !m.name.is_empty() {
                            names.categories.insert(hex(&e.entity_id), m.name);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // A Pin List's coordinate names its Channel only through the derivation.
    let mut channel_ids: HashSet<String> = known_channels.iter().cloned().collect();
    channel_ids.extend(names.channels.keys().cloned());
    let pins_channel: HashMap<[u8; 32], String> = channel_ids
        .iter()
        .filter_map(|c| crate::simd::hex::hex_to_bytes_32_checked(c).map(|b| (super::super::derive::pins_locator(community_id, &ChannelId(b)), c.clone())))
        .collect();

    let mut out = Vec::new();
    for chain in &chains {
        for (i, e) in chain.iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| chain[p]);
            // A first edition is a creation only when it names no predecessor.
            let genesis = prev.is_none() && e.prev_hash.is_none();
            let partial = prev.is_none() && !genesis;
            let base = |action: &'static str, target: Option<Value>, changes: Vec<Value>| AuditEntry {
                id: hex(&e.self_hash),
                at: e.created_at.saturating_mul(1000),
                actor: Some(npub(&e.author)),
                action,
                target,
                changes,
                partial,
            };
            let eid = hex(&e.entity_id);
            match e.vsk.as_str() {
                vsk::COMMUNITY_METADATA => {
                    let Ok(new) = serde_json::from_str::<control::CommunityMetadata>(&e.content) else { continue };
                    if genesis {
                        out.push(base("community_create", None, vec![json!({ "field": "name", "new": new.name })]));
                        continue;
                    }
                    let old = prev.and_then(|p| serde_json::from_str::<control::CommunityMetadata>(&p.content).ok());
                    let changes = match &old {
                        Some(old) => [
                            text_change("name", Some(&old.name), Some(&new.name)),
                            text_change("description", old.description.as_deref(), new.description.as_deref()),
                            image_change("icon", old.icon.as_ref(), new.icon.as_ref()),
                            image_change("banner", old.banner.as_ref(), new.banner.as_ref()),
                            list_change("relays", &relay_list(old, false), &relay_list(&new, false)),
                            list_change("voice_relays", &relay_list(old, true), &relay_list(&new, true)),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                        None => Vec::new(),
                    };
                    out.push(base("community_update", None, changes));
                }
                vsk::ROLE_V2 if e.entity_id == everyone => {
                    let Some((perms, _)) = super::super::roles::parse_everyone_content(&e.content) else { continue };
                    if genesis {
                        out.push(base("roles_activate", Some(json!({ "kind": "everyone" })), Vec::new()));
                        continue;
                    }
                    let old = prev.and_then(|p| super::super::roles::parse_everyone_content(&p.content)).map(|o| o.0);
                    let changes = old.and_then(|o| bit_change("permissions", o, perms)).into_iter().collect();
                    out.push(base("everyone_update", Some(json!({ "kind": "everyone" })), changes));
                }
                vsk::ROLE_V2 => {
                    use super::super::roles::RoleV2;
                    let target = Some(names.role(&eid));
                    match super::super::roles::parse_role_v2_content(&e.content) {
                        Some(RoleV2::Deleted { .. }) => out.push(base("role_delete", target, Vec::new())),
                        Some(RoleV2::Live(new)) => {
                            let old = prev.and_then(|p| match super::super::roles::parse_role_v2_content(&p.content) {
                                Some(RoleV2::Live(r)) => Some(r),
                                _ => None,
                            });
                            if genesis {
                                out.push(base("role_create", target, role_changes(None, &new)));
                            } else {
                                out.push(base("role_update", target, old.map(|o| role_changes(Some(&o), &new)).unwrap_or_default()));
                            }
                        }
                        None => {}
                    }
                }
                vsk::ROLE => {
                    let Some(new) = super::super::roles::parse_role_content(&e.content) else { continue };
                    let target = Some(names.role(&new.role_id.to_ascii_lowercase()));
                    let old = prev.and_then(|p| super::super::roles::parse_role_content(&p.content));
                    if genesis {
                        out.push(base("role_create", target, role_changes(None, &new)));
                    } else {
                        out.push(base("role_update", target, old.map(|o| role_changes(Some(&o), &new)).unwrap_or_default()));
                    }
                }
                vsk::GRANT => {
                    let Some(new) = super::super::roles::parse_grant_content(&e.content) else { continue };
                    let target = Some(member(&new.member.to_ascii_lowercase()));
                    let lower = |ids: &[String]| ids.iter().map(|r| r.to_ascii_lowercase()).collect::<Vec<_>>();
                    let now = lower(&new.role_ids);
                    let before = match prev.and_then(|p| super::super::roles::parse_grant_content(&p.content)) {
                        Some(g) => lower(&g.role_ids),
                        None if genesis => Vec::new(),
                        // Compacted: what they hold, without what changed.
                        None => {
                            let held: Vec<Value> = now.iter().map(|r| names.role(r)).collect();
                            out.push(base("member_roles_update", target, vec![json!({ "field": "roles", "held": held })]));
                            continue;
                        }
                    };
                    let added: Vec<Value> = now.iter().filter(|r| !before.contains(r)).map(|r| names.role(r)).collect();
                    let removed: Vec<Value> = before.iter().filter(|r| !now.contains(r)).map(|r| names.role(r)).collect();
                    // A Grant re-published only to carry the staff key changes nobody's roles.
                    if added.is_empty() && removed.is_empty() {
                        continue;
                    }
                    out.push(base("member_roles_update", target, vec![json!({ "field": "roles", "added": added, "removed": removed })]));
                }
                vsk::BANLIST => {
                    let Some(now) = super::super::roles::parse_banlist_content(&e.content) else { continue };
                    let before = match prev.and_then(|p| super::super::roles::parse_banlist_content(&p.content)) {
                        Some(b) => b,
                        None if genesis => Vec::new(),
                        None => {
                            let held: Vec<Value> = now.iter().map(|h| member(h)).collect();
                            out.push(base("banlist_update", None, vec![json!({ "field": "banned", "held": held })]));
                            continue;
                        }
                    };
                    for (n, h) in now.iter().filter(|h| !before.contains(h)).enumerate() {
                        out.push(AuditEntry { id: format!("{}:b{n}", hex(&e.self_hash)), ..base("member_ban", Some(member(h)), Vec::new()) });
                    }
                    for (n, h) in before.iter().filter(|h| !now.contains(h)).enumerate() {
                        out.push(AuditEntry { id: format!("{}:u{n}", hex(&e.self_hash)), ..base("member_unban", Some(member(h)), Vec::new()) });
                    }
                }
                vsk::CHANNEL_METADATA => {
                    let target = Some(names.channel(&eid));
                    let new = (readers.channel)(e);
                    if new.as_ref().is_some_and(|m| m.deleted == Some(true)) || control::parse_sealed_channel_meta(&e.content).is_some_and(|s| s.deleted) {
                        out.push(base("channel_delete", target, Vec::new()));
                        continue;
                    }
                    let Some(new) = new else {
                        // A Private Channel sealed under a key this device doesn't hold.
                        out.push(base(if genesis { "channel_create" } else { "channel_update" }, target, Vec::new()));
                        continue;
                    };
                    if genesis {
                        let mut changes = vec![json!({ "field": "view", "new": view_of(&new) })];
                        if new.private {
                            changes.push(json!({ "field": "private", "new": true }));
                        }
                        out.push(base("channel_create", target, changes));
                        continue;
                    }
                    let old = prev.and_then(|p| (readers.channel)(p));
                    let changes = old.map(|old| channel_changes(&old, &new, &names)).unwrap_or_default();
                    out.push(base("channel_update", target, changes));
                }
                vsk::CATEGORY => {
                    let Ok(new) = serde_json::from_str::<control::CategoryMetadata>(&e.content) else { continue };
                    let target = Some(names.category(&eid));
                    if new.deleted == Some(true) {
                        out.push(base("category_delete", target, Vec::new()));
                    } else if genesis {
                        out.push(base("category_create", target, Vec::new()));
                    } else {
                        let old = prev.and_then(|p| serde_json::from_str::<control::CategoryMetadata>(&p.content).ok());
                        let changes = old
                            .map(|old| {
                                let mut c: Vec<Value> = text_change("name", Some(&old.name), Some(&new.name)).into_iter().collect();
                                if old.position != new.position {
                                    c.push(json!({ "field": "position" }));
                                }
                                c
                            })
                            .unwrap_or_default();
                        out.push(base("category_update", target, changes));
                    }
                }
                vsk::CHANNEL_ACL => {
                    let Some(new) = super::super::roles::parse_overwrite_content(&e.content) else { continue };
                    let target = Some(json!({ "kind": "overwrite", "for": names.overwrite_target(&new.target), "in": names.scope(&new.scope) }));
                    let old = prev.and_then(|p| super::super::roles::parse_overwrite_content(&p.content));
                    let (oa, od) = old.as_ref().map_or((0, 0), |o| (o.allow.0, o.deny.0));
                    let (na, nd) = (new.allow.0, new.deny.0);
                    let changes: Vec<Value> = [bit_change("allow", oa, na), bit_change("deny", od, nd)].into_iter().flatten().collect();
                    let action = if na == 0 && nd == 0 {
                        "overwrite_delete"
                    } else if genesis || old.as_ref().is_some_and(|o| o.allow.0 == 0 && o.deny.0 == 0) {
                        "overwrite_create"
                    } else {
                        "overwrite_update"
                    };
                    if !partial && changes.is_empty() {
                        continue;
                    }
                    out.push(base(action, target, if partial { Vec::new() } else { changes }));
                }
                vsk::CHANNEL_SYNC => {
                    let Some((channel, category)) = super::super::roles::parse_sync_content(&e.content) else { continue };
                    let before = prev.and_then(|p| super::super::roles::parse_sync_content(&p.content)).and_then(|s| s.1);
                    let target = Some(names.channel(&channel));
                    match category {
                        Some(k) if before.as_deref() != Some(k.as_str()) => {
                            out.push(base("channel_sync", target, vec![json!({ "field": "category", "new": names.category(&k) })]));
                        }
                        None if before.is_some() => out.push(base("channel_unsync", target, Vec::new())),
                        _ => {}
                    }
                }
                vsk::PINS => {
                    let Some(channel) = pins_channel.get(&e.entity_id) else { continue };
                    let channel_target = names.channel(channel);
                    let Some(now) = (readers.pins)(e, channel) else {
                        out.push(base("pins_update", Some(channel_target), Vec::new()));
                        continue;
                    };
                    let before = match prev.map(|p| (readers.pins)(p, channel)) {
                        Some(Some(b)) => b,
                        None if genesis => Vec::new(),
                        _ => {
                            out.push(AuditEntry { partial: true, ..base("pins_update", Some(channel_target), Vec::new()) });
                            continue;
                        }
                    };
                    let message = |id: &str, author: &str| json!({ "kind": "message", "id": id, "author": npub_of(author), "channel": channel_target });
                    for (n, (id, author)) in now.iter().filter(|(id, _)| !before.iter().any(|b| &b.0 == id)).enumerate() {
                        out.push(AuditEntry { id: format!("{}:p{n}", hex(&e.self_hash)), ..base("message_pin", Some(message(id, author)), Vec::new()) });
                    }
                    for (n, (id, author)) in before.iter().filter(|(id, _)| !now.iter().any(|b| &b.0 == id)).enumerate() {
                        out.push(AuditEntry { id: format!("{}:x{n}", hex(&e.self_hash)), ..base("message_unpin", Some(message(id, author)), Vec::new()) });
                    }
                }
                vsk::INVITE_LINKS => {
                    let Ok(now) = super::super::invite::parse_registry_content(&e.content) else { continue };
                    let before = match prev.map(|p| super::super::invite::parse_registry_content(&p.content)) {
                        Some(Ok(b)) => b,
                        None if genesis => Vec::new(),
                        _ => continue,
                    };
                    let added = now.iter().filter(|k| !before.contains(k)).count();
                    let removed = before.iter().filter(|k| !now.contains(k)).count();
                    if added > 0 {
                        out.push(AuditEntry { id: format!("{}:a", hex(&e.self_hash)), ..base("invite_create", None, vec![json!({ "field": "count", "new": added })]) });
                    }
                    if removed > 0 {
                        out.push(AuditEntry { id: format!("{}:r", hex(&e.self_hash)), ..base("invite_delete", None, vec![json!({ "field": "count", "new": removed })]) });
                    }
                }
                // Tenure, the Writers ledger and the tombstone record how a decision is kept,
                // not a decision of their own.
                _ => {}
            }
        }
    }
    (out, NamesView(names))
}

/// The names [`control_entries`] gathered, for the Guestbook and local halves of the log.
pub(crate) struct NamesView(Names);

fn role_changes(old: Option<&crate::community::roles::Role>, new: &crate::community::roles::Role) -> Vec<Value> {
    let Some(old) = old else {
        let mut c = vec![json!({ "field": "name", "new": new.name })];
        if new.color != 0 {
            c.push(json!({ "field": "color", "new": new.color }));
        }
        c.extend(bit_change("permissions", 0, new.permissions.0));
        if new.display {
            c.push(json!({ "field": "display", "new": true }));
        }
        if new.mentionable {
            c.push(json!({ "field": "mentionable", "new": true }));
        }
        return c;
    };
    let mut c: Vec<Value> = text_change("name", Some(&old.name), Some(&new.name)).into_iter().collect();
    if old.color != new.color {
        c.push(json!({ "field": "color", "old": old.color, "new": new.color }));
    }
    c.extend(bit_change("permissions", old.permissions.0, new.permissions.0));
    if old.rank != new.rank || (old.rank.is_none() && old.position != new.position) {
        c.push(json!({ "field": "position" }));
    }
    if old.display != new.display {
        c.push(json!({ "field": "display", "new": new.display }));
    }
    if old.mentionable != new.mentionable {
        c.push(json!({ "field": "mentionable", "new": new.mentionable }));
    }
    c
}

fn channel_changes(old: &control::ChannelMetadata, new: &control::ChannelMetadata, names: &Names) -> Vec<Value> {
    let mut c: Vec<Value> = text_change("name", Some(&old.name), Some(&new.name)).into_iter().collect();
    c.extend(text_change("topic", str_extra(&old.extra, "topic"), str_extra(&new.extra, "topic")));
    let (ov, nv) = (view_of(old), view_of(new));
    if ov != nv {
        c.push(json!({ "field": "view", "old": ov, "new": nv }));
    }
    if !old.private && new.private {
        c.push(json!({ "field": "private", "new": true }));
    }
    let (ok, nk) = (str_extra(&old.extra, "category"), str_extra(&new.extra, "category"));
    if ok != nk {
        c.push(json!({ "field": "category", "old": ok.map(|k| names.category(k)), "new": nk.map(|k| names.category(k)) }));
    } else if str_extra(&old.extra, "position") != str_extra(&new.extra, "position") {
        c.push(json!({ "field": "position" }));
    }
    c
}

/// The Guestbook's moderation, judged as this device judges it everywhere else: Kicks and
/// Timeouts that stand, and each Refounding its recorded minter seeded.
fn guestbook_entries(community: &CommunityV2, cid_hex: &str) -> Vec<AuditEntry> {
    let Ok((events, _)) = crate::db::community::get_guestbook(cid_hex) else { return Vec::new() };
    let Ok(owner) = community.owner().map(|o| o.to_hex()) else { return Vec::new() };
    let roster = crate::db::community::get_community_roles(cid_hex).unwrap_or_default();
    let banned = crate::db::community::get_community_banlist(cid_hex).unwrap_or_default();
    let may = |actor: &PublicKey, target: &PublicKey, bit: u64, vac: Option<&crate::community::edition::AuthorityCitation>| {
        let (a, t) = (actor.to_hex(), target.to_hex());
        t != owner && !banned.contains(&a) && roster.can_act_on_member(&a, Some(&owner), &t, bit) && citation_is_synced(cid_hex, &owner, &a, vac)
    };
    let name_judge = super::nickname_judge(community).ok();
    let horizon = now_ms().saturating_add(guestbook::MAX_FUTURE_MS);
    let minters = epoch_minters(cid_hex);
    let severed = sever_marks(cid_hex);
    let mut rotated: BTreeMap<u64, (u64, PublicKey)> = BTreeMap::new();
    let mut out = Vec::new();
    for ev in &events {
        let id = hex(&ev.rumor_id);
        match &ev.entry {
            guestbook::GuestbookEntry::Kick { actor, target, citation, at_ms } if may(actor, target, Permissions::KICK, citation.as_ref()) => {
                out.push(AuditEntry { id, at: *at_ms, actor: Some(npub(actor)), action: "member_kick", target: Some(member(&target.to_hex())), changes: Vec::new(), partial: false });
            }
            guestbook::GuestbookEntry::Timeout { actor, target, until_secs, citation, at_ms } if may(actor, target, Permissions::MODERATE_MEMBERS, citation.as_ref()) => {
                let lifted = *until_secs <= at_ms / 1000;
                let changes = if lifted { Vec::new() } else { vec![json!({ "field": "until", "new": until_secs.saturating_mul(1000) })] };
                let action = if lifted { "member_timeout_remove" } else { "member_timeout" };
                out.push(AuditEntry { id, at: *at_ms, actor: Some(npub(actor)), action, target: Some(member(&target.to_hex())), changes, partial: false });
            }
            guestbook::GuestbookEntry::Snapshot { refounder, at_ms, .. } if ev.epoch.and_then(|e| minters.get(&e)).is_some_and(|m| *m == refounder.to_hex()) => {
                let slot = rotated.entry(ev.epoch.unwrap_or_default()).or_insert((*at_ms, *refounder));
                slot.0 = slot.0.min(*at_ms);
            }
            _ => {}
        }
    }
    // A Nickname that changes nothing (one a Refounding carried across) records no act: the
    // older plane's original goes first, so it names who made the change.
    let mut named: Vec<&guestbook::GuestbookEvent> = events
        .iter()
        .filter(|ev| matches!(&ev.entry, guestbook::GuestbookEntry::Nickname { actor, target, citation, at_ms, .. } if *at_ms <= horizon && name_judge.as_ref().is_some_and(|j| j(actor, target, citation.as_ref()))))
        .collect();
    named.sort_by_key(|ev| (ev.entry.at_ms(), ev.epoch.unwrap_or(0), ev.rumor_id));
    let mut held: HashMap<PublicKey, &str> = HashMap::new();
    for ev in named {
        let guestbook::GuestbookEntry::Nickname { actor, target, nick, at_ms, .. } = &ev.entry else { continue };
        let old = held.get(target).copied().unwrap_or("");
        if old == nick {
            continue;
        }
        held.insert(*target, nick);
        let changes = vec![json!({ "field": "nick", "old": (!old.is_empty()).then_some(old), "new": nick })];
        out.push(AuditEntry { id: hex(&ev.rumor_id), at: *at_ms, actor: Some(npub(actor)), action: "member_nick_update", target: Some(member(&target.to_hex())), changes, partial: false });
    }
    // A Refounding whose seed snapshot never landed still shows, dated when this device adopted it.
    for (epoch, rotator) in &minters {
        if rotated.contains_key(epoch) {
            continue;
        }
        if let (Some(secs), Ok(pk)) = (severed.get(epoch), PublicKey::from_hex(rotator)) {
            rotated.insert(*epoch, (secs.saturating_mul(1000), pk));
        }
    }
    for (epoch, (at, rotator)) in rotated {
        let changes = if severed.contains_key(&epoch) { vec![json!({ "field": "severed", "new": true })] } else { Vec::new() };
        out.push(AuditEntry { id: format!("rotate:{epoch}"), at, actor: Some(npub(&rotator)), action: "keys_rotate", target: None, changes, partial: false });
    }
    out
}

/// What this device noted of the acts no edition records.
fn local_entries(cid_hex: &str, names: &NamesView) -> Vec<AuditEntry> {
    let names = &names.0;
    let rows = crate::db::community::get_audit_local(cid_hex).unwrap_or_default();
    let mut out = Vec::new();
    // A mute re-sent when its target comes back is the same decision, not another one.
    let mut standing: HashMap<(String, String, &'static str), bool> = HashMap::new();
    for (id, at, raw) in rows {
        let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let (Some(action), Some(actor), Some(channel)) = (s("action"), s("actor"), s("channel")) else { continue };
        let action: &'static str = match action.as_str() {
            "message_delete" => "message_delete",
            "voice_mute" => "voice_mute",
            "voice_unmute" => "voice_unmute",
            "voice_deafen" => "voice_deafen",
            "voice_undeafen" => "voice_undeafen",
            "voice_move" => "voice_move",
            "voice_disconnect" => "voice_disconnect",
            _ => continue,
        };
        let (target, mut changes) = if action == "message_delete" {
            let author = s("author").unwrap_or_default();
            (json!({ "kind": "message", "author": npub_of(&author), "channel": names.channel(&channel) }), Vec::new())
        } else {
            let target = s("target").unwrap_or_default();
            let family = match action {
                "voice_mute" | "voice_unmute" => Some("mute"),
                "voice_deafen" | "voice_undeafen" => Some("deafen"),
                _ => None,
            };
            if let Some(family) = family {
                let on = matches!(action, "voice_mute" | "voice_deafen");
                if standing.insert((channel.clone(), target.clone(), family), on) == Some(on) {
                    continue;
                }
            }
            (member(&target), vec![json!({ "field": "channel", "new": names.channel(&channel) })])
        };
        if let Some(to) = s("to") {
            changes.push(json!({ "field": "to", "new": names.channel(&to) }));
        }
        out.push(AuditEntry { id, at, actor: Some(npub_of(&actor)), action, target: Some(target), changes, partial: false });
    }
    out
}

/// Note a moderator's delete of `author`'s message in `channel` (all hex).
pub(crate) fn note_moderation_delete(cid_hex: &str, event_id: &str, at_ms: u64, actor: &str, author: &str, channel: &str) {
    let entry = json!({ "action": "message_delete", "actor": actor, "author": author, "channel": channel });
    if let Err(e) = crate::db::community::record_audit_local(cid_hex, event_id, at_ms, &entry.to_string()) {
        crate::log_warn!("[audit] couldn't note a moderator's delete: {e}");
    }
}

/// Note a voice directive `actor` sent `target` in `channel` (all hex).
pub(crate) fn note_voice_directive(cid_hex: &str, event_id: &str, at_ms: u64, actor: &str, target: &str, channel: &str, directive: super::super::voice::Directive, to: Option<&[u8; 32]>) {
    let entry = json!({
        "action": format!("voice_{}", directive.as_str()),
        "actor": actor,
        "target": target,
        "channel": channel,
        "to": to.map(hex),
    });
    if let Err(e) = crate::db::community::record_audit_local(cid_hex, event_id, at_ms, &entry.to_string()) {
        crate::log_warn!("[audit] couldn't note a voice directive: {e}");
    }
}

/// `community` as it stood at `epoch`, for reading that epoch's Control Plane: the root it held
/// then, and the plane address it lived at if this device kept it.
pub(super) fn epoch_view(community: &CommunityV2, cid_hex: &str, epoch: Epoch, root: [u8; 32]) -> CommunityV2 {
    let mut view = community.clone();
    view.community_root = root;
    view.root_epoch = epoch;
    view.control_root = None;
    view.control_pk = crate::db::community::held_epoch_key(cid_hex, crate::community::CONTROL_PK_SCOPE, epoch.0)
        .ok()
        .flatten()
        .and_then(|pk| PublicKey::from_slice(&pk).ok())
        .or_else(|| {
            let cr = crate::db::community::held_epoch_key(cid_hex, crate::community::CONTROL_ROOT_SCOPE, epoch.0).ok().flatten()?;
            Some(super::super::derive::control_signer_group_key(&cr, community.id(), epoch).pk())
        });
    view
}

/// Every Control edition this device can read back: the current epoch whole, and each earlier
/// epoch whose plane it can still address. The second value says whether some history is missing.
async fn read_control_history<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(Vec<ParsedEdition>, bool), String> {
    let current = fetch_control_plane_whole(transport, community)
        .await
        .ok_or("this community's history couldn't be read in full; try again in a moment")?;
    let cid_hex = hex(&community.id().0);
    let roots: Vec<(Epoch, [u8; 32])> = crate::db::community::held_epoch_keys(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX)
        .unwrap_or_default()
        .into_iter()
        .filter(|(e, _)| e.0 < community.root_epoch.0)
        .collect();
    let mut compacted = (0..community.root_epoch.0).any(|e| !roots.iter().any(|(held, _)| held.0 == e));
    let views: Vec<CommunityV2> = roots.iter().map(|(e, root)| epoch_view(community, &cid_hex, *e, *root)).collect();
    let mut older: Vec<Option<Vec<ParsedEdition>>> = Vec::new();
    for batch in views.chunks(4) {
        let mut reads = Vec::new();
        for view in batch {
            reads.push(fetch_control_plane_whole(transport, view));
        }
        older.extend(futures_util::future::join_all(reads).await);
    }
    let mut by_hash: HashMap<[u8; 32], ParsedEdition> = current.into_iter().map(|e| (e.self_hash, e)).collect();
    for read in older {
        match read {
            // Every epoch carries at least the heads compacted into it: an empty read is a
            // plane this device addressed wrong, not an empty past.
            Some(eds) if !eds.is_empty() => {
                for e in eds {
                    by_hash.entry(e.self_hash).or_insert(e);
                }
            }
            _ => compacted = true,
        }
    }
    Ok((by_hash.into_values().collect(), compacted))
}

/// The head this device settled on for every entity it folds.
fn accepted_heads(cid_hex: &str) -> HashMap<[u8; 32], [u8; 32]> {
    crate::db::community::get_all_edition_heads_full(cid_hex)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(entity, f)| crate::simd::hex::hex_to_bytes_32_checked(&entity).map(|e| (e, f.2)))
        .collect()
}

/// A Channel edition's metadata as this device can open it: plain, or sealed under the key of
/// the epoch it names.
fn channel_metadata(community: &CommunityV2, e: &ParsedEdition) -> Option<control::ChannelMetadata> {
    let Some(sealed) = control::parse_sealed_channel_meta(&e.content) else {
        return serde_json::from_str::<control::ChannelMetadata>(&e.content).ok().map(control::sanitize_channel_metadata);
    };
    let id = ChannelId(e.entity_id);
    // A deleted Channel has left the community; its keys are still held by epoch.
    let gone = ChannelV2 { id, name: String::new(), private: true, key: None, epoch: Epoch(u64::MAX), voice: None, meta_custom: None, meta_extra: Default::default(), writers: None, sealed: None };
    let ch = community.channel(&id).unwrap_or(&gone);
    let conv = channel_conv_key_at(community, ch, sealed.epoch)?;
    control::open_sealed_channel_meta(&sealed, &conv, e.prev_hash.as_ref()).map(control::sanitize_channel_metadata)
}

fn channel_pins(community: &CommunityV2, e: &ParsedEdition, channel_hex: &str) -> Option<Vec<(String, String)>> {
    let id = ChannelId(crate::simd::hex::hex_to_bytes_32_checked(channel_hex)?);
    let gone = ChannelV2 { id, name: String::new(), private: true, key: None, epoch: Epoch(u64::MAX), voice: None, meta_custom: None, meta_extra: Default::default(), writers: None, sealed: None };
    let ch = community.channel(&id).unwrap_or(&gone);
    let read = super::super::pins::read_pin_list(&e.content, |epoch| channel_conv_key_at(community, ch, epoch));
    if read.sealed {
        return None;
    }
    Some(read.entries.iter().filter_map(|p| super::super::pins::verify_pin_entry(p, channel_hex)).map(|p| (p.rumor_id, p.author)).collect())
}

/// Where an act falls among others dated the same second (times are whole seconds): a
/// community's creation comes before what's made in it, which comes before what's done to it.
fn same_second_rank(action: &str) -> u8 {
    match action {
        "community_create" => 0,
        "channel_create" | "category_create" | "role_create" | "roles_activate" => 1,
        _ => 2,
    }
}

/// Newest first, acts in one second in the order they could have happened.
fn newest_first(entries: &mut [AuditEntry]) {
    entries.sort_by(|a, b| {
        b.at.cmp(&a.at)
            .then_with(|| same_second_rank(b.action).cmp(&same_second_rank(a.action)))
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// The whole Audit Log of `community`, newest first.
pub async fn audit_log<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<AuditLog, String> {
    let cid_hex = hex(&community.id().0);
    let (editions, compacted) = read_control_history(transport, community).await?;
    let heads = accepted_heads(&cid_hex);
    let known: Vec<String> = community.channels.iter().map(|c| hex(&c.id.0)).collect();
    let channel = |e: &ParsedEdition| channel_metadata(community, e);
    let pins = |e: &ParsedEdition, ch: &str| channel_pins(community, e, ch);
    let (mut entries, names) = control_entries(community.id(), &editions, &heads, &known, &Readers { channel: &channel, pins: &pins });
    entries.extend(guestbook_entries(community, &cid_hex));
    entries.extend(local_entries(&cid_hex, &names));
    let mut seen = HashSet::new();
    entries.retain(|e| seen.insert(e.id.clone()));
    newest_first(&mut entries);
    Ok(AuditLog { entries, compacted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    const CID: crate::community::CommunityId = crate::community::CommunityId([0x11; 32]);

    fn ed(author: &PublicKey, vsk: &str, entity: [u8; 32], version: u64, prev: Option<&ParsedEdition>, content: &str, at: u64) -> ParsedEdition {
        let mut self_hash = [0u8; 32];
        self_hash[..8].copy_from_slice(&at.to_be_bytes());
        self_hash[8] = version as u8;
        self_hash[9..].copy_from_slice(&entity[9..]);
        ParsedEdition {
            author: *author,
            vsk: vsk.to_string(),
            entity_id: entity,
            version,
            prev_hash: prev.map(|p| p.self_hash),
            content: content.to_string(),
            self_hash,
            created_at: at,
            inner_id: self_hash,
            authority: None,
            binding: None,
        }
    }

    fn role(id: [u8; 32], name: &str, perms: u64) -> String {
        json!({ "role_id": hex(&id), "name": name, "rank": "m", "permissions": perms.to_string(), "color": 0, "display": false, "mentionable": false }).to_string()
    }

    fn build(editions: &[ParsedEdition], heads: &[&ParsedEdition]) -> Vec<AuditEntry> {
        let heads: HashMap<[u8; 32], [u8; 32]> = heads.iter().map(|h| (h.entity_id, h.self_hash)).collect();
        let channel = |e: &ParsedEdition| serde_json::from_str::<control::ChannelMetadata>(&e.content).ok();
        let pins = |_: &ParsedEdition, _: &str| None;
        let (mut out, _) = control_entries(&CID, editions, &heads, &[], &Readers { channel: &channel, pins: &pins });
        out.sort_by_key(|e| e.at);
        out
    }

    #[test]
    fn a_community_is_created_before_anything_in_the_same_second() {
        let entry = |id: &str, action: &'static str, at: u64| AuditEntry { id: id.into(), at, actor: None, action, target: None, changes: Vec::new(), partial: false };
        // Ids that would put the community's creation above its #general were they the tiebreak.
        let mut log = vec![entry("00", "channel_create", 5_000), entry("ff", "community_create", 5_000), entry("aa", "roles_activate", 5_000), entry("11", "role_update", 9_000)];
        newest_first(&mut log);
        let order: Vec<&str> = log.iter().map(|e| e.action).collect();
        assert_eq!(order.first(), Some(&"role_update"), "newest first");
        assert_eq!(order.last(), Some(&"community_create"), "read bottom up: the community, then what's made in it");
    }

    #[test]
    fn a_roles_chain_reads_as_created_renamed_and_deleted() {
        let owner = Keys::generate().public_key();
        let id = [0x21; 32];
        let v1 = ed(&owner, vsk::ROLE_V2, id, 1, None, &role(id, "Mods", Permissions::KICK), 10);
        let v2 = ed(&owner, vsk::ROLE_V2, id, 2, Some(&v1), &role(id, "Moderators", Permissions::KICK | Permissions::BAN), 20);
        let v3 = ed(&owner, vsk::ROLE_V2, id, 3, Some(&v2), &json!({ "role_id": hex(&id), "deleted": true }).to_string(), 30);
        let out = build(&[v1, v2.clone(), v3.clone()], &[&v3]);
        let actions: Vec<_> = out.iter().map(|e| e.action).collect();
        assert_eq!(actions, ["role_create", "role_update", "role_delete"]);
        let rename = &out[1].changes;
        assert!(rename.iter().any(|c| c["field"] == "name" && c["old"] == "Mods" && c["new"] == "Moderators"));
        assert!(rename.iter().any(|c| c["field"] == "permissions" && c["added"] == json!([4]) && c["removed"] == json!([])));
        assert_eq!(out[2].target.as_ref().unwrap()["name"], "Moderators", "a deleted Role keeps the name it last had");
    }

    #[test]
    fn only_the_accepted_chain_is_history() {
        let owner = Keys::generate().public_key();
        let raider = Keys::generate().public_key();
        let id = [0x22; 32];
        let v1 = ed(&owner, vsk::ROLE_V2, id, 1, None, &role(id, "Mods", 0), 10);
        let fork = ed(&raider, vsk::ROLE_V2, id, 2, Some(&v1), &role(id, "Pwned", u64::MAX), 15);
        let v2 = ed(&owner, vsk::ROLE_V2, id, 2, Some(&v1), &role(id, "Staff", 0), 20);
        let out = build(&[v1, fork, v2.clone()], &[&v2]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.actor.as_deref() == Some(npub(&owner).as_str())), "the fold's rejected fork never shows");
        // No head, no history: an entity this device never accepted reads as nothing.
        let spam = ed(&raider, vsk::ROLE_V2, [0x23; 32], 1, None, &role([0x23; 32], "Spam", 0), 30);
        assert!(build(&[spam], &[]).is_empty());
    }

    #[test]
    fn a_banlist_edition_bans_and_unbans_each_member() {
        let owner = Keys::generate().public_key();
        let (a, b) = (Keys::generate().public_key().to_hex(), Keys::generate().public_key().to_hex());
        let id = crate::community::v2::derive::banlist_locator(&CID);
        let v1 = ed(&owner, vsk::BANLIST, id, 1, None, &json!([a]).to_string(), 10);
        let v2 = ed(&owner, vsk::BANLIST, id, 2, Some(&v1), &json!([b]).to_string(), 20);
        let out = build(&[v1, v2.clone()], &[&v2]);
        let seen: Vec<(&str, String)> = out.iter().map(|e| (e.action, e.target.as_ref().unwrap()["npub"].as_str().unwrap().to_string())).collect();
        assert!(seen.contains(&("member_ban", npub_of(&a))));
        assert!(seen.contains(&("member_unban", npub_of(&a))));
        assert!(seen.contains(&("member_ban", npub_of(&b))));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn a_grant_reads_as_roles_given_and_taken() {
        let owner = Keys::generate().public_key();
        let member = Keys::generate().public_key();
        let (mods, vip) = ([0x31; 32], [0x32; 32]);
        let id = crate::community::v2::derive::grant_locator(&CID, &member.to_bytes());
        let grant = |roles: &[[u8; 32]], wrap: Option<&str>| {
            let mut g = json!({ "member": member.to_hex(), "role_ids": roles.iter().map(hex).collect::<Vec<_>>() });
            if let Some(w) = wrap {
                g["control_wrap"] = w.into();
            }
            g.to_string()
        };
        let r = ed(&owner, vsk::ROLE_V2, mods, 1, None, &role(mods, "Mods", 0), 1);
        let v1 = ed(&owner, vsk::GRANT, id, 1, None, &grant(&[mods], None), 10);
        let v2 = ed(&owner, vsk::GRANT, id, 2, Some(&v1), &grant(&[mods], Some("wrap")), 20);
        let v3 = ed(&owner, vsk::GRANT, id, 3, Some(&v2), &grant(&[vip], Some("wrap")), 30);
        let out = build(&[r.clone(), v1, v2, v3.clone()], &[&r, &v3]);
        let grants: Vec<&AuditEntry> = out.iter().filter(|e| e.action == "member_roles_update").collect();
        assert_eq!(grants.len(), 2, "a Grant re-sent only to carry the staff key changes no roles");
        assert_eq!(grants[0].changes[0]["added"][0]["name"], "Mods");
        assert_eq!(grants[1].changes[0]["removed"][0]["name"], "Mods");
        assert_eq!(grants[1].changes[0]["added"][0]["id"], hex(&vip));
    }

    #[test]
    fn a_compacted_head_is_partial_not_a_creation() {
        let owner = Keys::generate().public_key();
        let id = [0x41; 32];
        let gone = ed(&owner, vsk::ROLE_V2, id, 4, None, &role(id, "Old", 0), 5);
        let head = ed(&owner, vsk::ROLE_V2, id, 5, Some(&gone), &role(id, "Mods", 0), 10);
        let out = build(&[head.clone()], &[&head]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].action, "role_update");
        assert!(out[0].partial && out[0].changes.is_empty());
    }

    #[test]
    fn channels_read_as_created_moved_and_deleted() {
        let owner = Keys::generate().public_key();
        let (id, cat) = ([0x51; 32], [0x52; 32]);
        let k = ed(&owner, vsk::CATEGORY, cat, 1, None, r#"{"name":"Staff","position":"m"}"#, 1);
        let v1 = ed(&owner, vsk::CHANNEL_METADATA, id, 1, None, r#"{"name":"lounge","private":false,"view":"voice"}"#, 10);
        let v2 = ed(&owner, vsk::CHANNEL_METADATA, id, 2, Some(&v1), &json!({ "name": "lounge", "private": false, "view": "voice", "category": hex(&cat) }).to_string(), 20);
        let v3 = ed(&owner, vsk::CHANNEL_METADATA, id, 3, Some(&v2), r#"{"name":"lounge","private":false,"deleted":true}"#, 30);
        let out = build(&[k.clone(), v1, v2, v3.clone()], &[&k, &v3]);
        let actions: Vec<_> = out.iter().map(|e| e.action).collect();
        assert_eq!(actions, ["category_create", "channel_create", "channel_update", "channel_delete"]);
        assert_eq!(out[1].changes[0]["new"], "voice");
        assert_eq!(out[2].changes[0]["new"]["name"], "Staff");
        assert_eq!(out[3].target.as_ref().unwrap()["name"], "lounge");
    }
}
