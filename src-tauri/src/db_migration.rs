use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;

use crate::{Message, Chat, ChatType};
use crate::crypto::{internal_encrypt, internal_decrypt};
use crate::db::{SlimMessage, get_store};

/// In-memory cache for chat_identifier → integer ID mappings
/// This avoids database lookups on every message operation
static CHAT_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// In-memory cache for npub → integer ID mappings
/// This avoids database lookups on every message operation
static USER_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// Slim version of Chat for database storage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChatDB {
    pub id: String,  // The semantic ID (npub or group_id) - used in code
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: crate::ChatMetadata,
    pub muted: bool,
}

/// Helper function to get or create integer chat ID from identifier
/// Uses in-memory cache for maximum speed, only hits DB on cache miss
fn get_or_create_chat_id<R: Runtime>(
    handle: &AppHandle<R>,
    chat_identifier: &str,
) -> Result<i64, String> {
    // Check cache first (fast path - no DB access)
    {
        let cache = CHAT_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(chat_identifier) {
            return Ok(id);
        }
    }
    
    // Cache miss - check database
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Try to get existing ID from database
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).ok();
    
    let id = if let Some(id) = existing_id {
        id
    } else {
        // Create new chat entry with minimal data
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
             VALUES (?1, 0, '[]', '', ?2, '{}', 0)",
            rusqlite::params![chat_identifier, now as i64],
        ).map_err(|e| format!("Failed to create chat: {}", e))?;
        
        // Get the auto-generated ID
        conn.last_insert_rowid()
    };
    
    // Return connection to pool
    crate::account_manager::return_db_connection(conn);
    
    // Update cache with the ID (write to both DB and cache)
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }
    
    Ok(id)
}

/// Helper function to get or create integer user ID from npub
/// Uses in-memory cache for maximum speed, only hits DB on cache miss
fn get_or_create_user_id<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str,
) -> Result<Option<i64>, String> {
    // If npub is empty, return None (for messages without author)
    if npub.is_empty() {
        return Ok(None);
    }
    
    // Check cache first (fast path - no DB access)
    {
        let cache = USER_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(npub) {
            return Ok(Some(id));
        }
    }
    
    // Cache miss - check database
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Try to get existing ID from database
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0)
    ).ok();
    
    let id = if let Some(id) = existing_id {
        id
    } else {
        // Create new profile entry with minimal data (just the npub)
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name) VALUES (?1, '', '')",
            rusqlite::params![npub],
        ).map_err(|e| format!("Failed to create profile stub: {}", e))?;
        
        // Get the auto-generated ID
        conn.last_insert_rowid()
    };
    
    // Return connection to pool
    crate::account_manager::return_db_connection(conn);
    
    // Update cache with the ID (write to both DB and cache)
    {
        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.insert(npub.to_string(), id);
    }
    
    Ok(Some(id))
}

/// Preload all ID mappings into memory cache on app startup
/// This ensures all subsequent lookups are instant (no DB access)
pub async fn preload_id_caches<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let _npub = match crate::account_manager::get_current_account() {
        Ok(n) => n,
        Err(_) => return Ok(()), // No account selected, skip
    };
    
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Load all chat ID mappings
    {
        let mut stmt = conn.prepare("SELECT chat_identifier, id FROM chats")
            .map_err(|e| format!("Failed to prepare chat query: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query chats: {}", e))?;
        
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.clear();
        
        for row in rows {
            let (identifier, id) = row.map_err(|e| format!("Failed to read chat row: {}", e))?;
            cache.insert(identifier, id);
        }
    }
    
    // Load all user ID mappings
    {
        let mut stmt = conn.prepare("SELECT npub, id FROM profiles")
            .map_err(|e| format!("Failed to prepare profile query: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query profiles: {}", e))?;
        
        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.clear();
        
        for row in rows {
            let (npub, id) = row.map_err(|e| format!("Failed to read profile row: {}", e))?;
            cache.insert(npub, id);
        }
    }
    
    // Return connection to pool
    crate::account_manager::return_db_connection(conn);
    
    Ok(())
}

