//! Chat database operations — CRUD for the chats table.

use serde::{Deserialize, Serialize};

use crate::chat::{Chat, ChatType, ChatMetadata};
use crate::compact::{encode_message_id, decode_message_id, NpubInterner};
use rusqlite::OptionalExtension;

/// Slim version of Chat for database storage.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChatDB {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    #[serde(default)]
    pub unread_from: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
    #[serde(default)]
    pub wallpaper_path: String,
    #[serde(default)]
    pub wallpaper_ts: u64,
    #[serde(default)]
    pub wallpaper_blur: u8,
    #[serde(default = "default_wallpaper_dim_slim")]
    pub wallpaper_dim: u8,
    #[serde(default)]
    pub wallpaper_url: String,
    #[serde(default)]
    pub wallpaper_uploader: String,
    /// Read-only: derived from the events at load, never written by a save.
    #[serde(default)]
    pub replied: bool,
}

fn default_wallpaper_dim_slim() -> u8 { 50 }

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
            unread_from: if chat.unread_from == [0u8; 32] {
                String::new()
            } else {
                decode_message_id(&chat.unread_from)
            },
            created_at: chat.created_at(),
            metadata: chat.metadata().clone(),
            muted: chat.muted(),
            wallpaper_path: chat.wallpaper_path.clone(),
            wallpaper_ts: chat.wallpaper_ts,
            wallpaper_blur: chat.wallpaper_blur,
            wallpaper_dim: chat.wallpaper_dim,
            wallpaper_url: chat.wallpaper_url.clone(),
            wallpaper_uploader: chat.wallpaper_uploader.clone(),
            replied: chat.replied,
        }
    }

    /// Convert back to full Chat (messages loaded separately).
    pub fn to_chat(&self, interner: &mut NpubInterner) -> Chat {
        let mut handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        // Self-heal DM rows persisted as bare stubs (participants '[]'): the id IS
        // the counterparty, and participant-keyed lookups need it present.
        if handles.is_empty()
            && matches!(self.chat_type, ChatType::DirectMessage)
            && self.id.starts_with("npub1")
        {
            handles.push(interner.intern(&self.id));
        }
        let mut chat = Chat::new(self.id.clone(), self.chat_type.clone(), handles);
        chat.last_read = if self.last_read.is_empty() {
            [0u8; 32]
        } else {
            encode_message_id(&self.last_read)
        };
        chat.unread_from = if self.unread_from.is_empty() {
            [0u8; 32]
        } else {
            encode_message_id(&self.unread_from)
        };
        chat.created_at = self.created_at;
        chat.metadata = self.metadata.clone();
        chat.muted = self.muted;
        chat.wallpaper_path = self.wallpaper_path.clone();
        chat.wallpaper_ts = self.wallpaper_ts;
        chat.wallpaper_blur = self.wallpaper_blur;
        chat.wallpaper_dim = self.wallpaper_dim;
        chat.wallpaper_url = self.wallpaper_url.clone();
        chat.wallpaper_uploader = self.wallpaper_uploader.clone();
        chat.replied = self.replied;
        chat
    }
}

/// Get all chats from the database.
pub fn get_all_chats() -> Result<Vec<SlimChatDB>, String> {
    let conn = super::get_db_connection_guard_static()?;

    // chat_type 1 was the removed MLS group variant — legacy rows are dropped at load.
    let mut stmt = conn.prepare(
        "SELECT chat_identifier, chat_type, participants, last_read, created_at, metadata, muted, \
                wallpaper_path, wallpaper_ts, wallpaper_blur, wallpaper_dim, \
                wallpaper_url, wallpaper_uploader, unread_from, \
                CASE WHEN chat_type = 0 THEN EXISTS( \
                    SELECT 1 FROM events e WHERE e.chat_id = chats.id AND e.kind IN (?1, ?2, ?3) AND e.mine = 1 \
                ) ELSE 0 END \
         FROM chats WHERE chat_type != 1 ORDER BY created_at DESC"
    ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let kinds = rusqlite::params![
        crate::stored_event::event_kind::CHAT_MESSAGE as i32,
        crate::stored_event::event_kind::PRIVATE_DIRECT_MESSAGE as i32,
        crate::stored_event::event_kind::FILE_ATTACHMENT as i32,
    ];
    let rows = stmt.query_map(kinds, |row| {
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
            unread_from: row.get(13)?,
            created_at: row.get::<_, i64>(4)? as u64,
            metadata,
            muted: row.get::<_, i32>(6)? != 0,
            wallpaper_path: row.get(7)?,
            wallpaper_ts: row.get::<_, i64>(8)? as u64,
            wallpaper_blur: row.get::<_, i32>(9)?.clamp(0, 30) as u8,
            wallpaper_dim: row.get::<_, i32>(10)?.clamp(0, 100) as u8,
            wallpaper_url: row.get(11)?,
            wallpaper_uploader: row.get(12)?,
            replied: row.get::<_, i32>(14)? != 0,
        })
    }).map_err(|e| format!("Failed to query chats: {}", e))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect chats: {}", e))
}

