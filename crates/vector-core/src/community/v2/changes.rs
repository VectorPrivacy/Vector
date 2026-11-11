//! Typed changes to a Community: what a refresh folded, told apart by comparing what this device
//! held before it with what it holds after. Local reads only. A session's first look at a
//! Community is its baseline, so nothing that stood before it reads as new, and a change is
//! reported only when it is [news](crate::news::is_news): made since the listener began.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use nostr_sdk::prelude::{PublicKey, ToBech32};
use serde::Serialize;

use super::community::CommunityV2;
use super::derive;
use crate::community::roles::{OverwriteTarget, Permissions, Role};
use crate::community::{ChannelId, CommunityId};
use crate::event_handler::InboundEventHandler;

/// One change to a Community, who made it and when.
///
/// When several people changed the same thing between two looks, the change reads as it stands
/// now: `by` made its last edit and `also_by` names the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CommunityChange {
    /// The npub that made it: the author of the edition or Guestbook entry that holds it, when
    /// this device knows it. A lapsed Timeout has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// When it was made (unix ms), as its author dated it; when this device saw it, if unknown.
    pub at: u64,
    /// Everyone else whose edit this change folds in, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_by: Vec<String>,
    #[serde(flatten)]
    pub kind: ChangeKind,
}

impl CommunityChange {
    pub(crate) fn new(by: Option<String>, at: u64, kind: ChangeKind) -> Self {
        CommunityChange { by, at, also_by: Vec::new(), kind }
    }
}

/// A Channel as a change reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ChannelInfo {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
    pub category_id: Option<String>,
    pub topic: Option<String>,
    /// `chat`, `forum` or `voice`.
    pub view: String,
}

/// A Category as a change reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CategoryInfo {
    pub category_id: String,
    pub name: String,
}

/// What changed. Members are npubs, every other id is lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeKind {
    Renamed { old: String, new: String },
    DescriptionChanged { old: Option<String>, new: Option<String> },
    /// The icon's new image URL, `None` when removed.
    IconChanged { url: Option<String> },
    BannerChanged { url: Option<String> },
    RelaysChanged { relays: Vec<String> },
    VoiceRelaysChanged { relays: Vec<String> },
    /// What every member holds before any Role; `None` before Roles v2.
    EveryoneChanged { old: Option<Permissions>, new: Option<Permissions> },
    RoleCreated { role: Role },
    /// Its name, colour, permissions or display changed. A move in the order is [`RolesReordered`](Self::RolesReordered).
    RoleUpdated { old: Role, new: Role },
    RoleDeleted { role: Role },
    /// The Role order changed; `order` is every Role id, top first.
    RolesReordered { order: Vec<String> },
    MemberRolesChanged { member: String, added: Vec<String>, removed: Vec<String> },
    MemberBanned { member: String },
    MemberUnbanned { member: String },
    MemberKicked { member: String },
    ChannelCreated { channel: ChannelInfo },
    ChannelUpdated { old: ChannelInfo, new: ChannelInfo },
    ChannelDeleted { channel: ChannelInfo },
    CategoryCreated { category: CategoryInfo },
    CategoryUpdated { old: CategoryInfo, new: CategoryInfo },
    CategoryDeleted { category: CategoryInfo },
    /// The sidebar order of Channels and Categories changed.
    LayoutChanged,
    /// A Channel's or Category's overwrite for one target. Both empty: it was removed.
    OverwriteChanged { scope_id: String, target: OverwriteTarget, allow: Permissions, deny: Permissions },
    /// A Channel now takes its Category's overwrites (`Some`), or its own again (`None`).
    ChannelSynced { channel_id: String, category_id: Option<String> },
    PinsChanged { channel_id: String },
    /// `until` is unix seconds.
    TimedOut { member: String, until: u64 },
    /// Lifted early (with `by`), or lapsed (without). A lapse is seen at the next refresh.
    TimeoutEnded { member: String },
    /// `None` when cleared.
    NicknameChanged { member: String, nickname: Option<String> },
}

/// What one look at a Community saw.
#[derive(Default, Clone, PartialEq)]
struct Snapshot {
    name: String,
    description: Option<String>,
    icon: Option<String>,
    banner: Option<String>,
    relays: Vec<String>,
    voice_relays: Vec<String>,
    everyone: Option<Permissions>,
    roles: BTreeMap<String, Role>,
    role_order: Vec<String>,
    grants: BTreeMap<String, BTreeSet<String>>,
    banned: BTreeSet<String>,
    channels: BTreeMap<String, ChannelInfo>,
    categories: BTreeMap<String, CategoryInfo>,
    layout: Vec<String>,
    overwrites: BTreeMap<(String, OverwriteTarget), (Permissions, Permissions)>,
    syncs: BTreeMap<String, String>,
    pins: BTreeMap<String, i64>,
    timeouts: BTreeMap<String, u64>,
    nicknames: BTreeMap<String, String>,
    /// Each Control entity's head version, which bounds the editions a change folds in.
    heads: BTreeMap<String, u64>,
}

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

fn npub_of_hex(h: &str) -> String {
    PublicKey::from_hex(h).ok().and_then(|p| p.to_bech32().ok()).unwrap_or_else(|| h.to_string())
}

