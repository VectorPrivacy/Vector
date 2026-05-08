//! File permission hardening utilities.
//!
//! This module provides platform-specific utilities for setting secure file permissions
//! on database directories and files.
//!
//! ## Platform Support
//!
//! - **Unix (macOS, Linux, iOS, Android)**: Uses file mode permissions (`chmod 0600`/`0700`)
//!   to restrict access to owner-only.
//! - **Mobile (iOS/Android)**: The application sandbox provides the primary security
//!   boundary. File permissions are applied as defense-in-depth.

use std::fs::OpenOptions;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::Error;

/// Result of atomic file creation.
///
/// Used to communicate whether a file was newly created or already existed,
/// which is important for determining key management strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCreationOutcome {
    /// The file was created by this call.
    Created,
    /// The file already existed (not modified).
    AlreadyExisted,
    /// The path is a special SQLite path (e.g., `:memory:`) that doesn't need pre-creation.
    Skipped,
}

/// Creates a directory with secure permissions (owner-only access).
///
/// - **Unix**: Creates the directory with mode 0700 (owner read/write/execute only).
///
/// # Arguments
///
/// * `path` - Path to the directory to create
///
/// # Errors
///
/// Returns an error if the directory cannot be created or permissions cannot be set.
pub fn create_secure_directory<P>(path: P) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    // Create the directory (and parents if needed)
    std::fs::create_dir_all(path)?;

    // Apply platform-specific permissions
    #[cfg(unix)]
    set_unix_directory_permissions(path)?;

    Ok(())
}

/// Sets secure permissions on an existing file (owner-only access).
///
/// - **Unix**: Sets mode 0600 (owner read/write only).
/// - **Other platforms**: No-op (hosts should store databases in app-private locations).
///
/// # Arguments
///
/// * `path` - Path to the file
///
/// # Errors
///
/// Returns an error if permissions cannot be set.
pub fn set_secure_file_permissions<P>(path: P) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        return Ok(());
    }

    #[cfg(unix)]
    set_unix_file_permissions(path)?;

    Ok(())
}

/// Atomically pre-creates a database file with secure permissions before opening.
///
/// Uses `O_CREAT | O_EXCL` (via `create_new`) to atomically create the file only
/// if it doesn't already exist. This prevents TOCTOU race conditions where
/// multiple processes might try to create the same database simultaneously.
///
/// This avoids a short window where SQLite might create the file with default
/// (umask-dependent) permissions. We create an empty file with secure permissions
/// first, then SQLite will open the existing file.
///
/// # Arguments
///
/// * `path` - Path to the database file
///
/// # Returns
///
/// - `Ok(FileCreationOutcome::Created)` if the file was created by this call
/// - `Ok(FileCreationOutcome::AlreadyExisted)` if the file already existed
/// - `Ok(FileCreationOutcome::Skipped)` for special paths like `:memory:`
///
/// # Errors
///
/// Returns an error if the file cannot be created or permissions cannot be set.
///
/// # Special Cases
///
/// - In-memory databases (":memory:") are skipped
/// - Empty paths are skipped
pub fn precreate_secure_database_file<P>(path: P) -> Result<FileCreationOutcome, Error>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    // Skip special SQLite paths (in-memory databases, empty paths)
    let path_str = path.to_string_lossy();
    if path_str.is_empty() || path_str == ":memory:" || path_str.starts_with(':') {
        return Ok(FileCreationOutcome::Skipped);
    }

    // Ensure parent directory exists with secure permissions
    if let Some(parent) = path.parent() {
        // Skip if parent is empty (e.g., for paths like "file.db" with no directory)
        if !parent.as_os_str().is_empty() && !parent.exists() {
            create_secure_directory(parent)?;
        }
    }

    // Atomically create the file only if it doesn't exist.
    // This uses O_CREAT | O_EXCL on Unix, which is atomic.
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_file) => {
            // File was created by us - set secure permissions
            set_secure_file_permissions(path)?;
            Ok(FileCreationOutcome::Created)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // File already exists - another process created it first
            Ok(FileCreationOutcome::AlreadyExisted)
        }
        Err(e) => Err(e.into()),
    }
}

