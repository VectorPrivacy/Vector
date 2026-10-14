//! Profile types and sync — compact internal representation + relay fetching.
//!
//! The `id` field is a u16 interner handle — the canonical npub string lives
//! in `NpubInterner` (single source of truth). Use `SlimProfile` at
//! serialization boundaries (frontend, DB).
//!
//! The `sync` submodule has the priority queue, background processor,
//! and `load_profile` relay fetch logic.

pub mod sync;

pub use sync::{SyncPriority, ProfileSyncHandler, NoOpProfileSyncHandler};

use nostr_sdk::prelude::Metadata;

use crate::compact::NO_NPUB;

// ============================================================================
// ProfileFlags — 4 bools packed into 1 byte
// ============================================================================

/// Kind-0 content key by which a user consents to being shown on someone's stream.
pub const STREAM_CONSENT_KEY: &str = "stream_consent";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileFlags(u8);

impl ProfileFlags {
    const MINE:           u8 = 0b0001;
    const BLOCKED:        u8 = 0b0010;
    const BOT:            u8 = 0b0100;
    const STREAM_CONSENT: u8 = 0b1000;

    #[inline] pub fn is_mine(self) -> bool    { self.0 & Self::MINE != 0 }
    #[inline] pub fn is_blocked(self) -> bool  { self.0 & Self::BLOCKED != 0 }
    #[inline] pub fn is_bot(self) -> bool      { self.0 & Self::BOT != 0 }
    #[inline] pub fn is_stream_consent(self) -> bool { self.0 & Self::STREAM_CONSENT != 0 }

    #[inline] pub fn set_mine(&mut self, v: bool)    { if v { self.0 |= Self::MINE } else { self.0 &= !Self::MINE } }
    #[inline] pub fn set_blocked(&mut self, v: bool)  { if v { self.0 |= Self::BLOCKED } else { self.0 &= !Self::BLOCKED } }
    #[inline] pub fn set_bot(&mut self, v: bool)      { if v { self.0 |= Self::BOT } else { self.0 &= !Self::BOT } }
    #[inline] pub fn set_stream_consent(&mut self, v: bool) { if v { self.0 |= Self::STREAM_CONSENT } else { self.0 &= !Self::STREAM_CONSENT } }
}

/// Whether a kind-0's content opts in to streams: `true` or the string `"true"`.
pub fn metadata_stream_consent(meta: &Metadata) -> bool {
    meta.custom.get(STREAM_CONSENT_KEY).is_some_and(|v| match v.as_bool() {
        Some(b) => b,
        None => v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true")),
    })
}

/// `meta` with the consent key set when `on`, and absent otherwise.
pub fn with_stream_consent(mut meta: Metadata, on: bool) -> Metadata {
    meta.custom.remove(STREAM_CONSENT_KEY);
    if on {
        meta.custom.insert(STREAM_CONSENT_KEY.to_string(), serde_json::Value::Bool(true));
    }
    meta
}

// ============================================================================
// Profile — compact internal representation
// ============================================================================

const NAME: usize = 0;
const DISPLAY_NAME: usize = 1;
const BANNER: usize = 2;
const AVATAR: usize = 3;
const ABOUT: usize = 4;
const AVATAR_CACHED: usize = 5;
const BANNER_CACHED: usize = 6;
const NICKNAME: usize = 7;
const LUD06: usize = 8;
const LUD16: usize = 9;
const NIP05: usize = 10;
const WEBSITE: usize = 11;
const STATUS_TITLE: usize = 12;
const STATUS_PURPOSE: usize = 13;
const STATUS_URL: usize = 14;
const FIELDS: usize = 15;

const WIDE: u16 = 1 << 15;
const FIELD_BITS: u16 = WIDE - 1;
const HEADER: usize = 4;

/// Every string of a profile in one heap block; `None` when all of them are empty.
///
/// `[mask u16][emoji count u16][end offset × slots][utf8]`: mask bit `i` marks field `i`
/// present, bit 15 widens the offsets from u16 to u32. The slots are the present fields in
/// field order, then each status emoji tag's shortcode and url; an offset is a slot's end.
#[derive(Clone, Default, PartialEq, Eq)]
struct ProfileText(Option<Box<[u8]>>);

impl ProfileText {
    fn build(fields: &[&str; FIELDS], emoji: &[(&str, &str)]) -> Self {
        let emoji = &emoji[..emoji.len().min(u16::MAX as usize)];
        let mut mask = 0u16;
        for (i, f) in fields.iter().enumerate() {
            if !f.is_empty() {
                mask |= 1 << i;
            }
        }
        if mask == 0 && emoji.is_empty() {
            return Self(None);
        }
        let slots = || fields.iter().copied().filter(|f| !f.is_empty()).chain(emoji.iter().flat_map(|&(s, u)| [s, u]));
        let count = mask.count_ones() as usize + 2 * emoji.len();
        let total: usize = slots().map(str::len).sum();
        assert!(total <= u32::MAX as usize, "profile text over 4 GiB");
        let wide = total > u16::MAX as usize;
        let width = if wide { 4 } else { 2 };

        let mut out = Vec::with_capacity(HEADER + width * count + total);
        out.extend_from_slice(&(mask | if wide { WIDE } else { 0 }).to_le_bytes());
        out.extend_from_slice(&(emoji.len() as u16).to_le_bytes());
        let mut end = 0;
        for s in slots() {
            end += s.len();
            if wide {
                out.extend_from_slice(&(end as u32).to_le_bytes());
            } else {
                out.extend_from_slice(&(end as u16).to_le_bytes());
            }
        }
        for s in slots() {
            out.extend_from_slice(s.as_bytes());
        }
        Self(Some(out.into_boxed_slice()))
    }

    #[inline]
    fn get(&self, field: usize) -> &str {
        let Some(b) = self.0.as_deref() else { return "" };
        let mask = u16::from_le_bytes([b[0], b[1]]);
        let bit = 1u16 << field;
        if mask & bit == 0 {
            return "";
        }
        Self::slot(b, (mask & FIELD_BITS & (bit - 1)).count_ones() as usize)
    }