fn npub(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

/// Every Channel's settings, in sidebar order.
pub fn channel_infos(community: &CommunityV2) -> Vec<ChannelInfo> {
    community
        .layout()
        .1
        .into_iter()
        .map(|(ch, cat)| ChannelInfo {
            channel_id: hex(&ch.id.0),
            name: ch.name.clone(),
            private: ch.private,
            category_id: cat.map(|k| hex(&k.id)),
            topic: ch.topic().map(str::to_string),
            view: ch.view().as_str().to_string(),
        })
        .collect()
}

/// Every Category, in sidebar order.
pub fn category_infos(community: &CommunityV2) -> Vec<CategoryInfo> {
    community.layout().0.into_iter().map(|k| CategoryInfo { category_id: hex(&k.id), name: k.name.clone() }).collect()
}

/// A Role as it compares: a Roles v2 position is derived from its rank, so only a reorder moves it.
fn comparable(role: &Role) -> Role {
    let mut r = role.clone();
    if r.rank.is_some() {
        r.position = 0;
        r.rank = None;
    }
    r
}

/// Whether the items `a` and `b` both hold sit in a different order.
fn reordered(a: &[String], b: &[String]) -> bool {
    let both: BTreeSet<&String> = a.iter().filter(|x| b.contains(x)).collect();
    a.iter().filter(|x| both.contains(x)).ne(b.iter().filter(|x| both.contains(x)))
}

fn read(community: &CommunityV2) -> Snapshot {
    let cid = hex(&community.id().0);
    let roster = crate::db::community::get_community_roles_shared(&cid).unwrap_or_default();
    let categories = category_infos(community);
    let mut layout: Vec<String> = categories.iter().map(|k| format!("k{}", k.category_id)).collect();
    let channel_list = channel_infos(community);
    layout.extend(channel_list.iter().map(|c| c.channel_id.clone()));
    let channels = channel_list.into_iter().map(|c| (c.channel_id.clone(), c)).collect();
    let pins = crate::db::community::get_community_pin_versions(&cid).unwrap_or_default().into_iter().collect();
    let mut order: Vec<&Role> = roster.roles.iter().collect();
    order.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.role_id.cmp(&b.role_id)));
    Snapshot {
        name: community.name.clone(),
        description: community.description.clone(),
        icon: community.icon.as_ref().map(|i| i.url.clone()),
        banner: community.banner.as_ref().map(|i| i.url.clone()),
        relays: community.relays.clone(),
        voice_relays: community.voice_relays(),
        everyone: roster.everyone,
        roles: roster.roles.iter().map(|r| (r.role_id.to_ascii_lowercase(), r.clone())).collect(),
        role_order: order.iter().map(|r| r.role_id.to_ascii_lowercase()).collect(),
        grants: roster
            .grants
            .iter()
            .map(|g| (g.member.to_ascii_lowercase(), g.role_ids.iter().map(|r| r.to_ascii_lowercase()).collect()))
            .collect(),
        banned: crate::db::community::get_community_banlist(&cid).unwrap_or_default().into_iter().collect(),
        channels,
        categories: categories.into_iter().map(|k| (k.category_id.clone(), k)).collect(),
        layout,
        overwrites: roster.overwrites.iter().map(|o| ((o.scope.to_ascii_lowercase(), o.target.clone()), (o.allow, o.deny))).collect(),
        syncs: roster.syncs.clone(),
        pins,
        timeouts: super::service::community_timeouts(community)
            .unwrap_or_default()
            .into_iter()
            .map(|(pk, (_, until))| (pk.to_hex(), until))
            .collect(),
        nicknames: super::service::nicknames_as_set(community)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, n)| !n.nick.is_empty())
            .map(|(pk, n)| (pk.to_hex(), n.nick))
            .collect(),
        heads: crate::db::community::get_all_edition_heads_full(&cid).unwrap_or_default().into_iter().map(|(entity, h)| (entity, h.1)).collect(),
    }
}

