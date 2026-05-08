//! Implementation of GroupStorage trait for SQLite storage.

use std::collections::BTreeSet;

use mdk_storage_traits::GroupId;
use mdk_storage_traits::groups::error::GroupError;
use mdk_storage_traits::groups::types::{Group, GroupExporterSecret, GroupRelay, SelfUpdateState};
use mdk_storage_traits::groups::{GroupStorage, MAX_MESSAGE_LIMIT, MessageSortOrder, Pagination};
use mdk_storage_traits::messages::types::Message;
use nostr::{PublicKey, RelayUrl};
use rusqlite::{OptionalExtension, params};

use crate::db::{Hash32, Nonce12};
use crate::validation::{
    MAX_ADMIN_PUBKEYS_JSON_SIZE, MAX_GROUP_DESCRIPTION_LENGTH, MAX_GROUP_NAME_LENGTH,
    validate_size, validate_string_length,
};
use crate::{MdkSqliteStorage, db};

#[inline]
fn into_group_err<T>(e: T) -> GroupError
where
    T: std::error::Error,
{
    GroupError::DatabaseError(e.to_string())
}

impl GroupStorage for MdkSqliteStorage {
    fn all_groups(&self) -> Result<Vec<Group>, GroupError> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM groups")
                .map_err(into_group_err)?;

            let groups_iter = stmt
                .query_map([], db::row_to_group)
                .map_err(into_group_err)?;

            let mut groups: Vec<Group> = Vec::new();