/// Clear ID caches (useful when switching accounts)
pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
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
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        let mut stmt = conn.prepare("SELECT chat_identifier, chat_type, participants, last_read, created_at, metadata, muted FROM chats ORDER BY created_at DESC")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            let participants_json: String = row.get(2)?;
            let participants: Vec<String> = serde_json::from_str(&participants_json).unwrap_or_default();
            
            let metadata_json: String = row.get(5)?;
            let metadata: crate::ChatMetadata = serde_json::from_str(&metadata_json).unwrap_or_default();
            
            let chat_type_int: i32 = row.get(1)?;
            let chat_type = crate::ChatType::from_i32(chat_type_int);
            
            Ok(SlimChatDB {
                id: row.get(0)?,  // chat_identifier (the semantic ID)
                chat_type,
                participants,
                last_read: row.get(3)?,
                created_at: row.get::<_, i64>(4)? as u64,
                metadata,
                muted: row.get::<_, i32>(6)? != 0,
            })
        })
        .map_err(|e| format!("Failed to query chats: {}", e))?;
        
        let chats: Vec<SlimChatDB> = rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect chats: {}", e))?;
        
        drop(stmt); // Explicitly drop stmt before returning connection
        crate::account_manager::return_db_connection(conn);
        return Ok(chats);
    }
    
    // Fallback to Store mode
    let store = get_store(handle);
    
    let encrypted: String = match store.get("chats") {
        Some(value) if value.is_string() => value.as_str().unwrap().to_string(),
        _ => return Ok(vec![]),
    };
    
    let json = internal_decrypt(encrypted, None).await
        .map_err(|e| format!("Failed to decrypt chats: {:?}", e))?;
    
    let slim_chats: Vec<SlimChatDB> = serde_json::from_str(&json)
        .map_err(|e| format!("Failed to deserialize chats: {}", e))?;
    
    Ok(slim_chats)
}

/// Save a single chat to the database
pub async fn save_chat<R: Runtime>(handle: AppHandle<R>, chat: &Chat) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode - use UPSERT to avoid CASCADE delete
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        let slim_chat = SlimChatDB::from(chat);
        let chat_identifier = &slim_chat.id;
        
        let chat_type_int = slim_chat.chat_type.to_i32();
        let participants_json = serde_json::to_string(&slim_chat.participants)
            .unwrap_or_else(|_| "[]".to_string());
        let metadata_json = serde_json::to_string(&slim_chat.metadata)
            .unwrap_or_else(|_| "{}".to_string());
        
        // Use INSERT ... ON CONFLICT DO UPDATE to avoid triggering CASCADE delete
        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(chat_identifier) DO UPDATE SET
                chat_type = excluded.chat_type,
                participants = excluded.participants,
                last_read = excluded.last_read,
                metadata = excluded.metadata,
                muted = excluded.muted",
            rusqlite::params![
                chat_identifier,
                chat_type_int,
                participants_json,
                slim_chat.last_read,
                slim_chat.created_at as i64,
                metadata_json,
                slim_chat.muted as i32,
            ],
        ).map_err(|e| format!("Failed to upsert chat: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    
    // After migration, all users have SQL accounts
    // This Store fallback is unreachable
    Err("No SQL account found - migration required".to_string())
}

/// Delete a chat and all its messages from the database
pub async fn delete_chat<R: Runtime>(handle: AppHandle<R>, chat_id: &str) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode - DELETE with CASCADE (messages auto-deleted)
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        conn.execute(
            "DELETE FROM chats WHERE id = ?1",
            rusqlite::params![chat_id],
        ).map_err(|e| format!("Failed to delete chat: {}", e))?;
        
        println!("[DB] Deleted chat and messages from SQL: {}", chat_id);
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    
    // After migration, all users have SQL accounts
    // This Store fallback is unreachable
    Err("No SQL account found - migration required".to_string())
}