/// Keep a wallpaper rumor (sealed at rest) until its chat's request is accepted; only a
/// newer one replaces it. Whether it was kept.
pub fn hold_wallpaper(chat_identifier: &str, created_at: u64, payload: &str) -> Result<bool, String> {
    /// Each row is one stranger's, so fabricated npubs could otherwise grow it without end.
    const MAX_HELD: i64 = 200;
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let sealed = crate::crypto::maybe_encrypt_text(payload)?;
    let n = conn.execute(
        "INSERT INTO dm_held_wallpapers (chat_id, created_at, payload) VALUES (?1, ?2, ?3)
         ON CONFLICT(chat_id) DO UPDATE SET created_at = excluded.created_at, payload = excluded.payload
         WHERE excluded.created_at > dm_held_wallpapers.created_at",
        rusqlite::params![chat_identifier, created_at as i64, sealed],
    )
    .map_err(|e| format!("hold_wallpaper: {e}"))?;
    conn.execute(
        "DELETE FROM dm_held_wallpapers WHERE chat_id NOT IN
            (SELECT chat_id FROM dm_held_wallpapers ORDER BY created_at DESC LIMIT ?1)",
        rusqlite::params![MAX_HELD],
    )
    .map_err(|e| format!("hold_wallpaper cap: {e}"))?;
    Ok(n > 0)
}

/// The wallpaper held for this chat, removed as it's handed over.
pub fn take_held_wallpaper(chat_identifier: &str) -> Result<Option<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let payload: Option<String> = conn
        .query_row(
            "DELETE FROM dm_held_wallpapers WHERE chat_id = ?1 RETURNING payload",
            rusqlite::params![chat_identifier],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("take_held_wallpaper: {e}"))?;
    Ok(payload.map(|p| crate::crypto::maybe_decrypt_text(&p)))
}

/// Withdraw a held wallpaper older than `created_at` (its sender since removed it).
pub fn drop_held_wallpaper_before(chat_identifier: &str, created_at: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM dm_held_wallpapers WHERE chat_id = ?1 AND created_at < ?2",
        rusqlite::params![chat_identifier, created_at as i64],
    )
    .map_err(|e| format!("drop_held_wallpaper_before: {e}"))?;
    Ok(())
}

/// Upsert a chat to the database.
/// Clear a chat's read marker to never-read. The [`save_slim_chat`] upsert
/// deliberately refuses to regress `last_read` to empty (a partially-hydrated
/// STATE chat would otherwise wipe the stored position), so a DELIBERATE
/// clear — Mark as Unread on a chat whose only message is the target — must
/// come through here or it silently no-ops.
pub fn clear_chat_last_read(chat_identifier: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE chats SET last_read = '' WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
    )
    .map_err(|e| format!("clear_chat_last_read: {e}"))?;
    Ok(())
}