            for group_result in groups_iter {
                match group_result {
                    Ok(group) => {
                        groups.push(group);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Failed to deserialize group row, skipping"
                        );
                    }
                }
            }

            Ok(groups)
        })
    }

    fn find_group_by_mls_group_id(
        &self,
        mls_group_id: &GroupId,
    ) -> Result<Option<Group>, GroupError> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM groups WHERE mls_group_id = ?")
                .map_err(into_group_err)?;

            stmt.query_row([mls_group_id.as_slice()], db::row_to_group)
                .optional()
                .map_err(into_group_err)
        })
    }

    fn find_group_by_nostr_group_id(
        &self,
        nostr_group_id: &[u8; 32],
    ) -> Result<Option<Group>, GroupError> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM groups WHERE nostr_group_id = ?")
                .map_err(into_group_err)?;

            stmt.query_row(params![nostr_group_id], db::row_to_group)
                .optional()
                .map_err(into_group_err)
        })
    }

    fn save_group(&self, group: Group) -> Result<(), GroupError> {
        // Validate group name and description lengths
        validate_string_length(&group.name, MAX_GROUP_NAME_LENGTH, "Group name")
            .map_err(|e| GroupError::InvalidParameters(e.to_string()))?;

        validate_string_length(
            &group.description,
            MAX_GROUP_DESCRIPTION_LENGTH,
            "Group description",
        )
        .map_err(|e| GroupError::InvalidParameters(e.to_string()))?;

        let admin_pubkeys_json: String =
            serde_json::to_string(&group.admin_pubkeys).map_err(|e| {
                GroupError::DatabaseError(format!("Failed to serialize admin pubkeys: {}", e))
            })?;

        // Validate admin pubkeys JSON size
        validate_size(
            admin_pubkeys_json.as_bytes(),
            MAX_ADMIN_PUBKEYS_JSON_SIZE,
            "Admin pubkeys JSON",
        )
        .map_err(|e| GroupError::InvalidParameters(e.to_string()))?;

        let last_message_id: Option<&[u8; 32]> =
            group.last_message_id.as_ref().map(|id| id.as_bytes());
        let last_message_at: Option<u64> = group.last_message_at.as_ref().map(|ts| ts.as_secs());
        let last_message_processed_at: Option<u64> = group
            .last_message_processed_at
            .as_ref()
            .map(|ts| ts.as_secs());

        let last_self_update_at: u64 = match group.self_update_state {
            SelfUpdateState::Required => 0,
            SelfUpdateState::CompletedAt(ts) => ts.as_secs(),
        };

        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO groups
             (mls_group_id, nostr_group_id, name, description, image_hash, image_key, image_nonce, admin_pubkeys, last_message_id,
              last_message_at, last_message_processed_at, epoch, state, last_self_update_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(mls_group_id) DO UPDATE SET
                nostr_group_id = excluded.nostr_group_id,
                name = excluded.name,
                description = excluded.description,
                image_hash = excluded.image_hash,
                image_key = excluded.image_key,
                image_nonce = excluded.image_nonce,
                admin_pubkeys = excluded.admin_pubkeys,
                last_message_id = excluded.last_message_id,
                last_message_at = excluded.last_message_at,
                last_message_processed_at = excluded.last_message_processed_at,
                epoch = excluded.epoch,
                state = excluded.state,
                last_self_update_at = excluded.last_self_update_at",
                params![
                    &group.mls_group_id.as_slice(),
                    &group.nostr_group_id,
                    &group.name,
                    &group.description,
                    &group.image_hash.map(Hash32::from),
                    &group.image_key.as_ref().map(|k| Hash32::from(**k)),
                    &group.image_nonce.as_ref().map(|n| Nonce12::from(**n)),
                    &admin_pubkeys_json,
                    last_message_id,
                    &last_message_at,
                    &last_message_processed_at,
                    &(group.epoch as i64),
                    group.state.as_str(),
                    &last_self_update_at
                ],
            )
            .map_err(into_group_err)?;

            Ok(())
        })
    }

    fn messages(
        &self,
        mls_group_id: &GroupId,
        pagination: Option<Pagination>,
    ) -> Result<Vec<Message>, GroupError> {
        let pagination = pagination.unwrap_or_default();
        let limit = pagination.limit();
        let offset = pagination.offset();

        // Validate limit is within allowed range
        if !(1..=MAX_MESSAGE_LIMIT).contains(&limit) {
            return Err(GroupError::InvalidParameters(format!(
                "Limit must be between 1 and {}, got {}",
                MAX_MESSAGE_LIMIT, limit
            )));
        }

        // First verify the group exists
        if self.find_group_by_mls_group_id(mls_group_id)?.is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        let sort_order = pagination.sort_order();

        self.with_connection(|conn| {
            let query = match sort_order {
                MessageSortOrder::CreatedAtFirst => {
                    "SELECT * FROM messages WHERE mls_group_id = ? \
                     ORDER BY created_at DESC, processed_at DESC, id DESC \
                     LIMIT ? OFFSET ?"
                }
                MessageSortOrder::ProcessedAtFirst => {
                    "SELECT * FROM messages WHERE mls_group_id = ? \
                     ORDER BY processed_at DESC, created_at DESC, id DESC \
                     LIMIT ? OFFSET ?"
                }
            };

            let mut stmt = conn.prepare(query).map_err(into_group_err)?;

            let messages_iter = stmt
                .query_map(
                    params![mls_group_id.as_slice(), limit as i64, offset as i64],
                    db::row_to_message,
                )
                .map_err(into_group_err)?;

            let mut messages: Vec<Message> = Vec::new();

            for message_result in messages_iter {
                let message: Message = message_result.map_err(into_group_err)?;
                messages.push(message);
            }

            Ok(messages)
        })
    }

    fn last_message(
        &self,
        mls_group_id: &GroupId,
        sort_order: MessageSortOrder,
    ) -> Result<Option<Message>, GroupError> {
        if self.find_group_by_mls_group_id(mls_group_id)?.is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        self.with_connection(|conn| {
            let query = match sort_order {
                MessageSortOrder::CreatedAtFirst => {
                    "SELECT * FROM messages WHERE mls_group_id = ? \
                     ORDER BY created_at DESC, processed_at DESC, id DESC \
                     LIMIT 1"
                }
                MessageSortOrder::ProcessedAtFirst => {
                    "SELECT * FROM messages WHERE mls_group_id = ? \
                     ORDER BY processed_at DESC, created_at DESC, id DESC \
                     LIMIT 1"
                }
            };

            conn.prepare(query)
                .map_err(into_group_err)?
                .query_row(params![mls_group_id.as_slice()], db::row_to_message)
                .optional()
                .map_err(into_group_err)
        })
    }

    fn admins(&self, mls_group_id: &GroupId) -> Result<BTreeSet<PublicKey>, GroupError> {
        // Get the group which contains the admin_pubkeys
        match self.find_group_by_mls_group_id(mls_group_id)? {
            Some(group) => Ok(group.admin_pubkeys),
            None => Err(GroupError::InvalidParameters("Group not found".to_string())),
        }
    }

    fn group_relays(&self, mls_group_id: &GroupId) -> Result<BTreeSet<GroupRelay>, GroupError> {
        // First verify the group exists
        if self.find_group_by_mls_group_id(mls_group_id)?.is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM group_relays WHERE mls_group_id = ?")
                .map_err(into_group_err)?;

            let relays_iter = stmt
                .query_map(params![mls_group_id.as_slice()], db::row_to_group_relay)
                .map_err(into_group_err)?;

            let mut relays: BTreeSet<GroupRelay> = BTreeSet::new();

            for relay_result in relays_iter {
                let relay: GroupRelay = relay_result.map_err(into_group_err)?;
                relays.insert(relay);
            }

            Ok(relays)
        })
    }

    fn replace_group_relays(
        &self,
        group_id: &GroupId,
        relays: BTreeSet<RelayUrl>,
    ) -> Result<(), GroupError> {
        // First verify the group exists
        if self.find_group_by_mls_group_id(group_id)?.is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        self.with_connection(|conn| {
            // Use a savepoint for atomicity (works both inside/outside an existing transaction).
            conn.execute_batch("SAVEPOINT mdk_replace_group_relays")
                .map_err(into_group_err)?;

            let result: Result<(), GroupError> = (|| {
                conn.execute(
                    "DELETE FROM group_relays WHERE mls_group_id = ?",
                    params![group_id.as_slice()],
                )
                .map_err(into_group_err)?;

                for relay_url in &relays {
                    conn.execute(
                        "INSERT INTO group_relays (mls_group_id, relay_url) VALUES (?, ?)",
                        params![group_id.as_slice(), relay_url.as_str()],
                    )
                    .map_err(into_group_err)?;
                }
                Ok(())
            })();

            match result {
                Ok(()) => conn
                    .execute_batch("RELEASE SAVEPOINT mdk_replace_group_relays")
                    .map_err(into_group_err),
                Err(e) => {
                    // Best-effort cleanup to keep connection usable.
                    let _ = conn.execute_batch(
                        "ROLLBACK TO SAVEPOINT mdk_replace_group_relays; \
                         RELEASE SAVEPOINT mdk_replace_group_relays;",
                    );
                    Err(e)
                }
            }
        })
    }

    fn get_group_exporter_secret(
        &self,
        mls_group_id: &GroupId,
        epoch: u64,
    ) -> Result<Option<GroupExporterSecret>, GroupError> {
        // First verify the group exists
        if self.find_group_by_mls_group_id(mls_group_id)?.is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM group_exporter_secrets WHERE mls_group_id = ? AND epoch = ?",
                )
                .map_err(into_group_err)?;

            stmt.query_row(
                params![mls_group_id.as_slice(), epoch],
                db::row_to_group_exporter_secret,
            )
            .optional()
            .map_err(into_group_err)
        })
    }

    fn save_group_exporter_secret(
        &self,
        group_exporter_secret: GroupExporterSecret,
    ) -> Result<(), GroupError> {
        if self
            .find_group_by_mls_group_id(&group_exporter_secret.mls_group_id)?
            .is_none()
        {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        self.with_connection(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO group_exporter_secrets (mls_group_id, epoch, secret) VALUES (?, ?, ?)",
                params![&group_exporter_secret.mls_group_id.as_slice(), &group_exporter_secret.epoch, group_exporter_secret.secret.as_ref()],
            )
            .map_err(into_group_err)?;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::Secret;
    use mdk_storage_traits::groups::types::GroupState;
    use mdk_storage_traits::messages::MessageStorage;
    use mdk_storage_traits::messages::types::MessageState;
    use mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes;
    use nostr::{EventId, Kind, Tags, Timestamp, UnsignedEvent};
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_save_and_find_group() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a test group
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));

        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };

        // Save the group
        let result = storage.save_group(group);
        assert!(result.is_ok());

        // Find by MLS group ID
        let found_group = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_group.nostr_group_id, nostr_group_id);

        // Find by Nostr group ID
        let found_group = storage
            .find_group_by_nostr_group_id(&nostr_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_group.mls_group_id, mls_group_id);

        // Get all groups
        let all_groups = storage.all_groups().unwrap();
        assert_eq!(all_groups.len(), 1);
    }

    #[test]
    fn test_group_name_length_validation() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a group with name exceeding the limit (255 characters)
        let oversized_name = "x".repeat(256);

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: oversized_name,
            description: "Test".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        // Should fail due to name length
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group name exceeds maximum length")
        );
    }

    #[test]
    fn test_group_description_length_validation() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a group with description exceeding the limit (2000 characters)
        let oversized_description = "x".repeat(2001);

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: oversized_description,
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        // Should fail due to description length
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group description exceeds maximum length")
        );
    }

    // Note: Comprehensive storage functionality tests are now in mdk-storage-traits/tests/
    // using shared test functions to ensure consistency between storage implementations

    #[test]
    fn test_messages_pagination() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a test group
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();

        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        storage.save_group(group).unwrap();

        // Create 25 test messages
        let pubkey = PublicKey::from_slice(&[1u8; 32]).unwrap();
        for i in 0..25 {
            let event_id = EventId::from_slice(&[i as u8; 32]).unwrap();
            let wrapper_event_id = EventId::from_slice(&[100 + i as u8; 32]).unwrap();

            let ts = Timestamp::from((1000 + i) as u64);
            let message = Message {
                id: event_id,
                pubkey,
                kind: Kind::from(1u16),
                mls_group_id: mls_group_id.clone(),
                created_at: ts,
                processed_at: ts,
                content: format!("Message {}", i),
                tags: Tags::new(),
                event: UnsignedEvent::new(
                    pubkey,
                    ts,
                    Kind::from(9u16),
                    vec![],
                    format!("content {}", i),
                ),
                wrapper_event_id,
                state: MessageState::Created,
                epoch: None,
            };

            storage.save_message(message).unwrap();
        }

        // Test pagination
        let page1 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(page1.len(), 10);
        // Should be newest first (highest timestamp)
        assert_eq!(page1[0].content, "Message 24");

        let page2 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(10))))
            .unwrap();
        assert_eq!(page2.len(), 10);
        assert_eq!(page2[0].content, "Message 14");

        let page3 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(20))))
            .unwrap();
        assert_eq!(page3.len(), 5); // Only 5 messages left
        assert_eq!(page3[0].content, "Message 4");

        // Test default messages() uses limit
        let default_messages = storage.messages(&mls_group_id, None).unwrap();
        assert_eq!(default_messages.len(), 25); // All messages since < 1000

        // Test: Verify no overlap between pages
        let first_id = page1[0].id;
        let second_page_ids: Vec<EventId> = page2.iter().map(|m| m.id).collect();
        assert!(
            !second_page_ids.contains(&first_id),
            "Pages should not overlap"
        );

        // Test: Offset beyond available messages returns empty
        let beyond = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(30))))
            .unwrap();
        assert_eq!(beyond.len(), 0);

        // Test: Limit of 0 should return error
        let result = storage.messages(&mls_group_id, Some(Pagination::new(Some(0), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test: Limit exceeding MAX should return error
        let result = storage.messages(&mls_group_id, Some(Pagination::new(Some(20000), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test: Non-existent group returns error
        let fake_group_id = GroupId::from_slice(&[99, 99, 99, 99]);
        let result = storage.messages(&fake_group_id, Some(Pagination::new(Some(10), Some(0))));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        // Test: Large offset should work (no MAX_OFFSET validation)
        let result = storage.messages(
            &mls_group_id,
            Some(Pagination::new(Some(10), Some(2_000_000))),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0); // No results at that offset
    }

    #[test]
    fn test_group_exporter_secret() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a test group
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();

        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        // Save the group
        storage.save_group(group).unwrap();

        // Create a group exporter secret
        let secret1 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 1,
            secret: Secret::new([0u8; 32]),
        };

        // Save the secret
        storage.save_group_exporter_secret(secret1).unwrap();

        // Get the secret and verify it was saved correctly
        let retrieved_secret = storage
            .get_group_exporter_secret(&mls_group_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(*retrieved_secret.secret, [0u8; 32]);

        // Create a second secret with same group_id and epoch but different secret value
        let secret2 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 1,
            secret: Secret::new([0u8; 32]),
        };

        // Save the second secret - this should replace the first one due to the "OR REPLACE" in the SQL
        storage.save_group_exporter_secret(secret2).unwrap();

        // Get the secret again and verify it was updated
        let retrieved_secret = storage
            .get_group_exporter_secret(&mls_group_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(*retrieved_secret.secret, [0u8; 32]);

        // Verify we can still save a different epoch
        let secret3 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 2,
            secret: Secret::new([0u8; 32]),
        };

        storage.save_group_exporter_secret(secret3).unwrap();

        // Verify both epochs exist
        let retrieved_secret1 = storage
            .get_group_exporter_secret(&mls_group_id, 1)
            .unwrap()
            .unwrap();
        let retrieved_secret2 = storage
            .get_group_exporter_secret(&mls_group_id, 2)
            .unwrap()
            .unwrap();

        assert_eq!(*retrieved_secret1.secret, [0u8; 32]);
        assert_eq!(*retrieved_secret2.secret, [0u8; 32]);
    }

    #[test]
    fn test_all_groups_skips_corrupted_rows() {
        // Use a file-based database so we can access it from multiple connections
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

        // Create and save two valid groups
        let mls_group_id1 = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id1 = generate_random_bytes(32).try_into().unwrap();
        let group1 = Group {
            mls_group_id: mls_group_id1.clone(),
            nostr_group_id: nostr_group_id1,
            name: "Group 1".to_string(),
            description: "First group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group1).unwrap();

        let mls_group_id2 = GroupId::from_slice(&[5, 6, 7, 8]);
        let nostr_group_id2 = generate_random_bytes(32).try_into().unwrap();
        let group2 = Group {
            mls_group_id: mls_group_id2.clone(),
            nostr_group_id: nostr_group_id2,
            name: "Group 2".to_string(),
            description: "Second group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group2).unwrap();

        let corrupt_conn = Connection::open(&db_path).unwrap();
        let corrupted_nostr_id_bytes = generate_random_bytes(32);
        let corrupted_nostr_id: [u8; 32] = corrupted_nostr_id_bytes.try_into().unwrap();
        corrupt_conn
            .execute(
                "INSERT INTO groups (mls_group_id, nostr_group_id, name, description, admin_pubkeys, epoch, state) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    &[9u8; 16], // Valid mls_group_id
                    &corrupted_nostr_id,
                    "Corrupted Group",
                    "This group has invalid state",
                    "[]", // Valid JSON for admin_pubkeys
                    0,
                    "invalid_state" // Invalid state that will fail deserialization
                ],
            )
            .unwrap();

        // all_groups should return the two valid groups and skip the corrupted one
        let all_groups = storage.all_groups().unwrap();
        assert_eq!(all_groups.len(), 2);
        assert_eq!(all_groups[0].mls_group_id, mls_group_id1);
        assert_eq!(all_groups[1].mls_group_id, mls_group_id2);
    }
}