/// Get all messages for a specific chat
pub async fn get_chat_messages<R: Runtime>(handle: &AppHandle<R>, chat_id: &str) -> Result<Vec<Message>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        // Get integer chat ID from identifier
        let chat_int_id: i64 = conn.query_row(
            "SELECT id FROM chats WHERE chat_identifier = ?1",
            rusqlite::params![chat_id],
            |row| row.get(0)
        ).map_err(|e| format!("Chat not found: {}", e))?;
        
        // Collect all data from database first, then drop statement before async operations
        let messages = {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.content_encrypted, m.replied_to, m.preview_metadata, m.attachments, m.reactions, m.at, m.mine, p.npub
                 FROM messages m
                 LEFT JOIN profiles p ON m.user_id = p.id
                 WHERE m.chat_id = ? ORDER BY m.at"
            ).map_err(|e| format!("Failed to prepare statement: {}", e))?;
            
            let rows = stmt.query_map([chat_int_id], |row| {
                let content_encrypted: String = row.get(1)?;
                let attachments_json: String = row.get(4)?;
                let reactions_json: String = row.get(5)?;
                let preview_json: Option<String> = row.get(3)?;
                
                // Decrypt content (async operation - we'll need to handle this differently)
                // For now, store encrypted and decrypt in a second pass
                Ok((
                    row.get::<_, String>(0)?, // id
                    content_encrypted,
                    row.get::<_, String>(2)?, // replied_to
                    preview_json,
                    attachments_json,
                    reactions_json,
                    row.get::<_, i64>(6)? as u64, // at
                    row.get::<_, i32>(7)? != 0, // mine
                    row.get::<_, Option<String>>(8)?, // npub (from profiles table via JOIN)
                ))
            })
            .map_err(|e| format!("Failed to query messages: {}", e))?;
            
            // Collect immediately to consume the iterator while stmt is still alive
            let result: Result<Vec<_>, _> = rows.collect();
            result.map_err(|e| format!("Failed to collect messages: {}", e))?
        }; // stmt is dropped here before async operations
        
        // Return connection to pool before async operations
        crate::account_manager::return_db_connection(conn);
        
        // Decrypt content for each message (now safe to await)
        let mut result = Vec::new();
        for (id, content_encrypted, replied_to, preview_json, attachments_json, reactions_json, at, mine, npub) in messages {
            // Decrypt content
            let content = internal_decrypt(content_encrypted, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
            
            let attachments: Vec<crate::Attachment> = serde_json::from_str(&attachments_json).unwrap_or_default();
            let reactions: Vec<crate::Reaction> = serde_json::from_str(&reactions_json).unwrap_or_default();
            let preview_metadata = preview_json.and_then(|p| serde_json::from_str(&p).ok());
            
            result.push(Message {
                id,
                content,
                replied_to,
                preview_metadata,
                attachments,
                reactions,
                at,
                pending: false,
                failed: false,
                mine,
                npub,
                wrapper_event_id: None, // Not stored in old Store format
            });
        }
        
        return Ok(result);
    }
    
    // Fallback to Store mode
    let store = get_store(handle);
    
    let messages_key = format!("chat_messages_{}", chat_id);
    let messages: HashMap<std::string::String, std::string::String> = match store.get(&messages_key) {
        Some(value) => {
            let msg_map: HashMap<std::string::String, std::string::String> = serde_json::from_value(value.clone())
                .map_err(|e| format!("Failed to deserialize chat messages: {}", e))?;
            println!("[DB DEBUG] Storage key '{}' contains {} encrypted messages", messages_key, msg_map.len());
            msg_map
        },
        None => {
            println!("[DB DEBUG] Storage key '{}' NOT FOUND - no messages for this chat", messages_key);
            return Ok(vec![]);
        }
    };
    
    let mut result = Vec::with_capacity(messages.len());
    
    for (_, encrypted) in messages.iter() {
        match internal_decrypt(encrypted.clone(), None).await {
            Ok(json) => {
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
    
    result.sort_by(|a, b| a.at.cmp(&b.at));
    
    Ok(result)
}
/// Save a single message to the database (efficient for incremental updates)
pub async fn save_message<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    message: &Message
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(npub) = crate::account_manager::get_current_account() {
        // SQL mode - single message upsert
        let _db_path = crate::account_manager::get_database_path(&handle, &npub)?;
        
        // Encrypt the message content
        let encrypted_content = internal_encrypt(message.content.clone(), None).await;
        
        let attachments_json = serde_json::to_string(&message.attachments)
            .unwrap_or_else(|_| "[]".to_string());
        let reactions_json = serde_json::to_string(&message.reactions)
            .unwrap_or_else(|_| "[]".to_string());
        let preview_json = message.preview_metadata.as_ref()
            .and_then(|p| serde_json::to_string(p).ok());
        
        // Get database connection
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        // Get or create integer chat ID
        let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;
        
        // Get or create integer user ID from npub
        let user_int_id = if let Some(ref npub_str) = message.npub {
            get_or_create_user_id(&handle, npub_str)?
        } else {
            None
        };
        
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id, wrapper_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                message.id,
                chat_int_id,
                encrypted_content,
                message.replied_to,
                preview_json,
                attachments_json,
                reactions_json,
                message.at as i64,
                message.mine as i32,
                user_int_id,
                message.wrapper_event_id,
            ],
        ).map_err(|e| format!("Failed to insert message {}: {}", message.id, e))?;
        
        crate::account_manager::return_db_connection(conn);
        
        return Ok(());
    }
    
    // After migration, all users have SQL accounts
    // This Store fallback is unreachable since save_chat_message requires get_current_account() to succeed
    Err("No SQL account found - migration required".to_string())
}

