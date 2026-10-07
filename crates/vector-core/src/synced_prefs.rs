//! Account preferences that follow you between your own devices: the block
//! list, the mute list, nicknames, and the general settings document.
//!
//! Each is a private, parameterized-replaceable kind 30078 with its own d tag,
//! NIP-44 self-encrypted, riding the SAME self-sync subscription as the
//! Community, Invite and Pinned lists — so each inherits boot sync, reconnect
//! re-sync and live cross-device edits with no new plumbing.
//!
//! **Vector's own lists, deliberately not NIP-51.** Social clients disagree on
//! what a mute is — several treat it as a soft block — so round-tripping
//! through the shared kind-10000 would blur the mute/block separation Vector
//! draws on purpose. Isolation costs interop and buys exactness.
//!
//! **Newest wins, whole list.** These are one person's settings edited from one
//! device at a time, so the replaceable event's own last-write-wins is the
//! merge. A device that was offline can therefore republish over a change it
//! never saw; the pre-publish fetch below shrinks that window, and these are
//! deliberate, infrequent actions rather than latency-sensitive ones.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::stored_event::event_kind;

pub const BLOCKS_D_TAG: &str = "vector/blocks";
pub const MUTES_D_TAG: &str = "vector/mutes";
pub const NICKNAMES_D_TAG: &str = "vector/nicknames";
pub const NOTIFY_D_TAG: &str = "vector/notify";
pub const RAIL_D_TAG: &str = "vector/rail";
pub const BANNERS_D_TAG: &str = "vector/banners";
pub const SETTINGS_D_TAG: &str = "vector/settings";

const BLOCKS_LOCAL_KEY: &str = "synced_blocks_local";
const MUTES_LOCAL_KEY: &str = "synced_mutes_local";
const NICKNAMES_LOCAL_KEY: &str = "synced_nicknames_local";
const NOTIFY_LOCAL_KEY: &str = "synced_notify_local";
const RAIL_LOCAL_KEY: &str = "synced_rail_local";
const BANNERS_LOCAL_KEY: &str = "synced_banners_local";
const SETTINGS_LOCAL_KEY: &str = "synced_settings_local";
/// Persisted, so a change survives a quit until the relays have it.
const SETTINGS_INTENT_KEY: &str = "synced_settings_intent";
const SETTINGS_SEEN_KEY: &str = "synced_settings_seen";

/// Set when a list has local edits the relays have not seen, cleared once they
/// have. Persisted, so a quit during the rail's publish debounce is recoverable
/// at the next boot instead of being silently lost.
const RAIL_DIRTY_KEY: &str = "synced_rail_dirty";

/// One NIP-44 event holds the whole list, so it inherits the same ~65KB
/// plaintext ceiling as the Community List. Blocks and nicknames scale with
/// contacts rather than being capped like pins, so the write path refuses to
/// grow a list past this rather than publishing something no reader can open.
const MAX_ENTRIES: usize = 2048;

const FETCH_TIMEOUT_SECS: u64 = 10;

/// A set of ids (npubs for blocks, chat ids for mutes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdList {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub ids: Vec<String>,
}

/// npub → nickname. A map rather than a list so a rename replaces rather than
/// duplicates, and so the wire form stays stable under reordering.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicknameMap {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

/// One scope's notification settings on the wire. Level is spelled out rather
/// than numbered so a future rung does not silently become an old one, and an
/// absent field means inherit, exactly as it does on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mute_until: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_everyone: Option<bool>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// scope id → settings, for communities, channels and DMs alike.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyMap {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub scopes: BTreeMap<String, NotifyEntry>,
}

impl NotifyMap {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"scopes\":{}}".to_string())
    }
    /// Refuses to grow past the ceiling rather than publishing a list no
    /// reader can open, the same rule the id lists follow.
    pub fn set(&mut self, scope_id: &str, entry: NotifyEntry) -> Result<(), String> {
        if scope_id.trim().is_empty() {
            return Err("empty scope id".to_string());
        }
        if entry == NotifyEntry::default() {
            self.scopes.remove(scope_id);
            return Ok(());
        }
        if !self.scopes.contains_key(scope_id) && self.scopes.len() >= MAX_ENTRIES {
            return Err(format!("this list is full ({MAX_ENTRIES} entries)"));
        }
        self.scopes.insert(scope_id.to_string(), entry);
        Ok(())
    }
}

/// Account-wide switches that follow the user between devices.
///
/// Keys this build does not know are carried through untouched: the document is
/// newest-wins as a whole, so an older device that dropped them on republish
/// would erase settings a newer one added.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncedSettings {
    #[serde(default = "one")]
    pub v: u32,
    /// Reveals identifiers and other developer-facing detail across the app.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
    #[serde(default, skip_serializing_if = "StreamerSettings::is_default")]
    pub streamer: StreamerSettings,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl SyncedSettings {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1}".to_string())
    }
    /// What `get_synced_settings` and `synced_settings_updated` hand the page.
    pub fn view(&self) -> serde_json::Value {
        serde_json::json!({
            "advanced": self.advanced,
            "streamer": {
                "on": self.streamer.on,
                "seed": self.streamer.seed,
                "notif": self.streamer.notif,
                "hide_wallpapers": self.streamer.hide_wallpapers,
            },
        })
    }
}

/// The streaming overlay's levels, in the spelling the wire and the page share.
pub const STREAMER_NOTIF_LEVELS: [&str; 4] = ["none", "hide_content", "hide_sender", "hide_all"];

