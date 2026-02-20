//! Profile database operations.
//!
//! This module handles:
//! - SlimProfile struct for serialization boundaries (DB + frontend)
//! - Profile CRUD operations

use serde::{Deserialize, Serialize};
use tauri::command;

use crate::{Profile, Status};
use crate::profile::ProfileFlags;
use crate::message::compact::{NpubInterner, secs_to_compact, secs_from_compact};

/// Serializable profile for DB storage and frontend communication.
///
/// This is the boundary type: Profile uses `id: u16` (interner handle) internally,
/// SlimProfile uses `id: String` (npub) for external interfaces.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
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
    pub avatar_cached: String,
    pub banner_cached: String,
}

impl Default for SlimProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            display_name: String::new(),
            nickname: String::new(),
            lud06: String::new(),
            lud16: String::new(),
            banner: String::new(),
            avatar: String::new(),
            about: String::new(),
            website: String::new(),
            nip05: String::new(),
            status: Status::new(),
            last_updated: 0,
            mine: false,
            bot: false,
            avatar_cached: String::new(),
            banner_cached: String::new(),
        }
    }
}

impl SlimProfile {
    /// Resolve a Profile's interned id to string for serialization.
    pub fn from_profile(profile: &Profile, interner: &NpubInterner) -> Self {
        SlimProfile {
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
            last_updated: secs_from_compact(profile.last_updated),
            mine: profile.flags.is_mine(),
            bot: profile.flags.is_bot(),
            avatar_cached: profile.avatar_cached.to_string(),
            banner_cached: profile.banner_cached.to_string(),
        }
    }

    /// Convert to internal Profile (id will be set by insert_or_replace_profile).
    pub fn to_profile(&self) -> crate::Profile {
        let mut flags = ProfileFlags::default();
        flags.set_mine(self.mine);
        flags.set_bot(self.bot);

        crate::Profile {
            id: crate::message::compact::NO_NPUB,
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
            last_updated: secs_to_compact(self.last_updated),
            flags,
            avatar_cached: self.avatar_cached.clone().into_boxed_str(),
            banner_cached: self.banner_cached.clone().into_boxed_str(),
        }
    }
}

// Function to get all profiles
pub async fn get_all_profiles() -> Result<Vec<SlimProfile>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, bot, avatar_cached, banner_cached FROM profiles")
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let profiles = stmt.query_map([], |row| {
        Ok(SlimProfile {
            id: row.get(0)?,  // npub column
            name: row.get(1)?,
            display_name: row.get(2)?,
            nickname: row.get(3)?,
            lud06: row.get(4)?,
            lud16: row.get(5)?,
            banner: row.get(6)?,
            avatar: row.get(7)?,
            about: row.get(8)?,
            website: row.get(9)?,
            nip05: row.get(10)?,
            status: crate::Status {
                title: row.get(11)?,
                purpose: String::new(),
                url: row.get(12)?,
            },
            last_updated: 0,
            mine: false,
            bot: row.get::<_, i32>(13)? != 0,
            avatar_cached: {
                let p: String = row.get(14)?;
                if !p.is_empty() && !std::path::Path::new(&p).exists() { String::new() } else { p }
            },
            banner_cached: {
                let p: String = row.get(15)?;
                if !p.is_empty() && !std::path::Path::new(&p).exists() { String::new() } else { p }
            },
        })
    })
    .map_err(|e| format!("Failed to query profiles: {}", e))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("Failed to collect profiles: {}", e))?;


    Ok(profiles)
}


// Public command to set a profile
#[command]
pub async fn set_profile(profile: SlimProfile) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, bot, avatar_cached, banner_cached)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(npub) DO UPDATE SET
            name = excluded.name,
            display_name = excluded.display_name,
            nickname = excluded.nickname,
            lud06 = excluded.lud06,
            lud16 = excluded.lud16,
            banner = excluded.banner,
            avatar = excluded.avatar,
            about = excluded.about,
            website = excluded.website,
            nip05 = excluded.nip05,
            status_content = excluded.status_content,
            status_url = excluded.status_url,
            bot = excluded.bot,
            avatar_cached = excluded.avatar_cached,
            banner_cached = excluded.banner_cached",
        rusqlite::params![
            profile.id,  // This is the npub
            profile.name,
            profile.display_name,
            profile.nickname,
            profile.lud06,
            profile.lud16,
            profile.banner,
            profile.avatar,
            profile.about,
            profile.website,
            profile.nip05,
            profile.status.title,
            profile.status.url,
            profile.bot as i32,
            profile.avatar_cached,
            profile.banner_cached,
        ],
    ).map_err(|e| format!("Failed to insert profile: {}", e))?;

    Ok(())
}