    #[inline]
    fn slot(b: &[u8], k: usize) -> &str {
        let mask = u16::from_le_bytes([b[0], b[1]]);
        let count = (mask & FIELD_BITS).count_ones() as usize + 2 * u16::from_le_bytes([b[2], b[3]]) as usize;
        let width = if mask & WIDE != 0 { 4 } else { 2 };
        let end = |j: usize| {
            let at = HEADER + width * j;
            if width == 4 {
                u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
            } else {
                u16::from_le_bytes([b[at], b[at + 1]]) as usize
            }
        };
        let data = HEADER + width * count;
        let start = if k == 0 { 0 } else { end(k - 1) };
        let bytes = &b[data + start..data + end(k)];
        debug_assert!(std::str::from_utf8(bytes).is_ok(), "profile slot {k} split a character");
        // SAFETY: `build` copied every slot whole from a `&str`, and a block is never written after.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    fn fields(&self) -> [&str; FIELDS] {
        std::array::from_fn(|i| self.get(i))
    }

    fn emoji(&self) -> impl ExactSizeIterator<Item = (&str, &str)> + '_ {
        let (b, first, n) = match self.0.as_deref() {
            Some(b) => (
                b,
                (u16::from_le_bytes([b[0], b[1]]) & FIELD_BITS).count_ones() as usize,
                u16::from_le_bytes([b[2], b[3]]) as usize,
            ),
            None => (&[][..], 0, 0),
        };
        (0..n).map(move |j| (Self::slot(b, first + 2 * j), Self::slot(b, first + 2 * j + 1)))
    }

    fn heap_bytes(&self) -> usize {
        self.0.as_ref().map_or(0, |b| b.len())
    }
}

const AVATAR_IN_CACHE: u8 = 1;
const BANNER_IN_CACHE: u8 = 2;

/// `{app_data}/cache/avatars` and `…/banners`, spelled as the image cache spells its paths.
fn image_cache_dirs() -> Option<&'static [String; 2]> {
    static DIRS: std::sync::OnceLock<[String; 2]> = std::sync::OnceLock::new();
    if let Some(dirs) = DIRS.get() {
        return Some(dirs);
    }
    let app = crate::db::get_app_data_dir().ok()?;
    Some(DIRS.get_or_init(|| ["avatars", "banners"].map(|sub| app.join("cache").join(sub).to_string_lossy().into_owned())))
}

/// `path`'s file name when the path is exactly `dir`, one separator, then that name.
fn cached_file_name<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let name = path.strip_prefix(dir)?.strip_prefix(std::path::MAIN_SEPARATOR)?;
    (!name.is_empty() && !name.contains(['/', std::path::MAIN_SEPARATOR])).then_some(name)
}

fn expand_cached(name: &str, dir: &str) -> String {
    let mut path = String::with_capacity(dir.len() + 1 + name.len());
    path.push_str(dir);
    path.push(std::path::MAIN_SEPARATOR);
    path.push_str(name);
    path
}

/// What a cached path is stored as (`kind` 0 = avatar, 1 = banner), and whether it was shortened.
fn store_cached(path: &str, kind: usize) -> (&str, bool) {
    match image_cache_dirs().and_then(|dirs| cached_file_name(path, &dirs[kind])) {
        Some(name) => (name, true),
        None => (path, false),
    }
}

/// Internal profile: a u16 interner handle, flags, and every string in one packed block.
///
/// A cached image path inside the image cache dir is held as its file name and rebuilt
/// on read, so [`SlimProfile`] and the DB still see the full path.
#[derive(Clone)]
pub struct Profile {
    pub id: u16,
    pub flags: ProfileFlags,
    in_cache: u8,
    pub last_updated: u32,
    text: ProfileText,
}

impl Default for Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for Profile {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.last_updated == other.last_updated
            && self.flags == other.flags
            && if self.in_cache == other.in_cache {
                self.text == other.text
            } else {
                (0..FIELDS).all(|i| i == AVATAR_CACHED || i == BANNER_CACHED || self.text.get(i) == other.text.get(i))
                    && self.avatar_cached() == other.avatar_cached()
                    && self.banner_cached() == other.banner_cached()
                    && self.text.emoji().eq(other.text.emoji())
            }
    }
}

impl std::fmt::Debug for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profile")
            .field("id", &self.id)
            .field("name", &self.name())
            .field("display_name", &self.display_name())
            .field("nickname", &self.nickname())
            .field("about", &self.about())
            .field("avatar", &self.avatar())
            .field("banner", &self.banner())
            .field("avatar_cached", &self.avatar_cached())
            .field("banner_cached", &self.banner_cached())
            .field("lud06", &self.lud06())
            .field("lud16", &self.lud16())
            .field("nip05", &self.nip05())
            .field("website", &self.website())
            .field("status_title", &self.status_title())
            .field("status_purpose", &self.status_purpose())
            .field("status_url", &self.status_url())
            .field("status_emoji_tags", &self.text.emoji().collect::<Vec<_>>())
            .field("last_updated", &self.last_updated)
            .field("flags", &self.flags)
            .finish()
    }
}

impl Profile {
    pub fn new() -> Self {
        Self { id: NO_NPUB, flags: ProfileFlags::default(), in_cache: 0, last_updated: 0, text: ProfileText(None) }
    }

    #[inline] pub fn name(&self) -> &str { self.text.get(NAME) }
    #[inline] pub fn display_name(&self) -> &str { self.text.get(DISPLAY_NAME) }
    #[inline] pub fn banner(&self) -> &str { self.text.get(BANNER) }
    #[inline] pub fn avatar(&self) -> &str { self.text.get(AVATAR) }
    #[inline] pub fn about(&self) -> &str { self.text.get(ABOUT) }
    #[inline] pub fn nickname(&self) -> &str { self.text.get(NICKNAME) }
    #[inline] pub fn lud06(&self) -> &str { self.text.get(LUD06) }
    #[inline] pub fn lud16(&self) -> &str { self.text.get(LUD16) }
    #[inline] pub fn nip05(&self) -> &str { self.text.get(NIP05) }
    #[inline] pub fn website(&self) -> &str { self.text.get(WEBSITE) }
    #[inline] pub fn status_title(&self) -> &str { self.text.get(STATUS_TITLE) }
    #[inline] pub fn status_purpose(&self) -> &str { self.text.get(STATUS_PURPOSE) }
    #[inline] pub fn status_url(&self) -> &str { self.text.get(STATUS_URL) }

