//! Notification levels and mutes, for communities, channels and DMs alike.
//!
//! Two settings per scope, doing different jobs. **Level** decides what rings:
//! every message, only your name, or nothing. **Mute** greys the row, drops its
//! unread weight, and clamps the level to at most `Mentions` — a volume knob,
//! not an off switch, which is why `Nothing` is not a synonym for it.
//!
//! Level resolves down the chain Community → Channel, most specific opinion
//! winning. Mute unions down it: muting a community mutes every channel in it.
//! A scope with no stored row inherits everything, so an untouched account
//! behaves exactly as it did before any of this existed.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// What rings. Ordered loudest first, so the numeric rank orders "quieter".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum NotifyLevel {
    All = 0,
    Mentions = 1,
    Nothing = 2,
}

impl NotifyLevel {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::All),
            1 => Some(Self::Mentions),
            2 => Some(Self::Nothing),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Nothing => "nothing",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "mentions" => Some(Self::Mentions),
            "nothing" | "none" => Some(Self::Nothing),
            _ => None,
        }
    }

    /// The quieter of the two, which is how a mute clamps a level.
    pub fn quieter_of(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }
}

/// What a message is, loudest first. Resolved once at classification so a
/// suppressed @everyone is `Normal` everywhere downstream: notification,
/// ping badge and muted-row count alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageClass {
    Everyone,
    Mention,
    Normal,
}

/// Whether a message of this class rings at this level.
pub fn passes(level: NotifyLevel, class: MessageClass) -> bool {
    match level {
        NotifyLevel::All => true,
        NotifyLevel::Mentions => !matches!(class, MessageClass::Normal),
        NotifyLevel::Nothing => false,
    }
}

/// `mute_until` sentinel for a mute with no end.
pub const MUTE_FOREVER: i64 = -1;
/// `mute_until` sentinel for "not muted".
pub const MUTE_OFF: i64 = 0;

pub fn is_muted_at(mute_until: i64, now_ms: i64) -> bool {
    mute_until == MUTE_FOREVER || mute_until > now_ms
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One scope's stored settings. `None` means inherit; `suppress_everyone` is
/// only ever set at community scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopePrefs {
    pub level: Option<NotifyLevel>,
    pub mute_until: i64,
    pub suppress_everyone: Option<bool>,
}

impl ScopePrefs {
    pub fn is_default(&self) -> bool {
        self.level.is_none() && self.mute_until == MUTE_OFF && self.suppress_everyone.is_none()
    }
    pub fn muted_at(&self, now_ms: i64) -> bool {
        is_muted_at(self.mute_until, now_ms)
    }
}

/// A scope's settings after the chain has been walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// The level in force before the mute clamp, which is what a menu ticks.
    pub level: NotifyLevel,
    pub muted: bool,
    /// The level after the clamp, which is what actually rings and badges.
    pub ring: NotifyLevel,
}

// ============================================================================
// Per-account cache
// ============================================================================

#[derive(Default)]
struct Store {
    loaded: bool,
    scopes: HashMap<String, ScopePrefs>,
    /// Channel ids their community still lists. A tombstoned channel keeps its
    /// chat row and its history, so "has a `community_id`" is not the same
    /// question as "has a row you can open and clear".
    live_channels: Option<std::collections::HashSet<String>>,
}

struct NotifyStore;

fn store() -> Arc<RwLock<Store>> {
    crate::db::current_session().scoped::<NotifyStore, _>()
}

/// Read the table into memory. Called once when a database is opened, because
/// every badge recount resolves every chat and a lazy load would put a SQLite
/// round trip on that path.
///
/// A failed read leaves the cache unloaded rather than latching an empty map,
/// and [`is_loaded`] is what tells a writer not to act on the emptiness.
pub fn warm() {
    let cell = store();
    if cell.read().map(|g| g.loaded).unwrap_or(false) {
        return;
    }
    let rows = match crate::db::notify::load_all() {
        Ok(rows) => rows,
        Err(_) => return,
    };
    if let Ok(mut g) = cell.write() {
        g.scopes = rows;
        g.loaded = true;
    };
}

/// Whether the table has actually been read. An unloaded cache answers
/// "default" for every scope, which is indistinguishable from "nothing is
/// stored" — so anything that WRITES state derived from it must check this
/// first, the same rule `synced_prefs::is_hydrated` applies to publishing.
pub fn is_loaded() -> bool {
    store().read().map(|g| g.loaded).unwrap_or(false)
}

/// Drop the cache so the next warm re-reads the table.
pub fn invalidate() {
    if let Ok(mut g) = store().write() {
        g.loaded = false;
        g.scopes.clear();
        g.live_channels = None;
    }
}