/// Save multiple messages for a specific chat (batch operation with transaction)
///
/// Note: This performs UPSERT operations - it only inserts/updates the provided messages,
/// it does NOT delete other messages in the chat. This is safe for incremental updates.
pub async fn save_chat_messages<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    messages: &[Message]
) -> Result<(), String> {
    // Skip if no messages to save
    if messages.is_empty() {
        return Ok(());
    }
    
    // For single message, use the optimized single-message function
    if messages.len() == 1 {
        return save_message(handle, chat_id, &messages[0]).await;
    }
    
    // Check if we have a current account (SQL mode)
    if let Ok(npub) = crate::account_manager::get_current_account() {
        let _db_path = crate::account_manager::get_database_path(&handle, &npub)?;
        
        // Get or create integer chat ID
        let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;
        
        
        // Encrypt all messages first and get user IDs (async operations before database transaction)
        let mut encrypted_messages = Vec::new();
        for message in messages {
            // Get or create user ID
            let user_int_id = if let Some(ref npub_str) = message.npub {
                get_or_create_user_id(&handle, npub_str)?
            } else {
                None
            };
            
            let encrypted_content = internal_encrypt(message.content.clone(), None).await;
            let attachments_json = serde_json::to_string(&message.attachments)
                .unwrap_or_else(|_| "[]".to_string());
            let reactions_json = serde_json::to_string(&message.reactions)
                .unwrap_or_else(|_| "[]".to_string());
            let preview_json = message.preview_metadata.as_ref()
                .and_then(|p| serde_json::to_string(p).ok());
            
            encrypted_messages.push((
                message.id.clone(),
                encrypted_content,
                message.replied_to.clone(),
                preview_json,
                attachments_json,
                reactions_json,
                message.at,
                message.mine,
                user_int_id,
                message.wrapper_event_id.clone(),
            ));
        }
        
        // Now do all database operations synchronously
        let mut conn = crate::account_manager::get_db_connection(&handle)?;
        
        let tx = conn.transaction()
            .map_err(|e| format!("Failed to start transaction: {}", e))?;
        
        // Use INSERT OR REPLACE to upsert individual messages (preserves other messages in the chat)
        for (id, encrypted_content, replied_to, preview_json, attachments_json, reactions_json, at, mine, user_int_id, wrapper_event_id) in encrypted_messages {
            if let Err(e) = tx.execute(
                "INSERT OR REPLACE INTO messages (id, chat_id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id, wrapper_event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    chat_int_id,
                    encrypted_content,
                    replied_to,
                    preview_json,
                    attachments_json,
                    reactions_json,
                    at as i64,
                    mine as i32,
                    user_int_id,
                    wrapper_event_id,
                ],
            ) {
                eprintln!("Failed to insert message {} for chat {}: {}",
                    &id[..8.min(id.len())], &chat_id[..8.min(chat_id.len())], e);
            }
        }
        
        let result = match tx.commit() {
            Ok(_) => Ok(()),
            Err(e) => {
                eprintln!("Failed to commit transaction for chat {}: {}",
                    &chat_id[..8.min(chat_id.len())], e);
                Err(format!("Failed to commit transaction: {}", e))
            }
        };
        
        crate::account_manager::return_db_connection(conn);
        return result;
    }
    
    // After migration, all users have SQL accounts
    // This Store fallback is unreachable
    Err("No SQL account found - migration required".to_string())
}

/// Delete a single message from the database
pub async fn delete_message<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    message_id: &str
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        // SQL mode - single message delete
        // Get connection once and reuse
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        // Get integer chat ID
        let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;
        
        conn.execute(
            "DELETE FROM messages WHERE id = ?1 AND chat_id = ?2",
            rusqlite::params![message_id, chat_int_id],
        ).map_err(|e| format!("Failed to delete message: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    
    // After migration, all users have SQL accounts
    // This Store fallback is unreachable
    Err("No SQL account found - migration required".to_string())
}

// ============================================================================
// MLS Metadata SQL Functions
// ============================================================================

/// Save MLS groups to SQL database (plaintext columns)
pub async fn save_mls_groups<R: Runtime>(
    handle: AppHandle<R>,
    groups: &[crate::mls::MlsGroupMetadata],
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        // Store each group in the mls_groups table (all fields as columns)
        for group in groups {
            conn.execute(
                "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, avatar_ref, created_at, updated_at, evicted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    group.group_id,
                    group.engine_group_id,
                    group.creator_pubkey,
                    group.name,
                    group.avatar_ref,
                    group.created_at as i64,
                    group.updated_at as i64,
                    group.evicted as i32,
                ],
            ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;
        }
        
        println!("[SQL] Saved {} MLS groups to mls_groups table", groups.len());
        crate::account_manager::return_db_connection(conn);
        Ok(())
    } else {
        // After migration, all users have SQL accounts
        Err("No SQL account found - migration required".to_string())
    }
}

/// Save a single MLS group to SQL database (plaintext columns) - more efficient for adding new groups
pub async fn save_mls_group<R: Runtime>(
    handle: AppHandle<R>,
    group: &crate::mls::MlsGroupMetadata,
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        // Insert or replace a single group
        conn.execute(
            "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, avatar_ref, created_at, updated_at, evicted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                group.group_id,
                group.engine_group_id,
                group.creator_pubkey,
                group.name,
                group.avatar_ref,
                group.created_at as i64,
                group.updated_at as i64,
                group.evicted as i32,
            ],
        ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;
        
        println!("[SQL] Saved 1 MLS group to mls_groups table");
        crate::account_manager::return_db_connection(conn);
        Ok(())
    } else {
        // After migration, all users have SQL accounts
        Err("No SQL account found - migration required".to_string())
    }
}