/// Streamer Mode: masks everyone who has not opted in, on every device of the account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamerSettings {
    #[serde(default)]
    pub on: bool,
    /// 16 random bytes as lowercase hex, re-minted at each go-live so per-stream colours reshuffle.
    #[serde(default)]
    pub seed: String,
    /// What notifications additionally hide while live, one of [`STREAMER_NOTIF_LEVELS`].
    #[serde(default = "notif_none")]
    pub notif: String,
    /// Chats show a plain background while live: a DM's wallpaper can say who it is with.
    #[serde(default = "yes")]
    pub hide_wallpapers: bool,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

fn notif_none() -> String {
    "none".to_string()
}

fn yes() -> bool {
    true
}

impl Default for StreamerSettings {
    fn default() -> Self {
        Self { on: false, seed: String::new(), notif: notif_none(), hide_wallpapers: true, other: serde_json::Map::new() }
    }
}

impl StreamerSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn set_on(&mut self, on: bool) {
        if on && !self.on {
            self.seed = fresh_seed();
        }
        self.on = on;
    }

    pub fn set_notif(&mut self, level: &str) -> Result<(), String> {
        if !STREAMER_NOTIF_LEVELS.contains(&level) {
            return Err(format!("Unknown notification level: {level}"));
        }
        self.notif = level.to_string();
        Ok(())
    }
}

fn fresh_seed() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    crate::simd::hex::bytes_to_hex_string(&b)
}

/// Settings changes the relays have not confirmed, re-applied over any copy adopted before they
/// do. Only the fields the user touched, so the rest of a sibling's newer copy stands.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct SettingsIntent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advanced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on: Option<bool>,
    /// The seed this device minted going live, so adoption keeps the colours already on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notif: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hide_wallpapers: Option<bool>,
}

impl SettingsIntent {
    fn apply(&self, s: &mut SyncedSettings) {
        if let Some(advanced) = self.advanced {
            s.advanced = advanced;
        }
        let st = &mut s.streamer;
        if let Some(on) = self.on {
            match self.seed.as_ref().filter(|seed| !seed.is_empty()) {
                Some(seed) if on => st.seed = seed.clone(),
                _ if on && !st.on => st.seed = fresh_seed(),
                _ => {}
            }
            st.on = on;
        }
        if let Some(level) = self.notif.as_deref() {
            if STREAMER_NOTIF_LEVELS.contains(&level) {
                st.notif = level.to_string();
            }
        }
        if let Some(hide) = self.hide_wallpapers {
            st.hide_wallpapers = hide;
        }
    }
}

fn one() -> u32 {
    1
}

impl IdList {
    /// Tolerant parse: a malformed payload degrades to empty rather than
    /// erroring, so one bad event can never wedge a sync.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"ids\":[]}".to_string())
    }
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }
    pub fn add(&mut self, id: &str) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("empty id".to_string());
        }
        if self.contains(id) {
            return Ok(());
        }
        if self.ids.len() >= MAX_ENTRIES {
            return Err(format!("this list is full ({MAX_ENTRIES} entries)"));
        }
        self.ids.push(id.to_string());
        Ok(())
    }
    pub fn remove(&mut self, id: &str) {
        self.ids.retain(|i| i != id);
    }
}

impl NicknameMap {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"names\":{}}".to_string())
    }
    /// An empty nickname CLEARS the entry — that is how the UI expresses
    /// "remove this nickname", and keeping a blank would republish it forever.
    pub fn set(&mut self, npub: &str, name: &str) -> Result<(), String> {
        if npub.trim().is_empty() {
            return Err("empty npub".to_string());
        }
        if name.trim().is_empty() {
            self.names.remove(npub);
            return Ok(());
        }
        if !self.names.contains_key(npub) && self.names.len() >= MAX_ENTRIES {
            return Err(format!("nickname list is full ({MAX_ENTRIES} entries)"));
        }
        self.names.insert(npub.to_string(), name.to_string());
        Ok(())
    }
}

/// Which list a call refers to. Keeps one set of network/storage plumbing for
/// all three rather than three near-identical copies that can drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pref {
    Blocks,
    Mutes,
    Nicknames,
    Notify,
    Rail,
    /// Communities whose banner the user hid.
    Banners,
    /// The general settings document.
    Settings,
}

/// Every list, in the order hydration walks them. `Notify` comes after `Mutes`
/// so a device holding both applies the richer one last and wins the overlap.
pub const ALL_PREFS: [Pref; 7] =
    [Pref::Blocks, Pref::Mutes, Pref::Nicknames, Pref::Notify, Pref::Rail, Pref::Banners, Pref::Settings];

impl Pref {
    pub fn d_tag(self) -> &'static str {
        match self {
            Pref::Blocks => BLOCKS_D_TAG,
            Pref::Mutes => MUTES_D_TAG,
            Pref::Nicknames => NICKNAMES_D_TAG,
            Pref::Notify => NOTIFY_D_TAG,
            Pref::Rail => RAIL_D_TAG,
            Pref::Banners => BANNERS_D_TAG,
            Pref::Settings => SETTINGS_D_TAG,
        }
    }
    fn local_key(self) -> &'static str {
        match self {
            Pref::Blocks => BLOCKS_LOCAL_KEY,
            Pref::Mutes => MUTES_LOCAL_KEY,
            Pref::Nicknames => NICKNAMES_LOCAL_KEY,
            Pref::Notify => NOTIFY_LOCAL_KEY,
            Pref::Rail => RAIL_LOCAL_KEY,
            Pref::Banners => BANNERS_LOCAL_KEY,
            Pref::Settings => SETTINGS_LOCAL_KEY,
        }
    }
    /// The d-tag → list routing used by the self-sync handler.
    pub fn from_d_tag(d: &str) -> Option<Self> {
        match d {
            BLOCKS_D_TAG => Some(Pref::Blocks),
            MUTES_D_TAG => Some(Pref::Mutes),
            NICKNAMES_D_TAG => Some(Pref::Nicknames),
            NOTIFY_D_TAG => Some(Pref::Notify),
            RAIL_D_TAG => Some(Pref::Rail),
            BANNERS_D_TAG => Some(Pref::Banners),
            SETTINGS_D_TAG => Some(Pref::Settings),
            _ => None,
        }
    }
}

