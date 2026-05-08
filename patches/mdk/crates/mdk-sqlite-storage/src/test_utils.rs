//! Shared test utilities for mdk-sqlite-storage tests.

use std::sync::OnceLock;

/// Ensures the mock keyring store is initialized exactly once for all tests.
///
/// `keyring_core::set_default_store` can only be called once per process,
/// so we use `OnceLock` to ensure it's only initialized on the first call.
pub fn ensure_mock_store() {
    static MOCK_STORE_INIT: OnceLock<()> = OnceLock::new();
    MOCK_STORE_INIT.get_or_init(|| {
        // Initialize the mock store for testing
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}