/// Load MLS groups from SQL database (plaintext columns)
pub async fn load_mls_groups<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<crate::mls::MlsGroupMetadata>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        // Load from mls_groups table
        let mut stmt = conn.prepare(
            "SELECT group_id, engine_group_id, creator_pubkey, name, avatar_ref, created_at, updated_at, evicted FROM mls_groups"
        ).map_err(|e| format!("Failed to prepare query: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            Ok(crate::mls::MlsGroupMetadata {
                group_id: row.get(0)?,
                engine_group_id: row.get(1)?,
                creator_pubkey: row.get(2)?,
                name: row.get(3)?,
                avatar_ref: row.get(4)?,
                created_at: row.get::<_, i64>(5)? as u64,
                updated_at: row.get::<_, i64>(6)? as u64,
                evicted: row.get::<_, i32>(7)? != 0,
            })
        }).map_err(|e| format!("Failed to query mls_groups: {}", e))?;
        
        let groups: Vec<crate::mls::MlsGroupMetadata> = rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect groups: {}", e))?;
        
        drop(stmt);
        crate::account_manager::return_db_connection(conn);
        Ok(groups)
    } else {
        // Fallback to Store
        let store = get_store(handle);
        let encrypted_opt = store.get("mls_groups")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        
        if let Some(enc) = encrypted_opt {
            let json = internal_decrypt(enc, None).await
                .map_err(|_| "Failed to decrypt MLS groups".to_string())?;
            let groups: Vec<crate::mls::MlsGroupMetadata> = serde_json::from_str(&json)
                .map_err(|e| format!("Failed to deserialize MLS groups: {}", e))?;
            Ok(groups)
        } else {
            Ok(Vec::new())
        }
    }
}

/// Save MLS keypackage index to SQL database (plaintext)
pub async fn save_mls_keypackages<R: Runtime>(
    handle: AppHandle<R>,
    packages: &[serde_json::Value],
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        // Clear existing keypackages
        conn.execute("DELETE FROM mls_keypackages", [])
            .map_err(|e| format!("Failed to clear MLS keypackages: {}", e))?;
        
        // Insert new keypackages
        for pkg in packages {
            let owner_pubkey = pkg.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or("");
            let device_id = pkg.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
            let keypackage_ref = pkg.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or("");
            let fetched_at = pkg.get("fetched_at").and_then(|v| v.as_u64()).unwrap_or(0);
            let expires_at = pkg.get("expires_at").and_then(|v| v.as_u64()).unwrap_or(0);
            
            conn.execute(
                "INSERT INTO mls_keypackages (owner_pubkey, device_id, keypackage_ref, fetched_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![owner_pubkey, device_id, keypackage_ref, fetched_at as i64, expires_at as i64],
            ).map_err(|e| format!("Failed to insert MLS keypackage: {}", e))?;
        }
        
        println!("[SQL] Saved {} MLS keypackages", packages.len());
        crate::account_manager::return_db_connection(conn);
        Ok(())
    } else {
        // After migration, all users have SQL accounts
        Err("No SQL account found - migration required".to_string())
    }
}

/// Load MLS keypackage index from SQL database (plaintext)
pub async fn load_mls_keypackages<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<serde_json::Value>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        let mut stmt = conn.prepare(
            "SELECT owner_pubkey, device_id, keypackage_ref, fetched_at, expires_at FROM mls_keypackages"
        ).map_err(|e| format!("Failed to prepare MLS keypackages query: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            let fetched_at: i64 = row.get(3)?;
            let expires_at: i64 = row.get(4)?;
            Ok(serde_json::json!({
                "owner_pubkey": row.get::<_, String>(0)?,
                "device_id": row.get::<_, String>(1)?,
                "keypackage_ref": row.get::<_, String>(2)?,
                "fetched_at": fetched_at as u64,
                "expires_at": expires_at as u64,
            }))
        }).map_err(|e| format!("Failed to query MLS keypackages: {}", e))?;
        
        let packages: Vec<serde_json::Value> = rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect MLS keypackages: {}", e))?;
        
        drop(stmt);
        crate::account_manager::return_db_connection(conn);
        Ok(packages)
    } else {
        // SQL is empty, check Store for migration
        let store = get_store(handle);
        let packages: Vec<serde_json::Value> = store.get("mls_keypackage_index")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        
        // Migrate to SQL if we found data in Store
        if !packages.is_empty() {
            println!("[MLS Migration] Migrating {} keypackages from Store to SQL", packages.len());
            let _ = save_mls_keypackages(handle.clone(), &packages).await;
        }
        
        Ok(packages)
    }
}