    /// NIP-30 tags from the status event, so `:shortcode:` in it renders.
    pub fn status_emoji_tags(&self) -> Vec<crate::types::EmojiTag> {
        self.text.emoji().map(|(s, u)| crate::types::EmojiTag { shortcode: s.to_string(), url: u.to_string() }).collect()
    }

    pub fn avatar_cached(&self) -> std::borrow::Cow<'_, str> { self.cached(AVATAR_CACHED, AVATAR_IN_CACHE, 0) }
    pub fn banner_cached(&self) -> std::borrow::Cow<'_, str> { self.cached(BANNER_CACHED, BANNER_IN_CACHE, 1) }
    #[inline] pub fn has_avatar_cached(&self) -> bool { !self.text.get(AVATAR_CACHED).is_empty() }
    #[inline] pub fn has_banner_cached(&self) -> bool { !self.text.get(BANNER_CACHED).is_empty() }

    fn cached(&self, field: usize, bit: u8, kind: usize) -> std::borrow::Cow<'_, str> {
        let stored = self.text.get(field);
        match image_cache_dirs() {
            Some(dirs) if self.in_cache & bit != 0 && !stored.is_empty() => expand_cached(stored, &dirs[kind]).into(),
            _ => stored.into(),
        }
    }

    pub fn set_name(&mut self, v: &str) { self.write(&[(NAME, v)], None); }
    pub fn set_display_name(&mut self, v: &str) { self.write(&[(DISPLAY_NAME, v)], None); }
    pub fn set_banner(&mut self, v: &str) { self.write(&[(BANNER, v)], None); }
    pub fn set_avatar(&mut self, v: &str) { self.write(&[(AVATAR, v)], None); }
    pub fn set_about(&mut self, v: &str) { self.write(&[(ABOUT, v)], None); }
    pub fn set_nickname(&mut self, v: &str) { self.write(&[(NICKNAME, v)], None); }
    pub fn set_lud06(&mut self, v: &str) { self.write(&[(LUD06, v)], None); }
    pub fn set_lud16(&mut self, v: &str) { self.write(&[(LUD16, v)], None); }
    pub fn set_nip05(&mut self, v: &str) { self.write(&[(NIP05, v)], None); }
    pub fn set_website(&mut self, v: &str) { self.write(&[(WEBSITE, v)], None); }
    pub fn set_status_title(&mut self, v: &str) { self.write(&[(STATUS_TITLE, v)], None); }
    pub fn set_status_purpose(&mut self, v: &str) { self.write(&[(STATUS_PURPOSE, v)], None); }
    pub fn set_status_url(&mut self, v: &str) { self.write(&[(STATUS_URL, v)], None); }
    pub fn set_status_emoji_tags(&mut self, tags: &[crate::types::EmojiTag]) { self.write(&[], Some(tags)); }

    /// Every status part in one rebuild.
    pub fn set_status(&mut self, title: &str, purpose: &str, url: &str, tags: &[crate::types::EmojiTag]) {
        self.write(&[(STATUS_TITLE, title), (STATUS_PURPOSE, purpose), (STATUS_URL, url)], Some(tags));
    }

    pub fn set_avatar_cached(&mut self, path: &str) { self.set_cached(AVATAR_CACHED, AVATAR_IN_CACHE, 0, path); }
    pub fn set_banner_cached(&mut self, path: &str) { self.set_cached(BANNER_CACHED, BANNER_IN_CACHE, 1, path); }

    fn set_cached(&mut self, field: usize, bit: u8, kind: usize, path: &str) {
        let (stored, short) = store_cached(path, kind);
        self.write(&[(field, stored)], None);
        if short { self.in_cache |= bit } else { self.in_cache &= !bit }
    }

    /// Rebuild the block with `updates` (and `tags`, when given) applied; nothing when none differ.
    fn write(&mut self, updates: &[(usize, &str)], tags: Option<&[crate::types::EmojiTag]>) -> bool {
        fn pair(t: &crate::types::EmojiTag) -> (&str, &str) {
            (t.shortcode.as_str(), t.url.as_str())
        }
        let mut fields = self.text.fields();
        let mut changed = false;
        for &(i, v) in updates {
            if fields[i] != v {
                fields[i] = v;
                changed = true;
            }
        }
        changed |= tags.is_some_and(|t| !self.text.emoji().eq(t.iter().map(pair)));
        if !changed {
            return false;
        }
        let emoji: Vec<(&str, &str)> = match tags {
            Some(t) => t.iter().map(pair).collect(),
            None => self.text.emoji().collect(),
        };
        let cleared = if fields[AVATAR_CACHED].is_empty() { AVATAR_IN_CACHE } else { 0 }
            | if fields[BANNER_CACHED].is_empty() { BANNER_IN_CACHE } else { 0 };
        self.text = ProfileText::build(&fields, &emoji);
        self.in_cache &= !cleared;
        true
    }

    /// Heap bytes held by the packed strings.
    pub fn heap_bytes(&self) -> usize {
        self.text.heap_bytes()
    }

    /// Merge Nostr Metadata into this Profile. Returns `true` if any fields changed.
    pub fn from_metadata(&mut self, meta: Metadata) -> bool {
        // Absence revokes: every caller passes a whole kind-0, so a missing key is an opt-out.
        let consent = metadata_stream_consent(&meta);

        let mut fields = self.text.fields();
        let mut in_cache = self.in_cache;
        let mut changed = false;
        for (i, value) in [
            (NAME, &meta.name),
            (DISPLAY_NAME, &meta.display_name),
            (LUD06, &meta.lud06),
            (LUD16, &meta.lud16),
            (BANNER, &meta.banner),
            (AVATAR, &meta.picture),
            (ABOUT, &meta.about),
            (WEBSITE, &meta.website),
            (NIP05, &meta.nip05),
        ] {
            let Some(value) = value.as_deref() else { continue };
            if fields[i] == value {
                continue;
            }
            fields[i] = value;
            changed = true;
            // A new image URL invalidates the file cached for the old one.
            if i == BANNER {
                fields[BANNER_CACHED] = "";
                in_cache &= !BANNER_IN_CACHE;
            } else if i == AVATAR {
                fields[AVATAR_CACHED] = "";
                in_cache &= !AVATAR_IN_CACHE;
            }
        }
        if changed {
            let emoji: Vec<(&str, &str)> = self.text.emoji().collect();
            self.text = ProfileText::build(&fields, &emoji);
            self.in_cache = in_cache;
        }

        if let Some(custom) = meta.custom.get("bot") {
            let bot_value = match custom.as_bool() {
                Some(b) => b,
                None => custom.as_str().map(|s| s.to_lowercase() == "true").unwrap_or(false),
            };
            if self.flags.is_bot() != bot_value {
                self.flags.set_bot(bot_value);
                changed = true;
            }
        }
        if self.flags.is_stream_consent() != consent {
            self.flags.set_stream_consent(consent);
            changed = true;
        }

        changed
    }
}

