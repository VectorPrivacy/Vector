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

- **MLS codec switched from JSON to postcard**: MLS storage serialization now uses `MlsCodec` (postcard binary format) instead of the removed `JsonCodec` (serde_json). In-memory data is ephemeral so no migration is needed, but the serialized byte format has changed. ([#179](https://github.com/marmot-protocol/mdk/pull/179))

### Changed

- **OpenMLS 0.8.0 Upgrade**: Updated `openmls` to 0.8.0 and `openmls_traits` to 0.5. Updated `lru` to 0.16.3 to resolve a security advisory. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Message Sorting**: The `messages()` method now sorts by `created_at DESC, processed_at DESC, id DESC`. The secondary sort by `processed_at` keeps messages in reception order when `created_at` is the same. The tertiary sort by `id` ensures deterministic ordering. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Thread-Safe Snapshots**: Implemented atomic snapshot support using internal locking. `create_group_snapshot`, `rollback_group_to_snapshot`, and `release_group_snapshot` are now supported for testing and race resolution. ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- **Unified Storage Architecture**: `MdkMemoryStorage` now directly implements OpenMLS's `StorageProvider<1>` trait instead of wrapping `openmls_memory_storage`. This enables unified in-memory storage for both MLS and MDK state, consistent with the SQLite implementation. ([#148](https://github.com/marmot-protocol/mdk/pull/148))
  - Removed `openmls_memory_storage` dependency
  - All MLS state is now stored in unified in-memory data structures
  - Consistent API with `MdkSqliteStorage` for easier testing
- Updated `messages()` implementation to accept `Option<Pagination>` parameter ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Updated `pending_welcomes()` implementation to accept `Option<Pagination>` parameter ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Storage Security**: Updated to use `Secret<T>` wrapper for secret values from storage traits, ensuring automatic memory zeroization ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- Simplified validation logic to use range contains pattern for better readability ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Simplified validation logic to use range contains pattern for better readability ([#110](https://github.com/marmot-protocol/mdk/pull/110))

### Added

- **Custom Message Sort Order**: `messages()` now respects the `sort_order` field in `Pagination`, supporting both `CreatedAtFirst` (default) and `ProcessedAtFirst` orderings. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Last Message by Sort Order**: Implemented `last_message()` to return the most recent message under a given sort order. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Epoch Lookup by Tag Content**: Implemented `find_message_epoch_by_tag_content` for in-memory storage, scanning cached group messages and matching serialized tags. ([#167](https://github.com/marmot-protocol/mdk/pull/167))
- **Retryable Message Support**: Updated storage implementation to handle `ProcessedMessageState::Retryable` transitions and persistence. ([#161](https://github.com/marmot-protocol/mdk/pull/161))
- **MLS Storage Module**: New `mls_storage` module with complete `StorageProvider<1>` implementation for OpenMLS integration ([#148](https://github.com/marmot-protocol/mdk/pull/148))
  - Postcard codec (`MlsCodec`) for serializing/deserializing OpenMLS types ([#179](https://github.com/marmot-protocol/mdk/pull/179))
  - Support for all 53 `StorageProvider<1>` methods
  - In-memory storage using `HashMap` for all MLS data types
- **Snapshot Support**: New `snapshot` module for creating and restoring storage snapshots, useful for testing rollback scenarios ([#148](https://github.com/marmot-protocol/mdk/pull/148))
- Implemented pagination support using `Pagination` struct for group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Implemented pagination support using `Pagination` struct for pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue AM)**: Added input validation constants and enforcement to prevent memory exhaustion attacks. New public constants: `DEFAULT_MAX_RELAYS_PER_GROUP`, `DEFAULT_MAX_MESSAGES_PER_GROUP`, `DEFAULT_MAX_GROUP_NAME_LENGTH`, `DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH`, `DEFAULT_MAX_ADMINS_PER_GROUP`, `DEFAULT_MAX_RELAYS_PER_WELCOME`, `DEFAULT_MAX_ADMINS_PER_WELCOME`, `DEFAULT_MAX_RELAY_URL_LENGTH`. Fixes [#82](https://github.com/marmot-protocol/mdk/issues/82) ([#147](https://github.com/marmot-protocol/mdk/pull/147))
- Added `ValidationLimits` struct for configurable validation limits, allowing users to override default memory exhaustion protection limits via `MdkMemoryStorage::with_limits()` ([#147](https://github.com/marmot-protocol/mdk/pull/147))

### Fixed

- **Security (Audit Issue AC)**: Fixed `nostr_group_id` cache collision vulnerability that allowed lookup hijacking and stale key entries. The `save_group` function now rejects saves when `nostr_group_id` already maps to a different `mls_group_id`, and removes stale entries when a group's identifier changes. ([#149](https://github.com/marmot-protocol/mdk/pull/149))
- Fixed compilation errors in `mdk-memory-storage` implementation and tests ([#148](https://github.com/marmot-protocol/mdk/pull/148))
- **Security (Audit Issue 6/Suggestion 6)**: Improved `save_message` performance from O(n) to expected/amortized O(1) by replacing `Vec<Message>` with `HashMap<EventId, Message>` for the messages-by-group cache. This addresses potential DoS risk from high message counts per group (threat model T.10.2 and T.10.4). Fixes [#92](https://github.com/marmot-protocol/mdk/issues/92) ([#134](https://github.com/marmot-protocol/mdk/pull/134))
- **Security (Audit Issue M)**: Fixed messages being overwritten across groups by updating `find_message_by_event_id()` to use group-scoped cache lookups. This prevents an attacker or faulty relay from causing message loss and misattribution across groups by reusing deterministic rumor IDs. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- **Security (Audit Issue Y)**: Secret values stored in memory are now wrapped in `Secret<T>` type, ensuring automatic memory zeroization and preventing sensitive cryptographic material from persisting in memory ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- **Security (Audit Issue Z)**: Added pagination to prevent memory exhaustion from unbounded loading of group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- **Security (Audit Issue AA)**: Added pagination to prevent memory exhaustion from unbounded loading of pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue AN)**: Fixed security issue where `save_message` would accept messages for non-existent groups, allowing cache pollution. Now verifies group existence before inserting messages into the cache. ([#113](https://github.com/marmot-protocol/mdk/pull/113))
- **Security (Audit Issue AO)**: Removed MLS group identifiers from error messages to prevent metadata leakage in logs and telemetry. Error messages now use generic "Group not found" instead of including the sensitive 32-byte MLS group ID. ([#112](https://github.com/marmot-protocol/mdk/pull/112))
- **Security (Audit Issue AM)**: Added input validation to prevent memory exhaustion from unbounded per-key values in LRU caches. Validation is enforced in `save_group`, `replace_group_relays`, `save_message`, and `save_welcome` to cap string lengths, collection sizes, and per-group message counts. Fixes [#82](https://github.com/marmot-protocol/mdk/issues/82) ([#147](https://github.com/marmot-protocol/mdk/pull/147))
- Fix `admins()` to return `InvalidParameters` error when group not found, instead of incorrectly returning `NoAdmins` ([#104](https://github.com/marmot-protocol/mdk/pull/104))

### Removed

- Removed `openmls_memory_storage` dependency in favor of direct `StorageProvider<1>` implementation ([#148](https://github.com/marmot-protocol/mdk/pull/148))

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

### Changed

- Bump lru from 0.14 to 0.16

## [0.42.0] - 2025-05-20

- First release ([#839](https://github.com/rust-nostr/nostr/pull/839))