/// Save MLS event cursors to SQL database (plaintext)
pub async fn save_mls_event_cursors<R: Runtime>(
    handle: AppHandle<R>,
    cursors: &HashMap<String, crate::mls::EventCursor>,
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        for (group_id, cursor) in cursors {
            conn.execute(
                "INSERT OR REPLACE INTO mls_event_cursors (group_id, last_seen_event_id, last_seen_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![group_id, &cursor.last_seen_event_id, cursor.last_seen_at as i64],
            ).map_err(|e| format!("Failed to save MLS event cursor: {}", e))?;
        }
        
        crate::account_manager::return_db_connection(conn);
        Ok(())
    } else {
        // After migration, all users have SQL accounts
        Err("No SQL account found - migration required".to_string())
    }
}

/// Load MLS event cursors from SQL database (plaintext)
pub async fn load_mls_event_cursors<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, crate::mls::EventCursor>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        let mut stmt = conn.prepare(
            "SELECT group_id, last_seen_event_id, last_seen_at FROM mls_event_cursors"
        ).map_err(|e| format!("Failed to prepare MLS event cursors query: {}", e))?;
        
        let rows = stmt.query_map([], |row| {
            let group_id: String = row.get(0)?;
            let last_seen_at: i64 = row.get(2)?;
            let cursor = crate::mls::EventCursor {
                last_seen_event_id: row.get(1)?,
                last_seen_at: last_seen_at as u64,
            };
            Ok((group_id, cursor))
        }).map_err(|e| format!("Failed to query MLS event cursors: {}", e))?;
        
        let cursors: HashMap<String, crate::mls::EventCursor> = rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| format!("Failed to collect MLS event cursors: {}", e))?;
        
        drop(stmt);
        crate::account_manager::return_db_connection(conn);
        Ok(cursors)
    } else {
        // SQL is empty, check Store for migration
        let store = get_store(handle);
        let cursors: HashMap<String, crate::mls::EventCursor> = store.get("mls_event_cursors")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        
        // Migrate to SQL if we found data in Store
        if !cursors.is_empty() {
            println!("[MLS Migration] Migrating {} event cursors from Store to SQL", cursors.len());
            let _ = save_mls_event_cursors(handle.clone(), &cursors).await;
        }
        
        Ok(cursors)
    }
}

/// Save MLS device ID to SQL database (plaintext)
pub async fn save_mls_device_id<R: Runtime>(
    handle: AppHandle<R>,
    device_id: &str,
) -> Result<(), String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;
        
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('mls_device_id', ?1)",
            rusqlite::params![device_id],
        ).map_err(|e| format!("Failed to save MLS device ID to SQL: {}", e))?;
        
        println!("[SQL] Saved MLS device ID");
        crate::account_manager::return_db_connection(conn);
        Ok(())
    } else {
        // After migration, all users have SQL accounts
        Err("No SQL account found - migration required".to_string())
    }
}

/// Load MLS device ID from SQL database (plaintext)
pub async fn load_mls_device_id<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Option<String>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        let device_id: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = 'mls_device_id'",
            [],
            |row| row.get(0)
        ).ok();
        
        crate::account_manager::return_db_connection(conn);
        Ok(device_id)
    } else {
        // SQL is empty, check Store for migration
        let store = get_store(handle);
        let device_id = store.get("mls_device_id")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        
        // Migrate to SQL if we found data in Store
        if let Some(ref id) = device_id {
            println!("[MLS Migration] Migrating device_id from Store to SQL");
            let _ = save_mls_device_id(handle.clone(), id).await;
        }
        
        Ok(device_id)
    }
}

/// Lightweight attachment reference for file deduplication
/// Contains only the data needed to reuse an existing upload
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttachmentRef {
    /// The SHA256 hash of the original file (used as ID)
    pub hash: String,
    /// The message ID containing this attachment
    pub message_id: String,
    /// The chat ID containing this message
    pub chat_id: String,
    /// The encrypted file URL on the server
    pub url: String,
    /// The encryption key
    pub key: String,
    /// The encryption nonce
    pub nonce: String,
    /// The file extension
    pub extension: String,
    /// The encrypted file size
    pub size: u64,
}

