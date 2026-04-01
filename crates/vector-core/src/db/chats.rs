//! Chat database operations — CRUD for the chats table.

use serde::{Deserialize, Serialize};

use crate::chat::{Chat, ChatType, ChatMetadata};
use crate::compact::{encode_message_id, decode_message_id, NpubInterner};

/// Slim version of Chat for database storage.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChatDB {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
}

impl SlimChatDB {
    /// Create from a Chat, resolving interned handles to strings for DB storage.
    pub fn from_chat(chat: &Chat, interner: &NpubInterner) -> Self {
        SlimChatDB {
            id: chat.id().clone(),
            chat_type: chat.chat_type().clone(),
            participants: chat.participants().iter()
                .filter_map(|&h| interner.resolve(h).map(|s| s.to_string()))
                .collect(),
            last_read: if *chat.last_read() == [0u8; 32] {
                String::new()
            } else {
                decode_message_id(chat.last_read())
            },
            created_at: chat.created_at(),
            metadata: chat.metadata().clone(),
            muted: chat.muted(),
        }
    }

    /// Convert back to full Chat (messages loaded separately).
    pub fn to_chat(&self, interner: &mut NpubInterner) -> Chat {
        let handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        let mut chat = Chat::new(self.id.clone(), self.chat_type.clone(), handles);
        chat.last_read = if self.last_read.is_empty() {
            [0u8; 32]
        } else {
            encode_message_id(&self.last_read)
        };
        chat.created_at = self.created_at;
        chat.metadata = self.metadata.clone();
        chat.muted = self.muted;
        chat
    }
}

/// Get all chats from the database.
pub fn get_all_chats() -> Result<Vec<SlimChatDB>, String> {
    let conn = super::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        "SELECT chat_identifier, chat_type, participants, last_read, created_at, metadata, muted \
         FROM chats ORDER BY created_at DESC"
    ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let participants_json: String = row.get(2)?;
        let participants: Vec<String> = serde_json::from_str(&participants_json).unwrap_or_default();

        let metadata_json: String = row.get(5)?;
        let metadata: ChatMetadata = serde_json::from_str(&metadata_json).unwrap_or_default();

        let chat_type_int: i32 = row.get(1)?;
        let chat_type = ChatType::from_i32(chat_type_int);

        Ok(SlimChatDB {
            id: row.get(0)?,
            chat_type,
            participants,
            last_read: row.get(3)?,
            created_at: row.get::<_, i64>(4)? as u64,
            metadata,
            muted: row.get::<_, i32>(6)? != 0,
        })
    }).map_err(|e| format!("Failed to query chats: {}", e))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect chats: {}", e))
}

/// Upsert a chat to the database.
pub fn save_slim_chat(slim_chat: &SlimChatDB) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    let chat_type_int = slim_chat.chat_type.to_i32();
    let participants_json = serde_json::to_string(&slim_chat.participants)
        .unwrap_or_else(|_| "[]".to_string());
    let metadata_json = serde_json::to_string(&slim_chat.metadata)
        .unwrap_or_else(|_| "{}".to_string());

    conn.execute(
        "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(chat_identifier) DO UPDATE SET \
            chat_type = excluded.chat_type, participants = excluded.participants, \
            last_read = excluded.last_read, metadata = excluded.metadata, muted = excluded.muted",
        rusqlite::params![
            slim_chat.id,
            chat_type_int,
            participants_json,
            slim_chat.last_read,
            slim_chat.created_at as i64,
            metadata_json,
            slim_chat.muted as i32,
        ],
    ).map_err(|e| format!("Failed to upsert chat: {}", e))?;

    Ok(())
}

/// Delete a chat and all its messages from the database.
pub fn delete_chat(chat_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM chats WHERE id = ?1",
        rusqlite::params![chat_id],
    ).map_err(|e| format!("Failed to delete chat: {}", e))?;
    Ok(())
}
