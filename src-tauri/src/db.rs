use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};
use tauri_plugin_store::StoreBuilder;
use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;
use std::collections::HashMap;

use crate::{Profile, Status, Attachment, Message, Reaction};
use crate::net::SiteMetadata;
use crate::crypto::{internal_encrypt, internal_decrypt};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VectorDB {
    pub db_version: Option<u64>,
    pub theme: Option<String>,
    pub pkey: Option<String>,
    pub seed: Option<String>,
    pub invite_code: Option<String>,
}

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
    /// Deprecated: Moved to Chat.last_read. This field is only kept for migration purposes.
    /// Follow-up plan to drop this field:
    /// 1. In the next release, stop using this field in the migration process
    /// 2. In a subsequent release, remove this field from the struct and all related code
    last_read: String,
    status: Status,
    muted: bool,
    // Omitting: messages, last_updated, typing_until, mine
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
            last_read: String::new(),
            status: Status::new(),
            muted: false,
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
            last_read: profile.last_read.clone(),
            status: profile.status.clone(),
            muted: profile.muted,
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
            last_read: self.last_read.clone(),
            status: self.status.clone(),
            last_updated: 0,      // Default value
            typing_until: 0,      // Default value
            mine: false,          // Default value
            muted: self.muted,
        }
    }
    
    // Getter for last_read field
    pub fn last_read(&self) -> &str {
        &self.last_read
    }
}

// Function to get all profiles
pub async fn get_all_profiles<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimProfile>, String> {
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

// Function to save all profiles
async fn save_all_profiles<R: Runtime>(handle: &AppHandle<R>, profiles: Vec<SlimProfile>) -> Result<(), String> {
    let store = get_store(handle);
    
    // Serialize to JSON
    let json = serde_json::to_string(&profiles)
        .map_err(|e| format!("Failed to serialize profiles: {}", e))?;
    
    // Encrypt the entire array
    let encrypted = internal_encrypt(json, None).await;
    
    // Store in the DB
    store.set("profiles".to_string(), serde_json::json!(encrypted));
    
    Ok(())
}

// Public command to set a profile
#[command]
pub async fn set_profile<R: Runtime>(handle: AppHandle<R>, profile: Profile) -> Result<(), String> {
    // Get current profiles
    let mut profiles = get_all_profiles(&handle).await?;
    
    // Convert the input profile to slim profile
    let new_slim_profile = SlimProfile::from(&profile);
    let profile_id = new_slim_profile.id.clone();
    
    // Find and replace the profile if it exists, or add it
    if let Some(pos) = profiles.iter().position(|p| p.id == profile_id) {
        profiles[pos] = new_slim_profile;
    } else {
        profiles.push(new_slim_profile);
    }
    
    // Save all profiles
    save_all_profiles(&handle, profiles).await
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
        }
    }
    
    // Get the contact ID
    pub fn contact(&self) -> &str {
        &self.contact
    }
}

pub fn get_store<R: Runtime>(handle: &AppHandle<R>) -> Arc<tauri_plugin_store::Store<R>> {
    StoreBuilder::new(handle, PathBuf::from(DB_PATH))
        .auto_save(Duration::from_secs(2))
        .build()
        .unwrap()
}

#[command]
pub fn get_db<R: Runtime>(handle: AppHandle<R>) -> Result<VectorDB, String> {
    let store = get_store(&handle);

    // Grab the DB version - giving us backwards-compat awareness and the ability to upgrade previous formats
    let db_version = match store.get("dbver") {
        Some(value) if value.is_number() => Some(value.as_number().unwrap().as_u64().unwrap()),
        _ => None,
    };

    // Extract optional fields
    let theme = match store.get("theme") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    let pkey = match store.get("pkey") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    let seed = match store.get("seed") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    let invite_code = match store.get("invite_code") {
        Some(value) if value.is_string() => Some(value.as_str().unwrap().to_string()),
        _ => None,
    };
    
    Ok(VectorDB {
        db_version,
        theme,
        pkey,
        seed,
        invite_code,
    })
}

#[command]
pub async fn set_db_version<R: Runtime>(handle: AppHandle<R>, version: u64) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("dbver".to_string(), serde_json::json!(version));
    Ok(())
}

#[command]
pub fn get_db_version<R: Runtime>(handle: AppHandle<R>) -> Result<Option<u64>, String> {
    let store = get_store(&handle);
    match store.get("dbver") {
        Some(value) if value.is_number() => Ok(value.as_number().unwrap().as_u64()),
        _ => Ok(None),
    }
}

#[command]
pub fn set_theme<R: Runtime>(handle: AppHandle<R>, theme: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("theme".to_string(), serde_json::json!(theme));
    Ok(())
}

