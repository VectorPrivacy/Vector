//! Database utilities for SQLite storage.

use std::collections::BTreeSet;
use std::io::{Error as IoError, ErrorKind};
use std::str::FromStr;

use mdk_storage_traits::GroupId;
use mdk_storage_traits::groups::types::{
    Group, GroupExporterSecret, GroupRelay, GroupState, SelfUpdateState,
};
use mdk_storage_traits::messages::types::{
    Message, MessageState, ProcessedMessage, ProcessedMessageState,
};
use mdk_storage_traits::welcomes::types::{
    ProcessedWelcome, ProcessedWelcomeState, Welcome, WelcomeState,
};
use nostr::{EventId, JsonUtil, Kind, PublicKey, RelayUrl, Tags, Timestamp, UnsignedEvent};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Type, ValueRef};
use rusqlite::{Error, Result as SqliteResult, Row};

/// Wrapper for [u8; 32] to implement rusqlite traits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hash32([u8; 32]);

impl From<[u8; 32]> for Hash32 {
    fn from(arr: [u8; 32]) -> Self {
        Hash32(arr)
    }
}

impl From<Hash32> for [u8; 32] {
    fn from(hash: Hash32) -> Self {
        hash.0
    }
}

impl ToSql for Hash32 {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_slice()))
    }
}

impl FromSql for Hash32 {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Blob(blob) => {
                if blob.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(blob);
                    Ok(Hash32(arr))
                } else {
                    Err(FromSqlError::InvalidBlobSize {
                        expected_size: 32,
                        blob_size: blob.len(),
                    })
                }
            }
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

/// Wrapper for [u8; 12] to implement rusqlite traits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce12([u8; 12]);

impl From<[u8; 12]> for Nonce12 {
    fn from(arr: [u8; 12]) -> Self {
        Nonce12(arr)
    }
}

impl From<Nonce12> for [u8; 12] {
    fn from(nonce: Nonce12) -> Self {
        nonce.0
    }
}

impl ToSql for Nonce12 {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_slice()))
    }
}