/// Sets Unix file permissions to 0600 (owner read/write only).
#[cfg(unix)]
fn set_unix_file_permissions(path: &Path) -> Result<(), Error> {
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|e| {
        Error::FilePermission(format!(
            "Failed to set file permissions on {:?}: {}",
            path, e
        ))
    })
}

/// Sets Unix directory permissions to 0700 (owner read/write/execute only).
#[cfg(unix)]
fn set_unix_directory_permissions(path: &Path) -> Result<(), Error> {
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, perms).map_err(|e| {
        Error::FilePermission(format!(
            "Failed to set directory permissions on {:?}: {}",
            path, e
        ))
    })
}

/// Verifies that a file or directory has appropriately restrictive permissions.
///
/// On Unix, this checks that the file/directory is not world-readable or group-readable.
/// Returns an error if permissions are too permissive.
///
/// # Arguments
///
/// * `path` - Path to check
///
/// # Errors
///
/// Returns an error if permissions are too permissive or if the check fails.
#[cfg(unix)]
#[must_use = "verify_permissions returns a Result that must be checked"]
pub fn verify_permissions<P>(path: P) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        return Ok(());
    }

    let metadata = std::fs::metadata(path)?;
    let mode = metadata.permissions().mode();

    // Check that no group or world permissions are set
    // (mode & 0o077) should be 0 for secure permissions
    if mode & 0o077 != 0 {
        return Err(Error::FilePermission(format!(
            "File {:?} has insecure permissions: {:o}. Expected owner-only access.",
            path,
            mode & 0o777
        )));
    }

    Ok(())
}