/// Every change `new` makes to `old`, unattributed: [`attribute`] names who made each.
fn diff(old: &Snapshot, new: &Snapshot) -> Vec<(ChangeKind, Source)> {
    let mut out = Vec::new();
    let meta = Source::Entity(Locator::Metadata);
    if old.name != new.name {
        out.push((ChangeKind::Renamed { old: old.name.clone(), new: new.name.clone() }, meta.clone()));
    }
    if old.description != new.description {
        out.push((ChangeKind::DescriptionChanged { old: old.description.clone(), new: new.description.clone() }, meta.clone()));
    }
    if old.icon != new.icon {
        out.push((ChangeKind::IconChanged { url: new.icon.clone() }, meta.clone()));
    }
    if old.banner != new.banner {
        out.push((ChangeKind::BannerChanged { url: new.banner.clone() }, meta.clone()));
    }
    if old.relays != new.relays {
        out.push((ChangeKind::RelaysChanged { relays: new.relays.clone() }, meta.clone()));
    }
    if old.voice_relays != new.voice_relays {
        out.push((ChangeKind::VoiceRelaysChanged { relays: new.voice_relays.clone() }, meta));
    }
    if old.everyone != new.everyone {
        out.push((ChangeKind::EveryoneChanged { old: old.everyone, new: new.everyone }, Source::Entity(Locator::Everyone)));
    }
    for (id, role) in &new.roles {
        let at = Source::Entity(Locator::Raw(id.clone()));
        match old.roles.get(id) {
            None => out.push((ChangeKind::RoleCreated { role: role.clone() }, at)),
            Some(was) if comparable(was) != comparable(role) => out.push((ChangeKind::RoleUpdated { old: was.clone(), new: role.clone() }, at)),
            _ => {}
        }
    }
    for (id, role) in &old.roles {
        if !new.roles.contains_key(id) {
            out.push((ChangeKind::RoleDeleted { role: role.clone() }, Source::Entity(Locator::Raw(id.clone()))));
        }
    }
    if reordered(&old.role_order, &new.role_order) {
        out.push((ChangeKind::RolesReordered { order: new.role_order.clone() }, Source::Unknown));
    }
    let none = BTreeSet::new();
    for member in old.grants.keys().chain(new.grants.keys()).collect::<BTreeSet<_>>() {
        let (was, now) = (old.grants.get(member).unwrap_or(&none), new.grants.get(member).unwrap_or(&none));
        if was != now {
            out.push((
                ChangeKind::MemberRolesChanged {
                    member: npub_of_hex(member),
                    added: now.difference(was).cloned().collect(),
                    removed: was.difference(now).cloned().collect(),
                },
                Source::Entity(Locator::Grant(member.clone())),
            ));
        }
    }
    for m in new.banned.difference(&old.banned) {
        out.push((ChangeKind::MemberBanned { member: npub_of_hex(m) }, Source::Entity(Locator::Banlist)));
    }
    for m in old.banned.difference(&new.banned) {
        out.push((ChangeKind::MemberUnbanned { member: npub_of_hex(m) }, Source::Entity(Locator::Banlist)));
    }
    for (id, ch) in &new.channels {
        let at = Source::Entity(Locator::Raw(id.clone()));
        match old.channels.get(id) {
            None => out.push((ChangeKind::ChannelCreated { channel: ch.clone() }, at)),
            Some(was) if was != ch => out.push((ChangeKind::ChannelUpdated { old: was.clone(), new: ch.clone() }, at)),
            _ => {}
        }
    }
    // A deleted Channel is reported by its purge (`note_channel_deleted`), which also sees one
    // folded while the session wasn't looking.
    for (id, k) in &new.categories {
        let at = Source::Entity(Locator::Raw(id.clone()));
        match old.categories.get(id) {
            None => out.push((ChangeKind::CategoryCreated { category: k.clone() }, at)),
            Some(was) if was != k => out.push((ChangeKind::CategoryUpdated { old: was.clone(), new: k.clone() }, at)),
            _ => {}
        }
    }
    for (id, k) in &old.categories {
        if !new.categories.contains_key(id) {
            out.push((ChangeKind::CategoryDeleted { category: k.clone() }, Source::Entity(Locator::Raw(id.clone()))));
        }
    }
    if reordered(&old.layout, &new.layout) {
        out.push((ChangeKind::LayoutChanged, Source::Unknown));
    }
    let cleared = (Permissions(0), Permissions(0));
    for key in old.overwrites.keys().chain(new.overwrites.keys()).collect::<BTreeSet<_>>() {
        let (was, now) = (old.overwrites.get(key).unwrap_or(&cleared), new.overwrites.get(key).unwrap_or(&cleared));
        if was != now {
            let (scope_id, target) = key.clone();
            let at = Source::Entity(Locator::Acl(scope_id.clone(), target.clone()));
            let target = match target {
                OverwriteTarget::Member(h) => OverwriteTarget::Member(npub_of_hex(&h)),
                t => t,
            };
            out.push((ChangeKind::OverwriteChanged { scope_id, target, allow: now.0, deny: now.1 }, at));
        }
    }
    for channel in old.syncs.keys().chain(new.syncs.keys()).collect::<BTreeSet<_>>() {
        let now = new.syncs.get(channel);
        if old.syncs.get(channel) != now {
            out.push((
                ChangeKind::ChannelSynced { channel_id: channel.clone(), category_id: now.cloned() },
                Source::Entity(Locator::Sync(channel.clone())),
            ));
        }
    }
    for (channel, version) in &new.pins {
        if old.pins.get(channel).is_some_and(|v| v != version) || (!old.pins.contains_key(channel) && old.channels.contains_key(channel)) {
            out.push((ChangeKind::PinsChanged { channel_id: channel.clone() }, Source::Entity(Locator::Pins(channel.clone()))));
        }
    }
    for (member, until) in &new.timeouts {
        if old.timeouts.get(member) != Some(until) {
            out.push((ChangeKind::TimedOut { member: npub_of_hex(member), until: *until }, Source::Timeout(member.clone(), *until)));
        }
    }
    for (member, until) in &old.timeouts {
        if !new.timeouts.contains_key(member) {
            out.push((ChangeKind::TimeoutEnded { member: npub_of_hex(member) }, Source::TimeoutEnded(member.clone(), *until)));
        }
    }
    for member in old.nicknames.keys().chain(new.nicknames.keys()).collect::<BTreeSet<_>>() {
        let now = new.nicknames.get(member);
        if old.nicknames.get(member) != now {
            out.push((
                ChangeKind::NicknameChanged { member: npub_of_hex(member), nickname: now.cloned() },
                Source::Nickname(member.clone()),
            ));
        }
    }
    out
}

/// Where a change's author is recorded.
#[derive(Clone)]
enum Source {
    Entity(Locator),
    /// The newest Timeout entry for this member (hex), setting it to `until`.
    Timeout(String, u64),
    /// A member's Timeout to `until` ended: lifted by whoever wrote an entry ending it, or lapsed.
    TimeoutEnded(String, u64),
    Nickname(String),
    Unknown,
}

/// A Control entity, by what derives its id.
#[derive(Clone)]
enum Locator {
    Metadata,
    Everyone,
    Banlist,
    /// The entity id is the thing's own id: a Role, Channel or Category.
    Raw(String),
    Grant(String),
    Acl(String, OverwriteTarget),
    Sync(String),
    Pins(String),
}

impl Locator {
    fn entity(&self, cid: &CommunityId) -> Option<[u8; 32]> {
        let id32 = |h: &str| crate::simd::hex::hex_to_bytes_32_checked(h);
        Some(match self {
            Locator::Metadata => cid.0,
            Locator::Everyone => derive::everyone_locator(cid),
            Locator::Banlist => derive::banlist_locator(cid),
            Locator::Raw(id) => id32(id)?,
            Locator::Grant(member) => derive::grant_locator(cid, &id32(member)?),
            Locator::Acl(scope, target) => derive::acl_locator(cid, &id32(scope)?, &super::roles::overwrite_target_bytes(cid, target)?),
            Locator::Sync(channel) => derive::sync_locator(cid, &ChannelId(id32(channel)?)),
            Locator::Pins(channel) => derive::pins_locator(cid, &ChannelId(id32(channel)?)),
        })
    }
}

