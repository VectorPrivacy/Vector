//! Memory-based storage implementation of the MdkStorageProvider trait for MDK welcomes

use mdk_storage_traits::welcomes::error::WelcomeError;
use mdk_storage_traits::welcomes::types::*;
use mdk_storage_traits::welcomes::{MAX_PENDING_WELCOMES_LIMIT, Pagination, WelcomeStorage};
use nostr::EventId;

use crate::MdkMemoryStorage;

impl WelcomeStorage for MdkMemoryStorage {
    fn save_welcome(&self, welcome: Welcome) -> Result<(), WelcomeError> {
        // Validate relay count to prevent memory exhaustion
        if welcome.group_relays.len() > self.limits.max_relays_per_welcome {
            return Err(WelcomeError::InvalidParameters(format!(
                "Welcome relay count exceeds maximum of {} (got {})",
                self.limits.max_relays_per_welcome,
                welcome.group_relays.len()
            )));
        }

        // Validate individual relay URL lengths
        for relay in &welcome.group_relays {
            if relay.as_str().len() > self.limits.max_relay_url_length {
                return Err(WelcomeError::InvalidParameters(format!(
                    "Relay URL exceeds maximum length of {} bytes",
                    self.limits.max_relay_url_length
                )));
            }
        }

        // Validate admin pubkeys count to prevent memory exhaustion
        if welcome.group_admin_pubkeys.len() > self.limits.max_admins_per_welcome {
            return Err(WelcomeError::InvalidParameters(format!(
                "Welcome admin count exceeds maximum of {} (got {})",
                self.limits.max_admins_per_welcome,
                welcome.group_admin_pubkeys.len()
            )));
        }

        let mut inner = self.inner.write();
        inner.welcomes_cache.put(welcome.id, welcome);

        Ok(())
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

        let inner = self.inner.read();
        let mut welcomes: Vec<Welcome> = inner
            .welcomes_cache
            .iter()
            .map(|(_, v)| v.clone())
            .filter(|welcome| welcome.state == WelcomeState::Pending)
            .collect();

        // Sort by ID (descending) for consistent ordering
        welcomes.sort_by(|a, b| b.id.cmp(&a.id));

        // Apply pagination
        let welcomes: Vec<Welcome> = welcomes.into_iter().skip(offset).take(limit).collect();

        Ok(welcomes)
    }

    fn find_welcome_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<Welcome>, WelcomeError> {
        let inner = self.inner.read();
        Ok(inner.welcomes_cache.peek(event_id).cloned())
    }

    fn save_processed_welcome(
        &self,
        processed_welcome: ProcessedWelcome,
    ) -> Result<(), WelcomeError> {
        let mut inner = self.inner.write();
        inner
            .processed_welcomes_cache
            .put(processed_welcome.wrapper_event_id, processed_welcome);

        Ok(())
    }

