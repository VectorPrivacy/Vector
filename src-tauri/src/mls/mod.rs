//! MLS (Message Layer Security) Module
//!
//! Re-exports vector-core's MLS implementation. Only the Tauri-specific
//! `MlsServiceTauri` extension trait (AppHandle-based constructor) lives here.

use tauri::{AppHandle, Runtime};

// Re-export from vector-core — only what src-tauri callers actually use
pub use vector_core::mls::{
    MlsGroup, MlsGroupProfile, MlsGroupFull,
    MlsError,
    MlsService,
    send_mls_message, emit_group_metadata_event,
    metadata_to_frontend,
};

/// Backwards-compatible alias for the old monolithic struct.
#[allow(dead_code)]
pub type MlsGroupMetadata = MlsGroupFull;

// ============================================================================
// Tauri-specific: AppHandle-based constructor
// ============================================================================

#[allow(dead_code)]
pub trait MlsServiceTauri {
    fn new_persistent<R: Runtime>(handle: &AppHandle<R>) -> Result<MlsService, MlsError>;
}

impl MlsServiceTauri for MlsService {
    fn new_persistent<R: Runtime>(handle: &AppHandle<R>) -> Result<MlsService, MlsError> {
        let npub = crate::account_manager::get_current_account()
            .map_err(|e| MlsError::StorageError(format!("No account selected: {}", e)))?;

        let mls_dir = crate::account_manager::get_mls_directory(handle, &npub)
            .map_err(|e| MlsError::StorageError(format!("Failed to get MLS directory: {}", e)))?;

        MlsService::init_at_path(mls_dir)
    }
}