/// One edition a fold advanced an entity through.
#[derive(Clone)]
struct Mark {
    version: u64,
    author: PublicKey,
    at_ms: u64,
}

/// Name each change's author and time from what this device holds: the editions the folds since
/// the last look advanced its entity through (`marks`), else the head of the Control entity that
/// carries it, or the Guestbook entry. Each source is read once, and only when asked. `seen_at`
/// dates what has no time of its own.
fn attribute(community: &CommunityV2, changes: Vec<(ChangeKind, Source)>, marks: &HashMap<String, Vec<Mark>>, seen_at: u64) -> Vec<CommunityChange> {
    let cid = community.id();
    let cid_hex = hex(&cid.0);
    let heads = OnceCell::new();
    let guestbook = OnceCell::new();
    let nicknames = OnceCell::new();
    let head_author = |entity: &str| -> Option<String> {
        let heads = heads.get_or_init(|| crate::db::community::get_all_edition_heads_full(&cid_hex).unwrap_or_default());
        let author = heads.get(entity)?.4?;
        PublicKey::from_slice(&author).ok().map(|p| npub(&p))
    };
    let judges = OnceCell::new();
    // The newest Timeout or Nickname entry aimed at `member` (hex) that its judge lets stand.
    let newest = |member: &str, timeout: bool| -> Option<(PublicKey, u64, Option<u64>, String)> {
        use super::guestbook::GuestbookEntry;
        let events = guestbook.get_or_init(|| crate::db::community::get_guestbook_standing(&cid_hex).ok());
        let (may_time_out, may_name) = judges.get_or_init(|| (super::service::timeout_judge(community).ok(), super::service::nickname_judge(community).ok()));
        let target = PublicKey::from_hex(member).ok()?;
        events
            .as_ref()?
            .iter()
            .filter_map(|e| match &e.entry {
                GuestbookEntry::Timeout { actor, target: t, at_ms, until_secs, citation } if timeout && *t == target => {
                    may_time_out.as_ref().is_some_and(|j| j(actor, t, citation.as_ref())).then_some((*actor, *at_ms, Some(*until_secs), String::new()))
                }
                GuestbookEntry::Nickname { actor, target: t, at_ms, nick, citation } if !timeout && *t == target => {
                    may_name.as_ref().is_some_and(|j| j(actor, t, citation.as_ref())).then(|| (*actor, *at_ms, None, nick.clone()))
                }
                _ => None,
            })
            .max_by_key(|(_, at, _, _)| *at)
    };
    // Something moved without one entity to name (a reorder) happened by the newest fold.
    let newest_fold = marks.values().flatten().map(|m| m.at_ms).max().unwrap_or(seen_at);
    changes
        .into_iter()
        .map(|(kind, source)| {
            let (by, at, also_by) = match source {
                Source::Entity(l) => {
                    let entity = l.entity(cid).map(|e| hex(&e));
                    match entity.as_ref().and_then(|e| marks.get(e)).and_then(|m| m.last().map(|last| (m, last))) {
                        Some((all, last)) => {
                            let mut others: Vec<String> = Vec::new();
                            for m in &all[..all.len() - 1] {
                                let who = npub(&m.author);
                                if m.author != last.author && !others.contains(&who) {
                                    others.push(who);
                                }
                            }
                            (Some(npub(&last.author)), last.at_ms, others)
                        }
                        None => (entity.as_deref().and_then(head_author), seen_at, Vec::new()),
                    }
                }
                Source::Timeout(member, until) => match newest(&member, true).filter(|(_, _, u, _)| *u == Some(until)) {
                    Some((actor, at_ms, ..)) => (Some(npub(&actor)), at_ms, Vec::new()),
                    None => (None, seen_at, Vec::new()),
                },
                // Lifted by whoever wrote an entry that ends it at once; a lapse has no author.
                Source::TimeoutEnded(member, until) => {
                    match newest(&member, true).filter(|(_, at_ms, u, _)| u.is_some_and(|u| u <= at_ms / 1000)) {
                        Some((actor, at_ms, ..)) => (Some(npub(&actor)), at_ms, Vec::new()),
                        None => (None, until.saturating_mul(1000).min(seen_at), Vec::new()),
                    }
                }
                Source::Nickname(member) => {
                    let standing = nicknames.get_or_init(|| super::service::nicknames_as_set(community).unwrap_or_default());
                    let at = newest(&member, false).map_or(seen_at, |(_, at_ms, ..)| at_ms);
                    let by = match PublicKey::from_hex(&member).ok().and_then(|pk| standing.get(&pk)).filter(|n| !n.nick.is_empty()) {
                        Some(n) => Some(npub(&n.actor)),
                        None => newest(&member, false).filter(|(.., nick)| nick.is_empty()).map(|(a, ..)| npub(&a)),
                    };
                    (by, at, Vec::new())
                }
                Source::Unknown => (None, newest_fold, Vec::new()),
            };
            CommunityChange { by, at, also_by, kind }
        })
        .collect()
}

#[derive(Default)]
struct Seen(Mutex<HashMap<[u8; 32], Snapshot>>);

/// Changes reported by the work that made them rather than by a comparison, waiting for a
/// handler: `(community id, change)`.
#[derive(Default)]
struct Pending(Mutex<std::collections::VecDeque<(String, CommunityChange)>>);

/// What waits for a handler that never comes (the app's) stays bounded.
const MAX_PENDING: usize = 256;

fn pending() -> std::sync::Arc<Pending> {
    crate::db::current_session().scoped::<Pending, _>()
}

