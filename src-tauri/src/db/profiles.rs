//! Profile database operations.

use tauri::command;

pub use vector_core::SlimProfile;
pub async fn get_all_profiles() -> Result<Vec<SlimProfile>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, bot, avatar_cached, banner_cached, is_blocked FROM profiles")
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
            is_blocked: row.get::<_, i32>(16).unwrap_or(0) != 0,
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
        "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, bot, avatar_cached, banner_cached, is_blocked)
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
            bot = excluded.bot,
            avatar_cached = excluded.avatar_cached,
            banner_cached = excluded.banner_cached,
            is_blocked = excluded.is_blocked",
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
            profile.is_blocked as i32,
        ],
    ).map_err(|e| format!("Failed to insert profile: {}", e))?;

    Ok(())
}