/// Build a file hash index from all attachments in the database
/// This is used for file deduplication without loading full message content
/// Returns a HashMap of file_hash -> AttachmentRef
pub async fn build_file_hash_index<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, AttachmentRef>, String> {
    let mut index: HashMap<String, AttachmentRef> = HashMap::new();
    
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        // Query all messages with non-empty attachments
        // The attachments field is stored as plaintext JSON, so no decryption needed!
        // Collect all data first, then drop the statement before returning connection
        let attachment_data: Vec<(String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT m.id, c.chat_identifier, m.attachments
                 FROM messages m
                 JOIN chats c ON m.chat_id = c.id
                 WHERE m.attachments != '[]'"
            ).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;
            
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?, // message_id
                    row.get::<_, String>(1)?, // chat_identifier
                    row.get::<_, String>(2)?, // attachments JSON
                ))
            }).map_err(|e| format!("Failed to query attachments: {}", e))?;
            
            // Collect immediately to consume the iterator while stmt is still alive
            let result: Result<Vec<_>, _> = rows.collect();
            result.map_err(|e| format!("Failed to collect attachment rows: {}", e))?
        }; // stmt is dropped here
        
        // Return connection to pool before processing
        crate::account_manager::return_db_connection(conn);
        
        // Process the collected data
        const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        for (message_id, chat_id, attachments_json) in attachment_data {
            // Parse the attachments JSON
            let attachments: Vec<crate::Attachment> = serde_json::from_str(&attachments_json)
                .unwrap_or_default();
            
            // Add each attachment to the index (skip empty hashes and empty URLs)
            for attachment in attachments {
                if !attachment.id.is_empty()
                    && attachment.id != EMPTY_FILE_HASH
                    && !attachment.url.is_empty()
                {
                    index.insert(attachment.id.clone(), AttachmentRef {
                        hash: attachment.id,
                        message_id: message_id.clone(),
                        chat_id: chat_id.clone(),
                        url: attachment.url,
                        key: attachment.key,
                        nonce: attachment.nonce,
                        extension: attachment.extension,
                        size: attachment.size,
                    });
                }
            }
        }
    }
    
    Ok(index)
}

/// Get paginated messages for a chat (newest first, with offset)
/// This allows loading messages on-demand instead of all at once
///
/// Parameters:
/// - chat_id: The chat identifier (npub for DMs, group_id for groups)
/// - limit: Maximum number of messages to return
/// - offset: Number of messages to skip from the newest
///
/// Returns messages in chronological order (oldest first within the batch)
pub async fn get_chat_messages_paginated<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        // Get integer chat ID from identifier
        let chat_int_id: i64 = conn.query_row(
            "SELECT id FROM chats WHERE chat_identifier = ?1",
            rusqlite::params![chat_id],
            |row| row.get(0)
        ).map_err(|e| format!("Chat not found: {}", e))?;
        
        // Query messages with pagination (newest first, then reverse for chronological order)
        // We use a subquery to get the right slice, then order chronologically
        let messages = {
            let mut stmt = conn.prepare(
                "SELECT id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id
                 FROM (
                     SELECT m.id, m.content_encrypted, m.replied_to, m.preview_metadata, m.attachments, m.reactions, m.at, m.mine, p.npub as user_id
                     FROM messages m
                     LEFT JOIN profiles p ON m.user_id = p.id
                     WHERE m.chat_id = ?1
                     ORDER BY m.at DESC
                     LIMIT ?2 OFFSET ?3
                 )
                 ORDER BY at ASC"
            ).map_err(|e| format!("Failed to prepare statement: {}", e))?;
            
            let rows = stmt.query_map(rusqlite::params![chat_int_id, limit as i64, offset as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?, // id
                    row.get::<_, String>(1)?, // content_encrypted
                    row.get::<_, String>(2)?, // replied_to
                    row.get::<_, Option<String>>(3)?, // preview_metadata
                    row.get::<_, String>(4)?, // attachments
                    row.get::<_, String>(5)?, // reactions
                    row.get::<_, i64>(6)? as u64, // at
                    row.get::<_, i32>(7)? != 0, // mine
                    row.get::<_, Option<String>>(8)?, // npub
                ))
            }).map_err(|e| format!("Failed to query messages: {}", e))?;
            
            let result: Result<Vec<_>, _> = rows.collect();
            result.map_err(|e| format!("Failed to collect messages: {}", e))?
        };
        
        crate::account_manager::return_db_connection(conn);
        
        // Decrypt content for each message
        let mut result = Vec::with_capacity(messages.len());
        for (id, content_encrypted, replied_to, preview_json, attachments_json, reactions_json, at, mine, npub) in messages {
            let content = internal_decrypt(content_encrypted, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
            
            let attachments: Vec<crate::Attachment> = serde_json::from_str(&attachments_json).unwrap_or_default();
            let reactions: Vec<crate::Reaction> = serde_json::from_str(&reactions_json).unwrap_or_default();
            let preview_metadata = preview_json.and_then(|p| serde_json::from_str(&p).ok());
            
            result.push(Message {
                id,
                content,
                replied_to,
                preview_metadata,
                attachments,
                reactions,
                at,
                pending: false,
                failed: false,
                mine,
                npub,
                wrapper_event_id: None, // Paginated queries don't need wrapper_event_id
            });
        }
        
        return Ok(result);
    }
    
    // Fallback: For Store mode, just use the existing function with manual slicing
    // This is less efficient but maintains backwards compatibility
    let all_messages = get_chat_messages(handle, chat_id).await?;
    let total = all_messages.len();
    
    if offset >= total {
        return Ok(vec![]);
    }
    
    // Calculate the slice (from the end, since we want newest first for offset)
    let end_idx = total.saturating_sub(offset);
    let start_idx = end_idx.saturating_sub(limit);
    
    Ok(all_messages[start_idx..end_idx].to_vec())
}

