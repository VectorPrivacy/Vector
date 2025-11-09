use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};
use tauri_plugin_store::StoreBuilder;
use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;

use crate::{Profile, Status, Attachment, Message, Reaction};
use crate::net::SiteMetadata;
use crate::crypto::{internal_encrypt, internal_decrypt};

const DB_PATH: &str = "vector.json";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct SlimProfile {
    pub id: String,
    name: String,
    display_name: String,
    nickname: String,
    lud06: String,
    lud16: String,
    banner: String,
    avatar: String,
    about: String,
    website: String,
    nip05: String,
    status: Status,
    muted: bool,
    bot: bool,
    // Omitting: messages, last_updated, mine
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
            muted: false,
            bot: false,
        }
    }
}

impl From<&Profile> for SlimProfile {
    fn from(profile: &Profile) -> Self {
        SlimProfile {
            id: profile.id.clone(),
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
            muted: profile.muted,
            bot: profile.bot,
        }
    }
}

impl SlimProfile {
    // Convert back to full Profile
    pub fn to_profile(&self) -> crate::Profile {
        crate::Profile {
            id: self.id.clone(),
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
            last_updated: 0,      // Default value
            mine: false,          // Default value
            muted: self.muted,
            bot: self.bot,
        }
    }
}

// Function to get all profiles
pub async fn get_all_profiles<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimProfile>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode - read from database
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot FROM profiles")
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
                    purpose: String::new(), // Not stored separately
                    url: row.get(12)?,
                },
                muted: row.get::<_, i32>(13)? != 0,
                bot: row.get::<_, i32>(14)? != 0,
            })
        })
        .map_err(|e| format!("Failed to query profiles: {}", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect profiles: {}", e))?;
        
        drop(stmt); // Explicitly drop stmt before returning connection
        crate::account_manager::return_db_connection(conn);
        return Ok(profiles);
    }
    
    // Fallback to Store mode (for backward compatibility during transition)
    let store = get_store(handle);
    
    // Get the encrypted profiles
    let encrypted: String = match store.get("profiles") {
        Some(value) if value.is_string() => value.as_str().unwrap().to_string(),
        _ => return Ok(vec![]), // No profiles or wrong format
    };
    
    // Decrypt
    let json = internal_decrypt(encrypted, None).await
        .expect("Failed to decrypt profiles");
    
    // Deserialize
    let slim_profiles: Vec<SlimProfile> = serde_json::from_str(&json)
        .map_err(|e| format!("Failed to deserialize profiles: {}", e))?;
    
    Ok(slim_profiles)
}


