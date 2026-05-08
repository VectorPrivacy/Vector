# Changelog

<!-- All notable changes to this project will be documented in this file. -->

<!-- The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), -->
<!-- and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). -->

<!-- Template

## Unreleased

### Breaking changes

### Changed

### Added

### Fixed

### Removed

### Deprecated

-->

## Unreleased

### Breaking changes

### Changed

### Added

### Fixed

### Removed

### Deprecated

## [0.6.0] - 2026-02-18

### Breaking changes

- **MLS codec switched from JSON to postcard**: MLS storage serialization now uses `MlsCodec` (postcard binary format) instead of the removed `JsonCodec` (serde_json). Existing SQLite databases contain JSON-encoded MLS data and are incompatible — they must be recreated. ([#179](https://github.com/marmot-protocol/mdk/pull/179))

### Changed

- **OpenMLS 0.8.0 Upgrade**: Updated `openmls` to 0.8.0 and `openmls_traits` to 0.5. Updated `time` (via `refinery`) to 0.3.47 to resolve a security advisory. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Message Sorting**: The `messages()` query now uses `ORDER BY created_at DESC, processed_at DESC, id DESC`. The secondary sort by `processed_at` keeps messages in reception order when `created_at` is the same. The tertiary sort by `id` ensures deterministic ordering. A new composite index `idx_messages_sorting` supports this query. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- Upgraded `nostr` dependency from 0.43 to 0.44, replacing deprecated `Timestamp::as_u64()` calls with `Timestamp::as_secs()` ([#162](https://github.com/marmot-protocol/mdk/pull/162))
- **Persistent Snapshots**: Implemented snapshot support by copying group-specific rows to a dedicated snapshot table. `create_group_snapshot`, `rollback_group_to_snapshot`, and `release_group_snapshot` persist across app restarts. ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- **Unified Storage Architecture**: `MdkSqliteStorage` now directly implements OpenMLS's `StorageProvider<1>` trait instead of wrapping `openmls_sqlite_storage`. This enables atomic transactions across MLS and MDK state, which is required for proper commit race resolution per MIP-03. ([#148](https://github.com/marmot-protocol/mdk/pull/148))
  - Removed `openmls_sqlite_storage` dependency
  - New unified schema in `V001__initial_schema.sql` replaces all previous migrations
  - All MLS tables (`openmls_*`) are now managed directly by MDK
  - Single database connection enables transactional consistency
- **Security (Audit Issue M)**: Changed `MessageStorage::find_message_by_event_id()` to require both `mls_group_id` and `event_id` parameters. This prevents messages from different groups from overwriting each other. Database migration V105 changes the messages table primary key from `id` to `(mls_group_id, id)`. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- Updated `messages()` implementation to accept `Option<Pagination>` parameter ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Updated `pending_welcomes()` implementation to accept `Option<Pagination>` parameter ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- Upgraded `refinery` from 0.8 to 0.9 to align with OpenMLS dependencies ([#142](https://github.com/marmot-protocol/mdk/pull/142))
- **Storage Security**: Updated storage operations to use `Secret<T>` wrapper for secret values, ensuring automatic memory zeroization when values are dropped ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- SQLite is now built with SQLCipher support (`bundled-sqlcipher`) instead of plain SQLite (`bundled`), enabling transparent AES-256 encryption at rest ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- Simplified validation logic to use range contains pattern for better readability ([#111](https://github.com/marmot-protocol/mdk/pull/111))

### Added

- **Self-update tracking column**: Added V004 migration adding `last_self_update_at INTEGER` column to the `groups` table for self-update state tracking. `NULL` = no obligation, `0` = required (MIP-02), `>0` = unix timestamp of last rotation (MIP-00). The column is also captured and restored by the snapshot/rollback mechanism. ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **Custom Message Sort Order**: `messages()` now respects the `sort_order` field in `Pagination`, supporting both `CreatedAtFirst` (default) and `ProcessedAtFirst` orderings via different SQL `ORDER BY` clauses. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Last Message by Sort Order**: Implemented `last_message()` to return the most recent message under a given sort order via `SELECT ... ORDER BY ... LIMIT 1`. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Processed-At-First Sort Index**: Added V003 migration creating `idx_messages_sorting_processed_at` composite index on `messages(mls_group_id, processed_at DESC, created_at DESC, id DESC)` for efficient `ProcessedAtFirst` queries. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Group `last_message_processed_at` Column**: Added `last_message_processed_at` column to the `groups` table via V002 migration to track when the last message was processed/received by this client. This enables consistent ordering between `group.last_message_id` and `get_messages()[0].id`. Existing groups are backfilled with their `last_message_at` value as a reasonable default. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Message `processed_at` Column**: Added `processed_at` column to the `messages` table via V002 migration to store when messages were processed/received by the client. Existing messages are backfilled with their `created_at` value as a reasonable default. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Epoch Lookup by Tag Content**: Implemented `find_message_epoch_by_tag_content` for SQLite storage using `SELECT epoch FROM messages WHERE tags LIKE ?` query. ([#167](https://github.com/marmot-protocol/mdk/pull/167))
- **Retryable Message Support**: Updated storage implementation to handle `ProcessedMessageState::Retryable` transitions and persistence. ([#161](https://github.com/marmot-protocol/mdk/pull/161))
- **MLS Storage Module**: New `mls_storage` module with complete `StorageProvider<1>` implementation for OpenMLS integration ([#148](https://github.com/marmot-protocol/mdk/pull/148))
  - Postcard codec (`MlsCodec`) for serializing/deserializing OpenMLS types ([#179](https://github.com/marmot-protocol/mdk/pull/179))
  - Support for all 53 `StorageProvider<1>` methods
  - Manages 8 OpenMLS tables: `openmls_group_data`, `openmls_proposals`, `openmls_own_leaf_nodes`, `openmls_key_packages`, `openmls_psks`, `openmls_signature_keys`, `openmls_encryption_keys`, `openmls_epoch_key_pairs`
- Input validation for storage operations to prevent unbounded writes ([#94](https://github.com/marmot-protocol/mdk/pull/94))
  - Message content limited to 1MB
  - Group names limited to 255 bytes
  - Group descriptions limited to 2000 bytes
  - JSON fields limited to 50-100KB
  - New `Validation` error variant for validation failures
- Automatic key management with `keyring-core`: `new()` constructor handles encryption key generation and secure storage automatically using the platform's native credential store (Keychain, Keystore, etc.) ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- New `keyring` module with `get_or_create_db_key()` and `delete_db_key()` utilities ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- New `encryption` module with `EncryptionConfig` struct and SQLCipher utilities ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- New encryption-related error variants: `InvalidKeyLength`, `WrongEncryptionKey`, `UnencryptedDatabaseWithEncryption`, `KeyGeneration`, `FilePermission`, `Keyring`, `KeyringNotInitialized`, `KeyringEntryMissingForExistingDatabase` ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- File permission hardening on Unix: database directories (0700) and files (0600) are created with owner-only access ([#102](https://github.com/marmot-protocol/mdk/pull/102))
- Implemented pagination support using `Pagination` struct for group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Implemented pagination support using `Pagination` struct for pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))

### Fixed

- **Snapshot/rollback now correctly captures OpenMLS data**: The snapshot and restore code used `group_id.as_slice()` (raw bytes) to query OpenMLS tables, but the `StorageProvider` writes group_id via `MlsCodec::serialize()` (serialized bytes). The WHERE clauses never matched, so snapshots silently captured zero OpenMLS rows and rollbacks left MLS cryptographic state untouched — creating a metadata/crypto split-brain where MDK metadata was rolled back but the MLS engine retained the wrong epoch's keys. ([#179](https://github.com/marmot-protocol/mdk/pull/179))
- **Security (Audit Issue M)**: Fixed messages being overwritten across groups due to non-scoped primary key. Changed messages table primary key from `id` to `(mls_group_id, id)` and updated `save_message()` to use `INSERT ... ON CONFLICT(mls_group_id, id) DO UPDATE` instead of `INSERT OR REPLACE`. This prevents an attacker or faulty relay from causing message loss and misattribution across groups by reusing deterministic rumor IDs. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- **Security (Audit Issue Y)**: Secret values stored in SQLite are now wrapped in `Secret<T>` type, ensuring automatic memory zeroization and preventing sensitive cryptographic material from persisting in memory ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- **Security (Audit Issue Z)**: Added pagination to prevent memory exhaustion from unbounded loading of group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- **Security (Audit Issue AO)**: Removed MLS group identifiers from error messages to prevent metadata leakage in logs and telemetry. Error messages now use generic "Group not found" instead of including the sensitive 32-byte MLS group ID. ([#112](https://github.com/marmot-protocol/mdk/pull/112))
- **Security (Audit Issue AA)**: Added pagination to prevent memory exhaustion from unbounded loading of pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue AB)**: Added size limits to prevent disk and CPU exhaustion from unbounded user input ([#94](https://github.com/marmot-protocol/mdk/pull/94))
- **Security (Audit Issue AG)**: `all_groups` now skips corrupted rows instead of failing on the first deserialization error, improving availability when database contains malformed data ([#115](https://github.com/marmot-protocol/mdk/pull/115))
- Propagate `last_message_id` parse errors in `row_to_group` instead of silently converting to `None` ([#105](https://github.com/marmot-protocol/mdk/pull/105))
- Changed `tokio::sync::Mutex` to `std::sync::Mutex` for SQLite connection to avoid panics when called from within tokio async runtime contexts ([#164](https://github.com/marmot-protocol/mdk/pull/164))

### Removed

- Removed `openmls_sqlite_storage` dependency in favor of direct `StorageProvider<1>` implementation ([#148](https://github.com/marmot-protocol/mdk/pull/148))
- Removed legacy migrations V100-V105 in favor of unified V001 schema ([#148](https://github.com/marmot-protocol/mdk/pull/148))

## [0.5.1] - 2025-10-01

### Changed

- Update MSRV to 1.90.0 (required by openmls 0.7.1)
- Update openmls to 0.7.1

## [0.5.0] - 2025-09-10

**Note**: This is the first release as an independent library. Previously, this code was part of the `rust-nostr` project.

### Breaking changes

- Library split from rust-nostr into independent MDK (Marmot Development Kit) project
- Remove group type from groups
- Replaced `save_group_relay` with `replace_group_relays` trait method ([#1056](https://github.com/rust-nostr/nostr/pull/1056))
- `image_hash` instead of `image_url` ([#1059](https://github.com/rust-nostr/nostr/pull/1059))

### Changed

- Upgrade openmls to v0.7.0

## [0.43.0] - 2025-07-28

No notable changes in this release.

## [0.42.0] - 2025-05-20

First release ([#842](https://github.com/rust-nostr/nostr/pull/842))