/// A Channel this device just purged as deleted. `community` is the document from before.
pub(crate) fn note_channel_deleted(community: &CommunityV2, channel: &ChannelId, deleted_by: Option<&PublicKey>) {
    let ch_hex = hex(&channel.0);
    let Some(info) = channel_infos(community).into_iter().find(|c| c.channel_id == ch_hex) else { return };
    let cid_hex = hex(&community.id().0);
    // The deleting edition, if a fold this session just went through it.
    let folded_by = folded().0.lock().unwrap_or_else(|e| e.into_inner()).get(&community.id().0).and_then(|e| e.get(&ch_hex)?.last().cloned());
    let (by, at) = match (deleted_by, folded_by) {
        (Some(pk), _) => (Some(npub(pk)), crate::clock::now_ms()),
        (None, Some(mark)) => (Some(npub(&mark.author)), mark.at_ms),
        (None, None) => (
            crate::db::community::get_all_edition_heads_full(&cid_hex)
                .ok()
                .and_then(|heads| heads.get(&ch_hex)?.4)
                .and_then(|author| PublicKey::from_slice(&author).ok())
                .map(|pk| npub(&pk)),
            crate::clock::now_ms(),
        ),
    };
    // Two purges racing for one Channel report it once.
    let reported = crate::db::current_session().scoped::<Pending, Mutex<std::collections::HashSet<(String, String)>>>();
    if !reported.lock().unwrap_or_else(|e| e.into_inner()).insert((cid_hex.clone(), ch_hex)) {
        return;
    }
    let pending = pending();
    let mut queue = pending.0.lock().unwrap_or_else(|e| e.into_inner());
    if queue.len() >= MAX_PENDING {
        queue.pop_front();
    }
    queue.push_back((cid_hex, CommunityChange::new(by, at, ChangeKind::ChannelDeleted { channel: info })));
}

/// Communities a fold is writing now: a look then would see half of it.
#[derive(Default)]
struct Persisting(Mutex<std::collections::HashSet<[u8; 32]>>);

fn persisting() -> std::sync::Arc<Persisting> {
    crate::db::current_session().scoped::<Persisting, _>()
}

/// Held while a fold writes `community`: a look waits for the fold's own, after it.
pub(crate) struct PersistGuard(Option<[u8; 32]>);

pub(crate) fn persist_guard(community: &CommunityId) -> PersistGuard {
    if !wanted() {
        return PersistGuard(None);
    }
    persisting().0.lock().unwrap_or_else(|e| e.into_inner()).insert(community.0);
    PersistGuard(Some(community.0))
}

impl Drop for PersistGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            persisting().0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        }
    }
}

/// Each entity's marks, by entity id.
type Marks = HashMap<String, Vec<Mark>>;

/// The editions folds advanced each entity through since the last look, per Community.
#[derive(Default)]
struct Folded(Mutex<HashMap<[u8; 32], Marks>>);

/// What one entity remembers between looks: a fold rarely carries more.
const MAX_MARKS: usize = 64;

fn folded() -> std::sync::Arc<Folded> {
    crate::db::current_session().scoped::<Folded, _>()
}

/// Whether a handler in this session wants the feed: a fold records its editions only then.
pub(crate) fn wanted() -> bool {
    crate::db::current_session().scoped::<Wanted, Wanted>().0.load(std::sync::atomic::Ordering::Relaxed)
}

/// The editions a fold just advanced `entity_hex` through: `(version, author, unix secs)`. A
/// version a fork settled differently since replaces what was noted for it.
pub(crate) fn note_folded(community: &CommunityId, entity_hex: &str, chain: Vec<(u64, PublicKey, u64)>) {
    if chain.is_empty() {
        return;
    }
    let log = folded();
    let mut held = log.0.lock().unwrap_or_else(|e| e.into_inner());
    let marks = held.entry(community.0).or_default().entry(entity_hex.to_string()).or_default();
    for (version, author, secs) in chain {
        marks.retain(|m| m.version != version);
        marks.push(Mark { version, author, at_ms: secs.saturating_mul(1000) });
    }
    marks.sort_by_key(|m| m.version);
    let over = marks.len().saturating_sub(MAX_MARKS);
    marks.drain(..over);
}

/// What the log holds for `entity_hex`: `(version, author)`, oldest first.
#[cfg(test)]
pub(crate) fn folded_marks(community: &CommunityId, entity_hex: &str) -> Vec<(u64, PublicKey)> {
    folded().0.lock().unwrap().get(&community.0).and_then(|e| e.get(entity_hex)).map(|m| m.iter().map(|m| (m.version, m.author)).collect()).unwrap_or_default()
}

/// The marks each entity moved through between `was` and `now`, taken. Ones past `now` wait for
/// the look that reads them, and ones at or below `was` are spent.
fn take_marks(community: &[u8; 32], was: &Snapshot, now: &Snapshot) -> HashMap<String, Vec<Mark>> {
    let log = folded();
    let mut held = log.0.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entities) = held.get_mut(community) else { return HashMap::new() };
    let mut out = HashMap::new();
    entities.retain(|entity, marks| {
        let floor = was.heads.get(entity).copied().unwrap_or(0);
        let head = now.heads.get(entity).copied().unwrap_or(0);
        let (used, later): (Vec<Mark>, Vec<Mark>) = marks.drain(..).filter(|m| m.version > floor).partition(|m| m.version <= head);
        if !used.is_empty() {
            out.insert(entity.clone(), used);
        }
        *marks = later;
        !marks.is_empty()
    });
    out
}

