//! MLS (Message Layer Security) group encryption.
//!
//! This module contains all MLS business logic, fully decoupled from Tauri:
//! - Group types (MlsGroup, MlsGroupProfile, MlsGroupFull)
//! - Error types (MlsError)
//! - MlsService — MDK engine management + group operations
//! - Event tracking and deduplication
//! - Desync detection
//! - Per-group sync locks

pub mod types;
pub mod tracking;
pub mod service;
pub mod messaging;
pub mod rumor_mls;
pub mod group_handler;
pub mod invites;
pub mod keypackage;

pub use types::{
    MlsGroup, MlsGroupProfile, MlsGroupFull,
    MlsError, EventCursor, KeyPackageIndexEntry,
    has_encoding_tag, metadata_to_frontend,
    record_group_failure, record_group_success,
};
pub use tracking::{
    is_mls_event_processed, track_mls_event_processed,
    wipe_legacy_mls_database,
};
pub use service::{
    MlsService, get_group_sync_lock, get_mls_directory,
    publish_event_with_retries, publish_and_merge_commit,
};
pub use messaging::{send_mls_message, emit_group_metadata_event};
pub use rumor_mls::{process_rumor_with_mls, parse_mls_imeta_attachments};
pub use group_handler::{handle_mls_group_message, handle_mls_group_message_with_handler};
pub use invites::{PendingInvite, list_invites, accept_invite, decline_invite};
pub use keypackage::{PublishedKeyPackage, publish_keypackage};
