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
pub mod rumor_mls;

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
pub use rumor_mls::{process_rumor_with_mls, parse_mls_imeta_attachments};
