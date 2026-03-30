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
// ProfileFlags — 3 bools packed into 1 byte
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileFlags(u8);

impl ProfileFlags {
    const MINE:    u8 = 0b001;
    const BLOCKED: u8 = 0b010;
    const BOT:     u8 = 0b100;

    #[inline] pub fn is_mine(self) -> bool    { self.0 & Self::MINE != 0 }
    #[inline] pub fn is_blocked(self) -> bool  { self.0 & Self::BLOCKED != 0 }
    #[inline] pub fn is_bot(self) -> bool      { self.0 & Self::BOT != 0 }

    #[inline] pub fn set_mine(&mut self, v: bool)    { if v { self.0 |= Self::MINE } else { self.0 &= !Self::MINE } }
    #[inline] pub fn set_blocked(&mut self, v: bool)  { if v { self.0 |= Self::BLOCKED } else { self.0 &= !Self::BLOCKED } }
    #[inline] pub fn set_bot(&mut self, v: bool)      { if v { self.0 |= Self::BOT } else { self.0 &= !Self::BOT } }
}

// ============================================================================
// Profile — compact internal representation
// ============================================================================

/// Internal profile with u16 interner handle. All string fields use `Box<str>`
/// (16B) instead of `String` (24B) — profile strings are write-once from metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: u16,
    pub name: Box<str>,
    pub display_name: Box<str>,
    pub nickname: Box<str>,
    pub lud06: Box<str>,
    pub lud16: Box<str>,
    pub banner: Box<str>,
    pub avatar: Box<str>,
    pub about: Box<str>,
    pub website: Box<str>,
    pub nip05: Box<str>,
    pub status_title: Box<str>,
    pub status_purpose: Box<str>,
    pub status_url: Box<str>,
    pub last_updated: u32,
    pub flags: ProfileFlags,
    pub avatar_cached: Box<str>,
    pub banner_cached: Box<str>,
}

impl Default for Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl Profile {
    pub fn new() -> Self {
        Self {
            id: NO_NPUB,
            name: Box::<str>::default(),
            display_name: Box::<str>::default(),
            nickname: Box::<str>::default(),
            lud06: Box::<str>::default(),
            lud16: Box::<str>::default(),
            banner: Box::<str>::default(),
            avatar: Box::<str>::default(),
            about: Box::<str>::default(),
            website: Box::<str>::default(),
            nip05: Box::<str>::default(),
            status_title: Box::<str>::default(),
            status_purpose: Box::<str>::default(),
            status_url: Box::<str>::default(),
            last_updated: 0,
            flags: ProfileFlags::default(),
            avatar_cached: Box::<str>::default(),
            banner_cached: Box::<str>::default(),
        }
    }

    /// Merge Nostr Metadata into this Profile. Returns `true` if any fields changed.
    pub fn from_metadata(&mut self, meta: Metadata) -> bool {
        let mut changed = false;

        if let Some(name) = meta.name {
            if *self.name != *name { self.name = name.into_boxed_str(); changed = true; }
        }
        if let Some(name) = meta.display_name {
            if *self.display_name != *name { self.display_name = name.into_boxed_str(); changed = true; }
        }
        if let Some(lud06) = meta.lud06 {
            if *self.lud06 != *lud06 { self.lud06 = lud06.into_boxed_str(); changed = true; }
        }
        if let Some(lud16) = meta.lud16 {
            if *self.lud16 != *lud16 { self.lud16 = lud16.into_boxed_str(); changed = true; }
        }
        if let Some(banner) = meta.banner {
            if *self.banner != *banner {
                self.banner = banner.into_boxed_str();
                self.banner_cached = Box::<str>::default();
                changed = true;
            }
        }
        if let Some(picture) = meta.picture {
            if *self.avatar != *picture {
                self.avatar = picture.into_boxed_str();
                self.avatar_cached = Box::<str>::default();
                changed = true;
            }
        }
        if let Some(about) = meta.about {
            if *self.about != *about { self.about = about.into_boxed_str(); changed = true; }
        }
        if let Some(website) = meta.website {
            if *self.website != *website { self.website = website.into_boxed_str(); changed = true; }
        }
        if let Some(nip05) = meta.nip05 {
            if *self.nip05 != *nip05 { self.nip05 = nip05.into_boxed_str(); changed = true; }
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
}

impl SlimProfile {
    /// Convert from internal Profile, resolving interner handle to npub.
    pub fn from_profile(profile: &Profile, interner: &crate::compact::NpubInterner) -> Self {
        Self {
            id: interner.resolve(profile.id).unwrap_or("").to_string(),
            name: profile.name.to_string(),
            display_name: profile.display_name.to_string(),
            nickname: profile.nickname.to_string(),
            lud06: profile.lud06.to_string(),
            lud16: profile.lud16.to_string(),
            banner: profile.banner.to_string(),
            avatar: profile.avatar.to_string(),
            about: profile.about.to_string(),
            website: profile.website.to_string(),
            nip05: profile.nip05.to_string(),
            status: Status {
                title: profile.status_title.to_string(),
                purpose: profile.status_purpose.to_string(),
                url: profile.status_url.to_string(),
            },
            last_updated: crate::compact::secs_from_compact(profile.last_updated),
            mine: profile.flags.is_mine(),
            bot: profile.flags.is_bot(),
            is_blocked: profile.flags.is_blocked(),
            avatar_cached: profile.avatar_cached.to_string(),
            banner_cached: profile.banner_cached.to_string(),
        }
    }

    /// Convert to internal Profile (for loading from DB).
    pub fn to_profile(&self) -> Profile {
        Profile {
            id: NO_NPUB,
            name: self.name.clone().into_boxed_str(),
            display_name: self.display_name.clone().into_boxed_str(),
            nickname: self.nickname.clone().into_boxed_str(),
            lud06: self.lud06.clone().into_boxed_str(),
            lud16: self.lud16.clone().into_boxed_str(),
            banner: self.banner.clone().into_boxed_str(),
            avatar: self.avatar.clone().into_boxed_str(),
            about: self.about.clone().into_boxed_str(),
            website: self.website.clone().into_boxed_str(),
            nip05: self.nip05.clone().into_boxed_str(),
            status_title: self.status.title.clone().into_boxed_str(),
            status_purpose: self.status.purpose.clone().into_boxed_str(),
            status_url: self.status.url.clone().into_boxed_str(),
            last_updated: crate::compact::secs_to_compact(self.last_updated),
            flags: {
                let mut f = ProfileFlags::default();
                f.set_mine(self.mine);
                f.set_bot(self.bot);
                f.set_blocked(self.is_blocked);
                f
            },
            avatar_cached: self.avatar_cached.clone().into_boxed_str(),
            banner_cached: self.banner_cached.clone().into_boxed_str(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub title: String,
    pub purpose: String,
    pub url: String,
}

impl Status {
    pub fn new() -> Self {
        Self { title: String::new(), purpose: String::new(), url: String::new() }
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}
