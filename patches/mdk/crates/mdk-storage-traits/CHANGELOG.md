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

### Changed

- **Integration tests moved to `mdk-core`**: The storage trait compliance tests (memory, SQLite, and cross-storage differential tests) have been moved to the `mdk-core` crate. This removes `mdk-memory-storage` and `mdk-sqlite-storage` as dev-dependencies, which previously prevented `cargo publish` from packaging this crate before the other two were on crates.io. ([#202](https://github.com/marmot-protocol/mdk/pull/202))

### Breaking changes

- **Self-update tracking via `SelfUpdateState` enum**: Replaced the `needs_self_update: bool` and `last_self_update_at: Option<Timestamp>` fields on `Group` with a single `self_update_state: SelfUpdateState` field. The new `SelfUpdateState` enum has two variants: `Required` (post-join obligation per MIP-02) and `CompletedAt(Timestamp)` (last rotation time for MIP-00 staleness checks). Group creators start with `CompletedAt(now)` since creating a group with a fresh key is effectively the first rotation. All code that constructs `Group` structs must be updated. ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **MLS codec switched from JSON to postcard**: `JsonCodec` has been removed and replaced with `MlsCodec`, which uses the `postcard` binary serialization format instead of `serde_json`. MLS storage keys and entities are now significantly more compact (~33 bytes for a 32-byte group_id vs ~130 bytes with JSON). All persisted MLS data is incompatible and must be recreated. ([#179](https://github.com/marmot-protocol/mdk/pull/179))
- **Group `last_message_processed_at` Field**: Added `last_message_processed_at: Option<Timestamp>` field to the `Group` struct to track when the last message was processed/received by this client. This enables consistent ordering between `group.last_message_id` and `get_messages()[0].id` by matching the `messages()` query sort order (`created_at DESC, processed_at DESC, id DESC`). This is a breaking change - all code that constructs `Group` structs must now provide this new field. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Message `processed_at` Field**: Added `processed_at: Timestamp` field to the `Message` struct to track when a message was processed/received by the client. This is distinct from `created_at`, which reflects the sender's timestamp and may differ due to clock skew between devices. This is a breaking change - all code that constructs `Message` structs must now provide this new field. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Retryable Message State**: Added `ProcessedMessageState::Retryable` variant and `MessageStorage::mark_processed_message_retryable()` method to support message retries after rollback or temporary failures. This is a breaking change because `ProcessedMessageState` is not marked `#[non_exhaustive]`, so downstream users must update exhaustive match statements to handle the new `Retryable` variant, and storage trait implementations must implement the new `mark_processed_message_retryable()` method. ([#161](https://github.com/marmot-protocol/mdk/pull/161))
- **Snapshot API**: Added group-scoped snapshot management methods to `MdkStorageProvider` trait to support MIP-03 commit race resolution. Implementations must now provide: ([#152](https://github.com/marmot-protocol/mdk/pull/152))
  - `create_group_snapshot(group_id, name)`: Create a named snapshot of a group's current state
  - `rollback_group_to_snapshot(group_id, name)`: Roll back a group's state to a named snapshot
  - `release_group_snapshot(group_id, name)`: Release/delete a group's named snapshot
- **Unified Storage Architecture**: The `MdkStorageProvider` trait now requires implementors to directly implement OpenMLS's `StorageProvider<1>` trait, enabling atomic transactions across MLS and MDK state. This is required for proper commit race resolution per MIP-03. ([#148](https://github.com/marmot-protocol/mdk/pull/148))
  - Removed `OpenMlsStorageProvider` associated type
  - Removed `openmls_storage()` and `openmls_storage_mut()` accessor methods
  - Storage implementations must now implement all required `StorageProvider<1>` methods directly
- **Security (Audit Issue M)**: Changed `MessageStorage::find_message_by_event_id()` to require both `mls_group_id` and `event_id` parameters. This prevents messages from different groups from overwriting each other by scoping lookups to a specific group. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- **Secret Type Wrapper**: Secret values now use `Secret<T>` wrapper for automatic zeroization ([#109](https://github.com/marmot-protocol/mdk/pull/109))
  - `Group.image_key` changed from `Option<[u8; 32]>` to `Option<Secret<[u8; 32]>>`
  - `Group.image_nonce` changed from `Option<[u8; 12]>` to `Option<Secret<[u8; 12]>>`
  - `GroupExporterSecret.secret` changed from `[u8; 32]` to `Secret<[u8; 32]>`
  - `Welcome.group_image_key` changed from `Option<[u8; 32]>` to `Option<Secret<[u8; 32]>>`
  - `Welcome.group_image_nonce` changed from `Option<[u8; 12]>` to `Option<Secret<[u8; 12]>>`
  - Code accessing these fields must use `Secret::new()` to wrap values or dereference/clone to access inner values ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- Changed `WelcomeStorage::pending_welcomes()` to accept `Option<Pagination>` parameter instead of having separate `pending_welcomes()` and `pending_welcomes_paginated()` methods ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- Removed `MAX_PENDING_WELCOMES_OFFSET` constant - offset validation removed to allow legitimate large-scale use cases ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- Changed `GroupStorage::messages()` to accept `Option<Pagination>` parameter instead of having separate `messages()` and `messages_paginated()` methods ([#111](https://github.com/marmot-protocol/mdk/pull/111))

### Changed

- **OpenMLS 0.8.0 Upgrade**: Updated `openmls` to 0.8.0 and `openmls_traits` to 0.5. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Message Sorting**: The `GroupStorage::messages()` method now sorts messages by `created_at DESC, processed_at DESC, id DESC` (instead of just `created_at DESC`). The secondary sort by `processed_at` keeps messages in reception order when `created_at` is the same (avoids visual reordering). The tertiary sort by `id` ensures deterministic ordering when both timestamps are equal. ([#166](https://github.com/marmot-protocol/mdk/pull/166))

### Added

- **`groups_needing_self_update()` default method**: Added a default implementation on `GroupStorage` that returns active groups needing a self-update based on `SelfUpdateState::Required` or `SelfUpdateState::CompletedAt` staleness exceeding a configurable threshold. ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **Custom Message Sort Order**: Added `MessageSortOrder` enum with `CreatedAtFirst` (default) and `ProcessedAtFirst` variants to allow clients to choose how messages are ordered. Added `sort_order` field to `Pagination` struct and `Pagination::with_sort_order()` constructor. Added `Message::processed_at_order_cmp()` and `Message::compare_processed_at_keys()` comparison methods for the processed-at-first ordering. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Last Message by Sort Order**: Added `GroupStorage::last_message()` method to query the most recent message in a group according to a given sort order. This allows clients using `ProcessedAtFirst` ordering to get a "last message" consistent with their `messages()` call, independent of the cached `Group::last_message_id` (which always reflects `CreatedAtFirst`). ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Epoch Lookup by Tag Content**: Added `find_message_epoch_by_tag_content` method to `MessageStorage` trait for looking up a message's epoch by searching serialized tag content. Used for MIP-04 media decryption epoch hint resolution. ([#167](https://github.com/marmot-protocol/mdk/pull/167))
- **MdkStorageError**: New error type for OpenMLS `StorageProvider` trait implementations, with variants for database, serialization, deserialization, not found, and other errors. ([#148](https://github.com/marmot-protocol/mdk/pull/148))
- **Secret Type and Zeroization**: Added `Secret<T>` wrapper type that automatically zeroizes memory on drop ([#109](https://github.com/marmot-protocol/mdk/pull/109))
  - Implements `Zeroize` trait for `[u8; 32]`, `[u8; 12]`, and `Vec<u8>`
  - Provides `Deref` and `DerefMut` for transparent access to wrapped values
  - Includes serde serialization support
  - Debug formatting hides secret values to prevent leaks
  - Comprehensive test suite including memory zeroization verification ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- Added `Pagination` struct with `limit` and `offset` fields for cleaner pagination API - now part of public API for external consumers ([#110](https://github.com/marmot-protocol/mdk/pull/110), [#111](https://github.com/marmot-protocol/mdk/pull/111))
- Added `DEFAULT_MESSAGE_LIMIT` (1000) and `MAX_MESSAGE_LIMIT` (10,000) constants for pagination validation ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Added `DEFAULT_PENDING_WELCOMES_LIMIT` (1000) and `MAX_PENDING_WELCOMES_LIMIT` (10,000) constants for pagination validation ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- Add tests for `admins()`, `messages()`, and `group_relays()` error cases when group not found ([#104](https://github.com/marmot-protocol/mdk/pull/104))

### Fixed

- **Security (Audit Issue Y)**: Secret values (encryption keys, nonces, exporter secrets) are now automatically zeroized when dropped, preventing memory leaks of sensitive cryptographic material ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- **Security (Audit Issue Z)**: Added pagination to prevent memory exhaustion from unbounded loading of group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- **Security (Audit Issue AA)**: Added pagination to prevent memory exhaustion from unbounded loading of pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))

## [0.5.1] - 2025-10-01

### Changed

- Update MSRV to 1.90.0 (required by openmls 0.7.1)
- Update openmls to 0.7.1

## [0.5.0] - 2025-09-10

**Note**: This is the first release as an independent library. Previously, this code was part of the `rust-nostr` project.

### Breaking changes

- Library split from rust-nostr into independent MDK (Marmot Development Kit) project
- Wrapped `GroupId` type to avoid leaking OpenMLS types
- Remove group type from groups
- Remove `save_group_relay` method ([#1056](https://github.com/rust-nostr/nostr/pull/1056))
- `image_hash` instead of `image_url` ([#1059](https://github.com/rust-nostr/nostr/pull/1059))

### Changed

- Upgrade openmls to v0.7.0

### Added

- Added `replace_group_relays` to make relay replace for groups an atomic operation ([#1056](https://github.com/rust-nostr/nostr/pull/1056))
- Comprehensive consistency testing framework for testing all mdk-storage-traits implementations for correctness and consistency ([#1056](https://github.com/rust-nostr/nostr/pull/1056))
- Added Serde support for GroupId

## [0.43.0] - 2025-07-28

No notable changes in this release.

## [0.42.0] - 2025-05-20

First release ([#836](https://github.com/rust-nostr/nostr/pull/836))
