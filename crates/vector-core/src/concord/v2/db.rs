//! Persistence for Concord v2 local state (the `concord2_*` tables).
//!
//! Stores the secrets this account *holds* — the community root and
//! private-channel keys — plus channel definitions and the signed Control
//! Plane edition seals (verbatim JSON: rebuilds the fold at boot and re-wraps
//! heads signature-intact in a compaction). The DB is already account-scoped
//! (`account_dir(npub)/vector.db`), so there is no npub column. At-rest
//! encryption mirrors v1: with Local Encryption on, every secret blob and
//! identifying text field wraps under the account's ENCRYPTION_KEY.

use nostr_sdk::prelude::PublicKey;
use rusqlite::{params, OptionalExtension};

use super::community::{Channel, Community};
use super::{ChannelId, ChannelKey, CommunityId, CommunityRoot, Epoch, OwnerSalt};

fn enc_key(k: &[u8; 32]) -> Result<Vec<u8>, String> {
    crate::crypto::maybe_encrypt_blob(k)
}

fn dec_key(stored: &[u8]) -> Result<[u8; 32], String> {
    let plain = crate::crypto::maybe_decrypt_blob(stored);
    plain
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32-byte key, got {} bytes", plain.len()))
}

fn enc_txt(s: &str) -> Result<String, String> {
    crate::crypto::maybe_encrypt_text(s)
}

fn dec_txt(s: &str) -> String {
    crate::crypto::maybe_decrypt_text(s)
}