pub fn save_slim_chat(slim_chat: &SlimChatDB) -> Result<(), String> {
    if slim_chat.id.is_empty() {
        return Err("refusing to persist a chat with an empty identifier".into());
    }
    let conn = super::get_write_connection_guard_static()?;

    let chat_type_int = slim_chat.chat_type.to_i32();
    let participants_json = serde_json::to_string(&slim_chat.participants)
        .unwrap_or_else(|_| "[]".to_string());
    let metadata_json = serde_json::to_string(&slim_chat.metadata)
        .unwrap_or_else(|_| "{}".to_string());

    conn.execute(
        // `last_read` never regresses to empty through a chat save: a STATE chat can
        // predate marker hydration (realtime-created, partial boot), and persisting its
        // empty marker would wipe the stored read position — resurrecting every message
        // since as phantom unread. Marker clears go through the dedicated
        // `UPDATE chats SET last_read` paths, not this upsert.
        "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted, wallpaper_path, wallpaper_ts, wallpaper_blur, wallpaper_dim, wallpaper_url, wallpaper_uploader, unread_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(chat_identifier) DO UPDATE SET \
            chat_type = excluded.chat_type, participants = excluded.participants, \
            last_read = CASE WHEN excluded.last_read = '' THEN chats.last_read ELSE excluded.last_read END, \
            metadata = excluded.metadata, muted = excluded.muted, \
            wallpaper_path = excluded.wallpaper_path, wallpaper_ts = excluded.wallpaper_ts, \
            wallpaper_blur = excluded.wallpaper_blur, wallpaper_dim = excluded.wallpaper_dim, \
            wallpaper_url = excluded.wallpaper_url, wallpaper_uploader = excluded.wallpaper_uploader, \
            unread_from = excluded.unread_from",
        rusqlite::params![
            slim_chat.id,
            chat_type_int,
            participants_json,
            slim_chat.last_read,
            slim_chat.created_at as i64,
            metadata_json,
            slim_chat.muted as i32,
            slim_chat.wallpaper_path,
            slim_chat.wallpaper_ts as i64,
            slim_chat.wallpaper_blur as i32,
            slim_chat.wallpaper_dim as i32,
            slim_chat.wallpaper_url,
            slim_chat.wallpaper_uploader,
            slim_chat.unread_from,
        ],
    ).map_err(|e| format!("Failed to upsert chat: {}", e))?;

    Ok(())
}

/// Delete a chat and all its messages from the database. `chat_identifier` is the
/// string id (npub for DMs, channel id for Communities) — NOT the integer PK.
pub fn delete_chat(chat_identifier: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // Drop messages first (explicit, not reliant on the FK cascade pragma being on).
    conn.execute(
        "DELETE FROM events WHERE chat_id IN (SELECT id FROM chats WHERE chat_identifier = ?1)",
        rusqlite::params![chat_identifier],
    ).map_err(|e| format!("Failed to delete chat events: {}", e))?;
    conn.execute(
        "DELETE FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
    ).map_err(|e| format!("Failed to delete chat: {}", e))?;
    super::id_cache::forget_chat_id(chat_identifier);
    Ok(())
}

#[cfg(test)]
mod tests {
    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(900);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        // Per-account row-id caches survive close_database; clear them so a stale entry from a prior
        // test's DB can't point into this fresh account's DB and FK-fail an insert.
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    // A chat save carrying an EMPTY marker (a STATE chat that predates marker
    // hydration) must not wipe the persisted read position — message persists
    // re-save the chat row constantly, and a wipe resurrects every message
    // since as phantom unread. A real marker still advances normally.
    #[test]
    fn chat_upsert_preserves_last_read_against_empty_marker() {
        let (_tmp, _guard) = init_test_db();
        let chat_id = "npub1markerkeeper";

        let mut slim = super::SlimChatDB {
            id: chat_id.to_string(),
            chat_type: crate::ChatType::DirectMessage,
            participants: vec![],
            last_read: "aa".repeat(32),
            unread_from: String::new(),
            created_at: 1000,
            metadata: crate::chat::ChatMetadata::default(),
            muted: false,
            wallpaper_path: String::new(),
            wallpaper_ts: 0,
            wallpaper_blur: 0,
            wallpaper_dim: 50,
            wallpaper_url: String::new(),
            wallpaper_uploader: String::new(),
            replied: false,
        };
        super::save_slim_chat(&slim).unwrap();

        // Un-hydrated STATE copy re-saves the row: marker survives.
        slim.last_read = String::new();
        super::save_slim_chat(&slim).unwrap();
        let chats = super::get_all_chats().unwrap();
        let chat = chats.iter().find(|c| c.id == chat_id).expect("chat saved");
        assert_eq!(chat.last_read, "aa".repeat(32), "empty marker must not wipe the stored one");

        // A real marker still advances.
        slim.last_read = "bb".repeat(32);
        super::save_slim_chat(&slim).unwrap();
        let chats = super::get_all_chats().unwrap();
        let chat = chats.iter().find(|c| c.id == chat_id).expect("chat saved");
        assert_eq!(chat.last_read, "bb".repeat(32), "non-empty marker advances normally");
    }