// Public command to set a profile
#[command]
pub async fn set_profile<R: Runtime>(handle: AppHandle<R>, profile: Profile) -> Result<(), String> {
    // Try SQL first if account is selected
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
                bot = excluded.bot",
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
            ],
        ).map_err(|e| format!("Failed to insert profile: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }

    // During migration, profiles are being loaded but account isn't selected yet
    // Just skip saving to Store since migration will handle it
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimMessage {
    pub id: String,
    pub content: String,
    pub replied_to: String,
    pub preview_metadata: Option<SiteMetadata>,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub at: u64,
    pub mine: bool,
    pub contact: String,  // Reference to which contact/profile this message belongs to
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub npub: Option<String>,  // Sender's npub (for group chats)
}

impl From<(&Message, String)> for SlimMessage {
    fn from((msg, contact_id): (&Message, String)) -> Self {
        SlimMessage {
            id: msg.id.clone(),
            content: msg.content.clone(),
            replied_to: msg.replied_to.clone(),
            preview_metadata: msg.preview_metadata.clone(),
            attachments: msg.attachments.clone(),
            reactions: msg.reactions.clone(),
            at: msg.at,
            mine: msg.mine,
            contact: contact_id.clone(),
            npub: msg.npub.clone(),
        }
    }
}

impl SlimMessage {
    // Convert back to full Message
    pub fn to_message(&self) -> Message {
        Message {
            id: self.id.clone(),
            content: self.content.clone(),
            replied_to: self.replied_to.clone(),
            preview_metadata: self.preview_metadata.clone(),
            attachments: self.attachments.clone(),
            reactions: self.reactions.clone(),
            at: self.at,
            pending: false, // Default values
            failed: false,  // Default values
            mine: self.mine,
            npub: self.npub.clone(),
        }
    }
}

pub fn get_store<R: Runtime>(handle: &AppHandle<R>) -> Arc<tauri_plugin_store::Store<R>> {
    StoreBuilder::new(handle, PathBuf::from(DB_PATH))
        .auto_save(Duration::from_secs(2))
        .build()
        .unwrap()
}


#[command]
pub fn get_theme<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    // Try SQL first
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        return get_sql_setting(handle.clone(), "theme".to_string());
    }
    
    // Fallback to Store (pre-login)
    let store = get_store(&handle);
    match store.get("theme") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_pkey<R: Runtime>(handle: AppHandle<R>, pkey: String) -> Result<(), String> {
    // Try SQL first if account is selected
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params!["pkey", pkey],
        ).map_err(|e| format!("Failed to insert pkey: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    
    // Fallback to Store (migration pending - user is re-encrypting with new PIN)
    let store = get_store(&handle);
    store.set("pkey", serde_json::json!(pkey));
    store.save().map_err(|e| format!("Failed to save pkey to Store: {}", e))?;
    Ok(())
}

#[command]
pub fn get_pkey<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        let result: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params!["pkey"],
            |row| row.get(0)
        ).ok();
        
        crate::account_manager::return_db_connection(conn);
        return Ok(result);
    }
    
    // Fallback to Store (pre-login)
    let store = get_store(&handle);
    match store.get("pkey") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub async fn set_seed<R: Runtime>(handle: AppHandle<R>, seed: String) -> Result<(), String> {
    let encrypted_seed = internal_encrypt(seed, None).await;
    
    let conn = crate::account_manager::get_db_connection(&handle)?;
    
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["seed", encrypted_seed],
    ).map_err(|e| format!("Failed to insert seed: {}", e))?;
    
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

#[command]
pub async fn get_seed<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    // Try SQL first
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        let encrypted_seed: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params!["seed"],
            |row| row.get(0)
        ).ok();
        
        crate::account_manager::return_db_connection(conn);
        
        if let Some(encrypted) = encrypted_seed {
            match internal_decrypt(encrypted, None).await {
                Ok(decrypted) => return Ok(Some(decrypted)),
                Err(_) => return Err("Failed to decrypt seed phrase".to_string()),
            }
        }
        
        return Ok(None);
    }
    
    // Fallback to Store (pre-login)
    let store = get_store(&handle);
    match store.get("seed") {
        Some(value) if value.is_string() => {
            let encrypted_seed = value.as_str().unwrap().to_string();
            match internal_decrypt(encrypted_seed, None).await {
                Ok(decrypted) => Ok(Some(decrypted)),
                Err(_) => Err("Failed to decrypt seed phrase".to_string()),
            }
        },
        _ => Ok(None),
    }
}

/// Set a setting value in SQL database
#[command]
pub fn set_sql_setting<R: Runtime>(handle: AppHandle<R>, key: String, value: String) -> Result<(), String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![&key, &value],
        ).map_err(|e| format!("Failed to set setting: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    Ok(())
}

/// Get a setting value from SQL database
#[command]
pub fn get_sql_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<Option<String>, String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        let result: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![&key],
            |row| row.get(0)
        ).ok();
        
        crate::account_manager::return_db_connection(conn);
        return Ok(result);
    }
    Ok(None)
}


#[command]
pub fn remove_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;
    
    let rows_affected = conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![key],
    ).map_err(|e| format!("Failed to delete setting: {}", e))?;
    
    crate::account_manager::return_db_connection(conn);
    Ok(rows_affected > 0)
}