/// Lists this account has reconciled with the relays this session, keyed by
/// d-tag. Per-account by construction: it lives on the Session, so a swap drops
/// it and the next account re-hydrates rather than inheriting this one's.
struct Hydrated;

fn hydrated_set() -> std::sync::Arc<std::sync::Mutex<std::collections::HashSet<&'static str>>> {
    crate::db::current_session().scoped::<Hydrated, _>()
}

/// Has `pref` been reconciled with the relays yet this session?
///
/// **The publish gate.** These lists are whole-list newest-wins projections of
/// local state, so publishing one before the relay copy has been applied would
/// overwrite another device's prefs with this device's emptier view — a fresh
/// login that mutes one chat before the subscription replay lands would erase
/// every block, mute and nickname set elsewhere. Reconcile, THEN publish.
pub fn is_hydrated(pref: Pref) -> bool {
    hydrated_set().lock().map(|h| h.contains(pref.d_tag())).unwrap_or(false)
}

/// Mark `pref` reconciled. Called when a copy is applied AND when the relays
/// confirm none exists — "there is nothing to preserve" is just as reconciled
/// as having read it, and without that a first-ever account could never publish.
pub fn mark_hydrated(pref: Pref) {
    if let Ok(mut h) = hydrated_set().lock() {
        h.insert(pref.d_tag());
    }
}

/// Pull every list once at login and apply it, so this device is reconciled
/// before the user can touch anything. The live subscription also delivers
/// these, but it races the user; this does not.
///
/// A list whose fetch FAILS stays un-hydrated, so it stays unpublishable — far
/// better to leave prefs un-synced for a session than to overwrite prefs we
/// could not read.
pub async fn hydrate_all(client: &Client) -> Vec<(Pref, String)> {
    let Some(my_pk) = crate::state::my_public_key() else { return Vec::new() };
    let mut applied = Vec::new();
    for pref in ALL_PREFS {
        // The rail is the one list that can hold edits the relays have never
        // seen: its publish is debounced, so a quit mid-drag leaves the newer
        // copy here. Applying the relay's older one would undo it.
        if pref == Pref::Rail && rail_is_dirty() {
            mark_hydrated(pref);
            schedule_rail_publish();
            continue;
        }
        let fetched = match fetch_raw(client, my_pk, pref).await {
            Ok(fetched) => fetched,
            Err(e) => {
                crate::log_warn!("[SyncedPrefs] reading {} failed, left unpublishable: {e}", pref.d_tag());
                continue;
            }
        };
        if pref == Pref::Settings {
            match adopt_settings(fetched.as_ref()) {
                Ok(adopted) => {
                    // A stale copy means the relays lack this device's newer one.
                    if adopted.publish || adopted.stale {
                        publish_settings_soon();
                    }
                    applied.extend(adopted.json.map(|json| (pref, json)));
                }
                Err(e) => crate::log_warn!("[SyncedPrefs] adopting {} failed: {e}", pref.d_tag()),
            }
            continue;
        }
        match fetched {
            Some(copy) => {
                if save_local_raw(pref, &copy.json).is_ok() {
                    mark_hydrated(pref);
                    applied.push((pref, copy.json));
                }
            }
            // No stored copy: nothing to preserve, so this device may publish.
            None => mark_hydrated(pref),
        }
    }
    applied
}

/// Raw JSON of a list's local mirror. Callers parse into whichever shape the
/// list uses; the storage layer stays shape-agnostic.
pub fn load_local_raw(pref: Pref) -> Option<String> {
    crate::db::settings::get_sql_setting(pref.local_key().to_string())
        .ok()
        .flatten()
}

pub fn save_local_raw(pref: Pref, json: &str) -> Result<(), String> {
    crate::db::settings::set_sql_setting(pref.local_key().to_string(), json.to_string())
}

pub fn load_blocks() -> IdList {
    load_local_raw(Pref::Blocks).map(|s| IdList::from_json(&s)).unwrap_or_default()
}
pub fn load_mutes() -> IdList {
    load_local_raw(Pref::Mutes).map(|s| IdList::from_json(&s)).unwrap_or_default()
}
pub fn load_nicknames() -> NicknameMap {
    load_local_raw(Pref::Nicknames).map(|s| NicknameMap::from_json(&s)).unwrap_or_default()
}
pub fn load_notify() -> NotifyMap {
    load_local_raw(Pref::Notify).map(|s| NotifyMap::from_json(&s)).unwrap_or_default()
}
pub fn load_hidden_banners() -> IdList {
    load_local_raw(Pref::Banners).map(|s| IdList::from_json(&s)).unwrap_or_default()
}

/// Hide or show a community's banner, committed locally. The caller publishes.
///
/// Refused until the relay copy has been read, or a fresh login would publish
/// its empty list over the banners hidden on another device.
pub fn set_banner_hidden(community_id: &str, hidden: bool) -> Result<IdList, String> {
    if !is_hydrated(Pref::Banners) {
        return Err("Still syncing your settings, try again in a moment".to_string());
    }
    let mut list = load_hidden_banners();
    if hidden {
        list.add(community_id)?;
    } else {
        list.remove(community_id);
    }
    save_local_raw(Pref::Banners, &list.to_json())?;
    Ok(list)
}

pub fn load_settings() -> SyncedSettings {
    load_local_raw(Pref::Settings).map(|s| SyncedSettings::from_json(&s)).unwrap_or_default()
}