/// Forget which channels are listed; their community's set was rewritten.
pub fn invalidate_live_channels() {
    if let Ok(mut g) = store().write() {
        g.live_channels = None;
    }
}

/// Whether `channel_id` is one its community still lists. A channel nobody
/// lists has no row to open, so its unread can never be seen or cleared.
pub fn channel_is_listed(channel_id: &str) -> bool {
    let cell = store();
    if let Ok(g) = cell.read() {
        if let Some(set) = g.live_channels.as_ref() {
            return set.contains(channel_id);
        }
    }
    // Unknown beats silently dropping a room's unread.
    let ids = match crate::db::notify::live_channel_ids() {
        Ok(ids) => ids,
        Err(_) => return true,
    };
    let listed = ids.contains(channel_id);
    if let Ok(mut g) = cell.write() {
        g.live_channels = Some(ids);
    }
    listed
}

pub fn prefs(scope_id: &str) -> ScopePrefs {
    store()
        .read()
        .ok()
        .and_then(|g| g.scopes.get(scope_id).copied())
        .unwrap_or_default()
}

pub fn all_prefs() -> HashMap<String, ScopePrefs> {
    store().read().map(|g| g.scopes.clone()).unwrap_or_default()
}

/// Write a scope through to the database and the cache.
pub fn set_prefs(scope_id: &str, next: ScopePrefs) -> Result<(), String> {
    warm();
    crate::db::notify::save(scope_id, &next)?;
    if let Ok(mut g) = store().write() {
        if next.is_default() {
            g.scopes.remove(scope_id);
        } else {
            g.scopes.insert(scope_id.to_string(), next);
        }
    }
    Ok(())
}

pub fn set_level(scope_id: &str, level: Option<NotifyLevel>) -> Result<(), String> {
    let mut next = prefs(scope_id);
    next.level = level;
    set_prefs(scope_id, next)
}

pub fn set_mute(scope_id: &str, mute_until: i64) -> Result<(), String> {
    let mut next = prefs(scope_id);
    next.mute_until = mute_until;
    set_prefs(scope_id, next)
}

pub fn set_suppress_everyone(scope_id: &str, suppress: Option<bool>) -> Result<(), String> {
    let mut next = prefs(scope_id);
    next.suppress_everyone = suppress;
    set_prefs(scope_id, next)
}

// ============================================================================
// Resolution
// ============================================================================

/// Whether an authorized @everyone may ping, for a community. Two levers: the
/// App Settings kill switch, which wins outright, and the community's own
/// choice under it. A channel has no say, deliberately.
///
/// Separate from [`resolve`] because it reads a setting from the database and
/// `resolve` runs once per chat on every badge recount.
pub fn everyone_allowed(community_id: Option<&str>) -> bool {
    let globally_muted = crate::db::settings::get_sql_setting("notif_mute_everyone".to_string())
        .ok()
        .flatten()
        .map_or(false, |v| v == "true");
    if globally_muted {
        return false;
    }
    !community_id.map_or(false, |cid| prefs(cid).suppress_everyone.unwrap_or(false))
}

/// Walk the chain for one scope. `community_id` is the channel's community, or
/// `None` for a DM or for a community resolving itself.
pub fn resolve(scope_id: &str, community_id: Option<&str>, now_ms: i64) -> Resolved {
    let own = prefs(scope_id);
    let parent = community_id
        .filter(|cid| *cid != scope_id)
        .map(prefs)
        .unwrap_or_default();
    resolve_from(own, parent, now_ms)
}

/// The chain arithmetic on its own: level takes the most specific opinion,
/// mute takes either, and the mute clamps the level it hands back.
pub fn resolve_from(own: ScopePrefs, parent: ScopePrefs, now_ms: i64) -> Resolved {
    let level = own.level.or(parent.level).unwrap_or(NotifyLevel::All);
    let muted = own.muted_at(now_ms) || parent.muted_at(now_ms);
    let ring = level.quieter_of(if muted { NotifyLevel::Mentions } else { NotifyLevel::All });
    Resolved { level, muted, ring }
}

/// The community a chat belongs to, or `None` for a DM.
fn community_of(chat: &crate::chat::Chat) -> Option<&str> {
    chat.metadata.custom_fields.get("community_id").map(|s| s.as_str())
}

/// A chat's settings, with its stored `muted` flag folded in. That flag is the
/// mirror this module keeps for the paths that predate it, and the landing
/// point for a mute arriving over the older `vector/mutes` projection, so it
/// counts as a mute in its own right rather than being assumed to agree.
pub fn resolve_chat(chat: &crate::chat::Chat) -> Resolved {
    let mut resolved = resolve(&chat.id, community_of(chat), now_ms());
    if chat.muted && !resolved.muted {
        resolved.muted = true;
        resolved.ring = resolved.level.quieter_of(NotifyLevel::Mentions);
    }
    resolved
}