    fn find_processed_welcome_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<ProcessedWelcome>, WelcomeError> {
        let inner = self.inner.read();
        Ok(inner.processed_welcomes_cache.peek(event_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::test_utils::cross_storage::create_test_welcome;
    use nostr::{EventId, Keys, Kind, PublicKey, RelayUrl, Tags, Timestamp, UnsignedEvent};

    use super::*;
    use crate::{
        DEFAULT_MAX_ADMINS_PER_WELCOME, DEFAULT_MAX_RELAY_URL_LENGTH,
        DEFAULT_MAX_RELAYS_PER_WELCOME,
    };

    fn create_welcome_with_relays(
        mls_group_id: GroupId,
        event_id: EventId,
        relay_count: usize,
    ) -> Welcome {
        let pubkey =
            PublicKey::parse("npub1a6awmmklxfmspwdv52qq58sk5c07kghwc4v2eaudjx2ju079cdqs2452ys")
                .unwrap();
        let created_at = Timestamp::now();
        let content = "Test welcome content".to_string();
        let tags = Tags::new();

        let event = UnsignedEvent {
            id: Some(event_id),
            pubkey,
            created_at,
            kind: Kind::Custom(444),
            tags,
            content,
        };

        let mut relays = BTreeSet::new();
        for i in 0..relay_count {
            relays.insert(RelayUrl::parse(&format!("wss://relay{}.example.com", i)).unwrap());
        }

        Welcome {
            id: event_id,
            event,
            mls_group_id,
            nostr_group_id: [0u8; 32],
            group_name: "Test Group".to_string(),
            group_description: "A test group".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: BTreeSet::from([pubkey]),
            group_relays: relays,
            welcomer: pubkey,
            member_count: 1,
            state: WelcomeState::Pending,
            wrapper_event_id: event_id,
        }
    }

    fn create_welcome_with_admins(
        mls_group_id: GroupId,
        event_id: EventId,
        admin_count: usize,
    ) -> Welcome {
        let pubkey =
            PublicKey::parse("npub1a6awmmklxfmspwdv52qq58sk5c07kghwc4v2eaudjx2ju079cdqs2452ys")
                .unwrap();
        let created_at = Timestamp::now();
        let content = "Test welcome content".to_string();
        let tags = Tags::new();

        let event = UnsignedEvent {
            id: Some(event_id),
            pubkey,
            created_at,
            kind: Kind::Custom(444),
            tags,
            content,
        };

        let mut admins = BTreeSet::new();
        for _ in 0..admin_count {
            admins.insert(Keys::generate().public_key());
        }

        Welcome {
            id: event_id,
            event,
            mls_group_id,
            nostr_group_id: [0u8; 32],
            group_name: "Test Group".to_string(),
            group_description: "A test group".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: admins,
            group_relays: BTreeSet::from([RelayUrl::parse("wss://relay.example.com").unwrap()]),
            welcomer: pubkey,
            member_count: 1,
            state: WelcomeState::Pending,
            wrapper_event_id: event_id,
        }
    }

    #[test]
    fn test_save_welcome_relay_count_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test with relay count at exactly the limit (should succeed)
        let event_id = EventId::from_hex(&format!("{:064x}", 1)).unwrap();
        let welcome = create_welcome_with_relays(
            mls_group_id.clone(),
            event_id,
            DEFAULT_MAX_RELAYS_PER_WELCOME,
        );
        assert!(storage.save_welcome(welcome).is_ok());

        // Test with relay count exceeding the limit (should fail)
        let event_id = EventId::from_hex(&format!("{:064x}", 2)).unwrap();
        let welcome = create_welcome_with_relays(
            mls_group_id.clone(),
            event_id,
            DEFAULT_MAX_RELAYS_PER_WELCOME + 1,
        );
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Welcome relay count exceeds maximum")
        );
    }

    #[test]
    fn test_save_welcome_relay_url_length_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let event_id = EventId::from_hex(&format!("{:064x}", 1)).unwrap();

        // Test with URL at exactly the limit (should succeed)
        let domain = "a".repeat(DEFAULT_MAX_RELAY_URL_LENGTH - 10);
        let url = format!("wss://{}.com", domain);
        let mut welcome = create_test_welcome(mls_group_id.clone(), event_id);
        welcome.group_relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        assert!(storage.save_welcome(welcome).is_ok());

        // Test with URL exceeding the limit (should fail)
        let event_id = EventId::from_hex(&format!("{:064x}", 2)).unwrap();
        let domain = "a".repeat(DEFAULT_MAX_RELAY_URL_LENGTH);
        let url = format!("wss://{}.com", domain);
        let mut welcome = create_test_welcome(mls_group_id.clone(), event_id);
        welcome.group_relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Relay URL exceeds maximum length")
        );
    }

    #[test]
    fn test_save_welcome_admin_count_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test with admin count at exactly the limit (should succeed)
        let event_id = EventId::from_hex(&format!("{:064x}", 1)).unwrap();
        let welcome = create_welcome_with_admins(
            mls_group_id.clone(),
            event_id,
            DEFAULT_MAX_ADMINS_PER_WELCOME,
        );
        assert!(storage.save_welcome(welcome).is_ok());

        // Test with admin count exceeding the limit (should fail)
        let event_id = EventId::from_hex(&format!("{:064x}", 2)).unwrap();
        let welcome = create_welcome_with_admins(
            mls_group_id.clone(),
            event_id,
            DEFAULT_MAX_ADMINS_PER_WELCOME + 1,
        );
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Welcome admin count exceeds maximum")
        );
    }

    #[test]
    fn test_pending_welcomes_pagination_memory() {
        let storage = MdkMemoryStorage::new();

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Create 25 pending welcomes with increasing IDs
        for i in 0..25 {
            let event_id = EventId::from_hex(&format!("{:064x}", i + 1)).unwrap();
            let welcome = create_test_welcome(mls_group_id.clone(), event_id);
            storage.save_welcome(welcome).unwrap();
        }

        // Test 1: Get all pending welcomes (should use default limit)
        let all_welcomes = storage.pending_welcomes(None).unwrap();
        assert_eq!(all_welcomes.len(), 25);

        // Test 2: Get first 10 welcomes
        let first_10 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(first_10.len(), 10);

        // Test 3: Get next 10 welcomes (offset 10)
        let next_10 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(10))))
            .unwrap();
        assert_eq!(next_10.len(), 10);

        // Test 4: Get last 5 welcomes (offset 20)
        let last_5 = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(20))))
            .unwrap();
        assert_eq!(last_5.len(), 5);

        // Test 5: Offset beyond available welcomes returns empty
        let beyond = storage
            .pending_welcomes(Some(Pagination::new(Some(10), Some(30))))
            .unwrap();
        assert_eq!(beyond.len(), 0);

        // Test 6: Verify no overlap between pages
        let first_id = first_10[0].id;
        let second_page_ids: Vec<EventId> = next_10.iter().map(|w| w.id).collect();
        assert!(
            !second_page_ids.contains(&first_id),
            "Pages should not overlap"
        );

        // Test 7: Verify ordering is descending by ID
        for i in 0..first_10.len() - 1 {
            assert!(
                first_10[i].id > first_10[i + 1].id,
                "Welcomes should be ordered by ID descending"
            );
        }

        // Test 8: Limit of 0 should return error
        let result = storage.pending_welcomes(Some(Pagination::new(Some(0), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test 9: Limit exceeding MAX should return error
        let result = storage.pending_welcomes(Some(Pagination::new(Some(20000), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test 10: Large offset should work (no MAX_OFFSET validation)
        let result = storage.pending_welcomes(Some(Pagination::new(Some(10), Some(2_000_000))));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0); // No results at that offset

        // Test 11: Empty results when no pending entries
        let storage2 = MdkMemoryStorage::new();
        let empty = storage2
            .pending_welcomes(Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(empty.len(), 0);
    }

    /// Test that custom validation limits work correctly for welcomes
    #[test]
    fn test_custom_welcome_limits() {
        use crate::ValidationLimits;

        // Create storage with custom smaller limits
        let limits = ValidationLimits::default()
            .with_max_relays_per_welcome(2)
            .with_max_admins_per_welcome(3)
            .with_max_relay_url_length(50);

        let storage = MdkMemoryStorage::with_limits(limits);

        // Verify limits are accessible
        assert_eq!(storage.limits().max_relays_per_welcome, 2);
        assert_eq!(storage.limits().max_admins_per_welcome, 3);
        assert_eq!(storage.limits().max_relay_url_length, 50);

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test relay count with custom limit (2 relays should succeed)
        let event_id = EventId::from_hex(&format!("{:064x}", 1)).unwrap();
        let welcome = create_welcome_with_relays(mls_group_id.clone(), event_id, 2);
        assert!(storage.save_welcome(welcome).is_ok());

        // Test relay count exceeding custom limit (3 relays should fail)
        let event_id = EventId::from_hex(&format!("{:064x}", 2)).unwrap();
        let welcome = create_welcome_with_relays(mls_group_id.clone(), event_id, 3);
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maximum of 2"));

        // Test admin count with custom limit (3 admins should succeed)
        let event_id = EventId::from_hex(&format!("{:064x}", 3)).unwrap();
        let welcome = create_welcome_with_admins(mls_group_id.clone(), event_id, 3);
        assert!(storage.save_welcome(welcome).is_ok());

        // Test admin count exceeding custom limit (4 admins should fail)
        let event_id = EventId::from_hex(&format!("{:064x}", 4)).unwrap();
        let welcome = create_welcome_with_admins(mls_group_id.clone(), event_id, 4);
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maximum of 3"));

        // Test relay URL length with custom limit (50 bytes)
        // URL "wss://a{40}.com" = 6 + 40 + 4 = 50 bytes (should succeed)
        let event_id = EventId::from_hex(&format!("{:064x}", 5)).unwrap();
        let mut welcome = create_test_welcome(mls_group_id.clone(), event_id);
        let domain = "a".repeat(40);
        let url = format!("wss://{}.com", domain);
        welcome.group_relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        assert!(storage.save_welcome(welcome).is_ok());

        // URL exceeding 50 bytes should fail
        let event_id = EventId::from_hex(&format!("{:064x}", 6)).unwrap();
        let mut welcome = create_test_welcome(mls_group_id.clone(), event_id);
        let domain = "a".repeat(45); // 6 + 45 + 4 = 55 bytes
        let url = format!("wss://{}.com", domain);
        welcome.group_relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        let result = storage.save_welcome(welcome);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("50 bytes"));
    }
}