/// Serialises every read-modify-write of the settings document, including adopting
/// a relay copy, so a change cannot land between a copy being read and being saved.
struct SettingsLock;

fn settings_lock() -> std::sync::Arc<std::sync::Mutex<()>> {
    crate::db::current_session().scoped::<SettingsLock, _>()
}

/// Turn Advanced Mode on or off, committed locally. The caller publishes.
///
/// Refused until the relay copy has been read, for the same reason as the banners.
pub fn set_advanced(on: bool) -> Result<SyncedSettings, String> {
    change_settings(|s, intent| {
        if !is_hydrated(Pref::Settings) {
            return Err("Still syncing your settings, try again in a moment".to_string());
        }
        s.advanced = on;
        intent.advanced = Some(on);
        Ok(())
    })
}

/// Go live or stop, in effect at once. The caller publishes.
///
/// Never refused for want of hydration: the change is held as an intent and
/// re-applied over the relay copy when it lands, then published.
pub fn set_streamer_on(on: bool) -> Result<SyncedSettings, String> {
    change_settings(|s, intent| {
        s.streamer.set_on(on);
        intent.on = Some(on);
        intent.seed = on.then(|| s.streamer.seed.clone());
        Ok(())
    })
}

/// Choose what notifications additionally hide while live. Held like [`set_streamer_on`].
pub fn set_streamer_notif(level: &str) -> Result<SyncedSettings, String> {
    change_settings(|s, intent| {
        s.streamer.set_notif(level)?;
        intent.notif = Some(s.streamer.notif.clone());
        Ok(())
    })
}

/// Choose whether chat wallpapers stay plain while live. Held like [`set_streamer_on`].
pub fn set_streamer_wallpapers(hide: bool) -> Result<SyncedSettings, String> {
    change_settings(|s, intent| {
        s.streamer.hide_wallpapers = hide;
        intent.hide_wallpapers = Some(hide);
        Ok(())
    })
}

/// Commit a change locally and hold it as an intent until the relays confirm it, so no copy
/// adopted meanwhile (an older one, or one adopted after a failed publish) can undo it.
fn change_settings(
    change: impl FnOnce(&mut SyncedSettings, &mut SettingsIntent) -> Result<(), String>,
) -> Result<SyncedSettings, String> {
    let lock = settings_lock();
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut intent = load_intent().unwrap_or_default();
    let mut settings = load_settings();
    change(&mut settings, &mut intent)?;
    save_local_raw(Pref::Settings, &settings.to_json())?;
    save_intent(&intent)?;
    Ok(settings)
}