fn hex32(s: &str) -> Result<[u8; 32], String> {
    crate::simd::hex::hex_to_bytes_32_checked(s)
        .ok_or_else(|| format!("corrupt or wrong-length 64-char hex id ({} chars)", s.len()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Persist a Community and all its channels (upsert).
pub fn save_community(community: &Community) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    let relays_json = serde_json::to_string(&community.relays).map_err(|e| e.to_string())?;
    let community_id = community.id.to_hex();
    let description = match &community.description {
        Some(d) => Some(enc_txt(d)?),
        None => None,
    };
    conn.execute(
        "INSERT INTO concord2_communities (community_id, owner, owner_salt, root, root_epoch, name, description, relays, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(community_id) DO UPDATE SET
             owner = ?2, owner_salt = ?3, root = ?4, root_epoch = ?5, name = ?6, description = ?7, relays = ?8",
        params![
            community_id,
            enc_txt(&community.owner.to_hex())?,
            enc_txt(&crate::simd::hex::bytes_to_hex_32(&community.owner_salt.0))?,
            enc_key(community.root.as_bytes())?,
            community.root_epoch.0 as i64,
            enc_txt(&community.name)?,
            description,
            enc_txt(&relays_json)?,
            now_secs(),
        ],
    )
    .map_err(|e| format!("save concord2 community: {e}"))?;

    for chan in &community.channels {
        let key_blob = match &chan.key {
            Some(k) => Some(enc_key(k.as_bytes())?),
            None => None,
        };
        conn.execute(
            "INSERT INTO concord2_channels (channel_id, community_id, name, private, deleted, channel_key, epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(channel_id) DO UPDATE SET
                 name = ?3, private = ?4, deleted = ?5, channel_key = ?6, epoch = ?7",
            params![
                chan.id.to_hex(),
                community_id,
                enc_txt(&chan.name)?,
                chan.private as i64,
                chan.deleted as i64,
                key_blob,
                chan.epoch.0 as i64,
            ],
        )
        .map_err(|e| format!("save concord2 channel: {e}"))?;
    }
    Ok(())
}

fn load_channels(
    conn: &rusqlite::Connection,
    community_id: &str,
) -> Result<Vec<Channel>, String> {
    let mut stmt = conn
        .prepare("SELECT channel_id, name, private, deleted, channel_key, epoch FROM concord2_channels WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<Vec<u8>>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut channels = Vec::new();
    for row in rows {
        let (id_hex, name, private, deleted, key_blob, epoch) = row.map_err(|e| e.to_string())?;
        let key = match key_blob {
            Some(blob) => Some(ChannelKey(dec_key(&blob)?)),
            None => None,
        };
        channels.push(Channel {
            id: ChannelId(hex32(&id_hex)?),
            name: dec_txt(&name),
            private: private != 0,
            deleted: deleted != 0,
            key,
            epoch: Epoch(epoch as u64),
        });
    }
    channels.sort_by_key(|c| c.id.0);
    Ok(channels)
}

pub fn load_community(id: &CommunityId) -> Result<Option<Community>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let community_id = id.to_hex();
    let row = conn
        .query_row(
            "SELECT owner, owner_salt, root, root_epoch, name, description, relays FROM concord2_communities WHERE community_id = ?1",
            params![community_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((owner, salt, root, root_epoch, name, description, relays)) = row else {
        return Ok(None);
    };
    let owner = PublicKey::from_slice(&hex32(&dec_txt(&owner))?).map_err(|e| e.to_string())?;
    let relays: Vec<String> = serde_json::from_str(&dec_txt(&relays)).unwrap_or_default();
    Ok(Some(Community {
        id: *id,
        owner,
        owner_salt: OwnerSalt(hex32(&dec_txt(&salt))?),
        root: CommunityRoot(dec_key(&root)?),
        root_epoch: Epoch(root_epoch as u64),
        name: dec_txt(&name),
        description: description.map(|d| dec_txt(&d)),
        relays,
        channels: load_channels(&conn, &community_id)?,
    }))
}

pub fn list_community_ids() -> Result<Vec<CommunityId>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT community_id FROM concord2_communities")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(CommunityId(hex32(&row.map_err(|e| e.to_string())?)?));
    }
    Ok(ids)
}

pub fn list_communities() -> Result<Vec<Community>, String> {
    let mut out = Vec::new();
    for id in list_community_ids()? {
        if let Some(c) = load_community(&id)? {
            out.push(c);
        }
    }
    Ok(out)
}

/// Resolve a channel hex id to its owning v2 community hex id — the routing
/// probe the Tauri commands use to branch v1/v2.
pub fn community_id_for_channel(channel_id: &str) -> Result<Option<String>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT community_id FROM concord2_channels WHERE channel_id = ?1",
        params![channel_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn community_created_at_ms(id: &CommunityId) -> Option<u64> {
    let conn = crate::db::get_db_connection_guard_static().ok()?;
    conn.query_row(
        "SELECT created_at FROM concord2_communities WHERE community_id = ?1",
        params![id.to_hex()],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(|secs| (secs as u64).saturating_mul(1000))
}

pub fn set_dissolved(id: &CommunityId) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE concord2_communities SET dissolved = 1 WHERE community_id = ?1",
        params![id.to_hex()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn is_dissolved(id: &CommunityId) -> bool {
    let Ok(conn) = crate::db::get_db_connection_guard_static() else {
        return false;
    };
    conn.query_row(
        "SELECT dissolved FROM concord2_communities WHERE community_id = ?1",
        params![id.to_hex()],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .unwrap_or(None)
    .map(|d| d != 0)
    .unwrap_or(false)
}

/// Delete a community and every scoped row (channels, editions). Chat/event
/// rows are handled by the caller's teardown.
pub fn delete_community(id: &CommunityId) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    let hex = id.to_hex();
    conn.execute("DELETE FROM concord2_channels WHERE community_id = ?1", params![hex])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM concord2_editions WHERE community_id = ?1", params![hex])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM concord2_communities WHERE community_id = ?1", params![hex])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist one edition's signed plaintext-seal JSON (INSERT OR IGNORE: an
/// edition is immutable, keyed by coordinate + version).
pub fn save_edition_seal(
    community_id: &CommunityId,
    eid: &[u8; 32],
    version: u64,
    seal_json: &str,
) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR IGNORE INTO concord2_editions (community_id, eid, version, seal_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            community_id.to_hex(),
            crate::simd::hex::bytes_to_hex_32(eid),
            version as i64,
            enc_txt(seal_json)?,
        ],
    )
    .map_err(|e| format!("save concord2 edition: {e}"))?;
    Ok(())
}

/// Load every persisted edition seal for a community (fold rebuild at boot).
pub fn load_edition_seals(community_id: &CommunityId) -> Result<Vec<String>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT seal_json FROM concord2_editions WHERE community_id = ?1 ORDER BY eid, version")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id.to_hex()], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(dec_txt(&row.map_err(|e| e.to_string())?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(5000);

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
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn test_community() -> Community {
        let owner = Keys::generate().public_key();
        let salt = OwnerSalt([0x33; 32]);
        let id = super::super::derive::community_id(&owner.to_bytes(), &salt);
        Community {
            id,
            owner,
            owner_salt: salt,
            root: CommunityRoot([0x44; 32]),
            root_epoch: Epoch(0),
            name: "Vector".into(),
            description: Some("Private messaging.".into()),
            relays: vec!["wss://a".into(), "wss://b".into()],
            channels: vec![
                Channel {
                    id: ChannelId([0x77; 32]),
                    name: "general".into(),
                    private: false,
                    deleted: false,
                    key: None,
                    epoch: Epoch(0),
                },
                Channel {
                    id: ChannelId([0x99; 32]),
                    name: "testers".into(),
                    private: true,
                    deleted: false,
                    key: Some(ChannelKey([0x42; 32])),
                    epoch: Epoch(1),
                },
            ],
        }
    }

    #[test]
    fn community_round_trips_with_keys_and_channels() {
        let (_tmp, _guard) = init_test_db();
        let community = test_community();
        save_community(&community).unwrap();

        let loaded = load_community(&community.id).unwrap().unwrap();
        assert_eq!(loaded.id, community.id);
        assert_eq!(loaded.owner, community.owner);
        assert_eq!(loaded.owner_salt, community.owner_salt);
        assert_eq!(loaded.root, community.root);
        assert_eq!(loaded.name, "Vector");
        assert_eq!(loaded.description.as_deref(), Some("Private messaging."));
        assert_eq!(loaded.relays, community.relays);
        assert_eq!(loaded.channels.len(), 2);
        let testers = loaded.channels.iter().find(|c| c.name == "testers").unwrap();
        assert_eq!(testers.key, Some(ChannelKey([0x42; 32])));
        assert_eq!(testers.epoch, Epoch(1));
        assert!(testers.private);

        assert_eq!(list_community_ids().unwrap(), vec![community.id]);
        assert_eq!(
            community_id_for_channel(&ChannelId([0x77; 32]).to_hex()).unwrap(),
            Some(community.id.to_hex())
        );
        assert_eq!(community_id_for_channel(&"f".repeat(64)).unwrap(), None);
    }

    #[test]
    fn upsert_follows_rotation_and_renames() {
        let (_tmp, _guard) = init_test_db();
        let mut community = test_community();
        save_community(&community).unwrap();

        // A refounding rotates the root; a control fold renames.
        community.root = CommunityRoot([0x55; 32]);
        community.root_epoch = Epoch(1);
        community.name = "Vector HQ".into();
        save_community(&community).unwrap();

        let loaded = load_community(&community.id).unwrap().unwrap();
        assert_eq!(loaded.root, CommunityRoot([0x55; 32]));
        assert_eq!(loaded.root_epoch, Epoch(1));
        assert_eq!(loaded.name, "Vector HQ");
        assert_eq!(loaded.channels.len(), 2, "channels upsert, never duplicate");
    }

    #[test]
    fn dissolved_flag_and_delete() {
        let (_tmp, _guard) = init_test_db();
        let community = test_community();
        save_community(&community).unwrap();
        assert!(!is_dissolved(&community.id));
        set_dissolved(&community.id).unwrap();
        assert!(is_dissolved(&community.id));

        delete_community(&community.id).unwrap();
        assert!(load_community(&community.id).unwrap().is_none());
        assert_eq!(community_id_for_channel(&ChannelId([0x77; 32]).to_hex()).unwrap(), None);
    }

    #[test]
    fn edition_seals_persist_immutably() {
        let (_tmp, _guard) = init_test_db();
        let community = test_community();
        save_community(&community).unwrap();

        save_edition_seal(&community.id, &[0x01; 32], 1, "{\"seal\":1}").unwrap();
        save_edition_seal(&community.id, &[0x01; 32], 2, "{\"seal\":2}").unwrap();
        // An edition is immutable: a replay at the same coordinate+version is ignored.
        save_edition_seal(&community.id, &[0x01; 32], 1, "{\"seal\":TAMPERED}").unwrap();

        let seals = load_edition_seals(&community.id).unwrap();
        assert_eq!(seals, vec!["{\"seal\":1}".to_string(), "{\"seal\":2}".to_string()]);
    }
}