/// Hand what waits to `handler`: for one Community, or (`None`) all of them.
fn deliver_pending(handler: &dyn InboundEventHandler, community_id: Option<&str>) {
    if !handler.wants_community_changes() {
        return;
    }
    let taken: Vec<(String, CommunityChange)> = {
        let pending = pending();
        let mut queue = pending.0.lock().unwrap_or_else(|e| e.into_inner());
        match community_id {
            None => queue.drain(..).collect(),
            Some(cid) => {
                let (mine, rest): (Vec<_>, Vec<_>) = queue.drain(..).partition(|(c, _)| c == cid);
                queue.extend(rest);
                mine
            }
        }
    };
    for (cid, change) in taken.iter().filter(|(_, c)| !made_here(c) && crate::news::is_news(c.at)) {
        handler.on_community_change(cid, change);
    }
}

fn seen() -> std::sync::Arc<Seen> {
    crate::db::current_session().scoped::<Seen, _>()
}

/// Whether a handler in this session asked for the feed: without one, nothing is read for it.
#[derive(Default)]
struct Wanted(std::sync::atomic::AtomicBool);

/// Take `community`'s baseline, if the feed is wanted and has none: a Community joined while
/// listening reports from its join on.
pub(crate) fn prime(community: &CommunityV2) {
    if !crate::db::current_session().scoped::<Wanted, Wanted>().0.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let seen = seen();
    let mut held = seen.0.lock().unwrap_or_else(|e| e.into_inner());
    held.entry(community.id().0).or_insert_with(|| read(community));
}

/// Take the baseline for every Community held, so the first refresh of each reports its
/// changes rather than becoming the baseline itself, then hand `handler` the deletions the
/// catch-up purged.
pub fn prime_all(handler: &dyn InboundEventHandler) {
    crate::db::current_session().scoped::<Wanted, Wanted>().0.store(true, std::sync::atomic::Ordering::Relaxed);
    {
        let seen = seen();
        let mut held = seen.0.lock().unwrap_or_else(|e| e.into_inner());
        for id in crate::db::community::list_community_ids().unwrap_or_default() {
            if let Ok(Some(c)) = crate::db::community::load_community_v2(&id) {
                held.entry(id.0).or_insert_with(|| read(&c));
            }
        }
    }
    deliver_pending(handler, None);
}

/// Drop what was seen of a Community this account no longer holds, so a rejoin starts fresh.
pub fn forget(community_id: &str) {
    if let Some(id) = crate::simd::hex::hex_to_bytes_32_checked(community_id) {
        seen().0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        folded().0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }
}

/// Report what changed in `community_id` since the last look, to a handler that asked for it.
pub fn notify(handler: &dyn InboundEventHandler, community_id: &str) {
    if !handler.wants_community_changes() {
        return;
    }
    let Some(id) = crate::simd::hex::hex_to_bytes_32_checked(community_id) else { return };
    if persisting().0.lock().unwrap_or_else(|e| e.into_inner()).contains(&id) {
        return;
    }
    // Everything is read under the lock: two refreshes reading out of order would report a
    // change and then its undo.
    let seen = seen();
    let mut held = seen.0.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(Some(community)) = crate::db::community::load_community_v2(&CommunityId(id)) else { return };
    let seen_at = crate::clock::now_ms();
    let now = read(&community);
    let changes = match held.insert(id, now.clone()) {
        Some(was) => {
            let marks = take_marks(&id, &was, &now);
            if was == now { Vec::new() } else { attribute(&community, diff(&was, &now), &marks, seen_at) }
        }
        // A first look is the baseline: what folded to it is spent.
        None => {
            folded().0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            Vec::new()
        }
    };
    drop(held);
    for change in changes.iter().filter(|c| !made_here(c) && crate::news::is_news(c.at)) {
        handler.on_community_change(community_id, change);
    }
    deliver_pending(handler, Some(community_id));
}

/// Communities with a look queued by [`notify_soon`].
#[derive(Default)]
struct Due(Mutex<std::collections::HashSet<[u8; 32]>>);

/// [`notify`], half a second from now and once for a burst: what any member can cause cheaply,
/// a Nickname say, never holds up the live feed.
pub fn notify_soon(handler: std::sync::Arc<dyn InboundEventHandler>, community_id: &str) {
    if !handler.wants_community_changes() {
        return;
    }
    let Some(id) = crate::simd::hex::hex_to_bytes_32_checked(community_id) else { return };
    let due = crate::db::current_session().scoped::<Due, Due>();
    if !due.0.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
        return;
    }
    let community_id = community_id.to_string();
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(std::time::Duration::from_millis(500)).await;
        due.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        notify(&*handler, &community_id);
    });
}

/// This account made it: the call that did returned its result, and the comparison would only
/// report it at whatever refresh came next.
fn made_here(change: &CommunityChange) -> bool {
    use nostr_sdk::prelude::ToBech32;
    let me = crate::state::my_public_key().and_then(|pk| pk.to_bech32().ok());
    change.by.is_some() && change.by == me
}