fn load_intent() -> Option<SettingsIntent> {
    crate::db::settings::get_sql_setting(SETTINGS_INTENT_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn save_intent(intent: &SettingsIntent) -> Result<(), String> {
    let json = serde_json::to_string(intent).map_err(|e| e.to_string())?;
    crate::db::settings::set_sql_setting(SETTINGS_INTENT_KEY.to_string(), json)
}

fn clear_intent() {
    let _ = crate::db::settings::remove_setting(SETTINGS_INTENT_KEY);
}

/// `created_at` of the newest settings copy this device adopted or saw published. Anything
/// older is a copy it already superseded, however late a relay delivers it.
fn settings_seen() -> u64 {
    crate::db::settings::get_sql_setting(SETTINGS_SEEN_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn set_settings_seen(at: u64) -> Result<(), String> {
    crate::db::settings::set_sql_setting(SETTINGS_SEEN_KEY.to_string(), at.to_string())
}

/// The last `created_at` this session signed settings with, so each publish lands strictly after
/// the one before it even while that one is still in flight.
struct SettingsSigned;

fn settings_signed() -> std::sync::Arc<std::sync::atomic::AtomicU64> {
    crate::db::current_session().scoped::<SettingsSigned, _>()
}

/// A list as one relay copy carries it: the plaintext and when it was signed.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayCopy {
    pub json: String,
    pub created_at: u64,
}

/// The outcome of taking a relay copy of the settings document.
#[derive(Debug, Clone, PartialEq)]
pub struct AdoptedSettings {
    /// The document now held, when the copy changed it.
    pub json: Option<String>,
    /// This device holds a change the relays have not seen.
    pub publish: bool,
    /// The copy is older than one this device already adopted or published, so it was ignored.
    pub stale: bool,
}

/// Take a relay copy of the settings (`None` = the relays hold none) and mark them reconciled.
///
/// A copy older than the newest this device adopted or published is ignored, so a lagging relay
/// or a late echo never rolls the document back. A held change is re-applied over a newer copy.
pub fn adopt_settings(relay: Option<&RelayCopy>) -> Result<AdoptedSettings, String> {
    let lock = settings_lock();
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let intent = load_intent();
    let adopted = match relay {
        Some(copy) if copy.created_at < settings_seen() => {
            AdoptedSettings { json: None, publish: false, stale: true }
        }
        Some(copy) => {
            let theirs = SyncedSettings::from_json(&copy.json);
            let mut merged = theirs.clone();
            if let Some(intent) = &intent {
                intent.apply(&mut merged);
            }
            set_settings_seen(copy.created_at)?;
            if merged == theirs {
                // The relays already say what we meant: usually our own publish echoing back.
                if intent.is_some() {
                    clear_intent();
                }
                save_local_raw(Pref::Settings, &copy.json)?;
                AdoptedSettings { json: Some(copy.json.clone()), publish: false, stale: false }
            } else {
                let merged = merged.to_json();
                save_local_raw(Pref::Settings, &merged)?;
                AdoptedSettings { json: Some(merged), publish: true, stale: false }
            }
        }
        // Relays that lost a copy this device once had get it back.
        None => AdoptedSettings { json: None, publish: intent.is_some() || settings_seen() > 0, stale: false },
    };
    mark_hydrated(Pref::Settings);
    Ok(adopted)
}

fn publish_settings_soon() {
    crate::db::spawn_bound(async {
        let Some(client) = crate::state::nostr_client() else { return };
        if let Err(e) = publish_settings(&client, false).await {
            crate::log_warn!("[SyncedPrefs] publishing {} failed: {e}", Pref::Settings.d_tag());
        }
    });
}

/// Publish the local settings, signed after every copy this device has adopted or signed so the
/// relays keep it over them.
///
/// With `refresh`, a newer relay copy is folded in first, so a device that missed a sibling's
/// change never publishes its stale document over it. That copy is returned for the caller to show.
pub async fn publish_settings(client: &Client, refresh: bool) -> Result<Option<String>, String> {
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;
    let mut shown = None;
    if refresh {
        if let Ok(Some(copy)) = fetch_raw(client, my_pk, Pref::Settings).await {
            if copy.created_at > settings_seen() {
                let adopted = adopt_settings(Some(&copy))?;
                shown = adopted.json;
                if !adopted.publish {
                    return Ok(shown);
                }
            }
        }
    }
    let (json, at) = claim_publish(Timestamp::now().as_secs());
    send_list(client, my_pk, Pref::Settings, &json, Some(at)).await?;
    settings_published(&json, at);
    Ok(shown)
}

/// The document to publish and the `created_at` to sign it with, taken together so a later
/// claim always carries the later document.
fn claim_publish(now: u64) -> (String, u64) {
    let lock = settings_lock();
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let signed = settings_signed();
    // Two publishes in one second would tie, and relays keep the lower id, not the later one.
    let at = now
        .max(settings_seen() + 1)
        .max(signed.load(std::sync::atomic::Ordering::SeqCst) + 1);
    signed.store(at, std::sync::atomic::Ordering::SeqCst);
    (load_settings().to_json(), at)
}

/// The relays now hold `json`, signed at `at`: older copies are superseded, and a held intent is
/// done once nothing has changed since.
fn settings_published(json: &str, at: u64) {
    let lock = settings_lock();
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    if at > settings_seen() {
        let _ = set_settings_seen(at);
    }
    if load_intent().is_some() && SyncedSettings::from_json(json) == load_settings() {
        clear_intent();
    }
}

pub fn load_rail() -> crate::rail_layout::RailLayout {
    load_local_raw(Pref::Rail)
        .map(|s| crate::rail_layout::RailLayout::from_json(&s))
        .unwrap_or_default()
}

async fn decrypt_event(my_pk: &PublicKey, event: &Event) -> Option<String> {
    if event.content.is_empty() {
        return None;
    }
    let signer = crate::signer::active_signer().ok()?;
    match signer.nip44_decrypt_async(my_pk, &event.content).await {
        Ok(plaintext) => Some(plaintext),
        Err(e) => {
            crate::log_warn!("[SyncedPrefs] decrypt {} failed: {}", event.kind.as_u16(), e);
            None
        }
    }
}

/// Fetch a list's newest relay copy, `Ok(None)` when the relays hold none.
///
/// `Err` when no relay answered or the copy can't be opened: neither says what the relays hold,
/// and treating it as "none" would let this device publish its emptier view over the real one.
pub async fn fetch_raw(client: &Client, my_pk: PublicKey, pref: Pref) -> Result<Option<RelayCopy>, String> {
    let filter = Filter::new()
        .author(my_pk)
        .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
        .identifier(pref.d_tag())
        .limit(1);
    let timeout = crate::relay_request_timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS));
    let events = crate::fetch_answered(client, filter, timeout).await?;
    let Some(event) = crate::newest_replaceable(events) else { return Ok(None) };
    let json = decrypt_event(&my_pk, &event)
        .await
        .ok_or_else(|| format!("couldn't open {}", pref.d_tag()))?;
    Ok(Some(RelayCopy { json, created_at: event.created_at.as_secs() }))
}

/// Persist locally, then publish self-encrypted. Settings always go out from the local copy,
/// through [`publish_settings`], which keeps their signing order.
pub async fn publish_raw(client: &Client, pref: Pref, json: &str) -> Result<(), String> {
    if pref == Pref::Settings {
        return publish_settings(client, false).await.map(|_| ());
    }
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;
    save_local_raw(pref, json)?;
    send_list(client, my_pk, pref, json, None).await
}

async fn send_list(
    client: &Client, my_pk: PublicKey, pref: Pref, json: &str, created_at: Option<u64>,
) -> Result<(), String> {
    let signer = crate::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
    let content = signer
        .nip44_encrypt_async(&my_pk, json)
        .await
        .map_err(|e| format!("nip44 encrypt {}: {e}", pref.d_tag()))?;
    let mut builder = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tag(Tag::identifier(pref.d_tag()));
    if let Some(at) = created_at {
        builder = builder.custom_created_at(Timestamp::from_secs(at));
    }
    crate::sign_and_send(client, builder)
        .await
        .map_err(|e| format!("publish {}: {e}", pref.d_tag()))?;
    Ok(())
}

/// Consume a sibling device's update. Never answers an echo with a publish — the
/// relay echoes our own publishes back on this same subscription, and that loops
/// forever. The one republish is a held settings intent the copy lacks, which an
/// echo of our own latest publish already carries. `None` for a settings copy older
/// than this device's, which changes nothing.
pub async fn ingest_remote(my_pk: &PublicKey, event: &Event) -> Option<(Pref, String)> {
    let d = event.tags.identifier().unwrap_or_default().to_string();
    let pref = Pref::from_d_tag(&d)?;
    let json = decrypt_event(my_pk, event).await?;
    if pref == Pref::Settings {
        let copy = RelayCopy { json, created_at: event.created_at.as_secs() };
        let adopted = match adopt_settings(Some(&copy)) {
            Ok(adopted) => adopted,
            Err(e) => {
                crate::log_warn!("[SyncedPrefs] adopting {} failed: {e}", pref.d_tag());
                return None;
            }
        };
        if adopted.publish {
            publish_settings_soon();
        }
        return adopted.json.map(|json| (pref, json));
    }
    if let Err(e) = save_local_raw(pref, &json) {
        crate::log_warn!("[SyncedPrefs] persisting {} failed: {e}", pref.d_tag());
        return None;
    }
    mark_hydrated(pref);
    if pref == Pref::Rail {
        set_rail_dirty(false);
    }
    Some((pref, json))
}

// ============================================================================
// The rail's debounced publish
// ============================================================================

/// How long the rail must sit still before its arrangement goes out.
///
/// Rearranging is burst activity: a minute of dragging communities in and out
/// of folders is ONE document at the end of it, not forty along the way. The
/// local mirror already holds every intermediate state, so nothing is at risk
/// while the timer runs.
const RAIL_PUBLISH_IDLE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Bumped by every rail edit. A sleeping publish that wakes to find a newer
/// generation was superseded mid-drag and simply stops, which is what collapses
/// a burst into one write.
struct RailPublishGen;

fn rail_gen() -> std::sync::Arc<std::sync::atomic::AtomicU64> {
    crate::db::current_session().scoped::<RailPublishGen, _>()
}

/// Whether the rail holds edits the relays have not seen.
pub fn rail_is_dirty() -> bool {
    crate::db::settings::get_sql_setting(RAIL_DIRTY_KEY.to_string())
        .ok()
        .flatten()
        .as_deref()
        == Some("1")
}

fn set_rail_dirty(dirty: bool) {
    let _ = crate::db::settings::set_sql_setting(
        RAIL_DIRTY_KEY.to_string(),
        if dirty { "1" } else { "0" }.to_string(),
    );
}

/// Commit an arrangement: on disk now, on the relays once the dragging stops.
///
/// The local mirror is the truth the rail paints from, so the UI never waits on
/// a relay to show a drag landing.
pub fn save_rail_debounced(layout: &crate::rail_layout::RailLayout) -> Result<(), String> {
    save_local_raw(Pref::Rail, &layout.to_json())?;
    set_rail_dirty(true);
    schedule_rail_publish();
    Ok(())
}

fn schedule_rail_publish() {
    let generation = rail_gen().fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(RAIL_PUBLISH_IDLE).await;
        if rail_gen().load(std::sync::atomic::Ordering::Relaxed) != generation {
            return;
        }
        flush_rail().await;
    });
}