// ============================================================================
// SlimProfile — serialization boundary (frontend, DB)
// ============================================================================

/// Profile with npub string instead of interner handle. Used for:
/// - Sending to frontend (JSON serializable)
/// - Persisting to database
/// - IPC between processes
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SlimProfile {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub nickname: String,
    pub lud06: String,
    pub lud16: String,
    pub banner: String,
    pub avatar: String,
    pub about: String,
    pub website: String,
    pub nip05: String,
    pub status: Status,
    pub last_updated: u64,
    pub mine: bool,
    pub bot: bool,
    pub is_blocked: bool,
    pub avatar_cached: String,
    pub banner_cached: String,
    #[serde(default)]
    pub stream_consent: bool,
}

impl SlimProfile {
    /// Convert from internal Profile, resolving interner handle to npub.
    pub fn from_profile(profile: &Profile, interner: &crate::compact::NpubInterner) -> Self {
        Self {
            id: interner.resolve(profile.id).unwrap_or("").to_string(),
            name: profile.name().to_string(),
            display_name: profile.display_name().to_string(),
            nickname: profile.nickname().to_string(),
            lud06: profile.lud06().to_string(),
            lud16: profile.lud16().to_string(),
            banner: profile.banner().to_string(),
            avatar: profile.avatar().to_string(),
            about: profile.about().to_string(),
            website: profile.website().to_string(),
            nip05: profile.nip05().to_string(),
            status: Status {
                title: profile.status_title().to_string(),
                purpose: profile.status_purpose().to_string(),
                url: profile.status_url().to_string(),
                emoji_tags: profile.status_emoji_tags(),
            },
            last_updated: crate::compact::secs_from_compact(profile.last_updated),
            mine: profile.flags.is_mine(),
            bot: profile.flags.is_bot(),
            is_blocked: profile.flags.is_blocked(),
            avatar_cached: profile.avatar_cached().into_owned(),
            banner_cached: profile.banner_cached().into_owned(),
            stream_consent: profile.flags.is_stream_consent(),
        }
    }

