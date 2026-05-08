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

### Deprecated -->

## Unreleased

### Breaking changes

### Changed

### Added

### Fixed

### Removed

### Deprecated

## [0.6.0] - 2026-02-18

### Changed

- **Storage trait compliance tests moved here from `mdk-storage-traits`**: The memory, SQLite, and cross-storage differential integration tests now live in `mdk-core/tests/`, which already had both storage crates as dev-dependencies. ([#202](https://github.com/marmot-protocol/mdk/pull/202))

### Breaking changes

- **`admin_pubkeys` wire format change**: The `admin_pubkeys` field in `TlsNostrGroupDataExtension` changed from `Vec<Vec<u8>>` (hex-encoded strings) to `Vec<[u8; 32]>` (raw 32-byte x-only public keys), aligning with MIP-01 v2. Extensions serialized with the old hex-encoded format are incompatible. ([#185](https://github.com/marmot-protocol/mdk/pull/185))
- **Required `i` tag on KeyPackage events**: KeyPackage events (kind 443) now include a required `i` tag containing the hex-encoded `KeyPackageRef` (per updated MIP-00 spec). `parse_key_package()` now requires the `i` tag to be present and validates that its value matches the computed `KeyPackageRef` from the event content. Events without the `i` tag will be rejected. The tag vector returned by `create_key_package_for_event` now contains one additional tag, shifting indices of subsequent tags. ([#182](https://github.com/marmot-protocol/mdk/pull/182))
- **`hash_ref_bytes` format change**: The `hash_ref_bytes` returned by `create_key_package_for_event` and consumed by `delete_key_package_from_storage_by_hash_ref` are now postcard-encoded instead of JSON-encoded. Previously stored hash_ref bytes are incompatible and cannot be deserialized. ([#179](https://github.com/marmot-protocol/mdk/pull/179))
- **`create_key_package_for_event` Return Type Change**: The return type changed from `(String, Vec<Tag>)` to `(String, Vec<Tag>, Vec<u8>)`. The third element is the serialized hash_ref of the key package, computed atomically during creation. This enables callers to track key packages for lifecycle management (publish → consume → cleanup) without needing to re-parse the package. Callers that don't need the hash_ref can destructure with `_`. ([#178](https://github.com/marmot-protocol/mdk/pull/178))
- **`create_key_package_for_event` Return Type Change**: The return type changed from `(String, [Tag; 7])` to `(String, Vec<Tag>)`. Most code patterns (iteration, indexing) continue to work unchanged. This change was necessary because the protected tag is now optional. ([#173](https://github.com/marmot-protocol/mdk/pull/173), related: [#168](https://github.com/marmot-protocol/mdk/issues/168))
- **`create_key_package_for_event` No Longer Adds Protected Tag**: The `create_key_package_for_event()` function no longer adds the NIP-70 protected tag (`["-"]`) by default. This is a behavioral change - existing code that relied on the protected tag being present will now produce key packages without it. Key packages can now be republished by third parties to any relay. This improves relay compatibility since many popular relays (Damus, Primal, nos.lol) reject protected events outright. For users who need the protected tag, use the new `create_key_package_for_event_with_options()` function with `protected: true`. ([#173](https://github.com/marmot-protocol/mdk/pull/173), related: [#168](https://github.com/marmot-protocol/mdk/issues/168))
- **OpenMLS 0.8.0 Upgrade**: Upgraded from a git-pinned openmls 0.7.1 to the crates.io openmls 0.8.0 release. This resolves security advisory [GHSA-8x3w-qj7j-gqhf](https://github.com/openmls/openmls/security/advisories/GHSA-8x3w-qj7j-gqhf) (improper tag validation) and moves GREASE support from a git pin to an official release. Companion crates updated: `openmls_traits` 0.5, `openmls_basic_credential` 0.5, `openmls_rust_crypto` 0.5. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Unified Storage Architecture**: `MdkProvider` now uses the storage provider directly as the OpenMLS `StorageProvider`, instead of accessing it via `openmls_storage()`. This enables atomic transactions across MLS and MDK state for proper commit race resolution per MIP-03. Storage implementations must now directly implement `StorageProvider<1>`. ([#148](https://github.com/marmot-protocol/mdk/pull/148))
- **Legacy Format Removal**: Removed support for legacy key package tag formats and extension formats that were deprecated after EOY 2025 migration period ([#146](https://github.com/marmot-protocol/mdk/pull/146))
  - Key package validation now only accepts MIP-00 compliant formats:
    - `mls_ciphersuite` tag must use hex format (e.g., `0x0001`), numeric (`1`) and string (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`) formats are no longer accepted
    - `mls_extensions` tag must use hex format (e.g., `0x000a`, `0xf2ee`), legacy string names (`RequiredCapabilities`, `LastResort`, etc.) and comma-separated strings are no longer accepted
    - Tag names must use `mls_ciphersuite` and `mls_extensions` prefixes; legacy `ciphersuite` and `extensions` tag names are no longer accepted
  - Removed `LegacyTlsNostrGroupDataExtension` struct and related migration code for pre-version extension format
  - Groups created before the version field was added to extensions are no longer supported
- **Security (Audit Issue M)**: Changed `get_message()` to require both `mls_group_id` and `event_id` parameters. This prevents messages from different groups from overwriting each other by scoping lookups to a specific group. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- **Credential Identity Encoding**: Removed support for legacy 64-byte UTF-8 hex-encoded credential identities ([#15](https://github.com/marmot-protocol/mdk/issues/15))
  - Credential identities must now be exactly 32 bytes (raw public key) per MIP-00
  - Key packages with 64-byte hex-encoded identities are no longer accepted
  - This completes the migration period that began in November 2024
- **Encrypted Media (MIP-04)**: The `derive_encryption_nonce()` function has been removed. All encrypted media must now include a random nonce in the IMETA tag (`n` field). Legacy media encrypted with deterministic nonces can no longer be decrypted. This is a breaking change to fix the security issue (Audit Issue U) where deterministic nonce derivation caused nonce reuse. ([#114](https://github.com/marmot-protocol/mdk/pull/114))
- **`get_messages()` Signature Change**: Changed `get_messages()` signature to accept `Option<Pagination>` parameter. Callers must now pass `None` for default pagination or `Some(Pagination::new(...))` for custom pagination ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- **`get_pending_welcomes()` Signature Change**: Changed `get_pending_welcomes()` to accept `Option<Pagination>` parameter for pagination support. Existing calls should pass `None` for default pagination. ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Error Variant Rename**: Replaced `Error::MissingWelcomeForProcessedWelcome` with `Error::WelcomePreviouslyFailed(String)`. When retrying a welcome that previously failed, the new error includes the original failure reason instead of a generic message. ([#136](https://github.com/marmot-protocol/mdk/pull/136))
- **Content Encoding**: Removed support for hex encoding in key package and welcome event content ([#98](https://github.com/marmot-protocol/mdk/pull/98))
  - Key packages and welcome events now require explicit `["encoding", "base64"]` tag
  - Events without encoding tags or with hex encoding are rejected
  - This change addresses security concerns about encoding ambiguity and downgrade attacks
  - Older key packages published without encoding tags are no longer supported
  - Clients should republish key packages with proper encoding tags when upgrading
- **MIP-02 Welcome Event Validation**: Encoding tag is now required for all welcome events ([#96](https://github.com/marmot-protocol/mdk/pull/96))
  - Welcome events must now include exactly 4 tags: `relays`, `e`, `client`, and `encoding`
  - The `encoding` tag must have a value of either "hex" or "base64"
  - Relay URLs are now validated to ensure they start with `wss://` or `ws://`
  - Events missing the encoding tag or with invalid relay URLs will be rejected

### Changed

- **Welcome processing uses builder API**: Welcome message parsing now uses `StagedWelcome::build_from_welcome` with `replace_old_group()` to handle openmls 0.8.0's `GroupId` uniqueness enforcement. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Message Processing Timestamps**: Messages now record both `created_at` (from the rumor event, reflecting sender's clock) and `processed_at` (when this client processed the message). This allows clients to choose their preferred ordering strategy - by creation time or by reception time. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **MIP-03 Commit Race Resolution**: Commits are now resolved deterministically based on timestamp (earliest wins) and event ID (lexicographically smallest wins). ([#152](https://github.com/marmot-protocol/mdk/pull/152))
  - When multiple valid commits are published for the same epoch, clients converge on the same "winning" commit.
  - If a "better" commit (earlier timestamp) arrives after a "worse" commit has been applied, the client automatically rolls back to the previous epoch and applies the winning commit.
  - This ensures consistent group state across all clients even with out-of-order message delivery.
- Upgraded `nostr` dependency from 0.43 to 0.44, replacing deprecated `Timestamp::as_u64()` calls with `Timestamp::as_secs()` ([#162](https://github.com/marmot-protocol/mdk/pull/162))
- **OpenMLS Dependency**: Updated to OpenMLS git main branch (commit b90ca23b) for GREASE support. This may introduce minor API changes from upstream. The dependency will be reverted to crates.io versions once OpenMLS releases a version with GREASE support. ([#142](https://github.com/marmot-protocol/mdk/pull/142))
- `create_group()` now supports creating single-member groups (groups with only the creator). This enables "message to self" functionality, setting up groups before inviting members, and multi-device scenarios. When no members are provided, the method returns an empty `welcome_rumors` vec. ([#138](https://github.com/marmot-protocol/mdk/pull/138))

### Added

- **`clear_pending_commit` method**: Added `MDK::clear_pending_commit(group_id)` to allow callers to roll back an uncommitted pending MLS commit. This is essential for recovering from failed relay publishes — without it, a single failed publish permanently blocks all future group operations with "pending commit exists" errors. Wraps OpenMLS's `MlsGroup::clear_pending_commit` with MDK's group-loading and error handling. ([#192](https://github.com/marmot-protocol/mdk/pull/192))
- **Self-update tracking**: `accept_welcome()` now sets `self_update_state` to `SelfUpdateState::Required` on the joined group (MIP-02 post-join obligation). `merge_pending_commit()` detects pure self-update commits and transitions the state to `SelfUpdateState::CompletedAt(now)`, recording the rotation timestamp for MIP-00 periodic staleness checks. `create_group()` initializes the state to `SelfUpdateState::NotRequired` (creator has no immediate obligation). ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **`groups_needing_self_update()` method**: Returns group IDs of active groups that need a self-update, either because the state is `Required` or because the last rotation is older than a configurable threshold. ([#184](https://github.com/marmot-protocol/mdk/pull/184))
- **KeyPackageRef `i` tag for efficient relay queries**: KeyPackage events now include an `i` tag with the hex-encoded `KeyPackageRef` (computed per RFC 9420 Section 5.2). This enables efficient relay queries for specific KeyPackages when processing Welcome messages, avoiding the need to download and decode all KeyPackage events. ([#182](https://github.com/marmot-protocol/mdk/pull/182))
- **KeyPackage deletion by hash_ref bytes**: Added `delete_key_package_from_storage_by_hash_ref()` to delete a key package using previously serialized hash_ref bytes. This enables delayed key material cleanup workflows where the hash_ref is obtained at creation time (via `create_key_package_for_event`) and used for deletion later. ([#178](https://github.com/marmot-protocol/mdk/pull/178))
- **Custom Message Sort Order**: `get_messages()` now supports custom sort orders via the `Pagination::sort_order` field. Added `get_last_message(group_id, sort_order)` method to retrieve the most recent message under a given sort order, enabling clients using `ProcessedAtFirst` ordering to get a consistent "last message" value. ([#171](https://github.com/marmot-protocol/mdk/pull/171))
- **`create_key_package_for_event_with_options`**: New function that allows specifying whether to include the NIP-70 protected tag. Use this if you need to publish to relays that accept protected events. ([#173](https://github.com/marmot-protocol/mdk/pull/173), related: [#168](https://github.com/marmot-protocol/mdk/issues/168))
- **MIP-04 Epoch Fallback for Media Decryption**: `decrypt_from_download` now resolves the correct decryption key via an O(1) epoch hint lookup instead of only using the current epoch's exporter secret. Added `NoExporterSecretForEpoch` variant to `EncryptedMediaError` for programmatic error matching. ([#167](https://github.com/marmot-protocol/mdk/pull/167))
- **`PreviouslyFailed` Result Variant**: Added `MessageProcessingResult::PreviouslyFailed` variant to handle cases where a previously failed message arrives again but the MLS group ID cannot be extracted. This prevents crashes in client applications by returning a result instead of throwing an error. ([#165](https://github.com/marmot-protocol/mdk/pull/165), fixes [#154](https://github.com/marmot-protocol/mdk/issues/154), [#159](https://github.com/marmot-protocol/mdk/issues/159))
- **Message Retry Support**: Implemented better handling for retryable message states. When a message fails processing, it now preserves the `message_event_id` and other context. Added logic to allow reprocessing of messages marked as `Retryable`, with automatic state recovery to `Processed` upon success. ([#161](https://github.com/marmot-protocol/mdk/pull/161))
- Configurable `out_of_order_tolerance` and `maximum_forward_distance` in `MdkConfig` for MLS sender ratchet settings. Default `out_of_order_tolerance` increased from 5 to 100 for better handling of out-of-order message delivery on Nostr relays. ([`#155`](https://github.com/marmot-protocol/mdk/pull/155))
- **Epoch Snapshots & Rollback**: Added `EpochSnapshotManager` to maintain historical epoch states for rollback. ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- **Configuration**: Added `epoch_snapshot_retention` to `MdkConfig` (default: 5) to control how many past epochs are retained for rollback support. ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- **Rollback Callback**: Added `MdkCallback` trait and `MdkBuilder::with_callback()` to allow applications to react to rollback events (e.g., to refresh UI). ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- **GREASE Support (RFC 9420 Section 13.5)**: KeyPackage capabilities now automatically include random GREASE values for extensibility testing. GREASE ensures implementations correctly handle unknown values and maintains protocol forward compatibility. Values are injected into ciphersuites, extensions, proposals, and credentials capabilities. ([#142](https://github.com/marmot-protocol/mdk/pull/142))
- New `MessageProcessingResult::PendingProposal` variant returned when a non-admin member receives a proposal. The proposal is stored as pending and awaits commitment by an admin. ([#122](https://github.com/marmot-protocol/mdk/pull/122))
- New error variant `IdentityChangeNotAllowed` for rejecting proposals and commits that attempt to change member identity ([#126](https://github.com/marmot-protocol/mdk/pull/126))
- Added `nostr_group_id` field to `NostrGroupDataUpdate` struct, enabling rotation of the Nostr group ID used for message routing per MIP-01 ([#127](https://github.com/marmot-protocol/mdk/pull/127))
- New error variant `AuthorMismatch` for message author verification failures ([#40](https://github.com/marmot-protocol/mdk/pull/40))
- New error variant `KeyPackageIdentityMismatch` for KeyPackage credential identity validation failures ([#41](https://github.com/marmot-protocol/mdk/pull/41))
- New error variant `MissingRumorEventId` for when a rumor event is missing its ID ([#107](https://github.com/marmot-protocol/mdk/pull/107))
- New error variants for Nostr event validation: `InvalidTimestamp`, `MissingGroupIdTag`, `InvalidGroupIdFormat`, `MultipleGroupIdTags` ([#128](https://github.com/marmot-protocol/mdk/pull/128))
- Added `max_event_age_secs` and `max_future_skew_secs` fields to `MdkConfig` for configurable message timestamp validation. Default values are 45 days and 5 minutes respectively. ([#128](https://github.com/marmot-protocol/mdk/pull/128))
- Added pagination support to `get_messages()` public API - accepts `Option<Pagination>` to control limit and offset for message retrieval ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Exposed `Pagination` struct (from `mdk_storage_traits::groups`) in public API for paginated message queries ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- Added pagination support to `get_pending_welcomes()` public API - accepts `Option<Pagination>` to control limit and offset for welcome retrieval ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- Exposed `Pagination` struct (from `mdk_storage_traits::welcomes`) in public API for paginated welcome queries ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **MIP-02 Welcome Event Validation**: Added comprehensive validation for welcome events ([#96](https://github.com/marmot-protocol/mdk/pull/96))
  - Validates event kind is 444 (MlsWelcome)
  - Validates presence of all required tags (order-independent for interoperability)
  - Validates relay URL format using `RelayUrl::parse()` for thorough validation
  - Validates non-empty content for `e` and `client` tags
  - Validates encoding tag value is either "hex" or "base64"

### Fixed

- **`clear_pending_commit` orphaned keypair**: When `clear_pending_commit` rolls back a `self_update` pending commit, it now deletes the new `SignatureKeyPair` that was eagerly stored in the provider during `self_update`. Previously, repeated failed self-update publishes would accumulate unreachable private key material in storage. ([#197](https://github.com/marmot-protocol/mdk/pull/197))
- **Welcome validation no longer requires `client` tag**: The `validate_welcome_event` function now correctly treats the `client` tag as optional per MIP-02. Previously, welcome events without a `client` tag were rejected, which would cause spec-compliant third-party implementations to be unable to send Welcome events to MDK-based clients. ([#186](https://github.com/marmot-protocol/mdk/pull/186))
- **Security dependency updates**: Updated `time` (0.3.44 → 0.3.47), `bytes` (1.11.0 → 1.11.1), and `lru` (0.16.2 → 0.16.3) to resolve Dependabot security advisories. ([#174](https://github.com/marmot-protocol/mdk/pull/174))
- **Message Ordering Consistency**: Fixed inconsistency where `group.last_message_id` might not match `get_messages()[0].id` due to different sorting logic. The `last_message_id` update logic now uses `created_at DESC, processed_at DESC, id DESC` ordering to match the `messages()` query, ensuring the first message returned is always the same as `last_message_id`. Added `last_message_processed_at` field to `Group` to track this secondary sort key. ([#166](https://github.com/marmot-protocol/mdk/pull/166))
- **Security**: Prevent `GroupId` leakage in `test_commit_race_simple_better_commit_wins` assertion failure messages to avoid exposing sensitive identifiers in logs. ([#152](https://github.com/marmot-protocol/mdk/pull/152))
- Fixed crash when processing messages that previously failed. Now returns `MessageProcessingResult::Unprocessable` instead of throwing an error, consistent with other unprocessable message handling. This prevents application crashes when duplicate failed messages arrive from relays. (Fixes [#154](https://github.com/marmot-protocol/mdk/issues/154)) ([#156](https://github.com/marmot-protocol/mdk/pull/156))
- **Security (Audit Suggestion 5)**: Prevent panic in `process_welcome` when rumor event ID is missing. A malformed or non-NIP-59-compliant rumor now returns a `MissingRumorEventId` error instead of panicking. ([#107](https://github.com/marmot-protocol/mdk/pull/107))
- **Security (Audit Issue A)**: Added admin authorization check for MLS commit messages. Previously, commits were merged without verifying the sender against `admin_pubkeys`, allowing non-admin members to modify group state. Now, `process_commit_message_for_group` validates that the commit sender is an admin before merging. ([#130](https://github.com/marmot-protocol/mdk/pull/130))
- **Security (Audit Issue B)**: Added author verification to message processing to prevent impersonation attacks. The rumor pubkey is now validated against the MLS sender's credential before processing application messages. ([#40](https://github.com/marmot-protocol/mdk/pull/40))
- **Security (Audit Issue C)**: Added validation for admin updates to prevent invalid configurations. Admin updates now reject empty admin sets and non-member public keys. ([#42](https://github.com/marmot-protocol/mdk/pull/42))
- **Security (Audit Issue D)**: Added identity binding verification for KeyPackage events. The credential identity is now validated against the event signer to prevent impersonation attacks. ([#41](https://github.com/marmot-protocol/mdk/pull/41))
- **Security (Audit Issue G)**: Fixed admin authorization to read from current MLS group state instead of potentially stale stored metadata. The `is_leaf_node_admin` and `is_member_admin` functions now derive admin status from the `NostrGroupDataExtension` in the MLS group context, preventing a race window where a recently demoted admin could still perform privileged operations. ([#108](https://github.com/marmot-protocol/mdk/pull/108))
- **Security (Audit Issue H)**: Added MIP-02 validation to prevent malformed welcome events from causing storage pollution and resource exhaustion ([#96](https://github.com/marmot-protocol/mdk/pull/96))
- **Security (Audit Issue I)**: Fixed proposals being incorrectly restricted to admins. Per the Marmot protocol specification, any member can create proposals (only admins can commit). Non-admin members can now submit legitimate proposals such as self key updates or leave proposals. When an admin receives a proposal, it is auto-committed; when a non-admin receives one, it is stored as pending. Added new `MessageProcessingResult::PendingProposal` variant to indicate proposals stored but not committed. ([#122](https://github.com/marmot-protocol/mdk/pull/122))
- **Security (Audit Issue L)**: Added identity validation in proposal and commit processing. Proposals and commits that attempt to modify MLS credential identity fields are now rejected, as required by MIP-00. This prevents attackers from changing the binding between a member and their Nostr public key identity. ([#126](https://github.com/marmot-protocol/mdk/pull/126))
- **Security (Audit Issue M)**: Fixed messages being overwritten across groups due to non-scoped primary key. Updated `get_message()` to require `mls_group_id` parameter and updated internal storage lookups to be group-scoped. This prevents an attacker or faulty relay from causing message loss and misattribution across groups by reusing deterministic rumor IDs. ([#124](https://github.com/marmot-protocol/mdk/pull/124))
- **Security (Audit Issue N)**: Fixed `self_update` to not require a cached exporter secret. Previously, the function would abort with `GroupExporterSecretNotFound` when the current epoch's exporter secret was missing from storage, even though the secret was only used for debug logging. This blocked key rotation for new members or after cache loss, degrading post-compromise security. ([#121](https://github.com/marmot-protocol/mdk/pull/121))
- **Security (Audit Issue O)**: Missing Hash Verification in decrypt_group_image Allows Storage-Level Blob Substitution ([#97](https://github.com/marmot-protocol/mdk/pull/97))
- **Security (Audit Issue Q)**: Fixed `remove_members` to use actual leaf indices from the ratchet tree instead of enumeration indices. Previously, using `enumerate()` to derive `LeafNodeIndex` caused removal of incorrect members when the tree had holes from prior removals. Now uses `member.index` directly. ([#120](https://github.com/marmot-protocol/mdk/pull/120))
- **Security (Audit Issue R)**: Refactor encoding handling to enforce base64 usage for key packages and welcome ([#98](https://github.com/marmot-protocol/mdk/pull/98))
- **Security (Audit Issue S)**: Added validation for mandatory `relays` tag in MLS KeyPackage events. The `validate_key_package_tags` function now requires a `relays` tag with at least one valid relay URL, preventing acceptance of unroutable key packages that could cause delivery failures or enable denial-of-service attacks. ([#118](https://github.com/marmot-protocol/mdk/pull/118))
- **Security (Audit Issue T)**: Fixed incomplete MIME type canonicalization in `validate_mime_type` ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue U)**: Fixed deterministic nonce derivation that caused nonce reuse and message linkability. Encryption now uses random nonces per encryption operation, stored in the IMETA tag. The nonce field (`n`) is now required in IMETA tags. ([#114](https://github.com/marmot-protocol/mdk/pull/114))
- **Security (Audit Issue V)**: Replaced hard-coded MIP-04 version check with dynamic validation. Previously, the media manager explicitly checked for 'mip04-v2', which would require code changes to support future versions. Now, it validates against the supported versions defined in the crypto module, allowing for smoother protocol upgrades while still rejecting insecure legacy versions (v1). ([#145](https://github.com/marmot-protocol/mdk/pull/145))
- **Security (Audit Issue W)**: Added MIME type validation and allowlist enforcement ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue Y)**: Encrypted media keys and nonces now use `Secret<T>` wrapper for automatic memory zeroization, preventing sensitive cryptographic material from persisting in memory ([#109](https://github.com/marmot-protocol/mdk/pull/109))
- **Security (Audit Issue Z)**: Added pagination to prevent memory exhaustion from unbounded loading of group messages ([#111](https://github.com/marmot-protocol/mdk/pull/111))
- **Security (Audit Issue AA)**: Added pagination to prevent memory exhaustion from unbounded loading of pending welcomes ([#110](https://github.com/marmot-protocol/mdk/pull/110))
- **Security (Audit Issue AE)**: Added comprehensive Nostr-based validations when processing messages per MIP-03 requirements. The `validate_event_and_extract_group_id` function now validates timestamp bounds using `MdkConfig` settings (rejects events >5 minutes in future or >45 days old by default), and enforces exactly one `h` tag requirement with proper format validation. Note: MDK-core delegates Nostr signature verification to nostr-sdk's relay pool layer; it does not perform signature verification itself. This prevents misrouting messages via manipulated tags and degrading availability through abnormal timestamps. ([#128](https://github.com/marmot-protocol/mdk/pull/128))
- **Security (Audit Issue AK)**: Fixed removed member commit processing to handle eviction gracefully. When a member is removed from a group and processes their removal commit, the group state is now set to `Inactive` instead of failing with a `UseAfterEviction` error. (Fixes [#80](https://github.com/marmot-protocol/mdk/issues/80)) ([#137](https://github.com/marmot-protocol/mdk/pull/137))
- **Security (Audit Issue AP)**: Early validation and decryption failures now persist failed processing state to prevent DoS via repeated expensive reprocessing of invalid events. Added deduplication check to reject previously failed messages immediately. Failure reasons are sanitized to prevent information leakage. ([#116](https://github.com/marmot-protocol/mdk/pull/116))

### Removed

- **`compute_key_package_hash_ref` removed**: This method is no longer needed now that `create_key_package_for_event` returns the hash_ref directly as its third tuple element. Callers should use the hash_ref from `create_key_package_for_event` instead. ([#178](https://github.com/marmot-protocol/mdk/pull/178))
- Removed `Error::ProposalFromNonAdmin` variant as proposals are now accepted from any member per the Marmot protocol specification ([#122](https://github.com/marmot-protocol/mdk/pull/122))
- Removed all traces of hex encoding support for content fields in key packages and welcome events ([#98](https://github.com/marmot-protocol/mdk/pull/98))

## [0.5.3] - 2025-11-14

### Changed

- **Key Package Formatting**: Centralized hex formatting with new `NostrTagFormat` trait
  - All MLS types (Ciphersuite, ExtensionType) now use consistent lowercase hex format via `to_nostr_tag()` method
  - Eliminates duplicate formatting logic across the codebase
  - Normalized ciphersuite strings for consistency
- **Key Package Validation**: Enhanced validation with closure-based approach
  - More flexible and maintainable validation architecture
  - Added MLS protocol version validation to ensure version 1 compliance
  - Improved error messages for validation failures

### Added

- **Key Package Tag Validation**: Added comprehensive validation for MIP-00 key package tags
  - Validates `mls_ciphersuite` tag format and value (must be `0x0001`)
  - Validates `mls_extensions` tag hex format (0x prefix + 4 hex digits)
  - Validates presence of all required extensions
  - Case-insensitive validation for hex values (handles both uppercase and lowercase)
  - Maintains backward compatibility with legacy tag formats
  - Added validation for MLS protocol version (must be version 1)
- **Protected Tag Support**: Key packages now include a `protected` tag to mark events as protected
- **Client Tags**: Added support for client-specific tags in key packages
  - Enables clients to identify themselves and their capabilities
- **Examples**: Added comprehensive examples demonstrating key package and group functionality
  - `key_package_inspection`: Demonstrates key package creation, validation, and inspection
  - `group_inspection`: Shows group creation, member management, and message processing
  - Examples showcase best practices for using the MDK library

### Fixed

- Blurhash Generation: Fixed blurhash generation to use RGBA format instead of RGB (changed `to_rgb8()` to `to_rgba8()`)

## [0.5.2] - 2025-10-16

### Breaking changes

- **Message Processing Results**: Enhanced `MessageProcessingResult` and `UpdateGroupResult` to include group context
  - `UpdateGroupResult` now includes `mls_group_id: GroupId` field
  - `MessageProcessingResult` variants `ExternalJoinProposal`, `Commit`, and `Unprocessable` are now struct variants with `mls_group_id: GroupId` field
  - External code pattern matching on these variants must be updated to use struct syntax: `MessageProcessingResult::Commit { .. }` instead of `MessageProcessingResult::Commit`
  - The `Proposal` variant remains unchanged but now contains `UpdateGroupResult` with the new field

### Added

- **Extension Versioning (MIP-01)**: Added version field to `NostrGroupDataExtension`
  - New `version` field (current version: 1) for forward/backward compatibility
  - Constant `NostrGroupDataExtension::CURRENT_VERSION` for version management
  - Automatic migration from legacy format (without version field) to version 1
  - Forward compatibility support for future versions with warnings
  - New `LegacyTlsNostrGroupDataExtension` struct for backward compatibility
  - Comprehensive version field tests including roundtrip, validation, and migration scenarios
- **Comprehensive Event Structure Testing**: Added 17 new compliance tests for MIP-00, MIP-02, and MIP-03
  - 7 tests for Welcome events (MIP-02): structure validation, content validation, KeyPackage references, relay tags, processing flow, and consistency tests
  - 10 tests for Group Message events (MIP-03): structure validation, ephemeral key rotation, commit events, group ID consistency, NIP-44 encryption validation, and complete lifecycle integration tests
  - Tests validate critical security properties (ephemeral keys per message), interoperability (event structure compliance), and prevent regressions
- New error variant `ExtensionFormatError` for extension formatting issues
- New error variant `InvalidExtensionVersion` for unsupported extension versions

### Fixed

- **MIP-00 Compliance**: Fixed key package tag format to match specification
  - `mls_ciphersuite` tag now uses single hex value format: `["mls_ciphersuite", "0x0001"]` instead of string format
  - `mls_extensions` tag now uses multiple hex values: `["mls_extensions", "0x0003", "0x000a", "0x0002", "0xf2ee"]` instead of single comma-separated string
  - Ensures interoperability with other Marmot protocol implementations

## [0.5.1] - 2025-10-01

### Changed

- Update MSRV to 1.90.0 (required by openmls 0.7.1)
- Update openmls to 0.7.1
- Cleanup dependencies (remove unused `rand` crate, make `kamadak-exif` non-optional)

## [0.5.0] - 2025-09-10

**Note**: This is the first release as an independent library. Previously, this code was part of the `rust-nostr` project.

### Breaking changes

- Library split from rust-nostr into independent MDK (Marmot Development Kit) project
- Wrapped `GroupId` from OpenMLS to avoid leaking external types
- Removed aggressive re-exports, use types directly
- Removed public `Result` type
- Smaller prelude focusing on essential exports
- Remove group type from groups ([1deb718](https://github.com/rust-nostr/nostr/commit/1deb718cf0a70c110537b505bdbad881d43d15cf))
- Removed `MDK::update_group_name`, `MDK::update_group_description`, `MDK::update_group_image` in favor of a single method for updating all group data
- Added `admins` member to the `NostrGroupConfigData` ([#1050](https://github.com/rust-nostr/nostr/pull/1050))
- Changed method signature of `MDK::create_group`. Removed the admins param. Admins are specified in the `NostrGroupConfigData`. ([#1050](https://github.com/rust-nostr/nostr/pull/1050))

### Changed

- Upgrade openmls to v0.7.0 ([b0616f4](https://github.com/rust-nostr/nostr/commit/b0616f4dca544b4076678255062b1133510f2813))

### Added

- **MIP-04 Support**: Full encrypted media support with privacy-focused EXIF handling
  - EXIF metadata sanitization with allowlist-based approach
  - Blurhash generation for image placeholders
  - ChaCha20-Poly1305 AEAD encryption with proper AAD binding
  - SHA-256 file hashing for integrity verification
  - Comprehensive image format support (JPEG, PNG, WebP, GIF)
  - Image dimension validation and metadata extraction
- Group image encryption and management (MIP-01)
- GitHub CI workflow with comprehensive test matrix
- LLM context documentation and development guides
- Improved synchronization between MLSGroup and stored Group state on all commits ([#1050](https://github.com/rust-nostr/nostr/pull/1050))
- Added `MDK::update_group_data` method to handle updates of any of the fields of the `NostrGroupDataExtension` ([#1050](https://github.com/rust-nostr/nostr/pull/1050))
- Added Serde support for GroupId

### Fixed

- Bug where group relays weren't being persisted properly on change in NostrGroupDataExtension ([#1056](https://github.com/rust-nostr/nostr/pull/1056))

## [0.43.0] - 2025-07-28

### Breaking changes

- Changed return type of `MDK::add_members` and `MDK::self_update` ([#934](https://github.com/rust-nostr/nostr/pull/934))
- Changed return type of all group and message methods to return Events instead of serialized MLS objects. ([#940](https://github.com/rust-nostr/nostr/pull/940))
- Changed the input params of `MDK::create_group`, and additional fields for `NostrGroupDataExtension` ([#965](https://github.com/rust-nostr/nostr/pull/965))
- `NostrGroupDataExtension` requires additional `image_nonce` field ([#1054](https://github.com/rust-nostr/nostr/pull/1054))
- `image_hash` instead of `image_url` ([#1059](https://github.com/rust-nostr/nostr/pull/1059))

### Added

- Add `MDK::add_members` method for adding members to an existing group ([#931](https://github.com/rust-nostr/nostr/pull/931))
- Add `MDK::remove_members` method for removing members from an existing group ([#934](https://github.com/rust-nostr/nostr/pull/934))
- Add `MDK::leave_group` method for creating a proposal to leave the group ([#940](https://github.com/rust-nostr/nostr/pull/940))
- Add processing of commit messages and basic processing of proposals. ([#940](https://github.com/rust-nostr/nostr/pull/940))
- Add `ProcessedMessageState` for processed commits ([#954](https://github.com/rust-nostr/nostr/pull/954))
- Add method to check previous exporter_secrets when NIP-44 decrypting kind 445 messages ([#954](https://github.com/rust-nostr/nostr/pull/954))
- Add methods to update group name, description and image ([#978](https://github.com/rust-nostr/nostr/pull/978))

## [0.42.0] - 2025-05-20

First release ([#843](https://github.com/rust-nostr/nostr/pull/843))
