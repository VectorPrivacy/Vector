//! Profile database operations.
//!
//! This module handles:
//! - SlimProfile struct for serialization boundaries (DB + frontend)
//! - Profile CRUD operations

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};

use crate::{Profile, Status};
use crate::message::compact::NpubInterner;

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
    pub muted: bool,
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
            muted: false,
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
            name: profile.name.clone(),
            display_name: profile.display_name.clone(),
            nickname: profile.nickname.clone(),
            lud06: profile.lud06.clone(),
            lud16: profile.lud16.clone(),
            banner: profile.banner.clone(),
            avatar: profile.avatar.clone(),
            about: profile.about.clone(),
            website: profile.website.clone(),
            nip05: profile.nip05.clone(),
            status: profile.status.clone(),
            last_updated: profile.last_updated,
            mine: profile.mine,
            muted: profile.muted,
            bot: profile.bot,
            avatar_cached: profile.avatar_cached.clone(),
            banner_cached: profile.banner_cached.clone(),
        }
    }

    /// Convert to internal Profile (id will be set by insert_or_replace_profile).
    pub fn to_profile(&self) -> crate::Profile {
        crate::Profile {
            id: crate::message::compact::NO_NPUB,
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            nickname: self.nickname.clone(),
            lud06: self.lud06.clone(),
            lud16: self.lud16.clone(),
            banner: self.banner.clone(),
            avatar: self.avatar.clone(),
            about: self.about.clone(),
            website: self.website.clone(),
            nip05: self.nip05.clone(),
            status: self.status.clone(),
            last_updated: self.last_updated,
            mine: self.mine,
            muted: self.muted,
            bot: self.bot,
            avatar_cached: self.avatar_cached.clone(),
            banner_cached: self.banner_cached.clone(),
        }
    }
}

// Function to get all profiles
pub async fn get_all_profiles<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimProfile>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot, avatar_cached, banner_cached FROM profiles")
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let profiles = stmt.query_map([], |row| {
        // Get cached paths and validate they exist on disk
        let avatar_cached: String = row.get(15)?;
        let banner_cached: String = row.get(16)?;

        // Only use cached paths if the files actually exist
        let validated_avatar_cached = if !avatar_cached.is_empty() && std::path::Path::new(&avatar_cached).exists() {
            avatar_cached
        } else {
            String::new()
        };
        let validated_banner_cached = if !banner_cached.is_empty() && std::path::Path::new(&banner_cached).exists() {
            banner_cached
        } else {
            String::new()
        };

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
            muted: row.get::<_, i32>(13)? != 0,
            bot: row.get::<_, i32>(14)? != 0,
            avatar_cached: validated_avatar_cached,
            banner_cached: validated_banner_cached,
        })
    })
    .map_err(|e| format!("Failed to query profiles: {}", e))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("Failed to collect profiles: {}", e))?;

    drop(stmt); // Explicitly drop stmt before returning connection
    crate::account_manager::return_db_connection(conn);
    Ok(profiles)
}


// Public command to set a profile
#[command]
pub async fn set_profile<R: Runtime>(handle: AppHandle<R>, profile: SlimProfile) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot, avatar_cached, banner_cached)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
            muted = excluded.muted,
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
            profile.muted as i32,
            profile.bot as i32,
            profile.avatar_cached,
            profile.banner_cached,
        ],
    ).map_err(|e| format!("Failed to insert profile: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}