    /// Convert to internal Profile (for loading from DB).
    pub fn to_profile(&self) -> Profile {
        let (avatar_cached, avatar_short) = store_cached(&self.avatar_cached, 0);
        let (banner_cached, banner_short) = store_cached(&self.banner_cached, 1);
        let mut fields = [""; FIELDS];
        fields[NAME] = &self.name;
        fields[DISPLAY_NAME] = &self.display_name;
        fields[BANNER] = &self.banner;
        fields[AVATAR] = &self.avatar;
        fields[ABOUT] = &self.about;
        fields[AVATAR_CACHED] = avatar_cached;
        fields[BANNER_CACHED] = banner_cached;
        fields[NICKNAME] = &self.nickname;
        fields[LUD06] = &self.lud06;
        fields[LUD16] = &self.lud16;
        fields[NIP05] = &self.nip05;
        fields[WEBSITE] = &self.website;
        fields[STATUS_TITLE] = &self.status.title;
        fields[STATUS_PURPOSE] = &self.status.purpose;
        fields[STATUS_URL] = &self.status.url;
        let emoji: Vec<(&str, &str)> =
            self.status.emoji_tags.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();

        let mut flags = ProfileFlags::default();
        flags.set_mine(self.mine);
        flags.set_bot(self.bot);
        flags.set_blocked(self.is_blocked);
        flags.set_stream_consent(self.stream_consent);
        Profile {
            id: NO_NPUB,
            flags,
            in_cache: if avatar_short { AVATAR_IN_CACHE } else { 0 } | if banner_short { BANNER_IN_CACHE } else { 0 },
            last_updated: crate::compact::secs_to_compact(self.last_updated),
            text: ProfileText::build(&fields, &emoji),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub title: String,
    pub purpose: String,
    pub url: String,
    /// NIP-30 tags from the status event; absent in DB rows (memory-only).
    #[serde(default)]
    pub emoji_tags: Vec<crate::types::EmojiTag>,
}

impl Status {
    pub fn new() -> Self {
        Self { title: String::new(), purpose: String::new(), url: String::new(), emoji_tags: Vec::new() }
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Npub keys
// ============================================================================

/// The 32 key bytes of a canonical npub: lowercase, `npub1`, valid bech32 checksum, zero padding.
///
/// Exactly one string decodes to each key, so a map keyed by it never merges two npubs.
pub(crate) fn npub_bytes(npub: &str) -> Option<[u8; 32]> {
    let s = npub.as_bytes();
    if s.len() != 63 || !s.starts_with(b"npub1") {
        return None;
    }
    let mut chk = NPUB_HRP_CHECKSUM;
    let mut out = [0u8; 32];
    let (mut acc, mut bits, mut n) = (0u32, 0u32, 0usize);
    for (i, &c) in s[5..].iter().enumerate() {
        let v = bech32_value(c)?;
        chk = bech32_step(chk, v);
        if i < 52 {
            acc = (acc << 5) | v as u32;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out[n] = (acc >> bits) as u8;
                n += 1;
                acc &= (1 << bits) - 1;
            }
        }
    }
    (chk == 1 && bits == 4 && acc == 0).then_some(out)
}

const BECH32_CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

const BECH32_VALUES: [u8; 128] = {
    let mut table = [u8::MAX; 128];
    let mut i = 0;
    while i < 32 {
        table[BECH32_CHARSET[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// The BCH generator terms XORed in for each value of the checksum's top five bits.
const BECH32_GEN: [u32; 32] = {
    const GEN: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
    let mut table = [0u32; 32];
    let mut top = 0;
    while top < 32 {
        let mut i = 0;
        while i < 5 {
            if (top >> i) & 1 == 1 {
                table[top] ^= GEN[i];
            }
            i += 1;
        }
        top += 1;
    }
    table
};

const fn bech32_step(chk: u32, v: u8) -> u32 {
    ((chk & 0x01ff_ffff) << 5) ^ v as u32 ^ BECH32_GEN[(chk >> 25) as usize]
}

/// A lowercase bech32 character's 5-bit value.
fn bech32_value(c: u8) -> Option<u8> {
    BECH32_VALUES.get(c as usize).copied().filter(|&v| v != u8::MAX)
}

/// The checksum state after the expanded `npub` prefix.
const NPUB_HRP_CHECKSUM: u32 = {
    let hrp = b"npub";
    let mut chk = 1;
    let mut i = 0;
    while i < hrp.len() {
        chk = bech32_step(chk, hrp[i] >> 5);
        i += 1;
    }
    chk = bech32_step(chk, 0);
    i = 0;
    while i < hrp.len() {
        chk = bech32_step(chk, hrp[i] & 31);
        i += 1;
    }
    chk
};

/// A map keyed by npub, holding a canonical npub as its 32 key bytes and anything else as given.
#[derive(Debug)]
pub(crate) struct NpubMap<V> {
    keys: std::collections::HashMap<[u8; 32], V>,
    other: std::collections::HashMap<Box<str>, V>,
}

impl<V> Default for NpubMap<V> {
    fn default() -> Self {
        Self { keys: Default::default(), other: Default::default() }
    }
}

impl<V> NpubMap<V> {
    pub(crate) fn get(&self, npub: &str) -> Option<&V> {
        match npub_bytes(npub) {
            Some(key) => self.keys.get(&key),
            None => self.other.get(npub),
        }
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, npub: &str) -> bool {
        self.get(npub).is_some()
    }

    pub(crate) fn insert(&mut self, npub: &str, value: V) {
        match npub_bytes(npub) {
            Some(key) => {
                self.keys.insert(key, value);
            }
            None => {
                self.other.insert(npub.into(), value);
            }
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&V) -> bool) {
        self.keys.retain(|_, v| keep(v));
        self.other.retain(|_, v| keep(v));
    }

    pub(crate) fn clear(&mut self) {
        self.keys.clear();
        self.other.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.keys.len() + self.other.len()
    }
}

#[cfg(test)]
mod stream_consent_tests {
    use super::*;

    fn kind0(json: &str) -> Metadata {
        Metadata::from_json(json).unwrap()
    }

    #[test]
    fn a_kind0_opts_in_with_a_bool_or_a_true_string() {
        for json in [r#"{"name":"a","stream_consent":true}"#, r#"{"name":"a","stream_consent":"true"}"#, r#"{"stream_consent":"TRUE"}"#] {
            let mut p = Profile::new();
            assert!(p.from_metadata(kind0(json)), "{json} changes the profile");
            assert!(p.flags.is_stream_consent(), "{json} opts in");
        }
        for json in [r#"{"name":"a","stream_consent":false}"#, r#"{"stream_consent":"yes"}"#, r#"{"stream_consent":1}"#, r#"{"name":"a"}"#] {
            let mut p = Profile::new();
            p.from_metadata(kind0(json));
            assert!(!p.flags.is_stream_consent(), "{json} does not opt in");
        }
    }

    #[test]
    fn a_newer_kind0_without_the_key_revokes_consent() {
        let mut p = Profile::new();
        p.flags.set_bot(true);
        p.from_metadata(kind0(r#"{"name":"a","stream_consent":true}"#));
        assert!(p.flags.is_stream_consent());

        assert!(p.from_metadata(kind0(r#"{"name":"a"}"#)), "dropping the key is a change");
        assert!(!p.flags.is_stream_consent(), "absence means no consent");
        assert!(p.flags.is_bot(), "the other flags are untouched");

        assert!(!p.from_metadata(kind0(r#"{"name":"a"}"#)), "an unchanged kind-0 is no change");
    }

    #[test]
    fn the_consent_bit_is_independent_of_the_others() {
        let mut f = ProfileFlags::default();
        f.set_mine(true);
        f.set_blocked(true);
        f.set_bot(true);
        assert!(!f.is_stream_consent());
        f.set_stream_consent(true);
        assert!(f.is_mine() && f.is_blocked() && f.is_bot() && f.is_stream_consent());
        f.set_stream_consent(false);
        assert!(f.is_mine() && f.is_blocked() && f.is_bot() && !f.is_stream_consent());
    }

    #[test]
    fn with_stream_consent_sets_or_omits_the_key_and_keeps_the_rest() {
        let meta = kind0(r#"{"name":"a","pronouns":"they","stream_consent":"true"}"#);
        let on = with_stream_consent(meta.clone(), true);
        assert_eq!(on.custom.get(STREAM_CONSENT_KEY), Some(&serde_json::Value::Bool(true)));
        assert_eq!(on.custom.get("pronouns").and_then(|v| v.as_str()), Some("they"));

        let off = with_stream_consent(meta, false);
        let json = off.as_json();
        assert!(!json.contains(STREAM_CONSENT_KEY), "off publishes no key at all: {json}");
        assert!(json.contains("pronouns") && off.name.as_deref() == Some("a"));
    }

    #[test]
    fn slim_profile_round_trips_consent() {
        let mut interner = crate::compact::NpubInterner::new();
        let mut p = Profile::new();
        p.id = interner.intern("npub1consent");
        p.flags.set_stream_consent(true);

        let slim = SlimProfile::from_profile(&p, &interner);
        assert!(slim.stream_consent);
        assert!(slim.to_profile().flags.is_stream_consent());

        let wire = serde_json::to_value(&slim).unwrap();
        assert_eq!(wire["stream_consent"], serde_json::Value::Bool(true));
        let back: SlimProfile = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, slim);

        let mut legacy = wire;
        legacy.as_object_mut().unwrap().remove("stream_consent");
        let old: SlimProfile = serde_json::from_value(legacy).unwrap();
        assert!(!old.stream_consent, "a payload without the field reads as no consent");
    }
}

#[cfg(test)]
mod packed_tests {
    use super::*;
    use crate::types::EmojiTag;

    fn tag(s: &str, u: &str) -> EmojiTag {
        EmojiTag { shortcode: s.into(), url: u.into() }
    }

    /// Every string field distinct, so a slot read from the wrong offset shows.
    fn full_slim(about: String) -> SlimProfile {
        SlimProfile {
            id: "npub1packed".into(),
            name: "alice".into(),
            display_name: "Alice 🐈 Wonderland".into(),
            nickname: "ally".into(),
            lud06: "lnurl1dp68gurn8ghj7".into(),
            lud16: "alice@getalby.com".into(),
            banner: "https://image.nostr.build/banner.png".into(),
            avatar: "https://image.nostr.build/avatar.jpg".into(),
            about,
            website: "https://alice.example".into(),
            nip05: "alice@nostrplebs.com".into(),
            status: Status {
                title: "Listening to :cat: 猫".into(),
                purpose: "general".into(),
                url: "https://status.example/now".into(),
                emoji_tags: vec![tag("cat", "https://e.example/cat.png"), tag("", ""), tag("dog", "https://e.example/dog.gif")],
            },
            last_updated: 1_790_000_123,
            mine: true,
            bot: false,
            is_blocked: true,
            avatar_cached: "/elsewhere/a.webp".into(),
            banner_cached: "/elsewhere/b.webp".into(),
            stream_consent: true,
        }
    }

    fn round_trip(slim: &SlimProfile) -> SlimProfile {
        let mut interner = crate::compact::NpubInterner::new();
        let mut p = slim.to_profile();
        p.id = interner.intern(&slim.id);
        SlimProfile::from_profile(&p, &interner)
    }

    #[test]
    fn profile_is_three_words() {
        assert!(std::mem::size_of::<Profile>() <= 24, "Profile grew to {}B", std::mem::size_of::<Profile>());
    }

    #[test]
    fn every_field_round_trips() {
        let slim = full_slim("a short bio".into());
        assert_eq!(round_trip(&slim), slim);
        let p = slim.to_profile();
        assert_eq!(p.name(), "alice");
        assert_eq!(p.display_name(), "Alice 🐈 Wonderland");
        assert_eq!(p.nickname(), "ally");
        assert_eq!(p.lud06(), "lnurl1dp68gurn8ghj7");
        assert_eq!(p.lud16(), "alice@getalby.com");
        assert_eq!(p.banner(), "https://image.nostr.build/banner.png");
        assert_eq!(p.avatar(), "https://image.nostr.build/avatar.jpg");
        assert_eq!(p.about(), "a short bio");
        assert_eq!(p.website(), "https://alice.example");
        assert_eq!(p.nip05(), "alice@nostrplebs.com");
        assert_eq!(p.status_title(), "Listening to :cat: 猫");
        assert_eq!(p.status_purpose(), "general");
        assert_eq!(p.status_url(), "https://status.example/now");
        assert_eq!(p.status_emoji_tags(), slim.status.emoji_tags);
        assert_eq!(p.avatar_cached(), "/elsewhere/a.webp");
        assert_eq!(p.banner_cached(), "/elsewhere/b.webp");
    }

    #[test]
    fn the_frontend_json_is_unchanged() {
        let slim = full_slim("bio".into());
        assert_eq!(serde_json::to_string(&round_trip(&slim)).unwrap(), serde_json::to_string(&slim).unwrap());
    }

    #[test]
    fn a_long_about_widens_the_offsets() {
        let about = "ab🐈".repeat(20_000);
        assert!(about.len() > u16::MAX as usize);
        let slim = full_slim(about.clone());
        let p = slim.to_profile();
        let block = p.text.0.as_deref().unwrap();
        assert_ne!(u16::from_le_bytes([block[0], block[1]]) & WIDE, 0, "past 64 KiB the offsets are u32");
        assert_eq!(p.about(), about);
        assert_eq!(round_trip(&slim), slim);
    }

    #[test]
    fn a_short_block_keeps_narrow_offsets() {
        let p = full_slim("bio".into()).to_profile();
        let block = p.text.0.as_deref().unwrap();
        assert_eq!(u16::from_le_bytes([block[0], block[1]]) & WIDE, 0);
    }

    #[test]
    fn any_mix_of_empty_fields_round_trips() {
        let values: Vec<String> = (0..FIELDS).map(|i| format!("v{i}-{}", "x".repeat(i))).collect();
        for skip in 0..FIELDS {
            let mut fields = [""; FIELDS];
            for i in 0..FIELDS {
                if i != skip && (i + skip) % 3 != 0 {
                    fields[i] = &values[i];
                }
            }
            let text = ProfileText::build(&fields, &[("s", "u")]);
            assert_eq!(text.fields(), fields, "mask {skip}");
            assert_eq!(text.emoji().collect::<Vec<_>>(), [("s", "u")]);
        }
    }

    #[test]
    fn nothing_set_holds_no_block() {
        assert_eq!(Profile::new().heap_bytes(), 0);
        let slim = SlimProfile { id: "npub1empty".into(), ..Default::default() };
        let p = slim.to_profile();
        assert!(p.text.0.is_none(), "a seen-but-unfetched placeholder allocates nothing");
        assert_eq!(round_trip(&slim), slim);

        let mut q = Profile::new();
        q.set_name("x");
        q.set_name("");
        assert!(q.text.0.is_none(), "clearing the last field frees the block");
    }

    #[test]
    fn the_block_is_one_exact_allocation() {
        let p = full_slim("bio".into()).to_profile();
        let strings: usize = p.text.fields().iter().map(|f| f.len()).sum::<usize>()
            + p.text.emoji().map(|(s, u)| s.len() + u.len()).sum::<usize>();
        let slots = (0..FIELDS).filter(|&i| !p.text.get(i).is_empty()).count() + 2 * p.text.emoji().len();
        assert_eq!(p.heap_bytes(), HEADER + 2 * slots + strings);
    }

    #[test]
    fn each_setter_writes_only_its_field() {
        type Get = fn(&Profile) -> String;
        type Set = fn(&mut Profile, &str);
        let cases: [(Get, Set); 15] = [
            (|p| p.name().into(), |p, v| p.set_name(v)),
            (|p| p.display_name().into(), |p, v| p.set_display_name(v)),
            (|p| p.banner().into(), |p, v| p.set_banner(v)),
            (|p| p.avatar().into(), |p, v| p.set_avatar(v)),
            (|p| p.about().into(), |p, v| p.set_about(v)),
            (|p| p.avatar_cached().into(), |p, v| p.set_avatar_cached(v)),
            (|p| p.banner_cached().into(), |p, v| p.set_banner_cached(v)),
            (|p| p.nickname().into(), |p, v| p.set_nickname(v)),
            (|p| p.lud06().into(), |p, v| p.set_lud06(v)),
            (|p| p.lud16().into(), |p, v| p.set_lud16(v)),
            (|p| p.nip05().into(), |p, v| p.set_nip05(v)),
            (|p| p.website().into(), |p, v| p.set_website(v)),
            (|p| p.status_title().into(), |p, v| p.set_status_title(v)),
            (|p| p.status_purpose().into(), |p, v| p.set_status_purpose(v)),
            (|p| p.status_url().into(), |p, v| p.set_status_url(v)),
        ];
        let base = full_slim("bio".into()).to_profile();
        let long = "long".repeat(5000);
        for (i, (get, set)) in cases.iter().enumerate() {
            for value in ["", "changed ✓", long.as_str()] {
                let mut p = base.clone();
                set(&mut p, value);
                assert_eq!(get(&p), value, "field {i}");
                for (j, (other, _)) in cases.iter().enumerate() {
                    if j != i {
                        assert_eq!(other(&p), other(&base), "setting field {i} moved field {j}");
                    }
                }
                assert_eq!(p.status_emoji_tags(), base.status_emoji_tags(), "field {i} kept the emoji");
            }
        }
    }

    #[test]
    fn an_unchanged_write_keeps_the_block() {
        let mut p = full_slim("bio".into()).to_profile();
        let before = p.text.0.as_deref().unwrap().as_ptr();
        p.set_name("alice");
        let tags = p.status_emoji_tags();
        p.set_status_emoji_tags(&tags);
        assert_eq!(p.text.0.as_deref().unwrap().as_ptr(), before);
    }

    #[test]
    fn status_writes_replace_title_purpose_url_and_tags() {
        let mut p = full_slim("bio".into()).to_profile();
        p.set_status("new", "music", "", &[tag("x", "https://x")]);
        assert_eq!((p.status_title(), p.status_purpose(), p.status_url()), ("new", "music", ""));
        assert_eq!(p.status_emoji_tags(), [tag("x", "https://x")]);
        assert_eq!(p.name(), "alice");
        p.set_status_emoji_tags(&[]);
        assert!(p.status_emoji_tags().is_empty());
        assert_eq!(p.status_title(), "new");
    }

    #[test]
    fn metadata_merges_like_the_field_by_field_version() {
        let mut p = full_slim("bio".into()).to_profile();
        let meta = Metadata::new().name("alice").display_name("New Name").about("bio");
        assert!(p.from_metadata(meta.clone().custom_field(STREAM_CONSENT_KEY, true)), "a new display name is a change");
        assert_eq!(p.display_name(), "New Name");
        assert_eq!(p.avatar_cached(), "/elsewhere/a.webp", "an unchanged picture keeps its cache");
        assert_eq!(p.status_emoji_tags().len(), 3, "metadata never touches the status");
        assert_eq!(p.nickname(), "ally");
        assert!(!p.from_metadata(meta.custom_field(STREAM_CONSENT_KEY, true)), "the same kind-0 changes nothing");

        let mut q = p.clone();
        q.from_metadata(Metadata::from_json(r#"{"picture":"https://new/a.png","banner":"https://new/b.png"}"#).unwrap());
        assert_eq!(q.avatar(), "https://new/a.png");
        assert!(!q.has_avatar_cached() && !q.has_banner_cached(), "a new image drops the old cached file");
        assert_eq!(q.name(), "alice", "absent keys keep their field");
    }

    #[test]
    fn debug_shows_the_fields() {
        let s = format!("{:?}", full_slim("bio".into()).to_profile());
        assert!(s.contains("\"alice\"") && s.contains("\"https://status.example/now\""), "{s}");
    }

    // ------------------------------------------------------------------
    // Cached image paths
    // ------------------------------------------------------------------

    fn sep() -> char {
        std::path::MAIN_SEPARATOR
    }

    #[test]
    fn a_path_in_the_cache_dir_keeps_only_its_file_name() {
        let dir = format!("{s}data{s}cache{s}avatars", s = sep());
        let path = format!("{dir}{}0123abcd.webp", sep());
        assert_eq!(cached_file_name(&path, &dir), Some("0123abcd.webp"));
        assert_eq!(expand_cached("0123abcd.webp", &dir), path);
    }

    #[test]
    fn a_path_outside_the_cache_dir_is_kept_whole() {
        let s = sep();
        let dir = format!("{s}data{s}cache{s}avatars");
        for path in [
            format!("{s}data{s}cache{s}banners{s}x.webp"),
            format!("{dir}x.webp"),
            format!("{dir}{s}nested{s}x.webp"),
            format!("{dir}{s}nested/x.webp"),
            format!("{dir}{s}"),
            dir.clone(),
            format!("{s}other{dir}{s}x.webp"),
            "x.webp".to_string(),
            String::new(),
        ] {
            assert_eq!(cached_file_name(&path, &dir), None, "{path}");
        }
    }

    fn test_cache_dir(sub: &str) -> String {
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::get_app_data_dir().unwrap().join("cache").join(sub).to_string_lossy().into_owned()
    }

    #[test]
    fn a_cached_path_in_the_app_cache_is_stored_short_and_read_whole() {
        let avatars = test_cache_dir("avatars");
        let banners = test_cache_dir("banners");
        let avatar = format!("{avatars}{}0123456789abcdef0123456789abcdef.webp", sep());
        let banner = format!("{banners}{}fedcba9876543210fedcba9876543210.png", sep());

        let mut p = Profile::new();
        p.set_avatar_cached(&avatar);
        p.set_banner_cached(&banner);
        assert_eq!(p.text.get(AVATAR_CACHED), "0123456789abcdef0123456789abcdef.webp");
        assert_eq!(p.text.get(BANNER_CACHED), "fedcba9876543210fedcba9876543210.png");
        assert_eq!(p.avatar_cached(), avatar);
        assert_eq!(p.banner_cached(), banner);

        let mut slim = full_slim("bio".into());
        slim.avatar_cached = avatar.clone();
        slim.banner_cached = banner.clone();
        assert_eq!(slim.to_profile().text.get(AVATAR_CACHED).len(), 37);
        assert_eq!(round_trip(&slim), slim, "the DB and the frontend still get full paths");

        let mut crossed = slim.clone();
        crossed.avatar_cached = banner.clone();
        crossed.banner_cached = avatar.clone();
        assert_eq!(round_trip(&crossed), crossed, "a path in the other kind's dir stays whole");
        assert_eq!(crossed.to_profile().text.get(AVATAR_CACHED), banner);
    }

    #[test]
    fn a_cached_path_elsewhere_is_stored_whole() {
        let avatars = test_cache_dir("avatars");
        let mut p = Profile::new();
        for path in [format!("{avatars}{s}nested{s}x.webp", s = sep()), "/tmp/x.webp".into(), "x.webp".into()] {
            p.set_avatar_cached(&path);
            assert_eq!(p.text.get(AVATAR_CACHED), path);
            assert_eq!(p.avatar_cached(), path);
        }
    }

    #[test]
    fn a_short_cached_path_follows_every_rewrite() {
        let avatars = test_cache_dir("avatars");
        let short = format!("{avatars}{}a.webp", sep());
        let mut p = Profile::new();
        p.set_avatar_cached(&short);
        p.set_avatar_cached("a.webp");
        assert_eq!(p.avatar_cached(), "a.webp", "a bare name stored as given reads back bare");
        p.set_avatar_cached(&short);
        assert_eq!(p.avatar_cached(), short);
        p.set_name("x");
        assert_eq!(p.avatar_cached(), short, "rebuilding for another field keeps it short");
        p.set_avatar_cached("");
        assert_eq!(p.avatar_cached(), "");
        assert!(!p.has_avatar_cached());

        p.set_avatar_cached(&short);
        p.from_metadata(Metadata::from_json(r#"{"picture":"https://new/a.png"}"#).unwrap());
        assert_eq!(p.avatar_cached(), "", "a new picture drops the short path too");
        p.set_avatar_cached("/tmp/b.webp");
        assert_eq!(p.avatar_cached(), "/tmp/b.webp");
    }

    #[test]
    fn equality_reads_through_the_short_form() {
        let avatars = test_cache_dir("avatars");
        let path = format!("{avatars}{}a.webp", sep());
        let mut short = Profile::new();
        short.set_avatar_cached(&path);
        let mut whole = Profile::new();
        whole.text = ProfileText::build(&std::array::from_fn(|i| if i == AVATAR_CACHED { path.as_str() } else { "" }), &[]);
        assert_eq!(short, whole);
        whole.set_name("y");
        assert_ne!(short, whole);
    }

    // ------------------------------------------------------------------
    // Npub keys
    // ------------------------------------------------------------------

    /// Test-only bech32 encoder over the decoder's own tables.
    fn encode_npub(data: &[u8]) -> String {
        let mut chk = NPUB_HRP_CHECKSUM;
        for &v in data {
            chk = bech32_step(chk, v);
        }
        for _ in 0..6 {
            chk = bech32_step(chk, 0);
        }
        chk ^= 1;
        let mut s = String::from("npub1");
        s.extend(data.iter().map(|&v| BECH32_CHARSET[v as usize] as char));
        s.extend((0..6).map(|i| BECH32_CHARSET[((chk >> (5 * (5 - i))) & 31) as usize] as char));
        s
    }

    fn five_bit(bytes: &[u8; 32], padding: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for &b in bytes {
            acc = ((acc << 8) | b as u32) & 0xffff;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(((acc >> bits) & 31) as u8);
            }
        }
        out.push((((acc << (5 - bits)) & 31) as u8) | padding);
        out
    }

    #[test]
    fn npub_bytes_matches_nostr() {
        use nostr_sdk::prelude::{Keys, ToBech32};
        for _ in 0..64 {
            let pk = Keys::generate().public_key();
            let npub = pk.to_bech32().unwrap();
            assert_eq!(npub_bytes(&npub), Some(pk.to_bytes()), "{npub}");
            assert_eq!(encode_npub(&five_bit(&pk.to_bytes(), 0)), npub);
        }
    }

    #[test]
    fn npub_bytes_refuses_every_other_spelling() {
        use nostr_sdk::prelude::{Keys, ToBech32};
        let pk = Keys::generate().public_key();
        let npub = pk.to_bech32().unwrap();
        let mut last = npub.clone().into_bytes();
        let i = last.len() - 1;
        last[i] = if last[i] == b'q' { b'p' } else { b'q' };
        for bad in [
            npub.to_uppercase(),
            String::from_utf8(last).unwrap(),
            npub.replacen("npub1", "nsec1", 1),
            npub[..62].to_string(),
            format!("{npub}q"),
            encode_npub(&five_bit(&pk.to_bytes(), 1)),
            "npub1alice".to_string(),
            String::new(),
        ] {
            assert_eq!(npub_bytes(&bad), None, "{bad}");
        }
    }

    #[test]
    fn npub_map_keys_npubs_by_bytes_and_keeps_the_rest() {
        use nostr_sdk::prelude::{Keys, ToBech32};
        let mut map = NpubMap::default();
        let npubs: Vec<String> = (0..50).map(|_| Keys::generate().public_key().to_bech32().unwrap()).collect();
        for (i, n) in npubs.iter().enumerate() {
            map.insert(n, i);
        }
        map.insert("npub1alice", 100);
        map.insert(&npubs[0].to_uppercase(), 101);
        assert_eq!(map.keys.len(), 50);
        assert_eq!(map.other.len(), 2);
        for (i, n) in npubs.iter().enumerate() {
            assert_eq!(map.get(n), Some(&i));
        }
        assert_eq!(map.get("npub1alice"), Some(&100));
        assert_eq!(map.get(&npubs[0].to_uppercase()), Some(&101), "a non-canonical spelling is its own key");
        map.retain(|&v| v % 2 == 0);
        assert_eq!(map.len(), 26);
        assert!(map.contains(&npubs[2]) && !map.contains(&npubs[1]));
        map.clear();
        assert_eq!(map.len(), 0);
    }
}