    // A DM is a request until we send in it; their messages alone, however many, don't
    // answer it, and the flag comes from the events rather than the page we boot with.
    #[tokio::test]
    async fn a_dm_counts_as_replied_once_we_send_in_it() {
        let (_tmp, _guard) = init_test_db();
        let chat_id = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let msg = |seed: u8, mine: bool| crate::types::Message {
            id: format!("{:02x}", seed).repeat(32),
            content: "hi".into(),
            at: 1_700_000_000_000 + seed as u64,
            mine,
            npub: Some(chat_id.clone()),
            ..Default::default()
        };
        let replied = || super::get_all_chats().unwrap().into_iter().find(|c| c.id == chat_id).expect("chat saved").replied;

        for seed in 1..=3 {
            crate::db::events::save_message(&chat_id, &msg(seed, false)).await.unwrap();
        }
        assert!(!replied(), "only their messages: still a request");

        crate::db::events::save_message(&chat_id, &msg(4, true)).await.unwrap();
        assert!(replied(), "one of ours answers it");
    }

    // Only a newer wallpaper replaces a held one, a removal withdraws an older one, and
    // handing it over empties the slot.
    #[test]
    fn a_held_wallpaper_keeps_the_newest_until_taken() {
        let (_tmp, _guard) = init_test_db();
        let chat = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        assert!(super::hold_wallpaper(&chat, 10, "ten").unwrap());
        assert!(!super::hold_wallpaper(&chat, 5, "five").unwrap(), "an older one never replaces it");
        assert!(super::hold_wallpaper(&chat, 20, "twenty").unwrap());

        super::drop_held_wallpaper_before(&chat, 15).unwrap();
        assert_eq!(super::take_held_wallpaper(&chat).unwrap().as_deref(), Some("twenty"), "a removal older than it leaves it");
        assert_eq!(super::take_held_wallpaper(&chat).unwrap(), None, "taken once");

        super::hold_wallpaper(&chat, 30, "thirty").unwrap();
        super::drop_held_wallpaper_before(&chat, 40).unwrap();
        assert_eq!(super::take_held_wallpaper(&chat).unwrap(), None, "a newer removal withdraws it");
    }

    // Regression: a non-npub id stub-created via get_or_create_chat_id must use the
    // Community discriminant (2), not the retired MLS value (1) which get_all_chats
    // drops — otherwise the chat (and its messages) vanish on the next reload.
    #[test]
    fn stub_created_non_npub_chat_survives_reload() {
        let (_tmp, _guard) = init_test_db();
        let channel_id = "abc123def456channelid";
        let _ = crate::db::id_cache::get_or_create_chat_id(channel_id).unwrap();

        let chats = super::get_all_chats().unwrap();
        let found = chats.iter().find(|c| c.id == channel_id)
            .expect("stub-created non-npub chat must survive get_all_chats");
        assert_eq!(found.chat_type, crate::ChatType::Community);
    }

    // Regression: a DM row stub-created by a message save (no prior chat row) must carry
    // its counterparty — a bare '[]' roster boots into STATE participant-less, and every
    // participant-keyed lookup (attachment downloads) misses forever after.
    #[test]
    fn stub_created_dm_chat_carries_its_counterparty() {
        let (_tmp, _guard) = init_test_db();
        let npub = "npub1stubcounterparty";
        let _ = crate::db::id_cache::get_or_create_chat_id(npub).unwrap();

        let chats = super::get_all_chats().unwrap();
        let found = chats.iter().find(|c| c.id == npub).expect("stub DM row exists");
        assert_eq!(found.chat_type, crate::ChatType::DirectMessage);
        assert_eq!(found.participants, vec![npub.to_string()], "the DM's id IS its participant");
    }

    // Rows already persisted with an empty roster (pre-fix stubs) heal at load: the id
    // is the counterparty, so to_chat re-derives it.
    #[test]
    fn to_chat_heals_bare_dm_participants() {
        let slim = super::SlimChatDB {
            id: "npub1baredmrow".to_string(),
            chat_type: crate::ChatType::DirectMessage,
            participants: vec![],
            last_read: String::new(),
            unread_from: String::new(),
            created_at: 1000,
            metadata: crate::chat::ChatMetadata::default(),
            muted: false,
            wallpaper_path: String::new(),
            wallpaper_ts: 0,
            wallpaper_blur: 0,
            wallpaper_dim: 50,
            wallpaper_url: String::new(),
            wallpaper_uploader: String::new(),
            replied: false,
        };
        let mut interner = crate::compact::NpubInterner::new();
        let chat = slim.to_chat(&mut interner);
        assert!(chat.has_participant("npub1baredmrow", &interner), "bare DM roster heals from the id");

        // A community row with no participants stays empty — only DMs derive from the id.
        let mut community = slim.clone();
        community.id = "aabbccddeeff00".to_string();
        community.chat_type = crate::ChatType::Community;
        let chat = community.to_chat(&mut interner);
        assert!(chat.participants().is_empty(), "non-DM rosters are not invented");
    }
}