/// Report one change this device saw happen rather than fold, such as an authorized kick.
pub fn notify_one(handler: &dyn InboundEventHandler, community_id: &str, change: CommunityChange) {
    if handler.wants_community_changes() && crate::news::is_news(change.at) {
        handler.on_community_change(community_id, &change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::transport::memory::MemoryRelay;
    use nostr_sdk::prelude::Keys;

    fn init() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(93_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());
        (tmp, guard, me)
    }

    #[derive(Default)]
    struct Changes(Mutex<Vec<CommunityChange>>);
    impl InboundEventHandler for Changes {
        fn wants_community_changes(&self) -> bool {
            true
        }
        fn on_community_change(&self, _community_id: &str, change: &CommunityChange) {
            self.0.lock().unwrap().push(change.clone());
        }
    }

    #[tokio::test]
    async fn a_fold_reports_each_change_once_with_its_author() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Den", vec!["wss://r".into()], None).await.unwrap();
        let cid = hex(&community.id().0);
        prime_all(&Changes::default());
        let meta = super::super::control::CommunityMetadata { name: "Lair".into(), relays: vec!["wss://r".into()], ..Default::default() };
        super::super::service::edit_community_metadata(&relay, &community, &meta).await.unwrap();
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        super::super::service::create_category(&relay, &held, "Staff").await.unwrap();
        let _ = super::super::service::follow_control(&relay, &held).await;

        // Its own changes aren't news to the account that made them.
        let own = Changes::default();
        notify(&own, &cid);
        assert!(own.0.lock().unwrap().is_empty(), "{:?}", own.0.lock().unwrap());
        // Read as another member would: the same changes, credited to their author.
        forget(&cid);
        prime_all(&Changes::default());
        let category = super::super::service::create_category(&relay, &crate::db::community::load_community_v2(community.id()).unwrap().unwrap(), "Staff 2").await;
        assert!(category.is_ok());
        crate::state::set_my_public_key(Keys::generate().public_key());

        let seen = Changes::default();
        notify(&seen, &cid);
        let got = seen.0.lock().unwrap().clone();
        let owner = Some(me.public_key().to_bech32().unwrap());
        assert!(
            matches!(&got[..], [CommunityChange { by, kind: ChangeKind::CategoryCreated { category }, .. }] if category.name == "Staff 2" && *by == owner),
            "one new category, credited to its author: {got:?}"
        );

        let again = Changes::default();
        notify(&again, &cid);
        assert!(again.0.lock().unwrap().is_empty(), "a change is reported once");
    }

    #[tokio::test]
    async fn a_deleted_channel_is_reported_by_its_purge_even_unwatched() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Den", vec!["wss://r".into()], None).await.unwrap();
        let ch = super::super::service::create_public_channel(&relay, &community, "temp").await.unwrap();
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        crate::VectorCore.register_v2_chats(&held, &crate::db::current_session()).await;
        // Deleted before any baseline: a catch-up fold, as far as the feed can tell.
        super::super::service::delete_channel(&relay, &held, &ch, "temp").await.unwrap();
        crate::state::set_my_public_key(Keys::generate().public_key());

        let seen = Changes::default();
        prime_all(&seen);
        let got = seen.0.lock().unwrap().clone();
        let owner = Some(me.public_key().to_bech32().unwrap());
        assert!(
            matches!(&got[..], [CommunityChange { by, kind: ChangeKind::ChannelDeleted { channel }, .. }] if channel.name == "temp" && *by == owner),
            "{got:?}"
        );
        let again = Changes::default();
        prime_all(&again);
        assert!(again.0.lock().unwrap().is_empty(), "once");
    }

    #[tokio::test]
    async fn a_change_several_people_made_names_each_and_when() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Den", vec!["wss://r".into()], None).await.unwrap();
        let (a, b) = (Keys::generate().public_key(), Keys::generate().public_key());
        let mark = |version, author, at_ms| Mark { version, author, at_ms };
        let marks: HashMap<String, Vec<Mark>> = [(hex(&community.id().0), vec![mark(2, a, 1_000), mark(3, b, 2_000), mark(4, a, 3_000)])].into();
        let renamed = || vec![(ChangeKind::Renamed { old: "Den".into(), new: "Lair".into() }, Source::Entity(Locator::Metadata))];

        let got = attribute(&community, renamed(), &marks, 9_000);
        assert_eq!((got[0].by.clone(), got[0].at, got[0].also_by.clone()), (Some(npub(&a)), 3_000, vec![npub(&b)]), "the last edit's author and time, and the other editor");

        let got = attribute(&community, renamed(), &HashMap::new(), 9_000);
        assert_eq!((got[0].by.clone(), got[0].at), (Some(npub(&me.public_key())), 9_000), "with no fold to read: the head's author, dated when seen");
    }

    #[tokio::test]
    async fn a_look_while_a_fold_writes_waits_for_the_folds_own() {
        let (_tmp, _guard, _me) = init();
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Den", vec!["wss://r".into()], None).await.unwrap();
        let cid = hex(&community.id().0);
        prime_all(&Changes::default());
        super::super::service::create_category(&relay, &community, "Mid-write").await.unwrap();
        crate::state::set_my_public_key(Keys::generate().public_key());

        let writing = persist_guard(community.id());
        let early = Changes::default();
        notify(&early, &cid);
        assert!(early.0.lock().unwrap().is_empty(), "half a fold is not looked at");
        drop(writing);
        let after = Changes::default();
        notify(&after, &cid);
        assert!(matches!(&after.0.lock().unwrap()[..], [CommunityChange { kind: ChangeKind::CategoryCreated { .. }, .. }]), "the baseline held: the fold's own look reports it");
    }

    #[test]
    fn a_look_takes_the_editions_since_the_last_and_later_ones_wait() {
        let (_tmp, _guard, _me) = init();
        let (cid, entity, pk) = (CommunityId([7; 32]), "ee".repeat(32), Keys::generate().public_key());
        let heads = |v: u64| Snapshot { heads: [(entity.clone(), v)].into(), ..Default::default() };
        let versions = |taken: &HashMap<String, Vec<Mark>>| taken.get(&entity).map(|m| m.iter().map(|m| m.version).collect::<Vec<_>>()).unwrap_or_default();
        note_folded(&cid, &entity, vec![(1, pk, 1), (2, pk, 2), (3, pk, 3)]);

        assert_eq!(versions(&take_marks(&cid.0, &heads(1), &heads(2))), vec![2], "the one between the looks; the first was already seen");
        assert_eq!(versions(&take_marks(&cid.0, &heads(2), &heads(3))), vec![3], "the next waited for its look");
        assert!(take_marks(&cid.0, &heads(3), &heads(3)).is_empty());
    }

    fn role(id: &str, name: &str, position: u32, perms: u64) -> Role {
        Role {
            role_id: id.into(),
            name: name.into(),
            position,
            permissions: Permissions(perms),
            scope: crate::community::roles::RoleScope::Server,
            color: 0,
            rank: None,
            display: false,
            mentionable: false,
            extra: Default::default(),
        }
    }

    fn kinds(old: &Snapshot, new: &Snapshot) -> Vec<ChangeKind> {
        diff(old, new).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn an_unchanged_snapshot_reports_nothing() {
        let s = Snapshot { name: "Den".into(), ..Default::default() };
        assert!(kinds(&s, &s.clone()).is_empty());
    }

    #[test]
    fn roles_report_create_update_and_delete() {
        let a = "aa".repeat(32);
        let b = "bb".repeat(32);
        let old = Snapshot { roles: [(a.clone(), role(&a, "Mods", 1, 8))].into(), ..Default::default() };
        let new = Snapshot { roles: [(a.clone(), role(&a, "Moderators", 1, 8)), (b.clone(), role(&b, "VIP", 2, 0))].into(), ..Default::default() };
        let got = kinds(&old, &new);
        assert!(got.iter().any(|k| matches!(k, ChangeKind::RoleUpdated { new, .. } if new.name == "Moderators")));
        assert!(got.iter().any(|k| matches!(k, ChangeKind::RoleCreated { role } if role.name == "VIP")));
        assert!(kinds(&new, &old).iter().any(|k| matches!(k, ChangeKind::RoleDeleted { role } if role.name == "VIP")));
    }

    #[test]
    fn a_reorder_is_one_change_not_one_per_role() {
        let (a, b) = ("aa".repeat(32), "bb".repeat(32));
        let ranked = |id: &str, pos: u32, rank: &str| Role { rank: Some(rank.into()), ..role(id, id, pos, 0) };
        let old = Snapshot { roles: [(a.clone(), ranked(&a, 1, "m")), (b.clone(), ranked(&b, 2, "t"))].into(), role_order: vec![a.clone(), b.clone()], ..Default::default() };
        let new = Snapshot { roles: [(a.clone(), ranked(&a, 2, "x")), (b.clone(), ranked(&b, 1, "t"))].into(), role_order: vec![b.clone(), a.clone()], ..Default::default() };
        assert_eq!(kinds(&old, &new), vec![ChangeKind::RolesReordered { order: vec![b, a] }]);
    }

    #[test]
    fn a_new_channel_moves_nothing_and_a_swap_does() {
        let (x, y, z) = ("x".repeat(64), "y".repeat(64), "z".repeat(64));
        let at = |ids: &[&String]| Snapshot { layout: ids.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        assert!(!kinds(&at(&[&x, &y]), &at(&[&x, &z, &y])).contains(&ChangeKind::LayoutChanged), "an insertion keeps the others' order");
        assert!(kinds(&at(&[&x, &y]), &at(&[&y, &x])).contains(&ChangeKind::LayoutChanged));
    }

    #[test]
    fn a_grant_reports_only_the_roles_it_moved() {
        let m = "cc".repeat(32);
        let old = Snapshot { grants: [(m.clone(), ["r1".to_string(), "r2".to_string()].into())].into(), ..Default::default() };
        let new = Snapshot { grants: [(m.clone(), ["r2".to_string(), "r3".to_string()].into())].into(), ..Default::default() };
        let got = kinds(&old, &new);
        assert_eq!(got.len(), 1);
        let ChangeKind::MemberRolesChanged { added, removed, .. } = &got[0] else { panic!("{got:?}") };
        assert_eq!((added.as_slice(), removed.as_slice()), (&["r3".to_string()][..], &["r1".to_string()][..]));
    }

    #[test]
    fn bans_timeouts_and_nicknames_read_both_ways() {
        let m = "dd".repeat(32);
        let old = Snapshot { nicknames: [(m.clone(), "kit".to_string())].into(), ..Default::default() };
        let new = Snapshot { banned: [m.clone()].into(), timeouts: [(m.clone(), 99)].into(), ..Default::default() };
        let got = kinds(&old, &new);
        assert!(got.iter().any(|k| matches!(k, ChangeKind::MemberBanned { .. })));
        assert!(got.iter().any(|k| matches!(k, ChangeKind::TimedOut { until: 99, .. })));
        assert!(got.iter().any(|k| matches!(k, ChangeKind::NicknameChanged { nickname: None, .. })));
        let back = kinds(&new, &old);
        assert!(back.iter().any(|k| matches!(k, ChangeKind::MemberUnbanned { .. })));
        assert!(back.iter().any(|k| matches!(k, ChangeKind::TimeoutEnded { .. })));
    }

    #[test]
    fn a_removed_overwrite_reports_empty_bits() {
        let key = ("ee".repeat(32), OverwriteTarget::Everyone);
        let old = Snapshot { overwrites: [(key, (Permissions(1 << 14), Permissions(0)))].into(), ..Default::default() };
        let got = kinds(&old, &Snapshot::default());
        assert!(matches!(&got[..], [ChangeKind::OverwriteChanged { allow: Permissions(0), deny: Permissions(0), .. }]));
    }

    #[test]
    fn a_new_channel_with_pins_reports_the_channel_not_the_pins() {
        let ch = "ff".repeat(32);
        let info = ChannelInfo { channel_id: ch.clone(), name: "general".into(), private: false, category_id: None, topic: None, view: "chat".into() };
        let new = Snapshot { channels: [(ch.clone(), info)].into(), pins: [(ch.clone(), 1)].into(), layout: vec![ch.clone()], ..Default::default() };
        let got = kinds(&Snapshot::default(), &new);
        assert!(got.iter().any(|k| matches!(k, ChangeKind::ChannelCreated { .. })));
        assert!(!got.iter().any(|k| matches!(k, ChangeKind::PinsChanged { .. })));
    }
}