/// Verifies permissions (no-op on platforms without specific support).
#[cfg(not(unix))]
#[must_use = "verify_permissions returns a Result that must be checked"]
pub fn verify_permissions<P>(_path: P) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    // On non-Unix platforms, we rely on app sandboxing and host configuration.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_secure_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_dir = temp_dir.path().join("secure_dir");

        create_secure_directory(&test_dir).unwrap();
        assert!(test_dir.exists());
        assert!(test_dir.is_dir());

        #[cfg(unix)]
        {
            let perms = std::fs::metadata(&test_dir).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn test_set_secure_file_permissions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("secure_file.db");

        // Create file
        std::fs::File::create(&test_file).unwrap();

        set_secure_file_permissions(&test_file).unwrap();

        #[cfg(unix)]
        {
            let perms = std::fs::metadata(&test_file).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn test_precreate_secure_database_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("subdir").join("database.db");

        let outcome = precreate_secure_database_file(&db_path).unwrap();
        assert_eq!(outcome, FileCreationOutcome::Created);

        assert!(db_path.exists());
        assert!(db_path.parent().unwrap().exists());

        #[cfg(unix)]
        {
            let file_perms = std::fs::metadata(&db_path).unwrap().permissions();
            assert_eq!(file_perms.mode() & 0o777, 0o600);

            let dir_perms = std::fs::metadata(db_path.parent().unwrap())
                .unwrap()
                .permissions();
            assert_eq!(dir_perms.mode() & 0o777, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_permissions_secure() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("secure.db");

        std::fs::File::create(&test_file).unwrap();
        set_secure_file_permissions(&test_file).unwrap();

        // Should pass verification
        verify_permissions(&test_file).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_permissions_insecure() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("insecure.db");

        std::fs::File::create(&test_file).unwrap();

        // Set world-readable permissions
        let perms = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Should fail verification
        let result = verify_permissions(&test_file);
        assert!(result.is_err());
    }

    #[test]
    fn test_precreate_skips_memory_database() {
        // In-memory databases should be skipped without error
        let result = precreate_secure_database_file(":memory:");
        assert_eq!(result.unwrap(), FileCreationOutcome::Skipped);

        // Other special SQLite paths
        let result = precreate_secure_database_file(":temp:");
        assert_eq!(result.unwrap(), FileCreationOutcome::Skipped);

        // Empty path
        let result = precreate_secure_database_file("");
        assert_eq!(result.unwrap(), FileCreationOutcome::Skipped);
    }

    #[test]
    fn test_precreate_returns_already_existed_for_existing_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("existing.db");

        // Create a file first
        std::fs::write(&db_path, b"existing content").unwrap();

        // precreate should return AlreadyExisted and not overwrite
        let outcome = precreate_secure_database_file(&db_path).unwrap();
        assert_eq!(outcome, FileCreationOutcome::AlreadyExisted);

        // Verify content is unchanged
        let content = std::fs::read(&db_path).unwrap();
        assert_eq!(content, b"existing content");
    }

    #[test]
    fn test_set_secure_file_permissions_nonexistent() {
        // Setting permissions on a non-existent file should succeed (no-op)
        let result = set_secure_file_permissions("/nonexistent/path/file.db");
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_nested_secure_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nested_dir = temp_dir.path().join("a").join("b").join("c").join("d");

        create_secure_directory(&nested_dir).unwrap();
        assert!(nested_dir.exists());
        assert!(nested_dir.is_dir());

        #[cfg(unix)]
        {
            // All directories in the chain should exist
            assert!(temp_dir.path().join("a").exists());
            assert!(temp_dir.path().join("a").join("b").exists());
            assert!(temp_dir.path().join("a").join("b").join("c").exists());

            // The deepest directory should have secure permissions
            let perms = std::fs::metadata(&nested_dir).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_permissions_nonexistent() {
        // Verifying permissions on non-existent file should succeed
        let result = verify_permissions("/nonexistent/path/file.db");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_permissions_group_readable() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("group_readable.db");

        std::fs::File::create(&test_file).unwrap();

        // Set group-readable permissions
        let perms = std::fs::Permissions::from_mode(0o640);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Should fail verification
        let result = verify_permissions(&test_file);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("insecure"));
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_permissions_world_writable() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("world_writable.db");

        std::fs::File::create(&test_file).unwrap();

        // Set world-writable permissions
        let perms = std::fs::Permissions::from_mode(0o666);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Should fail verification
        let result = verify_permissions(&test_file);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_secure_permissions_on_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let secure_dir = temp_dir.path().join("secure_test");

        create_secure_directory(&secure_dir).unwrap();

        // Verify permissions
        verify_permissions(&secure_dir).unwrap();
    }

    #[test]
    fn test_precreate_atomic_prevents_race() {
        // Test that atomic creation prevents races by calling precreate twice.
        // The first call should create, the second should return AlreadyExisted.
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("race_test.db");

        // First call creates the file
        let outcome1 = precreate_secure_database_file(&db_path).unwrap();
        assert_eq!(outcome1, FileCreationOutcome::Created);
        assert!(db_path.exists());

        // Second call should detect existing file atomically
        let outcome2 = precreate_secure_database_file(&db_path).unwrap();
        assert_eq!(outcome2, FileCreationOutcome::AlreadyExisted);
    }

    #[test]
    fn test_precreate_concurrent_threads() {
        use std::sync::Arc;
        use std::thread;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = Arc::new(temp_dir.path().join("concurrent_test.db"));

        // Spawn multiple threads trying to create the same file
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let path = Arc::clone(&db_path);
                thread::spawn(move || precreate_secure_database_file(path.as_ref()))
            })
            .collect();

        // Collect all results
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All should succeed
        assert!(outcomes.iter().all(|r| r.is_ok()));

        // Extract successful outcomes
        let outcomes: Vec<_> = outcomes.into_iter().map(|r| r.unwrap()).collect();

        // Exactly one thread should have created the file
        let created_count = outcomes
            .iter()
            .filter(|o| **o == FileCreationOutcome::Created)
            .count();
        assert_eq!(created_count, 1, "Expected exactly one Created outcome");

        // The rest should have seen AlreadyExisted
        let existed_count = outcomes
            .iter()
            .filter(|o| **o == FileCreationOutcome::AlreadyExisted)
            .count();
        assert_eq!(existed_count, 9, "Expected 9 AlreadyExisted outcomes");

        // File should exist
        assert!(db_path.exists());
    }

    // Windows-specific permission verification is intentionally not implemented.
}