/// What a chat's row should badge with: full unread at `All`, pings at
/// `Mentions`, nothing at `Nothing`.
pub fn ring_for_chat(chat: &crate::chat::Chat) -> NotifyLevel {
    resolve_chat(chat).ring
}

pub fn muted_for_chat(chat: &crate::chat::Chat) -> bool {
    resolve_chat(chat).muted
}

/// Whether a chat's COMMUNITY is itself silenced, rather than this one room inside it.
/// A surface that stands for the whole space must not grey because one channel is quiet.
pub fn community_muted_for_chat(chat: &crate::chat::Chat) -> bool {
    community_of(chat).map_or(false, |id| prefs(id).muted_at(now_ms()))
}

/// The predicate the older `vector/mutes` projection is both WRITTEN and READ
/// with. Publishing one shape and applying another is how an inherited mute
/// ratchets into a permanent per-channel one.
///
/// Chain-resolved, so a client that predates levels silences the same rooms
/// this one does. Indefinite-only, because that list cannot carry a deadline:
/// a timed mute in it comes back as a mute with no end, and the expiry that
/// should have corrected it happens at boot, where publishing is not allowed.
pub fn in_legacy_mute_list(own: ScopePrefs, parent: ScopePrefs, now_ms: i64) -> bool {
    resolve_from(own, parent, now_ms).muted
        && (own.mute_until == MUTE_FOREVER || parent.mute_until == MUTE_FOREVER)
}

/// [`in_legacy_mute_list`] for a chat, stored flag folded in like [`resolve_chat`].
pub fn legacy_muted_for_chat(chat: &crate::chat::Chat) -> bool {
    let own = prefs(&chat.id);
    let parent = community_of(chat)
        .filter(|cid| *cid != chat.id)
        .map(prefs)
        .unwrap_or_default();
    in_legacy_mute_list(own, parent, now_ms())
        // A mute that only exists as the legacy mirror is indefinite by nature.
        || (chat.muted && !resolve_from(own, parent, now_ms()).muted)
}

/// Whether an @everyone in this chat still counts as someone calling your name.
///
/// Reads the community's own setting only, never the App Settings switch: this
/// runs once per chat on every serialization, and that switch lives in the
/// database. It has never affected badge counts anyway, only delivery.
pub fn everyone_pings_for_chat(chat: &crate::chat::Chat) -> bool {
    !community_of(chat)
        .map(prefs)
        .and_then(|p| p.suppress_everyone)
        .unwrap_or(false)
}

// ============================================================================
// Expiry
// ============================================================================

/// The soonest mute expiry still ahead of `now_ms`, for arming one timer
/// rather than one per mute.
pub fn next_expiry(now_ms: i64) -> Option<i64> {
    store().read().ok().and_then(|g| {
        g.scopes
            .values()
            .map(|p| p.mute_until)
            .filter(|&u| u > now_ms)
            .min()
    })
}

/// Clear every mute whose timer has run out, returning the scopes that just
/// lapsed. Expiry is a write rather than a read: the callers of `muted` read a
/// cached boolean, and making them all consult the clock instead would be the
/// wrong fix.
pub fn sweep_expired(now_ms: i64) -> Vec<String> {
    warm();
    let cell = store();
    let lapsed: Vec<String> = match cell.read() {
        Ok(g) => g
            .scopes
            .iter()
            .filter(|(_, p)| p.mute_until > MUTE_OFF && p.mute_until <= now_ms)
            .map(|(id, _)| id.clone())
            .collect(),
        Err(_) => return Vec::new(),
    };
    if lapsed.is_empty() {
        return lapsed;
    }
    if crate::db::notify::clear_expired_mutes(now_ms).is_err() {
        return Vec::new();
    }
    if let Ok(mut g) = cell.write() {
        for id in &lapsed {
            let drop_row = match g.scopes.get_mut(id) {
                Some(p) => {
                    p.mute_until = MUTE_OFF;
                    p.is_default()
                }
                None => false,
            };
            if drop_row {
                g.scopes.remove(id);
            }
        }
    }
    lapsed
}

// ============================================================================
// Cross-device projection
// ============================================================================

impl ScopePrefs {
    pub fn to_wire(self) -> crate::synced_prefs::NotifyEntry {
        crate::synced_prefs::NotifyEntry {
            level: self.level.map(|l| l.as_str().to_string()),
            mute_until: self.mute_until,
            suppress_everyone: self.suppress_everyone,
        }
    }