/// Get the total message count for a chat
/// This is useful for the frontend to know how many messages exist without loading them all
pub async fn get_chat_message_count<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
) -> Result<usize, String> {
    // Check if we have a current account (SQL mode)
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(handle)?;
        
        // Get integer chat ID from identifier
        let chat_int_id: i64 = conn.query_row(
            "SELECT id FROM chats WHERE chat_identifier = ?1",
            rusqlite::params![chat_id],
            |row| row.get(0)
        ).map_err(|e| format!("Chat not found: {}", e))?;
        
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id = ?1",
            rusqlite::params![chat_int_id],
            |row| row.get(0)
        ).map_err(|e| format!("Failed to count messages: {}", e))?;
        
        crate::account_manager::return_db_connection(conn);
        
        return Ok(count as usize);
    }
    
    // Fallback: For Store mode, load all and count
    let all_messages = get_chat_messages(handle, chat_id).await?;
    Ok(all_messages.len())
}

/// Get the last N messages for a chat (for preview purposes)
/// This is optimized for getting just the most recent messages without loading the full history
pub async fn get_chat_last_messages<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    count: usize,
) -> Result<Vec<Message>, String> {
    // Just use paginated with offset 0
    get_chat_messages_paginated(handle, chat_id, count, 0).await
}

/// Check if a message exists in the database by its ID
/// This is used to prevent duplicate processing during sync
pub async fn message_exists_in_db<R: Runtime>(
    handle: &AppHandle<R>,
    message_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, let in-memory check handle it
    };
    
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check message existence: {}", e))?;
    
    crate::account_manager::return_db_connection(conn);
    
    Ok(exists)
}

/// Check if a wrapper (giftwrap) event ID exists in the database
/// This allows skipping the expensive unwrap operation for already-processed events
pub async fn wrapper_event_exists<R: Runtime>(
    handle: &AppHandle<R>,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, can't check
    };
    
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))?;
    
    crate::account_manager::return_db_connection(conn);
    
    Ok(exists)
}

/// Update the wrapper event ID for an existing message
/// This is called when we process a message that was previously stored without its wrapper ID
/// Returns: Ok(true) if updated, Ok(false) if message already had a wrapper_id (duplicate giftwrap)
pub async fn update_wrapper_event_id<R: Runtime>(
    handle: &AppHandle<R>,
    message_id: &str,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, nothing to update
    };
    
    let rows_updated = match conn.execute(
        "UPDATE messages SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, message_id],
    ) {
        Ok(n) => n,
        Err(e) => {
            crate::account_manager::return_db_connection(conn);
            return Err(format!("Failed to update wrapper event ID: {}", e));
        }
    };
    
    crate::account_manager::return_db_connection(conn);
    
    // Returns true if backfill succeeded, false if message already has a wrapper_id (duplicate giftwrap)
    Ok(rows_updated > 0)
}

/// Load recent wrapper_event_ids into a HashSet for fast duplicate detection
/// This preloads wrapper_ids from the last N days to avoid SQL queries during sync
pub async fn load_recent_wrapper_ids<R: Runtime>(
    handle: &AppHandle<R>,
    days: u64,
) -> Result<std::collections::HashSet<String>, String> {
    let mut wrapper_ids = std::collections::HashSet::new();
    
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(wrapper_ids), // No DB, return empty set
    };
    
    // Calculate timestamp for N days ago (in milliseconds, matching our `at` field)
    let cutoff_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64)
        .saturating_sub(days * 24 * 60 * 60 * 1000);
    
    // Query all wrapper_event_ids from recent messages
    let result: Result<Vec<String>, _> = {
        let mut stmt = conn.prepare(
            "SELECT wrapper_event_id FROM messages
             WHERE wrapper_event_id IS NOT NULL
             AND wrapper_event_id != ''
             AND at >= ?1"
        ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;
        
        let rows = stmt.query_map(rusqlite::params![cutoff_ms as i64], |row| {
            row.get::<_, String>(0)
        }).map_err(|e| format!("Failed to query wrapper_ids: {}", e))?;
        
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect wrapper_ids: {}", e))
    };
    
    crate::account_manager::return_db_connection(conn);
    
    match result {
        Ok(ids) => {
            for id in ids {
                wrapper_ids.insert(id);
            }
            Ok(wrapper_ids)
        }
        Err(_) => {
            Ok(wrapper_ids) // Return empty set on error, will fall back to DB queries
        }
    }
}
