//! Implementation of WelcomeStorage trait for SQLite storage.

use mdk_storage_traits::welcomes::error::WelcomeError;
use mdk_storage_traits::welcomes::types::{ProcessedWelcome, Welcome};
use mdk_storage_traits::welcomes::{MAX_PENDING_WELCOMES_LIMIT, Pagination, WelcomeStorage};
use nostr::{EventId, JsonUtil};
use rusqlite::{OptionalExtension, params};

use crate::db::{Hash32, Nonce12};
use crate::validation::{
    MAX_ADMIN_PUBKEYS_JSON_SIZE, MAX_EVENT_JSON_SIZE, MAX_GROUP_DESCRIPTION_LENGTH,
    MAX_GROUP_NAME_LENGTH, MAX_GROUP_RELAYS_JSON_SIZE, validate_size, validate_string_length,
};
use crate::{MdkSqliteStorage, db};

#[inline]
fn into_welcome_err<T>(e: T) -> WelcomeError
where
    T: std::error::Error,
{
    WelcomeError::DatabaseError(e.to_string())
}

impl WelcomeStorage for MdkSqliteStorage {
    fn save_welcome(&self, welcome: Welcome) -> Result<(), WelcomeError> {
        // Validate group name and description lengths
        validate_string_length(&welcome.group_name, MAX_GROUP_NAME_LENGTH, "Group name")
            .map_err(|e| WelcomeError::InvalidParameters(e.to_string()))?;

        validate_string_length(
            &welcome.group_description,
            MAX_GROUP_DESCRIPTION_LENGTH,
            "Group description",
        )
        .map_err(|e| WelcomeError::InvalidParameters(e.to_string()))?;

        // Serialize complex types to JSON
        let group_admin_pubkeys_json: String = serde_json::to_string(&welcome.group_admin_pubkeys)
            .map_err(|e| {
                WelcomeError::DatabaseError(format!("Failed to serialize admin pubkeys: {}", e))
            })?;

        // Validate admin pubkeys JSON size
        validate_size(
            group_admin_pubkeys_json.as_bytes(),
            MAX_ADMIN_PUBKEYS_JSON_SIZE,
            "Admin pubkeys JSON",
        )
        .map_err(|e| WelcomeError::InvalidParameters(e.to_string()))?;

        let group_relays_json: String =
            serde_json::to_string(&welcome.group_relays).map_err(|e| {
                WelcomeError::DatabaseError(format!("Failed to serialize group relays: {}", e))
            })?;

        // Validate group relays JSON size
        validate_size(
            group_relays_json.as_bytes(),
            MAX_GROUP_RELAYS_JSON_SIZE,
            "Group relays JSON",
        )
        .map_err(|e| WelcomeError::InvalidParameters(e.to_string()))?;

        // Serialize event to JSON
        let event_json = welcome.event.as_json();

        // Validate event JSON size
        validate_size(event_json.as_bytes(), MAX_EVENT_JSON_SIZE, "Event JSON")
            .map_err(|e| WelcomeError::InvalidParameters(e.to_string()))?;

        self.with_connection(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO welcomes
             (id, event, mls_group_id, nostr_group_id, group_name, group_description, group_image_hash, group_image_key, group_image_nonce,
              group_admin_pubkeys, group_relays, welcomer, member_count, state, wrapper_event_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    welcome.id.as_bytes(),
                    &event_json,
                    welcome.mls_group_id.as_slice(),
                    &welcome.nostr_group_id,
                    &welcome.group_name,
                    &welcome.group_description,
                    welcome.group_image_hash.map(Hash32::from),
                    welcome.group_image_key.as_ref().map(|k| Hash32::from(**k)),
                    welcome.group_image_nonce.as_ref().map(|n| Nonce12::from(**n)),
                    &group_admin_pubkeys_json,
                    &group_relays_json,
                    welcome.welcomer.as_bytes(),
                    welcome.member_count as u64,
                    welcome.state.as_str(),
                    welcome.wrapper_event_id.as_bytes(),
                ],
            )
            .map_err(into_welcome_err)?;

            Ok(())
        })
    }

    fn find_welcome_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<Welcome>, WelcomeError> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM welcomes WHERE id = ?")
                .map_err(into_welcome_err)?;

            stmt.query_row(params![event_id.as_bytes()], db::row_to_welcome)
                .optional()
                .map_err(into_welcome_err)
        })
    }

    fn pending_welcomes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<Vec<Welcome>, WelcomeError> {
        let pagination = pagination.unwrap_or_default();
        let limit = pagination.limit();
        let offset = pagination.offset();

        // Validate limit is within allowed range
        if !(1..=MAX_PENDING_WELCOMES_LIMIT).contains(&limit) {
            return Err(WelcomeError::InvalidParameters(format!(
                "Limit must be between 1 and {}, got {}",
                MAX_PENDING_WELCOMES_LIMIT, limit
            )));
        }

        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM welcomes WHERE state = 'pending' 
                     ORDER BY id DESC 
                     LIMIT ? OFFSET ?",
                )
                .map_err(into_welcome_err)?;

            let welcomes_iter = stmt
                .query_map(params![limit as i64, offset as i64], db::row_to_welcome)
                .map_err(into_welcome_err)?;

            let mut welcomes: Vec<Welcome> = Vec::new();

            for welcome_result in welcomes_iter {
                let welcome: Welcome = welcome_result.map_err(into_welcome_err)?;
                welcomes.push(welcome);
            }

            Ok(welcomes)
        })
    }

    fn save_processed_welcome(
        &self,
        processed_welcome: ProcessedWelcome,
    ) -> Result<(), WelcomeError> {
        // Convert welcome_event_id to string if it exists
        let welcome_event_id: Option<&[u8; 32]> = processed_welcome
            .welcome_event_id
            .as_ref()
            .map(|id| id.as_bytes());

        self.with_connection(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO processed_welcomes
             (wrapper_event_id, welcome_event_id, processed_at, state, failure_reason)
             VALUES (?, ?, ?, ?, ?)",
                params![
                    processed_welcome.wrapper_event_id.as_bytes(),
                    welcome_event_id,
                    processed_welcome.processed_at.as_secs(),
                    processed_welcome.state.as_str(),
                    &processed_welcome.failure_reason
                ],
            )
            .map_err(into_welcome_err)?;

            Ok(())
        })
    }

    fn find_processed_welcome_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<ProcessedWelcome>, WelcomeError> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM processed_welcomes WHERE wrapper_event_id = ?")
                .map_err(into_welcome_err)?;

            stmt.query_row(params![event_id.as_bytes()], db::row_to_processed_welcome)
                .optional()
                .map_err(into_welcome_err)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::test_utils::cross_storage::{
        create_test_group, create_test_processed_welcome, create_test_welcome,
    };
    use mdk_storage_traits::welcomes::types::{ProcessedWelcomeState, Welcome, WelcomeState};
    use nostr::{EventId, Kind, PublicKey, Timestamp, UnsignedEvent};

    use super::*;

    #[test]
    fn test_save_and_find_welcome() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // First create a group (welcomes require a valid group foreign key)
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let group = create_test_group(mls_group_id.clone());

        // Save the group
        let result = storage.save_group(group);
        assert!(result.is_ok(), "{:?}", result);

        // Create a test welcome using the helper
        let event_id = EventId::all_zeros();
        let welcome = create_test_welcome(mls_group_id.clone(), event_id);

        // Save the welcome
        let result = storage.save_welcome(welcome.clone());
        assert!(result.is_ok(), "{:?}", result);

        // Find by event ID
        let found_welcome = storage
            .find_welcome_by_event_id(&event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_welcome.id, event_id);
        assert_eq!(found_welcome.mls_group_id, mls_group_id);
        assert_eq!(found_welcome.state, welcome.state);

        // Test pending welcomes
        let pending_welcomes = storage.pending_welcomes(None).unwrap();
        assert_eq!(pending_welcomes.len(), 1);
        assert_eq!(pending_welcomes[0].id, event_id);
    }

    #[test]
    fn test_processed_welcome() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create test event IDs using helper methods
        let wrapper_event_id = EventId::all_zeros();
        let welcome_event_id =
            EventId::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();

        // Create a test processed welcome using the helper
        let processed_welcome =
            create_test_processed_welcome(wrapper_event_id, Some(welcome_event_id));

        // Save the processed welcome
        let result = storage.save_processed_welcome(processed_welcome.clone());
        assert!(result.is_ok());

        // Find by event ID
        let found_processed_welcome = storage
            .find_processed_welcome_by_event_id(&wrapper_event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_processed_welcome.wrapper_event_id, wrapper_event_id);
        assert_eq!(
            found_processed_welcome.welcome_event_id.unwrap(),
            welcome_event_id
        );
        assert_eq!(
            found_processed_welcome.state,
            ProcessedWelcomeState::Processed
        );
    }

    #[test]
    fn test_welcome_group_name_length_validation() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a group first
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let group = create_test_group(mls_group_id.clone());
        storage.save_group(group).unwrap();

        // Create a welcome with oversized group name
        let oversized_name = "x".repeat(256);

        let event_id = EventId::all_zeros();
        let pubkey = PublicKey::from_slice(&[1u8; 32]).unwrap();
        let wrapper_event_id =
            EventId::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();

        let welcome = Welcome {
            id: event_id,
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::now(),
                Kind::from(444u16),
                vec![],
                "content".to_string(),
            ),
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: [0u8; 32],
            group_name: oversized_name,
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: BTreeSet::new(),
            group_relays: BTreeSet::new(),
            welcomer: pubkey,
            member_count: 1,
            state: WelcomeState::Pending,
            wrapper_event_id,
        };

        // Should fail due to group name length
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group name exceeds maximum length")
        );
    }

    #[test]
    fn test_pending_welcomes_pagination() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a group first
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let group = create_test_group(mls_group_id.clone());
        storage.save_group(group).unwrap();

        // Create 25 pending welcomes
        for i in 0..25 {
            let event_id = EventId::from_hex(&format!(
                "{:064x}",
                i + 1 // Start from 1 to avoid all_zeros
            ))
            .unwrap();
            let welcome = create_test_welcome(mls_group_id.clone(), event_id);
            storage.save_welcome(welcome).unwrap();
        }

        // Test: Get all pending welcomes (should use default limit of 1000)
        let all_welcomes = storage.pending_welcomes(None).unwrap();
        assert_eq!(all_welcomes.len(), 25);

        // Test: Get first 10 welcomes
        let first_10 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(first_10.len(), 10);

        // Test: Get next 10 welcomes (offset 10)
        let next_10 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(10))))
            .unwrap();
        assert_eq!(next_10.len(), 10);

        // Test: Get last 5 welcomes (offset 20)
        let last_5 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(20))))
            .unwrap();
        assert_eq!(last_5.len(), 5);

        // Test: Offset beyond available welcomes
        let beyond = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(30))))
            .unwrap();
        assert_eq!(beyond.len(), 0);

        // Test: Verify no overlap between pages
        let first_id = first_10[0].id;
        let second_page_ids: Vec<EventId> = next_10.iter().map(|w| w.id).collect();
        assert!(!second_page_ids.contains(&first_id));

        // Test: Limit of 0 should return error
        let result = storage.pending_welcomes(Some(Pagination::new(Some(0), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test: Limit exceeding MAX should return error
        let result = storage.pending_welcomes(Some(Pagination::new(Some(20000), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test: Large offset should work (no MAX_OFFSET validation)
        let result = storage.pending_welcomes(Some(Pagination::new(Some(10), Some(2_000_000))));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0); // No results at that offset
    }
}