#[command]
pub fn get_theme<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("theme") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_whisper_auto_translate<R: Runtime>(handle: AppHandle<R>, to: bool) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("whisper_auto_translate".to_string(), serde_json::json!(to));
    Ok(())
}

#[command]
pub fn get_whisper_auto_translate<R: Runtime>(handle: AppHandle<R>) -> Result<Option<bool>, String> {
    let store = get_store(&handle);
    match store.get("whisper_auto_translate") {
        Some(value) if value.is_boolean() => Ok(Some(value.as_bool().unwrap())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_whisper_auto_transcribe<R: Runtime>(handle: AppHandle<R>, to: bool) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("whisper_auto_transcribe".to_string(), serde_json::json!(to));
    Ok(())
}

#[command]
pub fn get_whisper_auto_transcribe<R: Runtime>(handle: AppHandle<R>) -> Result<Option<bool>, String> {
    let store = get_store(&handle);
    match store.get("whisper_auto_transcribe") {
        Some(value) if value.is_boolean() => Ok(Some(value.as_bool().unwrap())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_whisper_model_name<R: Runtime>(handle: AppHandle<R>, name: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("whisper_model_name".to_string(), serde_json::json!(name));
    Ok(())
}

#[command]
pub fn get_whisper_model_name<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("whisper_model_name") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn set_pkey<R: Runtime>(handle: AppHandle<R>, pkey: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("pkey".to_string(), serde_json::json!(pkey));
    Ok(())
}

#[command]
pub fn get_pkey<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("pkey") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub async fn set_seed<R: Runtime>(handle: AppHandle<R>, seed: String) -> Result<(), String> {
    let store = get_store(&handle);
    // Encrypt the seed phrase before storing it
    let encrypted_seed = internal_encrypt(seed, None).await;
    store.set("seed".to_string(), serde_json::json!(encrypted_seed));
    Ok(())
}

#[command]
pub async fn get_seed<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("seed") {
        Some(value) if value.is_string() => {
            let encrypted_seed = value.as_str().unwrap().to_string();
            // Decrypt the seed phrase
            match internal_decrypt(encrypted_seed, None).await {
                Ok(decrypted) => Ok(Some(decrypted)),
                Err(_) => Err("Failed to decrypt seed phrase".to_string()),
            }
        },
        _ => Ok(None),
    }
}

#[command]
pub fn set_invite_code<R: Runtime>(handle: AppHandle<R>, code: String) -> Result<(), String> {
    let store = get_store(&handle);
    store.set("invite_code".to_string(), serde_json::json!(code));
    Ok(())
}

#[command]
pub fn get_invite_code<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let store = get_store(&handle);
    match store.get("invite_code") {
        Some(value) if value.is_string() => Ok(Some(value.as_str().unwrap().to_string())),
        _ => Ok(None),
    }
}

#[command]
pub fn remove_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<bool, String> {
    let store = get_store(&handle);
    let deleted = store.delete(&key);
    Ok(deleted)
}

pub fn nuke<R: Runtime>(handle: AppHandle<R>) -> Result<(), tauri_plugin_store::Error>{
    let store = get_store(&handle);
    store.clear();

    // We explicitly save to ensure the automated debounce isn't missed in case of immediate shutdown
    store.save()
}

// OLD MESSAGE FUNCTIONS - ONLY FOR READING DURING MIGRATION
// DO NOT USE FOR NEW WRITES - USE THE CHAT-BASED SYSTEM IN db_migration.rs

pub async fn old_get_all_messages<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<(Message, String)>, String> {
    let store = get_store(handle);
    
    // Get the messages map
    let messages: HashMap<String, String> = match store.get("messages") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("Failed to deserialize messages map: {}", e))?,
        None => return Ok(vec![]), // No messages stored
    };
    
    let mut result = Vec::with_capacity(messages.len());
    
    // Process each message
    for (_, encrypted) in messages.iter() {
        // Decrypt
        match internal_decrypt(encrypted.clone(), None).await {
            Ok(json) => {
                // Deserialize
                match serde_json::from_str::<SlimMessage>(&json) {
                    Ok(slim) => {
                        // Extract the contact ID and message
                        let contact_id = slim.contact().to_string();
                        let message = slim.to_message();
                        
                        // Store both pieces of information
                        result.push((message, contact_id));
                    },
                    Err(e) => {
                        eprintln!("Error deserializing message: {}", e);
                        // Continue processing other messages
                    }
                }
            },
            Err(_) => {
                eprintln!("Error decrypting message...");
                // Continue processing other messages
            }
        }
    }
    
    Ok(result)
}