impl FromSql for Nonce12 {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Blob(blob) => {
                if blob.len() == 12 {
                    let mut arr = [0u8; 12];
                    arr.copy_from_slice(blob);
                    Ok(Nonce12(arr))
                } else {
                    Err(FromSqlError::InvalidBlobSize {
                        expected_size: 12,
                        blob_size: blob.len(),
                    })
                }
            }
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

#[inline]
fn map_to_text_boxed_error<T>(e: T) -> Error
where
    T: std::error::Error + Send + Sync + 'static,
{
    Error::FromSqlConversionFailure(0, Type::Text, Box::new(e))
}

#[inline]
fn map_invalid_text_data(msg: &str) -> Error {
    Error::FromSqlConversionFailure(
        0,
        Type::Text,
        Box::new(IoError::new(ErrorKind::InvalidData, msg)),
    )
}

#[inline]
fn map_invalid_blob_data(msg: &str) -> Error {
    Error::FromSqlConversionFailure(
        0,
        Type::Blob,
        Box::new(IoError::new(ErrorKind::InvalidData, msg)),
    )
}

/// Convert a row to a Group struct
pub fn row_to_group(row: &Row) -> SqliteResult<Group> {
    let mls_group_id: GroupId = GroupId::from_slice(row.get_ref("mls_group_id")?.as_blob()?);
    let nostr_group_id: [u8; 32] = row.get("nostr_group_id")?;
    let name: String = row.get("name")?;
    let description: String = row.get("description")?;
    let image_hash: Option<[u8; 32]> = row
        .get::<_, Option<Hash32>>("image_hash")?
        .map(|h| h.into());
    let image_key: Option<mdk_storage_traits::Secret<[u8; 32]>> = row
        .get::<_, Option<Hash32>>("image_key")?
        .map(|h| mdk_storage_traits::Secret::new(h.into()));
    let image_nonce: Option<mdk_storage_traits::Secret<[u8; 12]>> = row
        .get::<_, Option<Nonce12>>("image_nonce")?
        .map(|n| mdk_storage_traits::Secret::new(n.into()));

    // Parse admin pubkeys from JSON
    let admin_pubkeys_json: &str = row.get_ref("admin_pubkeys")?.as_str()?;
    let admin_pubkeys: BTreeSet<PublicKey> =
        serde_json::from_str(admin_pubkeys_json).map_err(map_to_text_boxed_error)?;

    let last_message_id_blob: Option<&[u8]> = row.get_ref("last_message_id")?.as_blob_or_null()?;
    let last_message_at: Option<u64> = row.get("last_message_at")?;
    let last_message_processed_at: Option<u64> = row.get("last_message_processed_at")?;
    let last_message_id: Option<EventId> = match last_message_id_blob {
        Some(id_blob) => Some(
            EventId::from_slice(id_blob)
                .map_err(|_| map_invalid_blob_data("Invalid last message ID"))?,
        ),
        None => None,
    };
    let last_message_at: Option<Timestamp> = last_message_at.map(Timestamp::from_secs);
    let last_message_processed_at: Option<Timestamp> =
        last_message_processed_at.map(Timestamp::from_secs);

    let state: &str = row.get_ref("state")?.as_str()?;
    let state: GroupState =
        GroupState::from_str(state).map_err(|_| map_invalid_text_data("Invalid group state"))?;

    let epoch: u64 = row.get("epoch")?;

    let self_update_state: SelfUpdateState = match row.get::<_, u64>("last_self_update_at")? {
        0 => SelfUpdateState::Required,
        ts => SelfUpdateState::CompletedAt(Timestamp::from_secs(ts)),
    };

    Ok(Group {
        mls_group_id,
        nostr_group_id,
        name,
        description,
        admin_pubkeys,
        last_message_id,
        last_message_at,
        last_message_processed_at,
        epoch,
        state,
        image_hash,
        image_key,
        image_nonce,
        self_update_state,
    })
}

/// Convert a row to a GroupRelay struct
pub fn row_to_group_relay(row: &Row) -> SqliteResult<GroupRelay> {
    let mls_group_id: GroupId = GroupId::from_slice(row.get_ref("mls_group_id")?.as_blob()?);
    let relay_url: &str = row.get_ref("relay_url")?.as_str()?;

    // Parse relay URL
    let relay_url: RelayUrl =
        RelayUrl::parse(relay_url).map_err(|_| map_invalid_text_data("Invalid relay URL"))?;

    Ok(GroupRelay {
        mls_group_id,
        relay_url,
    })
}

/// Convert a row to a GroupExporterSecret struct
pub fn row_to_group_exporter_secret(row: &Row) -> SqliteResult<GroupExporterSecret> {
    let mls_group_id: GroupId = GroupId::from_slice(row.get_ref("mls_group_id")?.as_blob()?);
    let epoch: u64 = row.get("epoch")?;
    let secret: [u8; 32] = row.get("secret")?;

    Ok(GroupExporterSecret {
        mls_group_id,
        epoch,
        secret: mdk_storage_traits::Secret::new(secret),
    })
}

/// Convert a row to a Message struct
pub fn row_to_message(row: &Row) -> SqliteResult<Message> {
    let id_blob: &[u8] = row.get_ref("id")?.as_blob()?;
    let pubkey_blob: &[u8] = row.get_ref("pubkey")?.as_blob()?;
    let kind_value: u16 = row.get("kind")?;
    let mls_group_id: GroupId = GroupId::from_slice(row.get_ref("mls_group_id")?.as_blob()?);
    let created_at_value: u64 = row.get("created_at")?;
    // processed_at may be NULL for rows created before the migration
    let processed_at_value: Option<u64> = row.get("processed_at")?;
    let content: String = row.get("content")?;
    let tags_json: &str = row.get_ref("tags")?.as_str()?;
    let event_json: &str = row.get_ref("event")?.as_str()?;
    let wrapper_event_id_blob: &[u8] = row.get_ref("wrapper_event_id")?.as_blob()?;
    let epoch: Option<u64> = row.get("epoch")?;
    let state_str: &str = row.get_ref("state")?.as_str()?;

    // Parse values
    let id: EventId =
        EventId::from_slice(id_blob).map_err(|_| map_invalid_blob_data("Invalid event ID"))?;

    let pubkey: PublicKey = PublicKey::from_slice(pubkey_blob)
        .map_err(|_| map_invalid_blob_data("Invalid public key"))?;

    let kind: Kind = Kind::from(kind_value);
    let created_at: Timestamp = Timestamp::from(created_at_value);
    // Fall back to created_at if processed_at is NULL (for backward compatibility)
    let processed_at: Timestamp = Timestamp::from(processed_at_value.unwrap_or(created_at_value));

    let tags: Tags = serde_json::from_str(tags_json).map_err(map_to_text_boxed_error)?;

    let event: UnsignedEvent =
        UnsignedEvent::from_json(event_json).map_err(map_to_text_boxed_error)?;

    let wrapper_event_id: EventId = EventId::from_slice(wrapper_event_id_blob)
        .map_err(|_| map_invalid_blob_data("Invalid wrapper event ID"))?;

    let state: MessageState =
        MessageState::from_str(state_str).map_err(|_| map_invalid_text_data("Invalid state"))?;

    Ok(Message {
        id,
        pubkey,
        kind,
        mls_group_id,
        created_at,
        processed_at,
        content,
        tags,
        event,
        wrapper_event_id,
        epoch,
        state,
    })
}

/// Convert a row to a ProcessedMessage struct
pub fn row_to_processed_message(row: &Row) -> SqliteResult<ProcessedMessage> {
    let wrapper_event_id_blob: &[u8] = row.get_ref("wrapper_event_id")?.as_blob()?;
    let message_event_id_blob: Option<&[u8]> =
        row.get_ref("message_event_id")?.as_blob_or_null()?;
    let processed_at_value: u64 = row.get("processed_at")?;
    let epoch: Option<u64> = row.get("epoch")?;
    let mls_group_id_blob: Option<&[u8]> = row.get_ref("mls_group_id")?.as_blob_or_null()?;
    let state_str: &str = row.get_ref("state")?.as_str()?;
    let failure_reason: Option<String> = row.get("failure_reason")?;

    // Parse values
    let wrapper_event_id: EventId = EventId::from_slice(wrapper_event_id_blob)
        .map_err(|_| map_invalid_blob_data("Invalid wrapper event ID"))?;

    let message_event_id: Option<EventId> = match message_event_id_blob {
        Some(id_blob) => Some(
            EventId::from_slice(id_blob)
                .map_err(|_| map_invalid_blob_data("Invalid message event ID"))?,
        ),
        None => None,
    };

    let mls_group_id: Option<GroupId> = mls_group_id_blob.map(GroupId::from_slice);

    let processed_at: Timestamp = Timestamp::from_secs(processed_at_value);
    let state: ProcessedMessageState = ProcessedMessageState::from_str(state_str)
        .map_err(|_| map_invalid_text_data("Invalid state"))?;

    Ok(ProcessedMessage {
        wrapper_event_id,
        message_event_id,
        processed_at,
        epoch,
        mls_group_id,
        state,
        failure_reason,
    })
}

/// Convert a row to a Welcome struct
pub fn row_to_welcome(row: &Row) -> SqliteResult<Welcome> {
    let id_blob: &[u8] = row.get_ref("id")?.as_blob()?;
    let event_json: &str = row.get_ref("event")?.as_str()?;
    let mls_group_id: GroupId = GroupId::from_slice(row.get_ref("mls_group_id")?.as_blob()?);
    let nostr_group_id: [u8; 32] = row.get("nostr_group_id")?;
    let group_name: String = row.get("group_name")?;
    let group_description: String = row.get("group_description")?;
    let group_image_hash: Option<[u8; 32]> = row
        .get::<_, Option<Hash32>>("group_image_hash")?
        .map(|h| h.into());
    let group_image_key: Option<mdk_storage_traits::Secret<[u8; 32]>> = row
        .get::<_, Option<Hash32>>("group_image_key")?
        .map(|h| mdk_storage_traits::Secret::new(h.into()));
    let group_image_nonce: Option<mdk_storage_traits::Secret<[u8; 12]>> = row
        .get::<_, Option<Nonce12>>("group_image_nonce")?
        .map(|n| mdk_storage_traits::Secret::new(n.into()));
    let group_admin_pubkeys_json: &str = row.get_ref("group_admin_pubkeys")?.as_str()?;
    let group_relays_json: &str = row.get_ref("group_relays")?.as_str()?;
    let welcomer_blob: &[u8] = row.get_ref("welcomer")?.as_blob()?;
    let member_count: u64 = row.get("member_count")?;
    let state_str: &str = row.get_ref("state")?.as_str()?;
    let wrapper_event_id_blob: &[u8] = row.get_ref("wrapper_event_id")?.as_blob()?;

    // Parse values
    let id: EventId =
        EventId::from_slice(id_blob).map_err(|_| map_invalid_blob_data("Invalid event ID"))?;

    let event: UnsignedEvent =
        UnsignedEvent::from_json(event_json).map_err(map_to_text_boxed_error)?;

    let group_admin_pubkeys: BTreeSet<PublicKey> =
        serde_json::from_str(group_admin_pubkeys_json).map_err(map_to_text_boxed_error)?;

    let group_relays: BTreeSet<RelayUrl> =
        serde_json::from_str(group_relays_json).map_err(map_to_text_boxed_error)?;

    let welcomer: PublicKey = PublicKey::from_slice(welcomer_blob)
        .map_err(|_| map_invalid_blob_data("Invalid welcomer public key"))?;

    let wrapper_event_id: EventId = EventId::from_slice(wrapper_event_id_blob)
        .map_err(|_| map_invalid_blob_data("Invalid wrapper event ID"))?;

    let state: WelcomeState =
        WelcomeState::from_str(state_str).map_err(|_| map_invalid_text_data("Invalid state"))?;

    Ok(Welcome {
        id,
        event,
        mls_group_id,
        nostr_group_id,
        group_name,
        group_description,
        group_image_hash,
        group_image_key,
        group_image_nonce,
        group_admin_pubkeys,
        group_relays,
        welcomer,
        member_count: member_count as u32,
        state,
        wrapper_event_id,
    })
}

/// Convert a row to a ProcessedWelcome struct
pub fn row_to_processed_welcome(row: &Row) -> SqliteResult<ProcessedWelcome> {
    let wrapper_event_id_blob: &[u8] = row.get_ref("wrapper_event_id")?.as_blob()?;
    let welcome_event_id_blob: Option<&[u8]> =
        row.get_ref("welcome_event_id")?.as_blob_or_null()?;
    let processed_at_value: u64 = row.get("processed_at")?;
    let state_str: &str = row.get_ref("state")?.as_str()?;
    let failure_reason: Option<String> = row.get("failure_reason")?;

    // Parse values
    let wrapper_event_id: EventId = EventId::from_slice(wrapper_event_id_blob)
        .map_err(|_| map_invalid_blob_data("Invalid wrapper event ID"))?;

    let welcome_event_id: Option<EventId> = match welcome_event_id_blob {
        Some(id_blob) => Some(
            EventId::from_slice(id_blob)
                .map_err(|_| map_invalid_blob_data("Invalid welcome event ID"))?,
        ),
        None => None,
    };

    let processed_at: Timestamp = Timestamp::from_secs(processed_at_value);
    let state: ProcessedWelcomeState = ProcessedWelcomeState::from_str(state_str)
        .map_err(|_| map_invalid_text_data("Invalid state"))?;

    Ok(ProcessedWelcome {
        wrapper_event_id,
        welcome_event_id,
        processed_at,
        state,
        failure_reason,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// Helper to create a test database with the groups table schema
    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE groups (
                mls_group_id BLOB PRIMARY KEY,
                nostr_group_id BLOB NOT NULL,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                image_hash BLOB,
                image_key BLOB,
                image_nonce BLOB,
                admin_pubkeys TEXT NOT NULL,
                last_message_id BLOB,
                last_message_at INTEGER,
                last_message_processed_at INTEGER,
                epoch INTEGER NOT NULL,
                state TEXT NOT NULL,
                last_self_update_at INTEGER NOT NULL DEFAULT 0
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_row_to_group_with_valid_last_message_id() {
        let conn = create_test_db();

        // A valid EventId is 32 bytes
        let valid_event_id = [0xabu8; 32];

        conn.execute(
            "INSERT INTO groups VALUES (?, ?, ?, ?, NULL, NULL, NULL, ?, ?, NULL, NULL, ?, ?, 0)",
            rusqlite::params![
                &[1u8, 2, 3, 4][..], // mls_group_id
                &[0u8; 32][..],      // nostr_group_id
                "Test Group",        // name
                "Description",       // description
                "[]",                // admin_pubkeys (empty JSON array)
                &valid_event_id[..], // last_message_id (valid 32-byte blob)
                0i64,                // epoch
                "active",            // state
            ],
        )
        .unwrap();

        let mut stmt = conn.prepare("SELECT * FROM groups").unwrap();
        let result = stmt.query_row([], row_to_group);

        assert!(result.is_ok());
        let group = result.unwrap();
        assert!(group.last_message_id.is_some());
    }

    #[test]
    fn test_row_to_group_with_null_last_message_id() {
        let conn = create_test_db();

        conn.execute(
            "INSERT INTO groups VALUES (?, ?, ?, ?, NULL, NULL, NULL, ?, NULL, NULL, NULL, ?, ?, 0)",
            rusqlite::params![
                &[1u8, 2, 3, 4][..], // mls_group_id
                &[0u8; 32][..],      // nostr_group_id
                "Test Group",        // name
                "Description",       // description
                "[]",                // admin_pubkeys (empty JSON array)
                0i64,                // epoch
                "active",            // state
            ],
        )
        .unwrap();

        let mut stmt = conn.prepare("SELECT * FROM groups").unwrap();
        let result = stmt.query_row([], row_to_group);

        assert!(result.is_ok());
        let group = result.unwrap();
        assert!(group.last_message_id.is_none());
    }

    #[test]
    fn test_row_to_group_with_invalid_last_message_id_length() {
        let conn = create_test_db();

        // An invalid EventId - wrong length (16 bytes instead of 32)
        let invalid_event_id = [0xabu8; 16];

        conn.execute(
            "INSERT INTO groups VALUES (?, ?, ?, ?, NULL, NULL, NULL, ?, ?, NULL, NULL, ?, ?, 0)",
            rusqlite::params![
                &[1u8, 2, 3, 4][..],   // mls_group_id
                &[0u8; 32][..],        // nostr_group_id
                "Test Group",          // name
                "Description",         // description
                "[]",                  // admin_pubkeys (empty JSON array)
                &invalid_event_id[..], // last_message_id (invalid 16-byte blob)
                0i64,                  // epoch
                "active",              // state
            ],
        )
        .unwrap();

        let mut stmt = conn.prepare("SELECT * FROM groups").unwrap();
        let result = stmt.query_row([], row_to_group);

        // Should fail with an error, not silently return None
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Invalid last message ID"),
            "Expected error message to contain 'Invalid last message ID', got: {}",
            err
        );
    }

    #[test]
    fn test_row_to_group_with_empty_last_message_id_blob() {
        let conn = create_test_db();

        // An empty blob is also invalid for EventId
        let empty_blob: [u8; 0] = [];

        conn.execute(
            "INSERT INTO groups VALUES (?, ?, ?, ?, NULL, NULL, NULL, ?, ?, NULL, NULL, ?, ?, 0)",
            rusqlite::params![
                &[1u8, 2, 3, 4][..], // mls_group_id
                &[0u8; 32][..],      // nostr_group_id
                "Test Group",        // name
                "Description",       // description
                "[]",                // admin_pubkeys (empty JSON array)
                &empty_blob[..],     // last_message_id (empty blob)
                0i64,                // epoch
                "active",            // state
            ],
        )
        .unwrap();

        let mut stmt = conn.prepare("SELECT * FROM groups").unwrap();
        let result = stmt.query_row([], row_to_group);

        // Should fail with an error, not silently return None
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Invalid last message ID"),
            "Expected error message to contain 'Invalid last message ID', got: {}",
            err
        );
    }
}
