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

- **Self-update tracking field change**: The `Group` record's `needs_self_update: bool` and `last_self_update_at: Option<u64>` fields have been replaced with a single `self_update_state: String` field. Values are `"required"` (post-join obligation) or `"completed_at:<unix_timestamp>"` (last rotation time). ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **`KeyPackageResult` now includes `hash_ref`**: The `KeyPackageResult` struct returned by `create_key_package_for_event` and `create_key_package_for_event_with_options` now includes a `hash_ref: Vec<u8>` field containing the serialized hash reference of the key package. This enables callers to track key packages for lifecycle management without re-parsing. ([#178](https://github.com/marmot-protocol/mdk/pull/178))
- **`create_key_package_for_event` No Longer Adds Protected Tag**: The `create_key_package_for_event()` function no longer adds the NIP-70 protected tag by default. This is a behavioral change - existing code that relied on the protected tag being present will now produce key packages without it. Key packages can now be republished by third parties to any relay. For users who need the protected tag, use the new `create_key_package_for_event_with_options()` function with `protected: true`. ([#173](https://github.com/marmot-protocol/mdk/pull/173), related: [#168](https://github.com/marmot-protocol/mdk/issues/168))
- **Security (Audit Issue M)**: Changed `get_message()` to require both `mls_group_id` and `event_id` parameters. This prevents messages from different groups from overwriting each other by scoping lookups to a specific group. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- Renamed `Message.processed_at` to `Message.created_at` for semantic accuracy. The field represents when a message was created, not when it was processed by the system. ([`#163`](https://github.com/marmot-protocol/mdk/pull/163))

### Changed

- Upgraded `nostr` dependency from 0.43 to 0.44, replacing deprecated `Timestamp::as_u64()` calls with `Timestamp::as_secs()` ([#162](https://github.com/marmot-protocol/mdk/pull/162))
- Changed `get_messages()` to accept optional `limit` and `offset` parameters for pagination control. Existing calls must be updated to pass `None, None` for default behavior (limit: 1000, offset: 0), or specify values for custom pagination. ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Changed `get_pending_welcomes()` to accept optional `limit` and `offset` parameters for pagination control. Existing calls must be updated to pass `None, None` for default behavior (limit: 1000, offset: 0), or specify values for custom pagination. ([#119](https://github.com/marmot-protocol/mdk/pull/119))
- Changed `new_mdk()`, `new_mdk_with_key()`, and `new_mdk_unencrypted()` to accept an optional `MdkConfig` parameter for customizing MDK behavior. Existing calls must be updated to pass `None` for default behavior. ([`#155`](https://github.com/marmot-protocol/mdk/pull/155))

### Added

- **`clear_pending_commit` method**: Added `clear_pending_commit(group_id)` to allow callers to roll back an uncommitted pending MLS commit. ([#196](https://github.com/marmot-protocol/mdk/pull/196))
- **`groups_needing_self_update()` method**: Returns hex-encoded group IDs of active groups that need a self-update (post-join obligation or stale rotation), given a threshold in seconds. ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **Custom Message Sort Order**: `get_messages()` now accepts an optional `sort_order` parameter (`"created_at_first"` or `"processed_at_first"`) to control message ordering. Defaults to `"created_at_first"` when omitted. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **Last Message by Sort Order**: Added `get_last_message(mls_group_id, sort_order)` method to retrieve the most recent message under a given sort order, so clients using `"processed_at_first"` can get a "last message" consistent with their `get_messages()` ordering. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **`create_key_package_for_event_with_options`**: New function that allows specifying whether to include the NIP-70 protected tag. Use this if you need to publish to relays that accept protected events. ([#173](https://github.com/marmot-protocol/mdk/pull/173), related: [#168](https://github.com/marmot-protocol/mdk/issues/168))
- **Group `last_message_processed_at` Field**: The `Group` record now includes an optional `last_message_processed_at: u64` field (Unix timestamp) indicating when the last message was received/processed by this client. This complements `last_message_at` (sender's timestamp) and ensures `last_message_id` is consistent with the first message returned by `get_messages()`. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Message `processed_at` Field**: The `Message` record now includes a `processed_at: u64` field (Unix timestamp) indicating when this client received/processed the message. This complements the existing `created_at` field (sender's timestamp) and helps clients handle clock skew between devices - messages can now be displayed in reception order if desired. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **`PreviouslyFailed` Result Variant**: Added `ProcessMessageResult.PreviouslyFailed` enum variant to handle cases where a previously failed message arrives again but the MLS group ID cannot be extracted. This prevents crashes in client applications (fixes [#153](https://github.com/marmot-protocol/mdk/issues/153)) by returning a result instead of throwing an exception. ([#165](https://github.com/marmot-protocol/mdk/pull/165), fixes [#154](https://github.com/marmot-protocol/mdk/issues/154), [#159](https://github.com/marmot-protocol/mdk/issues/159))
- Added `MdkConfig` record for configuring MDK behavior, including `out_of_order_tolerance` and `maximum_forward_distance` settings for MLS sender ratchet configuration. All fields are optional and default to sensible values. ([`#155`](https://github.com/marmot-protocol/mdk/pull/155))
- Exposed pagination control for `get_messages()` to foreign language bindings via optional `limit` and `offset` parameters. ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Exposed pagination control for `get_pending_welcomes()` to foreign language bindings via optional `limit` and `offset` parameters. ([#119](https://github.com/marmot-protocol/mdk/pull/119))

### Fixed

- **Security**: Secret values in bindings now use `Secret<T>` wrapper for automatic memory zeroization, preventing sensitive cryptographic material from persisting in memory ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- **Build**: Improved Android cross-compilation by requiring `ANDROID_OPENSSL_DIR` environment variable pointing to prebuilt OpenSSL libraries, with clear error messages explaining the required directory structure ([#140](https://github.com/marmot-protocol/mdk/pull/140))
- **Build**: Added `RANLIB` configuration for Android NDK toolchain to fix OpenSSL library installation ([#140](https://github.com/marmot-protocol/mdk/pull/140))
- **Build**: Added Rust target installation checks for both Android and iOS builds with helpful error messages showing how to install missing targets ([#140](https://github.com/marmot-protocol/mdk/pull/140))
- **Build**: Fixed Windows CI builds for Python and Ruby bindings by installing OpenSSL via vcpkg, resolving `libsqlite3-sys` build failures caused by missing `OPENSSL_DIR` ([#144](https://github.com/marmot-protocol/mdk/pull/144))
- **Build**: Fixed Windows linker errors for Python and Ruby bindings by adding missing `crypt32` and `user32` system library links required by statically-linked OpenSSL ([#172](https://github.com/marmot-protocol/mdk/pull/172))

## [0.5.3] - 2025-12-09

First bindings release ([commit](https://github.com/marmot-protocol/mdk/commit/8d05c9b499564277bdd1d1fe27fcc702eadf4d54))