    pub fn from_wire(entry: &crate::synced_prefs::NotifyEntry) -> Self {
        Self {
            level: entry.level.as_deref().and_then(NotifyLevel::from_label),
            mute_until: entry.mute_until,
            suppress_everyone: entry.suppress_everyone,
        }
    }
}

/// This device's table as the publishable projection.
pub fn to_wire() -> crate::synced_prefs::NotifyMap {
    let mut map = crate::synced_prefs::NotifyMap::default();
    for (scope_id, prefs) in all_prefs() {
        let _ = map.set(&scope_id, prefs.to_wire());
    }
    map
}

/// Adopt a sibling device's list wholesale. Returns the scopes whose settings
/// actually moved, so the caller only has to repaint those.
pub fn apply_wire(map: &crate::synced_prefs::NotifyMap) -> Result<Vec<String>, String> {
    // Adopting a list REPLACES the table, so an unloaded cache would compare the
    // incoming set against an emptiness that only means "could not read" and
    // report nothing moved.
    warm();
    if !is_loaded() {
        return Err("notification preferences not read yet".to_string());
    }
    let next: HashMap<String, ScopePrefs> = map
        .scopes
        .iter()
        .map(|(id, entry)| (id.clone(), ScopePrefs::from_wire(entry)))
        .filter(|(_, p)| !p.is_default())
        .collect();
    let before = all_prefs();
    crate::db::notify::replace_all(&next)?;
    if let Ok(mut g) = store().write() {
        g.scopes = next.clone();
        g.loaded = true;
    }
    let mut moved: Vec<String> = next
        .iter()
        .filter(|(id, p)| before.get(*id) != Some(p))
        .map(|(id, _)| id.clone())
        .collect();
    moved.extend(before.keys().filter(|id| !next.contains_key(*id)).cloned());
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_clamps_the_level_to_mentions() {
        assert_eq!(NotifyLevel::All.quieter_of(NotifyLevel::Mentions), NotifyLevel::Mentions);
        // Nothing is already quieter than the clamp, so a mute cannot make it louder.
        assert_eq!(NotifyLevel::Nothing.quieter_of(NotifyLevel::Mentions), NotifyLevel::Nothing);
        assert_eq!(NotifyLevel::Mentions.quieter_of(NotifyLevel::All), NotifyLevel::Mentions);
    }

    #[test]
    fn level_decides_which_classes_ring() {
        for class in [MessageClass::Everyone, MessageClass::Mention, MessageClass::Normal] {
            assert!(passes(NotifyLevel::All, class));
            assert!(!passes(NotifyLevel::Nothing, class));
        }
        assert!(passes(NotifyLevel::Mentions, MessageClass::Mention));
        assert!(passes(NotifyLevel::Mentions, MessageClass::Everyone));
        assert!(!passes(NotifyLevel::Mentions, MessageClass::Normal));
    }

    #[test]
    fn mute_sentinels() {
        assert!(is_muted_at(MUTE_FOREVER, 10_000));
        assert!(!is_muted_at(MUTE_OFF, 10_000));
        assert!(is_muted_at(10_001, 10_000));
        assert!(!is_muted_at(10_000, 10_000), "an expiry that has arrived is over");
    }

    #[test]
    fn a_scope_at_every_default_stores_nothing() {
        assert!(ScopePrefs::default().is_default());
        assert!(!ScopePrefs { mute_until: MUTE_FOREVER, ..Default::default() }.is_default());
        assert!(!ScopePrefs { level: Some(NotifyLevel::Nothing), ..Default::default() }.is_default());
    }

    fn at(level: Option<NotifyLevel>, mute_until: i64) -> ScopePrefs {
        ScopePrefs { level, mute_until, suppress_everyone: None }
    }

    #[test]
    fn a_channel_opinion_beats_its_community() {
        let r = resolve_from(at(Some(NotifyLevel::All), 0), at(Some(NotifyLevel::Nothing), 0), 0);
        assert_eq!(r.level, NotifyLevel::All);
        assert_eq!(r.ring, NotifyLevel::All);
    }

    #[test]
    fn a_channel_with_no_opinion_takes_its_community() {
        let r = resolve_from(at(None, 0), at(Some(NotifyLevel::Mentions), 0), 0);
        assert_eq!(r.level, NotifyLevel::Mentions);
    }

    #[test]
    fn nothing_stored_anywhere_rings_for_everything() {
        let r = resolve_from(ScopePrefs::default(), ScopePrefs::default(), 0);
        assert_eq!(r.level, NotifyLevel::All);
        assert!(!r.muted);
    }

    #[test]
    fn a_community_mute_reaches_a_channel_that_set_none_itself() {
        // Mute unions downward, unlike level: the channel cannot opt out of it.
        let r = resolve_from(at(Some(NotifyLevel::All), MUTE_OFF), at(None, MUTE_FOREVER), 0);
        assert!(r.muted);
        assert_eq!(r.level, NotifyLevel::All, "the level underneath is untouched");
        assert_eq!(r.ring, NotifyLevel::Mentions, "but the mute clamps what rings");
    }

    #[test]
    fn a_mute_never_makes_a_quiet_channel_louder() {
        let r = resolve_from(at(Some(NotifyLevel::Nothing), MUTE_FOREVER), ScopePrefs::default(), 0);
        assert_eq!(r.ring, NotifyLevel::Nothing);
    }

    #[test]
    fn a_lapsed_timer_stops_muting_without_anything_being_written() {
        let prefs = at(None, 5_000);
        assert!(resolve_from(prefs, ScopePrefs::default(), 4_999).muted);
        assert!(!resolve_from(prefs, ScopePrefs::default(), 5_000).muted);
    }

    #[test]
    fn the_legacy_list_names_a_channel_its_community_muted() {
        // Chain-resolved, so a client that predates levels goes quiet in the same rooms.
        let channel = in_legacy_mute_list(ScopePrefs::default(), at(None, MUTE_FOREVER), 0);
        assert!(channel);
    }

    #[test]
    fn publishing_and_applying_the_legacy_list_agree() {
        // The bug this guards: publish with one predicate and apply with another, and
        // an inherited mute round-trips into a permanent per-channel one.
        let community = at(None, MUTE_FOREVER);
        let channel = ScopePrefs::default();
        let published = in_legacy_mute_list(channel, community, 0);
        let on_apply = in_legacy_mute_list(channel, community, 0);
        assert_eq!(published, on_apply, "the applier must read what the publisher wrote");
        assert!(published);
    }

    #[test]
    fn a_timed_mute_never_enters_the_legacy_list() {
        // It carries no deadline, so anything in it comes back indefinite, and the
        // expiry that should correct that happens at boot where publishing is barred.
        assert!(!in_legacy_mute_list(at(None, 9_000), ScopePrefs::default(), 0));
        assert!(!in_legacy_mute_list(ScopePrefs::default(), at(None, 9_000), 0));
        assert!(in_legacy_mute_list(at(None, MUTE_FOREVER), ScopePrefs::default(), 0));
    }

    #[test]
    fn a_lapsed_indefinite_free_scope_leaves_the_legacy_list() {
        let expired = at(None, 5_000);
        assert!(!in_legacy_mute_list(expired, ScopePrefs::default(), 6_000));
    }

    #[test]
    fn sweep_picks_exactly_the_timers_that_have_arrived() {
        // Mirrors `db::notify::clear_expired_mutes`'s `mute_until > 0 AND <= ?1`.
        let lapsed = |until: i64, now: i64| until > MUTE_OFF && until <= now;
        assert!(!lapsed(MUTE_FOREVER, 10_000), "indefinite never lapses");
        assert!(!lapsed(MUTE_OFF, 10_000), "unmuted has no timer");
        assert!(lapsed(10_000, 10_000), "an expiry that has arrived is over");
        assert!(!lapsed(10_001, 10_000));
    }

    #[test]
    fn scope_prefs_round_trip_through_the_wire_shape() {
        let prefs = ScopePrefs {
            level: Some(NotifyLevel::Mentions),
            mute_until: 1_700_000_000_000,
            suppress_everyone: Some(true),
        };
        assert_eq!(ScopePrefs::from_wire(&prefs.to_wire()), prefs);
        // An entry that says nothing must come back as the default, or a sync
        // would keep resurrecting rows the writer meant to drop.
        assert!(ScopePrefs::from_wire(&crate::synced_prefs::NotifyEntry::default()).is_default());
    }

    #[test]
    fn level_round_trips_through_both_wire_forms() {
        for level in [NotifyLevel::All, NotifyLevel::Mentions, NotifyLevel::Nothing] {
            assert_eq!(NotifyLevel::from_u8(level.as_u8()), Some(level));
            assert_eq!(NotifyLevel::from_label(level.as_str()), Some(level));
        }
        // The sync payload predates the rename, so both spellings must read.
        assert_eq!(NotifyLevel::from_label("none"), Some(NotifyLevel::Nothing));
        assert_eq!(NotifyLevel::from_label("whatever"), None);
        assert_eq!(NotifyLevel::from_u8(9), None);
    }
}