/// Publish the arrangement if it has unsent edits.
///
/// A failure leaves the dirty flag set rather than retrying in a loop: the next
/// edit reschedules, and failing that the next boot does, because the flag is
/// persisted. That is also what covers a quit inside the debounce window — the
/// publish never ran, and the arrangement is not lost.
pub async fn flush_rail() {
    if !rail_is_dirty() || !is_hydrated(Pref::Rail) {
        return;
    }
    let Some(client) = crate::state::nostr_client() else { return };
    let Some(json) = load_local_raw(Pref::Rail) else { return };
    match publish_raw(&client, Pref::Rail, &json).await {
        Ok(()) => set_rail_dirty(false),
        Err(e) => crate::log_warn!("[SyncedPrefs] rail publish deferred: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_tags_round_trip_and_are_distinct() {
        for p in ALL_PREFS {
            assert_eq!(Pref::from_d_tag(p.d_tag()), Some(p));
        }
        // A tag belonging to another 30078 list must not resolve here, or the
        // self-sync router would hand a Community List to the block ingest.
        assert_eq!(Pref::from_d_tag("vector/communities"), None);
        assert_eq!(Pref::from_d_tag("vector/pinned"), None);
        assert_eq!(Pref::from_d_tag(""), None);
    }

    #[test]
    fn id_lists_add_idempotently_and_remove_tolerantly() {
        let mut l = IdList::default();
        l.add("npub1a").unwrap();
        l.add("npub1a").unwrap();
        assert_eq!(l.ids.len(), 1, "a second add is not a second entry");
        l.remove("never-present");
        l.remove("npub1a");
        assert!(l.ids.is_empty());
        assert!(l.add("  ").is_err(), "an empty id is refused, not stored");
    }

    #[test]
    fn an_empty_nickname_clears_rather_than_storing_a_blank() {
        let mut n = NicknameMap::default();
        n.set("npub1a", "Landlord").unwrap();
        assert_eq!(n.names.get("npub1a").map(String::as_str), Some("Landlord"));
        n.set("npub1a", "").unwrap();
        assert!(!n.names.contains_key("npub1a"), "clearing removes the key, not blanks it");
    }

    #[test]
    fn malformed_payloads_degrade_to_empty_instead_of_erroring() {
        assert!(IdList::from_json("not json").ids.is_empty());
        assert!(IdList::from_json("{}").ids.is_empty());
        assert!(NicknameMap::from_json("[]").names.is_empty());
        // An unknown version still yields its entries rather than being dropped.
        let future = IdList::from_json("{\"v\":99,\"ids\":[\"a\"],\"extra\":1}");
        assert_eq!(future.ids, vec!["a".to_string()]);
    }

    #[test]
    fn lists_refuse_to_grow_past_the_event_ceiling() {
        let mut l = IdList::default();
        for i in 0..MAX_ENTRIES {
            l.add(&format!("id{i}")).unwrap();
        }
        assert!(l.add("one-too-many").is_err(), "a list that cannot be opened is worse than a refusal");
        // Removing frees a slot again.
        l.remove("id0");
        assert!(l.add("one-too-many").is_ok());
    }

    #[test]
    fn settings_keep_keys_this_build_does_not_know() {
        let mut s = SyncedSettings::from_json("{\"v\":1,\"advanced\":false,\"future\":{\"a\":1}}");
        assert!(!s.advanced);
        s.advanced = true;
        let back = SyncedSettings::from_json(&s.to_json());
        assert!(back.advanced);
        assert_eq!(back.other.get("future"), Some(&serde_json::json!({ "a": 1 })), "a newer device's key survives our republish");
        assert!(SyncedSettings::from_json("not json") == SyncedSettings::default());
        assert!(!SyncedSettings::default().to_json().contains("advanced"), "off is the absent default");
    }

    #[test]
    fn streamer_keys_this_build_does_not_know_survive_too() {
        let json = r#"{"v":1,"streamer":{"on":true,"seed":"ab","notif":"hide_all","tint":3},"future":1}"#;
        let mut s = SyncedSettings::from_json(json);
        assert!(s.streamer.on);
        assert_eq!(s.streamer.notif, "hide_all");
        s.advanced = true;
        let back = SyncedSettings::from_json(&s.to_json());
        assert_eq!(back.streamer.other.get("tint"), Some(&serde_json::json!(3)));
        assert_eq!(back.other.get("future"), Some(&serde_json::json!(1)));
        assert!(!back.other.contains_key("streamer"), "a known key is not double-carried");
    }

    #[test]
    fn streamer_defaults_stay_off_the_wire_and_on_the_view() {
        let s = SyncedSettings::from_json(r#"{"v":1}"#);
        assert_eq!(s.streamer.notif, "none");
        assert!(!s.to_json().contains("streamer"));
        assert_eq!(
            s.view(),
            serde_json::json!({ "advanced": false, "streamer": { "on": false, "seed": "", "notif": "none", "hide_wallpapers": true } })
        );
    }

    #[test]
    fn wallpapers_hide_by_default_and_showing_them_is_kept_through_adoption() {
        let (_tmp, _guard) = init_test_db();
        assert!(SyncedSettings::from_json(r#"{"v":1,"streamer":{"on":true}}"#).streamer.hide_wallpapers);
        set_streamer_wallpapers(false).unwrap();
        assert!(SyncedSettings::from_json(&load_settings().to_json()).streamer.other.is_empty());
        assert!(load_settings().to_json().contains(r#""hide_wallpapers":false"#), "off is the one value on the wire");
        let adopted = adopt_settings(Some(&copy(r#"{"v":1,"streamer":{"on":true,"seed":"ab"}}"#, 100))).unwrap();
        assert!(adopted.publish, "the relays lack it");
        let held = load_settings().streamer;
        assert!(held.on && !held.hide_wallpapers, "theirs and ours both stand");
    }

    #[test]
    fn going_live_reseeds_and_staying_live_does_not() {
        let mut s = StreamerSettings::default();
        s.set_on(true);
        let first = s.seed.clone();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "lowercase hex: {first}");
        s.set_on(true);
        assert_eq!(s.seed, first, "on to on keeps the colours");
        s.set_on(false);
        assert_eq!(s.seed, first, "stopping keeps the last seed");
        s.set_on(true);
        assert_ne!(s.seed, first, "each go-live reshuffles");
    }

    #[test]
    fn the_overlay_takes_only_known_levels() {
        let mut s = StreamerSettings::default();
        for level in STREAMER_NOTIF_LEVELS {
            s.set_notif(level).unwrap();
            assert_eq!(s.notif, level);
        }
        assert!(s.set_notif("hide_everything").is_err());
        assert_eq!(s.notif, "hide_all", "a refused level changes nothing");
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let account = Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn copy(json: &str, created_at: u64) -> RelayCopy {
        RelayCopy { json: json.to_string(), created_at }
    }

    #[test]
    fn a_go_live_before_hydration_outlasts_the_relay_copy_and_is_published() {
        let (_tmp, _guard) = init_test_db();
        assert!(!is_hydrated(Pref::Settings));
        assert!(set_advanced(true).is_err(), "Advanced Mode still waits for the relay copy");

        let live = set_streamer_on(true).expect("never refused before hydration");
        set_streamer_notif("hide_sender").unwrap();
        assert!(load_settings().streamer.on, "in effect at once");
        let seed = live.streamer.seed.clone();

        let relay = r#"{"v":1,"advanced":true,"streamer":{"on":false,"seed":"00","notif":"none"},"future":1}"#;
        let adopted = adopt_settings(Some(&copy(relay, 100))).unwrap();
        assert!(adopted.publish, "the relays lack our change");
        assert!(is_hydrated(Pref::Settings));

        let held = load_settings();
        assert!(held.streamer.on && held.advanced, "ours and theirs both stand");
        assert_eq!(held.streamer.seed, seed, "the colours on screen are kept");
        assert_eq!(held.streamer.notif, "hide_sender");
        assert_eq!(held.other.get("future"), Some(&serde_json::json!(1)));
        assert_eq!(adopted.json.as_deref(), Some(held.to_json().as_str()), "the page is shown the merge");

        assert!(load_intent().is_some(), "held until the relays have it");
        let (json, at) = claim_publish(100);
        assert!(at > 100, "signed after the copy it supersedes");
        settings_published(&json, at);
        assert!(load_intent().is_none());
        let echo = adopt_settings(Some(&copy(&json, at))).unwrap();
        assert!(!echo.publish, "our own echo is never answered");
    }

    #[test]
    fn an_echo_that_beats_the_publish_ack_retires_the_intent() {
        let (_tmp, _guard) = init_test_db();
        set_streamer_on(true).unwrap();
        let (json, at) = claim_publish(100);
        let adopted = adopt_settings(Some(&copy(&json, at))).unwrap();
        assert!(!adopted.publish);
        assert!(load_intent().is_none());
    }

    #[test]
    fn with_no_relay_copy_a_held_change_is_published_as_is() {
        let (_tmp, _guard) = init_test_db();
        set_streamer_notif("hide_all").unwrap();
        let adopted = adopt_settings(None).unwrap();
        assert!(adopted.publish);
        assert_eq!(load_settings().streamer.notif, "hide_all");
    }

    #[test]
    fn with_nothing_held_hydration_publishes_nothing() {
        let (_tmp, _guard) = init_test_db();
        assert!(!adopt_settings(None).unwrap().publish);
        assert!(!adopt_settings(Some(&copy(r#"{"v":1,"advanced":true}"#, 100))).unwrap().publish);
        assert!(load_settings().advanced);
    }

    #[test]
    fn a_change_made_while_the_publish_is_pending_rides_the_intent() {
        let (_tmp, _guard) = init_test_db();
        set_streamer_on(true).unwrap();
        adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        set_streamer_notif("hide_content").unwrap();
        // A relay copy without it, after a failed publish and a restart.
        let adopted = adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        assert!(adopted.publish);
        assert_eq!(load_settings().streamer.notif, "hide_content");
        assert!(load_settings().streamer.on);
    }

    #[test]
    fn a_go_live_after_hydration_outlasts_a_failed_publish() {
        let (_tmp, _guard) = init_test_db();
        adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        set_streamer_on(true).unwrap();
        // The publish never landed; the next boot reads the old copy back.
        let adopted = adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        assert!(adopted.publish, "the go-live is published again");
        assert!(load_settings().streamer.on, "and is not undone meanwhile");
    }

    #[test]
    fn an_older_copy_never_rolls_the_settings_back() {
        let (_tmp, _guard) = init_test_db();
        let on = r#"{"v":1,"streamer":{"on":true,"seed":"ab","notif":"hide_all"}}"#;
        adopt_settings(Some(&copy(on, 200))).unwrap();
        let off = r#"{"v":1,"streamer":{"on":false,"seed":"ab","notif":"none"}}"#;
        let late = adopt_settings(Some(&copy(off, 150))).unwrap();
        assert!(late.stale && late.json.is_none() && !late.publish);
        let held = load_settings();
        assert!(held.streamer.on);
        assert_eq!(held.streamer.notif, "hide_all");
        // A sibling's genuinely newer change still lands.
        assert!(adopt_settings(Some(&copy(off, 201))).unwrap().json.is_some());
        assert!(!load_settings().streamer.on);
    }

    #[test]
    fn publishes_inside_one_second_are_signed_in_order() {
        let (_tmp, _guard) = init_test_db();
        adopt_settings(Some(&copy(r#"{"v":1}"#, 1_000))).unwrap();
        set_streamer_on(true).unwrap();
        let (first, a) = claim_publish(1_000);
        set_streamer_on(false).unwrap();
        let (_, b) = claim_publish(1_000);
        set_streamer_on(true).unwrap();
        let (last, c) = claim_publish(1_000);
        assert!(a > 1_000 && b > a && c > b, "{a} {b} {c}");

        // The acks land in any order; only the one matching what is held retires the intent.
        settings_published(&first, a);
        assert!(load_intent().is_some(), "the first publish is not what is held now");
        settings_published(&last, c);
        assert!(load_intent().is_none());

        // The echo of the middle publish arrives last and changes nothing.
        let off = SyncedSettings::from_json(&last);
        let mut off = off.clone();
        off.streamer.on = false;
        let echo = adopt_settings(Some(&copy(&off.to_json(), b))).unwrap();
        assert!(echo.stale);
        assert!(load_settings().streamer.on, "still live after the late echo");

        // A restart re-reads an older relay copy: ignored, and hydration republishes ours.
        let reread = adopt_settings(Some(&copy(&first, a))).unwrap();
        assert!(reread.stale);
        assert_eq!(load_settings().to_json(), last);
    }

    #[test]
    fn an_earlier_go_live_echoing_back_keeps_the_latest_seed() {
        let (_tmp, _guard) = init_test_db();
        adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        set_streamer_on(true).unwrap();
        let (first, a) = claim_publish(100);
        set_streamer_on(false).unwrap();
        let latest = set_streamer_on(true).unwrap().streamer.seed;
        let echo = adopt_settings(Some(&copy(&first, a))).unwrap();
        assert!(echo.publish, "the relays hold an older go-live");
        let held = load_settings().streamer;
        assert!(held.on);
        assert_eq!(held.seed, latest, "the colours on screen now are kept");
    }

    #[test]
    fn advanced_mode_rides_the_intent_too() {
        let (_tmp, _guard) = init_test_db();
        adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        set_advanced(true).unwrap();
        let adopted = adopt_settings(Some(&copy(r#"{"v":1}"#, 100))).unwrap();
        assert!(adopted.publish);
        assert!(load_settings().advanced);
    }

    #[test]
    fn nickname_order_is_stable_across_a_round_trip() {
        // BTreeMap, so two devices building the same set emit identical bytes —
        // no spurious republish churn from map iteration order.
        let mut a = NicknameMap::default();
        a.set("npub1z", "Zed").unwrap();
        a.set("npub1a", "Ann").unwrap();
        let mut b = NicknameMap::default();
        b.set("npub1a", "Ann").unwrap();
        b.set("npub1z", "Zed").unwrap();
        assert_eq!(a.to_json(), b.to_json(), "insertion order must not change the wire form");
    }
}
