use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, Emitter};
use std::collections::HashMap;

use crate::{Message, Chat, ChatType};
use crate::crypto::{internal_encrypt, internal_decrypt};
use crate::db::{SlimMessage, get_store};

/// Slim version of Chat for database storage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChatDB {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: crate::ChatMetadata,
    pub muted: bool,
}

impl From<&Chat> for SlimChatDB {
    fn from(chat: &Chat) -> Self {
        SlimChatDB {
            id: chat.id().clone(),
            chat_type: chat.chat_type().clone(),
            participants: chat.participants().clone(),
            last_read: chat.last_read().clone(),
            created_at: chat.created_at(),
            metadata: chat.metadata().clone(),
            muted: chat.muted(),
        }
    }
}

impl SlimChatDB {
    // Convert back to full Chat (messages will be loaded separately)
    pub fn to_chat(&self) -> Chat {
        let mut chat = Chat::new(self.id.clone(), self.chat_type.clone(), self.participants.clone());
        chat.last_read = self.last_read.clone();
        chat.created_at = self.created_at;
        chat.metadata = self.metadata.clone();
        chat.muted = self.muted;
        chat
    }
}

/// Get all chats from the database
pub async fn get_all_chats<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimChatDB>, String> {
    let store = get_store(handle);
    
    // Get the encrypted chats
    let encrypted: String = match store.get("chats") {
        Some(value) if value.is_string() => value.as_str().unwrap().to_string(),
        _ => return Ok(vec![]), // No chats or wrong format
    };
    
    // Decrypt
    let json = internal_decrypt(encrypted, None).await
        .map_err(|e| format!("Failed to decrypt chats: {:?}", e))?;
    
    // Deserialize
    let slim_chats: Vec<SlimChatDB> = serde_json::from_str(&json)
        .map_err(|e| format!("Failed to deserialize chats: {}", e))?;
    
    Ok(slim_chats)
}

/// Save all chats to the database
async fn save_all_chats<R: Runtime>(handle: &AppHandle<R>, chats: Vec<SlimChatDB>) -> Result<(), String> {
    let store = get_store(handle);
    
    // Serialize to JSON
    let json = serde_json::to_string(&chats)
        .map_err(|e| format!("Failed to serialize chats: {}", e))?;
    
    // Encrypt the entire array
    let encrypted = internal_encrypt(json, None).await;
    
    // Store in the DB
    store.set("chats".to_string(), serde_json::json!(encrypted));
    
    Ok(())
}

/// Save a single chat to the database
pub async fn save_chat<R: Runtime>(handle: AppHandle<R>, chat: &Chat) -> Result<(), String> {
    // Get current chats
    let mut chats = get_all_chats(&handle).await?;
    
    // Convert the input chat to slim chat
    let new_slim_chat = SlimChatDB::from(chat);
    let chat_id = new_slim_chat.id.clone();
    
    // Find and replace the chat if it exists, or add it
    if let Some(pos) = chats.iter().position(|c| c.id == chat_id) {
        chats[pos] = new_slim_chat;
    } else {
        chats.push(new_slim_chat);
    }
    
    // Save all chats
    save_all_chats(&handle, chats).await
}

/// Get all messages for a specific chat
pub async fn get_chat_messages<R: Runtime>(handle: &AppHandle<R>, chat_id: &str) -> Result<Vec<Message>, String> {
    let store = get_store(handle);
    
    // Get the messages map for this chat
    let messages_key = format!("chat_messages_{}", chat_id);
    let messages: HashMap<std::string::String, std::string::String> = match store.get(&messages_key) {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("Failed to deserialize chat messages: {}", e))?,
        None => return Ok(vec![]), // No messages stored for this chat
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
                        let message = slim.to_message();
                        result.push(message);
                    },
                    Err(e) => {
                        eprintln!("Error deserializing chat message: {}", e);
                    }
                }
            },
            Err(e) => {
                eprintln!("Error decrypting chat message: {:?}", e);
            }
        }
    }
    
    // Sort messages by timestamp
    result.sort_by(|a, b| a.at.cmp(&b.at));
    
    Ok(result)
}

/// Save messages for a specific chat
pub async fn save_chat_messages<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    messages: &[Message]
) -> Result<(), String> {
    let store = get_store(&handle);
    
    // Create a map of message ID to encrypted message
    let mut messages_map: HashMap<String, String> = HashMap::new();
    
    // Process all messages
    for message in messages {
        // Convert to slim message (contact field is not needed for chat messages)
        let slim_message = SlimMessage {
            id: message.id.clone(),
            content: message.content.clone(),
            replied_to: message.replied_to.clone(),
            preview_metadata: message.preview_metadata.clone(),
            attachments: message.attachments.clone(),
            reactions: message.reactions.clone(),
            at: message.at,
            mine: message.mine,
            contact: String::new(), // Not used for chat messages
        };
        
        // Serialize to JSON
        let json = serde_json::to_string(&slim_message)
            .map_err(|e| format!("Failed to serialize chat message: {}", e))?;
        
        // Encrypt the JSON
        let encrypted = internal_encrypt(json, None).await;
        
        // Add to the map
        messages_map.insert(message.id.clone(), encrypted);
    }
    
    // Save to the DB with chat-specific key
    let messages_key = format!("chat_messages_{}", chat_id);
    store.set(messages_key, serde_json::json!(messages_map));
    
    Ok(())
}

/// Migrate existing profile-based messages to chat-based storage
/// This function should be called during app initialization to migrate old data
///
/// Migration scenarios tested:
/// 1. Profile-only last_read: When a profile has last_read but no chat exists, the chat will be created with that last_read value
/// 2. Both set: When both profile and chat have last_read values, the profile value overwrites the chat value (since this is a one-time migration)
/// 3. Null/empty values: When profile.last_read is empty, chat.last_read remains unchanged
/// 4. No matching profile: When a chat is created for a profile that doesn't exist, chat.last_read remains unchanged
pub async fn migrate_profile_messages_to_chats<R: Runtime>(
    handle: &AppHandle<R>,
    profile_messages: Vec<(Message, String)> // (message, profile_id)
) -> Result<Vec<Chat>, String> {
    // Load all profiles to access their last_read values for migration
    let profiles_result = crate::db::get_all_profiles(handle).await;
    let profiles = match profiles_result {
        Ok(profiles) => profiles,
        Err(e) => {
            eprintln!("Warning: Failed to load profiles for last_read migration: {}", e);
            Vec::new() // Continue with empty profiles if loading fails
        }
    };
    let profile_map: std::collections::HashMap<String, crate::db::SlimProfile> =
        profiles.into_iter().map(|p| (p.id.clone(), p)).collect();
    
    let mut chats: HashMap<String, Chat> = HashMap::new();
    let total_messages = profile_messages.len();
    
    // Emit initial progress
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 0,
        "total": total_messages,
        "message": "Migrating chats"
    })).unwrap();
    
    // Group messages by chat ID (create DM chats for each profile)
    for (index, (message, profile_id)) in profile_messages.into_iter().enumerate() {
        // Get or create chat
        let chat = chats.entry(profile_id.clone()).or_insert_with(|| {
            Chat::new_dm(profile_id.clone())
        });
        
        // Add message to chat
        chat.internal_add_message(message);
        
        // Emit progress every 10 messages or on last message
        if (index + 1) % 10 == 0 || index + 1 == total_messages {
            handle.emit("progress_operation", serde_json::json!({
                "type": "progress",
                "current": index + 1,
                "total": total_messages,
                "message": "Migrating chats"
            })).unwrap();
        }
    }
    
    // Apply last_read from profiles to chats
    for (profile_id, chat) in chats.iter_mut() {
        if let Some(profile) = profile_map.get(profile_id) {
            if !profile.last_read().is_empty() {
                chat.last_read = profile.last_read().to_string();
            }
        }
    }
    
    // Convert to vector
    let chat_vec: Vec<Chat> = chats.into_values().collect();
    let total_chats = chat_vec.len();
    
    // Save all chats and their messages
    for (index, chat) in chat_vec.iter().enumerate() {
        save_chat(handle.clone(), chat).await?;
        let all_messages = chat.messages.clone();
        save_chat_messages(handle.clone(), &chat.id, &all_messages).await?;
        
        // Emit progress for chat saving
        handle.emit("progress_operation", serde_json::json!({
            "type": "progress",
            "current": index + 1,
            "total": total_chats,
            "message": "Saving chats"
        })).unwrap();
    }
    
    Ok(chat_vec)
}

/// Migration function to update database version and mark migration as complete
pub async fn complete_migration<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    // Set database version to indicate migration is complete
    // Version 2 indicates chat-based storage (v1 was profile-based storage)
    crate::db::set_db_version(handle.clone(), 2).await?;
    
    // Clean up deprecated DB keys after successful migration
    let store = get_store(&handle);
    
    // Delete the old messages key since they're now stored in chat-based format
    store.delete("messages");
    
    // Save the store to persist the deletion
    store.save().map_err(|e| format!("Failed to save store after cleanup: {}", e))?;
    
    Ok(())
}

/// Check if migration is needed
pub async fn is_migration_needed<R: Runtime>(handle: &AppHandle<R>) -> Result<bool, String> {
    // Check the database version - migration is needed if version is less than 2
    match crate::db::get_db_version(handle.clone()) {
        Ok(Some(version)) => {
            // Migration is needed if version is less than 2 (chat-based storage version)
            Ok(version < 2)
        },
        Ok(None) => {
            // No version set - this is a new account or very old account needing migration
            Ok(true)
        },
        Err(e) => Err(format!("Failed to get database version: {}", e))
    }
}
