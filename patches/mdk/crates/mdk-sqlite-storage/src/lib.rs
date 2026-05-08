//! SQLite-based storage implementation for MDK.
//!
//! This module provides a SQLite-based storage implementation for MDK (Marmot Development Kit).
//! It implements the [`MdkStorageProvider`] trait, allowing it to be used as a persistent storage backend.
//!
//! SQLite-based storage is persistent and will be saved to a file. It's useful for production applications
//! where data persistence is required.
//!
//! # Encryption
//!
//! This crate uses SQLCipher for transparent encryption of MLS state at rest with keys stored securely
//! in the platform's native keyring (Keychain, Keystore, etc.).
//!
//! ## Setup (Required First)
//!
//! Before using MDK, the host application must initialize a platform-specific keyring store:
//!
//! ```ignore
//! // macOS/iOS
//! use apple_native_keyring_store::AppleStore;
//! keyring_core::set_default_store(AppleStore::new());
//!
//! // Windows
//! use windows_native_keyring_store::WindowsStore;
//! keyring_core::set_default_store(WindowsStore::new());
//!
//! // Linux
//! use linux_keyutils_keyring_store::KeyutilsStore;
//! keyring_core::set_default_store(KeyutilsStore::new());
//! ```
//!
//! ## Creating Encrypted Storage (Recommended)
//!
//! ```ignore
//! use mdk_sqlite_storage::MdkSqliteStorage;
//!
//! // MDK handles key generation and storage automatically
//! let storage = MdkSqliteStorage::new(
//!     "/path/to/db.sqlite",
//!     "com.example.myapp",      // Service identifier
//!     "mdk.db.key.default"      // Key identifier
//! )?;
//! ```
//!
//! ## Direct Key Management (Advanced)
//!
//! If you need to manage encryption keys yourself:
//!
//! ```no_run
//! use mdk_sqlite_storage::{EncryptionConfig, MdkSqliteStorage};
//!
//! let key = [0u8; 32]; // Your securely stored key
//! let config = EncryptionConfig::new(key);
//! let storage = MdkSqliteStorage::new_with_key("/path/to/db.sqlite", config)?;
//! # Ok::<(), mdk_sqlite_storage::error::Error>(())
//! ```
//!
//! # Security Recommendations
//!
//! - **Use [`MdkSqliteStorage::new`]**: It handles key generation and secure storage automatically
//! - **Never log encryption keys**: The [`EncryptionConfig`] debug output redacts the key
//! - **Use unique keys per database**: Don't reuse keys across different databases

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::path::Path;
use std::sync::Arc;

use mdk_storage_traits::{Backend, GroupId, MdkStorageError, MdkStorageProvider};
use openmls_traits::storage::{StorageProvider, traits};
use rusqlite::Connection;
use std::sync::Mutex;

mod db;
pub mod encryption;
pub mod error;
mod groups;
pub mod keyring;
mod messages;
mod migrations;
mod mls_storage;
mod permissions;
#[cfg(test)]
mod test_utils;
mod validation;
mod welcomes;

pub use self::encryption::EncryptionConfig;
use self::error::Error;
use self::mls_storage::{GroupDataType, STORAGE_PROVIDER_VERSION};
pub use self::permissions::verify_permissions;
use self::permissions::{
    FileCreationOutcome, precreate_secure_database_file, set_secure_file_permissions,
};

/// A SQLite-based storage implementation for MDK.
///
/// This struct implements the MdkStorageProvider trait for SQLite databases.
/// It directly interfaces with a SQLite database for storing MLS data, using
/// a single unified connection for both MLS cryptographic state and MDK-specific
/// data (groups, messages, welcomes).
///
/// # Unified Storage Architecture
///
/// This implementation provides atomic transactions across all MLS and MDK state
/// by using a single database connection. This enables proper rollback for
/// commit race resolution as required by the Marmot Protocol.
///
/// # Encryption
///
/// All databases are encrypted by default using SQLCipher. Keys are stored securely
/// in the platform's native keyring (Keychain, Keystore, etc.).
///
/// # Example
///
/// ```ignore
/// use mdk_sqlite_storage::MdkSqliteStorage;
///
/// // Create encrypted storage (production - recommended)
/// let storage = MdkSqliteStorage::new(
///     "/path/to/db.sqlite",
///     "com.example.myapp",
///     "mdk.db.key.default"
/// )?;
/// ```
pub struct MdkSqliteStorage {
    /// The unified SQLite connection for both MLS and MDK state
    connection: Arc<Mutex<Connection>>,
}

impl MdkSqliteStorage {
    /// Creates a new encrypted [`MdkSqliteStorage`] with automatic key management.
    ///
    /// This is the recommended constructor for production use. The database encryption key
    /// is automatically retrieved from (or generated and stored in) the platform's native
    /// keyring (Keychain on macOS/iOS, Keystore on Android, etc.).
    ///
    /// # Prerequisites
    ///
    /// The host application must initialize a platform-specific keyring store before calling
    /// this method. See the module documentation for setup instructions.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the SQLite database file.
    /// * `service_id` - A stable, host-defined application identifier (e.g., reverse-DNS like
    ///   `"com.example.myapp"`). This should be unique per application.
    /// * `db_key_id` - A stable identifier for this database's key (e.g., `"mdk.db.key.default"`
    ///   or `"mdk.db.key.<profile_id>"` for multi-profile apps).
    ///
    /// # Key Management
    ///
    /// - If no key exists for the given identifiers, a new 32-byte key is generated using
    ///   cryptographically secure randomness and stored in the keyring.
    /// - On subsequent calls with the same identifiers, the existing key is retrieved.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No keyring store has been initialized
    /// - The keyring is unavailable or inaccessible
    /// - An existing database cannot be decrypted with the stored key
    /// - The database file cannot be created or opened
    ///
    /// # Example
    ///
    /// ```ignore
    /// use mdk_sqlite_storage::MdkSqliteStorage;
    ///
    /// // First, initialize the platform keyring (do this once at app startup)
    /// // keyring_core::set_default_store(platform_specific_store);
    ///
    /// // Then create storage with automatic key management
    /// let storage = MdkSqliteStorage::new(
    ///     "/path/to/db.sqlite",
    ///     "com.example.myapp",
    ///     "mdk.db.key.default"
    /// )?;
    /// ```
    pub fn new<P>(file_path: P, service_id: &str, db_key_id: &str) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let file_path = file_path.as_ref();

        // Atomically create the database file first, BEFORE making key decisions.
        // This prevents TOCTOU races where another process could create the file
        // between our existence check and key generation.
        let creation_outcome = precreate_secure_database_file(file_path)?;

        let config = match creation_outcome {
            FileCreationOutcome::Created | FileCreationOutcome::Skipped => {
                // We created the file (or it's a special path like :memory:).
                // Safe to generate a new key since we own this database.
                keyring::get_or_create_db_key(service_id, db_key_id)?
            }
            FileCreationOutcome::AlreadyExisted => {
                // File already existed - another thread/process may have created it.
                // We must retrieve the existing key, not generate a new one.
                //
                // IMPORTANT: Check the keyring FIRST, before checking if the file is encrypted.
                // This handles the race condition where another thread has created the file
                // and stored the key in the keyring, but hasn't yet written the encrypted
                // header to the database file. If we checked the file first, we'd see an
                // empty file and incorrectly return UnencryptedDatabaseWithEncryption.
                match keyring::get_db_key(service_id, db_key_id)? {
                    Some(config) => {
                        // Key exists in keyring - another thread/process is initializing
                        // (or has initialized) this database with encryption. Use that key.
                        config
                    }
                    None => {
                        // No key in keyring. Check if the database file appears unencrypted.
                        // This catches the case where someone tries to use new() on a
                        // database that was created with new_unencrypted().
                        if !encryption::is_database_encrypted(file_path)? {
                            return Err(Error::UnencryptedDatabaseWithEncryption);
                        }

                        // Database appears encrypted but no key in keyring - unrecoverable.
                        return Err(Error::KeyringEntryMissingForExistingDatabase {
                            db_path: file_path.display().to_string(),
                            service_id: service_id.to_string(),
                            db_key_id: db_key_id.to_string(),
                        });
                    }
                }
            }
        };

        Self::new_internal_skip_precreate(file_path, Some(config))
    }

    /// Creates a new encrypted [`MdkSqliteStorage`] with a directly provided encryption key.
    ///
    /// Use this method when you want to manage encryption keys yourself rather than using
    /// the platform keyring. For most applications, prefer [`Self::new`] which handles
    /// key management automatically.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the SQLite database file.
    /// * `config` - Encryption configuration containing the 32-byte key.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The encryption key is invalid
    /// - An existing database cannot be decrypted with the provided key
    /// - An existing database was created without encryption
    /// - The database file cannot be created or opened
    /// - File permissions cannot be set
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mdk_sqlite_storage::{EncryptionConfig, MdkSqliteStorage};
    ///
    /// let key = [0u8; 32]; // Your securely stored key
    /// let config = EncryptionConfig::new(key);
    /// let storage = MdkSqliteStorage::new_with_key("/path/to/db.sqlite", config)?;
    /// # Ok::<(), mdk_sqlite_storage::error::Error>(())
    /// ```
    pub fn new_with_key<P>(file_path: P, config: EncryptionConfig) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let file_path = file_path.as_ref();

        // If the database exists, verify it's encrypted before trying to use the key.
        // This provides a clearer error than letting apply_encryption fail.
        if file_path.exists() && !encryption::is_database_encrypted(file_path)? {
            return Err(Error::UnencryptedDatabaseWithEncryption);
        }

        Self::new_internal(file_path, Some(config))
    }

    /// Creates a new unencrypted [`MdkSqliteStorage`] with the provided file path.
    ///
    /// ⚠️ **WARNING**: This creates an unencrypted database. Sensitive MLS state including
    /// exporter secrets will be stored in plaintext. Only use this for development or testing.
    ///
    /// For production use, use [`Self::new`] or [`Self::new_with_key`] instead.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the SQLite database file.
    ///
    /// # Returns
    ///
    /// A Result containing a new instance of [`MdkSqliteStorage`] or an error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mdk_sqlite_storage::MdkSqliteStorage;
    ///
    /// // ⚠️ Unencrypted - for development only
    /// let storage = MdkSqliteStorage::new_unencrypted("/path/to/db.sqlite")?;
    /// # Ok::<(), mdk_sqlite_storage::error::Error>(())
    /// ```
    pub fn new_unencrypted<P>(file_path: P) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        tracing::warn!(
            "Creating unencrypted database. Sensitive MLS state will be stored in plaintext. \
             For production use, use new() or new_with_key() instead."
        );
        Self::new_internal(file_path, None)
    }

    /// Internal constructor that handles both encrypted and unencrypted database creation.
    ///
    /// This is used by constructors that haven't already pre-created the file.
    fn new_internal<P>(
        file_path: P,
        encryption_config: Option<EncryptionConfig>,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let file_path = file_path.as_ref();

        // Pre-create database file with secure permissions to avoid permission race
        precreate_secure_database_file(file_path)?;

        Self::new_internal_skip_precreate(file_path, encryption_config)
    }

    /// Internal constructor that skips file pre-creation.
    ///
    /// Used when the caller has already atomically pre-created the file
    /// (e.g., in `new()` which uses atomic creation for TOCTOU prevention).
    fn new_internal_skip_precreate(
        file_path: &Path,
        encryption_config: Option<EncryptionConfig>,
    ) -> Result<Self, Error> {
        // Create or open the unified SQLite database connection
        let mut connection = Self::open_connection(file_path, encryption_config.as_ref())?;

        // Apply all migrations (both OpenMLS tables and MDK tables)
        migrations::run_migrations(&mut connection)?;

        // Ensure secure permissions on the database file and any sidecar files
        Self::apply_secure_permissions(file_path)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Opens a SQLite connection with optional encryption.
    fn open_connection(
        file_path: &Path,
        encryption_config: Option<&EncryptionConfig>,
    ) -> Result<Connection, Error> {
        let conn = Connection::open(file_path)?;

        // Apply encryption if configured (must be done before any other operations)
        if let Some(config) = encryption_config {
            encryption::apply_encryption(&conn, config)?;
        }

        // Enable foreign keys (after encryption is set up)
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        Ok(conn)
    }

    /// Applies secure file permissions to the database and related files.
    ///
    /// # Defense in Depth for Sidecar Files
    ///
    /// SQLite creates sidecar files (`-wal`, `-shm`, `-journal`) dynamically during
    /// database operations. We apply permissions to these files if they exist at
    /// initialization time, but files created afterward may have default permissions
    /// until the next `MdkSqliteStorage` instance is created.
    ///
    /// This is acceptable because of our layered security approach:
    ///
    /// 1. **Directory permissions**: The parent directory is created with 0700 permissions
    ///    (owner-only access). Even if sidecar files have more permissive default permissions,
    ///    other users cannot traverse into the directory to access them.
    ///
    /// 2. **SQLCipher encryption**: All data written to sidecar files is encrypted.
    ///    The `-wal` and `-journal` files contain encrypted page data, making them
    ///    unreadable without the encryption key regardless of file permissions.
    ///
    /// 3. **Mobile sandboxing**: On iOS and Android, the application sandbox provides
    ///    the primary security boundary, making file permissions defense-in-depth.
    ///
    /// Alternative approaches (e.g., `PRAGMA journal_mode = MEMORY`) were considered
    /// but rejected because they sacrifice crash durability, which is unacceptable
    /// for MLS cryptographic state.
    fn apply_secure_permissions(db_path: &Path) -> Result<(), Error> {
        // Skip special SQLite paths (in-memory databases, etc.)
        let path_str = db_path.to_string_lossy();
        if path_str.is_empty() || path_str == ":memory:" || path_str.starts_with(':') {
            return Ok(());
        }

        // Apply to main database file
        set_secure_file_permissions(db_path)?;

        // Apply to common SQLite sidecar files if they exist.
        // Note: Files created after this point will have default permissions, but are
        // still protected by directory permissions and SQLCipher encryption (see above).
        let parent = db_path.parent();
        let stem = db_path.file_name().and_then(|n| n.to_str());

        if let (Some(parent), Some(stem)) = (parent, stem) {
            for suffix in &["-wal", "-shm", "-journal"] {
                let sidecar = parent.join(format!("{}{}", stem, suffix));
                if sidecar.exists() {
                    set_secure_file_permissions(&sidecar)?;
                }
            }
        }

        Ok(())
    }

    /// Creates a new in-memory [`MdkSqliteStorage`] for testing purposes.
    ///
    /// In-memory databases are not encrypted and do not persist data.
    ///
    /// # Returns
    ///
    /// A Result containing a new in-memory instance of [`MdkSqliteStorage`] or an error.
    #[cfg(test)]
    pub fn new_in_memory() -> Result<Self, Error> {
        // Create an in-memory SQLite database
        let mut connection = Connection::open_in_memory()?;

        // Enable foreign keys
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;

        // Run all migrations (both OpenMLS tables and MDK tables)
        migrations::run_migrations(&mut connection)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Provides access to the underlying connection for MDK storage operations.
    ///
    /// This method is for internal use by the group, message, and welcome storage implementations.
    pub(crate) fn with_connection<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> T,
    {
        let conn = self.connection.lock().unwrap();
        f(&conn)
    }

    /// Creates a snapshot of a group's state by copying all group-related rows
    /// to the snapshot table.
    fn snapshot_group_state(&self, group_id: &GroupId, name: &str) -> Result<(), Error> {
        let conn = self.connection.lock().unwrap();
        let group_id_bytes = group_id.as_slice();
        // MLS storage uses MlsCodec serialization for group_id keys.
        // We need to use the same serialization to match the stored keys.
        let mls_group_id_bytes = mls_storage::MlsCodec::serialize(group_id)
            .map_err(|e| Error::Database(e.to_string()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::Database(format!("Time error: {}", e)))?
            .as_secs() as i64;

        // Begin transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| Error::Database(e.to_string()))?;

        let result = (|| -> Result<(), Error> {
            // Helper to insert snapshot rows
            let mut insert_stmt = conn
                .prepare_cached(
                    "INSERT INTO group_state_snapshots
                 (snapshot_name, group_id, table_name, row_key, row_data, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            // Snapshot all 7 tables (4 OpenMLS + 3 MDK)
            // OpenMLS tables use MlsCodec-serialized group_id as their key
            Self::snapshot_openmls_group_data(
                &conn,
                &mut insert_stmt,
                name,
                group_id_bytes,
                &mls_group_id_bytes,
                now,
            )?;
            Self::snapshot_openmls_proposals(
                &conn,
                &mut insert_stmt,
                name,
                group_id_bytes,
                &mls_group_id_bytes,
                now,
            )?;
            Self::snapshot_openmls_own_leaf_nodes(
                &conn,
                &mut insert_stmt,
                name,
                group_id_bytes,
                &mls_group_id_bytes,
                now,
            )?;
            Self::snapshot_openmls_epoch_key_pairs(
                &conn,
                &mut insert_stmt,
                name,
                group_id_bytes,
                &mls_group_id_bytes,
                now,
            )?;
            // MDK tables use raw bytes for mls_group_id
            Self::snapshot_groups_table(&conn, &mut insert_stmt, name, group_id_bytes, now)?;
            Self::snapshot_group_relays(&conn, &mut insert_stmt, name, group_id_bytes, now)?;
            Self::snapshot_group_exporter_secrets(
                &conn,
                &mut insert_stmt,
                name,
                group_id_bytes,
                now,
            )?;

            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| Error::Database(e.to_string()))?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Snapshot helper: openmls_group_data table
    fn snapshot_openmls_group_data(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        mls_group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT group_id, data_type, group_data FROM openmls_group_data WHERE group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([mls_group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let gid: Vec<u8> = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let data_type: String = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let data: Vec<u8> = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&(&gid, &data_type))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "openmls_group_data",
                    row_key,
                    data,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: openmls_proposals table
    fn snapshot_openmls_proposals(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        mls_group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT group_id, proposal_ref, proposal FROM openmls_proposals WHERE group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([mls_group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let gid: Vec<u8> = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let proposal_ref: Vec<u8> = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let proposal: Vec<u8> = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&(&gid, &proposal_ref))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "openmls_proposals",
                    row_key,
                    proposal,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: openmls_own_leaf_nodes table
    fn snapshot_openmls_own_leaf_nodes(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        mls_group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT id, group_id, leaf_node FROM openmls_own_leaf_nodes WHERE group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([mls_group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let id: i64 = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let gid: Vec<u8> = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let leaf_node: Vec<u8> = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&id).map_err(|e| Error::Database(e.to_string()))?;
            let row_data = serde_json::to_vec(&(&gid, &leaf_node))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "openmls_own_leaf_nodes",
                    row_key,
                    row_data,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: openmls_epoch_key_pairs table
    fn snapshot_openmls_epoch_key_pairs(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        mls_group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT group_id, epoch_id, leaf_index, key_pairs
                 FROM openmls_epoch_key_pairs WHERE group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([mls_group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let gid: Vec<u8> = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let epoch_id: Vec<u8> = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let leaf_index: i64 = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let key_pairs: Vec<u8> = row.get(3).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&(&gid, &epoch_id, leaf_index))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "openmls_epoch_key_pairs",
                    row_key,
                    key_pairs,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: groups table (MDK)
    fn snapshot_groups_table(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT mls_group_id, nostr_group_id, name, description, admin_pubkeys,
                        last_message_id, last_message_at, last_message_processed_at, epoch, state,
                        image_hash, image_key, image_nonce, last_self_update_at
                 FROM groups WHERE mls_group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let mls_group_id: Vec<u8> = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let nostr_group_id: Vec<u8> = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let name_val: String = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let description: String = row.get(3).map_err(|e| Error::Database(e.to_string()))?;
            let admin_pubkeys: String = row.get(4).map_err(|e| Error::Database(e.to_string()))?;
            let last_message_id: Option<Vec<u8>> =
                row.get(5).map_err(|e| Error::Database(e.to_string()))?;
            let last_message_at: Option<i64> =
                row.get(6).map_err(|e| Error::Database(e.to_string()))?;
            let last_message_processed_at: Option<i64> =
                row.get(7).map_err(|e| Error::Database(e.to_string()))?;
            let epoch: i64 = row.get(8).map_err(|e| Error::Database(e.to_string()))?;
            let state: String = row.get(9).map_err(|e| Error::Database(e.to_string()))?;
            let image_hash: Option<Vec<u8>> =
                row.get(10).map_err(|e| Error::Database(e.to_string()))?;
            let image_key: Option<Vec<u8>> =
                row.get(11).map_err(|e| Error::Database(e.to_string()))?;
            let image_nonce: Option<Vec<u8>> =
                row.get(12).map_err(|e| Error::Database(e.to_string()))?;
            let last_self_update_at: i64 =
                row.get(13).map_err(|e| Error::Database(e.to_string()))?;

            let row_key =
                serde_json::to_vec(&mls_group_id).map_err(|e| Error::Database(e.to_string()))?;
            let row_data = serde_json::to_vec(&(
                &nostr_group_id,
                &name_val,
                &description,
                &admin_pubkeys,
                &last_message_id,
                &last_message_at,
                &last_message_processed_at,
                epoch,
                &state,
                &image_hash,
                &image_key,
                &image_nonce,
                &last_self_update_at,
            ))
            .map_err(|e| Error::Database(e.to_string()))?;

            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "groups",
                    row_key,
                    row_data,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: group_relays table
    fn snapshot_group_relays(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare("SELECT id, mls_group_id, relay_url FROM group_relays WHERE mls_group_id = ?")
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let id: i64 = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let mls_group_id: Vec<u8> = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let relay_url: String = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&id).map_err(|e| Error::Database(e.to_string()))?;
            let row_data = serde_json::to_vec(&(&mls_group_id, &relay_url))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "group_relays",
                    row_key,
                    row_data,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Snapshot helper: group_exporter_secrets table
    fn snapshot_group_exporter_secrets(
        conn: &rusqlite::Connection,
        insert_stmt: &mut rusqlite::CachedStatement<'_>,
        snapshot_name: &str,
        group_id_bytes: &[u8],
        now: i64,
    ) -> Result<(), Error> {
        let mut stmt = conn
            .prepare(
                "SELECT mls_group_id, epoch, secret FROM group_exporter_secrets WHERE mls_group_id = ?",
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut rows = stmt
            .query([group_id_bytes])
            .map_err(|e| Error::Database(e.to_string()))?;

        while let Some(row) = rows.next().map_err(|e| Error::Database(e.to_string()))? {
            let mls_group_id: Vec<u8> = row.get(0).map_err(|e| Error::Database(e.to_string()))?;
            let epoch: i64 = row.get(1).map_err(|e| Error::Database(e.to_string()))?;
            let secret: Vec<u8> = row.get(2).map_err(|e| Error::Database(e.to_string()))?;
            let row_key = serde_json::to_vec(&(&mls_group_id, epoch))
                .map_err(|e| Error::Database(e.to_string()))?;
            insert_stmt
                .execute(rusqlite::params![
                    snapshot_name,
                    group_id_bytes,
                    "group_exporter_secrets",
                    row_key,
                    secret,
                    now
                ])
                .map_err(|e| Error::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Restores a group's state from a snapshot by deleting current rows
    /// and re-inserting from the snapshot table.
    fn restore_group_from_snapshot(&self, group_id: &GroupId, name: &str) -> Result<(), Error> {
        let conn = self.connection.lock().unwrap();
        let group_id_bytes = group_id.as_slice();
        // MLS storage uses a serde-compatible binary serialization codec for group_id keys.
        // We need to use the same serialization to match the stored keys.
        let mls_group_id_bytes = mls_storage::MlsCodec::serialize(group_id)
            .map_err(|e| Error::Database(e.to_string()))?;

        // Check if snapshot exists BEFORE starting transaction or deleting any data.
        // This prevents data loss if the snapshot name is typo'd or doesn't exist.
        // This matches the memory storage behavior which returns NotFound error.
        let snapshot_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM group_state_snapshots WHERE snapshot_name = ? AND group_id = ?)",
                rusqlite::params![name, group_id_bytes],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        if !snapshot_exists {
            return Err(Error::Database("Snapshot not found".to_string()));
        }

        // 1. Read snapshot data into memory FIRST, before any deletions.
        // This is critical because the groups table has ON DELETE CASCADE to
        // group_state_snapshots - if we delete the group first, the snapshot
        // rows get deleted too!
        let snapshot_rows: Vec<(String, Vec<u8>, Vec<u8>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT table_name, row_key, row_data FROM group_state_snapshots
                     WHERE snapshot_name = ? AND group_id = ?",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let rows = stmt
                .query_map(rusqlite::params![name, group_id_bytes], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| Error::Database(e.to_string()))?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| Error::Database(e.to_string()))?
        };

        // Also read OTHER snapshots for this group (different names) so we can
        // restore them after the CASCADE deletion. This preserves multiple snapshots.
        #[allow(clippy::type_complexity)]
        let other_snapshots: Vec<(String, String, Vec<u8>, Vec<u8>, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT snapshot_name, table_name, row_key, row_data, created_at
                     FROM group_state_snapshots
                     WHERE group_id = ? AND snapshot_name != ?",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let rows = stmt
                .query_map(rusqlite::params![group_id_bytes, name], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(|e| Error::Database(e.to_string()))?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| Error::Database(e.to_string()))?
        };

        // Begin transaction for atomicity - critical to prevent data loss on failure
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| Error::Database(e.to_string()))?;

        let result = (|| -> Result<(), Error> {
            // 2. Delete current rows for this group from all 7 tables
            // OpenMLS tables use MlsCodec-serialized group_id as their key
            conn.execute(
                "DELETE FROM openmls_group_data WHERE group_id = ?",
                [&mls_group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute(
                "DELETE FROM openmls_proposals WHERE group_id = ?",
                [&mls_group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute(
                "DELETE FROM openmls_own_leaf_nodes WHERE group_id = ?",
                [&mls_group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute(
                "DELETE FROM openmls_epoch_key_pairs WHERE group_id = ?",
                [&mls_group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            // For MDK tables, we need to disable foreign key checks temporarily
            // or delete in the right order to avoid FK violations
            conn.execute(
                "DELETE FROM group_exporter_secrets WHERE mls_group_id = ?",
                [group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute(
                "DELETE FROM group_relays WHERE mls_group_id = ?",
                [group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute(
                "DELETE FROM groups WHERE mls_group_id = ?",
                [group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            // Note: The CASCADE will have deleted the snapshot rows, but we already
            // have the data in memory (snapshot_rows).

            // 3. Restore from in-memory snapshot data
            // IMPORTANT: We must restore "groups" first because group_relays and
            // group_exporter_secrets have FK constraints that reference groups.
            // Process in two passes: groups first, then everything else.
            for (table_name, row_key, row_data) in &snapshot_rows {
                if table_name != "groups" {
                    continue;
                }
                let mls_group_id: Vec<u8> =
                    serde_json::from_slice(row_key).map_err(|e| Error::Database(e.to_string()))?;
                #[allow(clippy::type_complexity)]
                let (
                    nostr_group_id,
                    name_val,
                    description,
                    admin_pubkeys,
                    last_message_id,
                    last_message_at,
                    last_message_processed_at,
                    epoch,
                    state,
                    image_hash,
                    image_key,
                    image_nonce,
                    last_self_update_at,
                ): (
                    Vec<u8>,
                    String,
                    String,
                    String,
                    Option<Vec<u8>>,
                    Option<i64>,
                    Option<i64>,
                    i64,
                    String,
                    Option<Vec<u8>>,
                    Option<Vec<u8>>,
                    Option<Vec<u8>>,
                    i64,
                ) = serde_json::from_slice(row_data).map_err(|e| Error::Database(e.to_string()))?;
                conn.execute(
                    "INSERT INTO groups (mls_group_id, nostr_group_id, name, description, admin_pubkeys,
                                        last_message_id, last_message_at, last_message_processed_at, epoch, state,
                                        image_hash, image_key, image_nonce, last_self_update_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        mls_group_id,
                        nostr_group_id,
                        name_val,
                        description,
                        admin_pubkeys,
                        last_message_id,
                        last_message_at,
                        last_message_processed_at,
                        epoch,
                        state,
                        image_hash,
                        image_key,
                        image_nonce,
                        last_self_update_at
                    ],
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            }

            // Now restore all other tables (groups already done above)
            for (table_name, row_key, row_data) in &snapshot_rows {
                match table_name.as_str() {
                    "openmls_group_data" => {
                        let (gid, data_type): (Vec<u8>, String) =
                            serde_json::from_slice(row_key)
                                .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO openmls_group_data (provider_version, group_id, data_type, group_data)
                             VALUES (1, ?, ?, ?)",
                            rusqlite::params![gid, data_type, row_data],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    "openmls_proposals" => {
                        let (gid, proposal_ref): (Vec<u8>, Vec<u8>) =
                            serde_json::from_slice(row_key)
                                .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO openmls_proposals (provider_version, group_id, proposal_ref, proposal)
                             VALUES (1, ?, ?, ?)",
                            rusqlite::params![gid, proposal_ref, row_data],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    "openmls_own_leaf_nodes" => {
                        let (gid, leaf_node): (Vec<u8>, Vec<u8>) = serde_json::from_slice(row_data)
                            .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO openmls_own_leaf_nodes (provider_version, group_id, leaf_node)
                             VALUES (1, ?, ?)",
                            rusqlite::params![gid, leaf_node],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    "openmls_epoch_key_pairs" => {
                        let (gid, epoch_id, leaf_index): (Vec<u8>, Vec<u8>, i64) =
                            serde_json::from_slice(row_key)
                                .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO openmls_epoch_key_pairs (provider_version, group_id, epoch_id, leaf_index, key_pairs)
                             VALUES (1, ?, ?, ?, ?)",
                            rusqlite::params![gid, epoch_id, leaf_index, row_data],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    "groups" => {
                        // Already restored in the first pass above
                    }
                    "group_relays" => {
                        let (mls_group_id, relay_url): (Vec<u8>, String) =
                            serde_json::from_slice(row_data)
                                .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO group_relays (mls_group_id, relay_url) VALUES (?, ?)",
                            rusqlite::params![mls_group_id, relay_url],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    "group_exporter_secrets" => {
                        let (mls_group_id, epoch): (Vec<u8>, i64) = serde_json::from_slice(row_key)
                            .map_err(|e| Error::Database(e.to_string()))?;
                        conn.execute(
                            "INSERT INTO group_exporter_secrets (mls_group_id, epoch, secret) VALUES (?, ?, ?)",
                            rusqlite::params![mls_group_id, epoch, row_data],
                        )
                        .map_err(|e| Error::Database(e.to_string()))?;
                    }
                    _ => {
                        // Unknown table, skip
                    }
                }
            }

            // 4. Delete the consumed snapshot (may be no-op if CASCADE already deleted them)
            conn.execute(
                "DELETE FROM group_state_snapshots WHERE snapshot_name = ? AND group_id = ?",
                rusqlite::params![name, group_id_bytes],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            // 5. Re-insert other snapshots that were deleted by CASCADE
            // This preserves multiple snapshots when rolling back to one of them.
            for (snap_name, table_name, row_key, row_data, created_at) in &other_snapshots {
                conn.execute(
                    "INSERT INTO group_state_snapshots (snapshot_name, group_id, table_name, row_key, row_data, created_at)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    rusqlite::params![snap_name, group_id_bytes, table_name, row_key, row_data, created_at],
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| Error::Database(e.to_string()))?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Deletes a snapshot that is no longer needed.
    fn delete_group_snapshot(&self, group_id: &GroupId, name: &str) -> Result<(), Error> {
        let conn = self.connection.lock().unwrap();
        conn.execute(
            "DELETE FROM group_state_snapshots WHERE snapshot_name = ? AND group_id = ?",
            rusqlite::params![name, group_id.as_slice()],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }
}

/// Implementation of [`MdkStorageProvider`] for SQLite-based storage.
impl MdkStorageProvider for MdkSqliteStorage {
    /// Returns the backend type.
    ///
    /// # Returns
    ///
    /// [`Backend::SQLite`] indicating this is a SQLite-based storage implementation.
    fn backend(&self) -> Backend {
        Backend::SQLite
    }

    fn create_group_snapshot(&self, group_id: &GroupId, name: &str) -> Result<(), MdkStorageError> {
        self.snapshot_group_state(group_id, name)
            .map_err(|e| MdkStorageError::Database(e.to_string()))
    }

    fn rollback_group_to_snapshot(
        &self,
        group_id: &GroupId,
        name: &str,
    ) -> Result<(), MdkStorageError> {
        self.restore_group_from_snapshot(group_id, name)
            .map_err(|e| MdkStorageError::Database(e.to_string()))
    }

    fn release_group_snapshot(
        &self,
        group_id: &GroupId,
        name: &str,
    ) -> Result<(), MdkStorageError> {
        self.delete_group_snapshot(group_id, name)
            .map_err(|e| MdkStorageError::Database(e.to_string()))
    }

    fn list_group_snapshots(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(String, u64)>, MdkStorageError> {
        let conn = self.connection.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "SELECT DISTINCT snapshot_name, created_at FROM group_state_snapshots
                 WHERE group_id = ? ORDER BY created_at ASC",
            )
            .map_err(|e| MdkStorageError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(rusqlite::params![group_id.as_slice()], |row| {
                let name: String = row.get(0)?;
                let created_at: i64 = row.get(1)?;
                Ok((name, created_at as u64))
            })
            .map_err(|e| MdkStorageError::Database(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| MdkStorageError::Database(e.to_string()))
    }

    fn prune_expired_snapshots(&self, min_timestamp: u64) -> Result<usize, MdkStorageError> {
        let conn = self.connection.lock().unwrap();
        let deleted = conn
            .execute(
                "DELETE FROM group_state_snapshots WHERE created_at < ?",
                rusqlite::params![min_timestamp as i64],
            )
            .map_err(|e| MdkStorageError::Database(e.to_string()))?;
        Ok(deleted)
    }
}

// ============================================================================
// OpenMLS StorageProvider<1> Implementation
// ============================================================================

impl StorageProvider<STORAGE_PROVIDER_VERSION> for MdkSqliteStorage {
    type Error = MdkStorageError;

    // ========================================================================
    // Write Methods
    // ========================================================================

    fn write_mls_join_config<GroupId, MlsGroupJoinConfig>(
        &self,
        group_id: &GroupId,
        config: &MlsGroupJoinConfig,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(conn, group_id, GroupDataType::JoinGroupConfig, config)
        })
    }

    fn append_own_leaf_node<GroupId, LeafNode>(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNode: traits::LeafNode<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::append_own_leaf_node(conn, group_id, leaf_node))
    }

    fn queue_proposal<GroupId, ProposalRef, QueuedProposal>(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
        QueuedProposal: traits::QueuedProposal<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::queue_proposal(conn, group_id, proposal_ref, proposal)
        })
    }

    fn write_tree<GroupId, TreeSync>(
        &self,
        group_id: &GroupId,
        tree: &TreeSync,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        TreeSync: traits::TreeSync<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(conn, group_id, GroupDataType::Tree, tree)
        })
    }

    fn write_interim_transcript_hash<GroupId, InterimTranscriptHash>(
        &self,
        group_id: &GroupId,
        interim_transcript_hash: &InterimTranscriptHash,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::InterimTranscriptHash,
                interim_transcript_hash,
            )
        })
    }

    fn write_context<GroupId, GroupContext>(
        &self,
        group_id: &GroupId,
        group_context: &GroupContext,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupContext: traits::GroupContext<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(conn, group_id, GroupDataType::Context, group_context)
        })
    }

    fn write_confirmation_tag<GroupId, ConfirmationTag>(
        &self,
        group_id: &GroupId,
        confirmation_tag: &ConfirmationTag,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::ConfirmationTag,
                confirmation_tag,
            )
        })
    }

    fn write_group_state<GroupState, GroupId>(
        &self,
        group_id: &GroupId,
        group_state: &GroupState,
    ) -> Result<(), Self::Error>
    where
        GroupState: traits::GroupState<STORAGE_PROVIDER_VERSION>,
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(conn, group_id, GroupDataType::GroupState, group_state)
        })
    }

    fn write_message_secrets<GroupId, MessageSecrets>(
        &self,
        group_id: &GroupId,
        message_secrets: &MessageSecrets,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MessageSecrets: traits::MessageSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::MessageSecrets,
                message_secrets,
            )
        })
    }

    fn write_resumption_psk_store<GroupId, ResumptionPskStore>(
        &self,
        group_id: &GroupId,
        resumption_psk_store: &ResumptionPskStore,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::ResumptionPskStore,
                resumption_psk_store,
            )
        })
    }

    fn write_own_leaf_index<GroupId, LeafNodeIndex>(
        &self,
        group_id: &GroupId,
        own_leaf_index: &LeafNodeIndex,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::OwnLeafIndex,
                own_leaf_index,
            )
        })
    }

    fn write_group_epoch_secrets<GroupId, GroupEpochSecrets>(
        &self,
        group_id: &GroupId,
        group_epoch_secrets: &GroupEpochSecrets,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_group_data(
                conn,
                group_id,
                GroupDataType::GroupEpochSecrets,
                group_epoch_secrets,
            )
        })
    }

    fn write_signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
        &self,
        public_key: &SignaturePublicKey,
        signature_key_pair: &SignatureKeyPair,
    ) -> Result<(), Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_signature_key_pair(conn, public_key, signature_key_pair)
        })
    }

    fn write_encryption_key_pair<EncryptionKey, HpkeKeyPair>(
        &self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), Self::Error>
    where
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_encryption_key_pair(conn, public_key, key_pair)
        })
    }

    fn write_encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::write_encryption_epoch_key_pairs(
                conn, group_id, epoch, leaf_index, key_pairs,
            )
        })
    }

    fn write_key_package<HashReference, KeyPackage>(
        &self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
        KeyPackage: traits::KeyPackage<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::write_key_package(conn, hash_ref, key_package))
    }

    fn write_psk<PskId, PskBundle>(
        &self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), Self::Error>
    where
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
        PskBundle: traits::PskBundle<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::write_psk(conn, psk_id, psk))
    }

    // ========================================================================
    // Read Methods
    // ========================================================================

    fn mls_group_join_config<GroupId, MlsGroupJoinConfig>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MlsGroupJoinConfig>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::JoinGroupConfig)
        })
    }

    fn own_leaf_nodes<GroupId, LeafNode>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNode: traits::LeafNode<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_own_leaf_nodes(conn, group_id))
    }

    fn queued_proposal_refs<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_queued_proposal_refs(conn, group_id))
    }

    fn queued_proposals<GroupId, ProposalRef, QueuedProposal>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
        QueuedProposal: traits::QueuedProposal<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_queued_proposals(conn, group_id))
    }

    fn tree<GroupId, TreeSync>(&self, group_id: &GroupId) -> Result<Option<TreeSync>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        TreeSync: traits::TreeSync<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::Tree)
        })
    }

    fn group_context<GroupId, GroupContext>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupContext>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupContext: traits::GroupContext<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::Context)
        })
    }

    fn interim_transcript_hash<GroupId, InterimTranscriptHash>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<InterimTranscriptHash>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::InterimTranscriptHash)
        })
    }

    fn confirmation_tag<GroupId, ConfirmationTag>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ConfirmationTag>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::ConfirmationTag)
        })
    }

    fn group_state<GroupState, GroupId>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupState>, Self::Error>
    where
        GroupState: traits::GroupState<STORAGE_PROVIDER_VERSION>,
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::GroupState)
        })
    }

    fn message_secrets<GroupId, MessageSecrets>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MessageSecrets>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MessageSecrets: traits::MessageSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::MessageSecrets)
        })
    }

    fn resumption_psk_store<GroupId, ResumptionPskStore>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ResumptionPskStore>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::ResumptionPskStore)
        })
    }

    fn own_leaf_index<GroupId, LeafNodeIndex>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<LeafNodeIndex>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::OwnLeafIndex)
        })
    }

    fn group_epoch_secrets<GroupId, GroupEpochSecrets>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupEpochSecrets>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_group_data(conn, group_id, GroupDataType::GroupEpochSecrets)
        })
    }

    fn signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_signature_key_pair(conn, public_key))
    }

    fn encryption_key_pair<HpkeKeyPair, EncryptionKey>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, Self::Error>
    where
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_encryption_key_pair(conn, public_key))
    }

    fn encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::read_encryption_epoch_key_pairs(conn, group_id, epoch, leaf_index)
        })
    }

    fn key_package<HashReference, KeyPackage>(
        &self,
        hash_ref: &HashReference,
    ) -> Result<Option<KeyPackage>, Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
        KeyPackage: traits::KeyPackage<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_key_package(conn, hash_ref))
    }

    fn psk<PskBundle, PskId>(&self, psk_id: &PskId) -> Result<Option<PskBundle>, Self::Error>
    where
        PskBundle: traits::PskBundle<STORAGE_PROVIDER_VERSION>,
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::read_psk(conn, psk_id))
    }

    // ========================================================================
    // Delete Methods
    // ========================================================================

    fn remove_proposal<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::remove_proposal(conn, group_id, proposal_ref))
    }

    fn delete_own_leaf_nodes<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::delete_own_leaf_nodes(conn, group_id))
    }

    fn delete_group_config<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::JoinGroupConfig)
        })
    }

    fn delete_tree<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::Tree)
        })
    }

    fn delete_confirmation_tag<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::ConfirmationTag)
        })
    }

    fn delete_group_state<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::GroupState)
        })
    }

    fn delete_context<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::Context)
        })
    }

    fn delete_interim_transcript_hash<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::InterimTranscriptHash)
        })
    }

    fn delete_message_secrets<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::MessageSecrets)
        })
    }

    fn delete_all_resumption_psk_secrets<GroupId>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::ResumptionPskStore)
        })
    }

    fn delete_own_leaf_index<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::OwnLeafIndex)
        })
    }

    fn delete_group_epoch_secrets<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_group_data(conn, group_id, GroupDataType::GroupEpochSecrets)
        })
    }

    fn clear_proposal_queue<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::clear_proposal_queue(conn, group_id))
    }

    fn delete_signature_key_pair<SignaturePublicKey>(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::delete_signature_key_pair(conn, public_key))
    }

    fn delete_encryption_key_pair<EncryptionKey>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<(), Self::Error>
    where
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::delete_encryption_key_pair(conn, public_key))
    }

    fn delete_encryption_epoch_key_pairs<GroupId, EpochKey>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| {
            mls_storage::delete_encryption_epoch_key_pairs(conn, group_id, epoch, leaf_index)
        })
    }

    fn delete_key_package<HashReference>(&self, hash_ref: &HashReference) -> Result<(), Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::delete_key_package(conn, hash_ref))
    }

    fn delete_psk<PskId>(&self, psk_id: &PskId) -> Result<(), Self::Error>
    where
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
    {
        self.with_connection(|conn| mls_storage::delete_psk(conn, psk_id))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::Secret;
    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::groups::types::{
        Group, GroupExporterSecret, GroupState, SelfUpdateState,
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_new_in_memory() {
        let storage = MdkSqliteStorage::new_in_memory();
        assert!(storage.is_ok());
        let storage = storage.unwrap();
        assert_eq!(storage.backend(), Backend::SQLite);
    }

    #[test]
    fn test_backend_type() {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();
        assert_eq!(storage.backend(), Backend::SQLite);
        assert!(storage.backend().is_persistent());
    }

    #[test]
    fn test_file_based_storage() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test_db.sqlite");

        // Create a new storage
        let storage = MdkSqliteStorage::new_unencrypted(&db_path);
        assert!(storage.is_ok());

        // Verify file exists
        assert!(db_path.exists());

        // Create a second instance that connects to the same file
        let storage2 = MdkSqliteStorage::new_unencrypted(&db_path);
        assert!(storage2.is_ok());

        // Clean up
        drop(storage);
        drop(storage2);
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_database_tables() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("migration_test.sqlite");

        // Create a new SQLite database
        let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

        // Verify the database has been properly initialized with migrations
        storage.with_connection(|conn| {
            // Check if the tables exist
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap();
            let table_names: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();

            // Check for MDK tables
            assert!(table_names.contains(&"groups".to_string()));
            assert!(table_names.contains(&"messages".to_string()));
            assert!(table_names.contains(&"welcomes".to_string()));
            assert!(table_names.contains(&"processed_messages".to_string()));
            assert!(table_names.contains(&"processed_welcomes".to_string()));
            assert!(table_names.contains(&"group_relays".to_string()));
            assert!(table_names.contains(&"group_exporter_secrets".to_string()));

            // Check for OpenMLS tables
            assert!(table_names.contains(&"openmls_group_data".to_string()));
            assert!(table_names.contains(&"openmls_proposals".to_string()));
            assert!(table_names.contains(&"openmls_own_leaf_nodes".to_string()));
            assert!(table_names.contains(&"openmls_key_packages".to_string()));
            assert!(table_names.contains(&"openmls_psks".to_string()));
            assert!(table_names.contains(&"openmls_signature_keys".to_string()));
            assert!(table_names.contains(&"openmls_encryption_keys".to_string()));
            assert!(table_names.contains(&"openmls_epoch_key_pairs".to_string()));
        });

        // Drop explicitly to release all resources
        drop(storage);
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_group_exporter_secrets() {
        // Create an in-memory SQLite database
        let storage = MdkSqliteStorage::new_in_memory().unwrap();

        // Create a test group
        let mls_group_id = GroupId::from_slice(vec![1, 2, 3, 4].as_slice());
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "A test group for exporter secrets".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        // Save the group
        storage.save_group(group.clone()).unwrap();

        // Create test group exporter secrets for different epochs
        let secret_epoch_0 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 0,
            secret: Secret::new([0u8; 32]),
        };

        let secret_epoch_1 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 1,
            secret: Secret::new([0u8; 32]),
        };

        // Save the exporter secrets
        storage
            .save_group_exporter_secret(secret_epoch_0.clone())
            .unwrap();
        storage
            .save_group_exporter_secret(secret_epoch_1.clone())
            .unwrap();

        // Test retrieving exporter secrets
        let retrieved_secret_0 = storage.get_group_exporter_secret(&mls_group_id, 0).unwrap();
        assert!(retrieved_secret_0.is_some());
        let retrieved_secret_0 = retrieved_secret_0.unwrap();
        assert_eq!(retrieved_secret_0, secret_epoch_0);

        let retrieved_secret_1 = storage.get_group_exporter_secret(&mls_group_id, 1).unwrap();
        assert!(retrieved_secret_1.is_some());
        let retrieved_secret_1 = retrieved_secret_1.unwrap();
        assert_eq!(retrieved_secret_1, secret_epoch_1);

        // Test non-existent epoch
        let non_existent_epoch = storage
            .get_group_exporter_secret(&mls_group_id, 999)
            .unwrap();
        assert!(non_existent_epoch.is_none());

        // Test non-existent group
        let non_existent_group_id = GroupId::from_slice(&[9, 9, 9, 9]);
        let result = storage.get_group_exporter_secret(&non_existent_group_id, 0);
        assert!(result.is_err());

        // Test overwriting an existing secret
        let updated_secret_0 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 0,
            secret: Secret::new([0u8; 32]),
        };
        storage
            .save_group_exporter_secret(updated_secret_0.clone())
            .unwrap();

        let retrieved_updated_secret = storage
            .get_group_exporter_secret(&mls_group_id, 0)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved_updated_secret, updated_secret_0);

        // Test trying to save a secret for a non-existent group
        let invalid_secret = GroupExporterSecret {
            mls_group_id: non_existent_group_id.clone(),
            epoch: 0,
            secret: Secret::new([0u8; 32]),
        };
        let result = storage.save_group_exporter_secret(invalid_secret);
        assert!(result.is_err());
    }

    // ========================================
    // Encryption tests
    // ========================================

    mod encryption_tests {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        use std::thread;

        use mdk_storage_traits::Secret;
        use mdk_storage_traits::groups::GroupStorage;
        use mdk_storage_traits::groups::types::{Group, GroupExporterSecret, GroupState};
        use mdk_storage_traits::messages::MessageStorage;
        use mdk_storage_traits::test_utils::cross_storage::{
            create_test_group, create_test_message, create_test_welcome,
        };
        use mdk_storage_traits::welcomes::WelcomeStorage;
        use nostr::EventId;

        use super::*;
        use crate::test_utils::ensure_mock_store;

        #[test]
        fn test_encrypted_storage_creation() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("encrypted.db");

            let config = EncryptionConfig::generate().unwrap();
            let storage = MdkSqliteStorage::new_with_key(&db_path, config);
            assert!(storage.is_ok());

            // Verify file exists
            assert!(db_path.exists());

            // Verify the database is encrypted (file header is not plain SQLite)
            assert!(
                encryption::is_database_encrypted(&db_path).unwrap(),
                "Database should be encrypted"
            );
        }

        #[test]
        fn test_encrypted_storage_reopen_with_correct_key() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("encrypted_reopen.db");

            // Create with a key
            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();
                // Do some operations to ensure the database is properly initialized
                let _ = storage.backend();
            }

            // Reopen with the same key
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2);
            assert!(
                storage2.is_ok(),
                "Should be able to reopen with correct key"
            );
        }

        #[test]
        fn test_encrypted_storage_wrong_key_fails() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("encrypted_wrong_key.db");

            // Create with key1
            let config1 = EncryptionConfig::generate().unwrap();
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config1).unwrap();
                drop(storage);
            }

            // Try to open with a different key
            let config2 = EncryptionConfig::generate().unwrap();
            let result = MdkSqliteStorage::new_with_key(&db_path, config2);

            assert!(result.is_err(), "Opening with wrong key should fail");

            // Verify it's the correct error type
            match result {
                Err(error::Error::WrongEncryptionKey) => {}
                Err(e) => panic!("Expected WrongEncryptionKey error, got: {:?}", e),
                Ok(_) => panic!("Expected error but got success"),
            }
        }

        #[test]
        fn test_unencrypted_cannot_read_encrypted() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("encrypted_only.db");

            // Create encrypted database
            let config = EncryptionConfig::generate().unwrap();
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();
                drop(storage);
            }

            // Try to open without encryption
            let result = MdkSqliteStorage::new_unencrypted(&db_path);

            // This should fail because the database is encrypted
            assert!(
                result.is_err(),
                "Opening encrypted database without key should fail"
            );
        }

        #[test]
        fn test_encrypted_storage_data_persistence() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("encrypted_persist.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            // Create storage and save a group
            let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let group = Group {
                    mls_group_id: mls_group_id.clone(),
                    nostr_group_id: [0u8; 32],
                    name: "Encrypted Group".to_string(),
                    description: "Testing encrypted persistence".to_string(),
                    admin_pubkeys: BTreeSet::new(),
                    last_message_id: None,
                    last_message_at: None,
                    last_message_processed_at: None,
                    epoch: 0,
                    state: GroupState::Active,
                    image_hash: None,
                    image_key: None,
                    image_nonce: None,
                    self_update_state: SelfUpdateState::Required,
                };

                storage.save_group(group).unwrap();
            }

            // Reopen and verify the data is still there
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let found_group = storage2.find_group_by_mls_group_id(&mls_group_id).unwrap();
            assert!(found_group.is_some());
            assert_eq!(found_group.unwrap().name, "Encrypted Group");
        }

        #[test]
        fn test_file_permissions_are_secure() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("secure_perms.db");

            let config = EncryptionConfig::generate().unwrap();
            let _storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

            // On Unix, verify permissions are restrictive
            #[cfg(unix)]
            {
                let metadata = std::fs::metadata(&db_path).unwrap();
                let mode = metadata.permissions().mode();

                // Check that group and world permissions are not set
                assert_eq!(
                    mode & 0o077,
                    0,
                    "Database file should have owner-only permissions, got {:o}",
                    mode & 0o777
                );
            }
        }

        #[test]
        fn test_encrypted_storage_multiple_groups() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("multi_groups.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            // Create storage and save multiple groups
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                for i in 0..5 {
                    let mls_group_id = GroupId::from_slice(&[i; 8]);
                    let mut group = create_test_group(mls_group_id);
                    group.name = format!("Group {}", i);
                    group.description = format!("Description {}", i);
                    storage.save_group(group).unwrap();
                }
            }

            // Reopen and verify all groups
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let groups = storage2.all_groups().unwrap();
            assert_eq!(groups.len(), 5);

            for i in 0..5u8 {
                let mls_group_id = GroupId::from_slice(&[i; 8]);
                let group = storage2
                    .find_group_by_mls_group_id(&mls_group_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(group.name, format!("Group {}", i));
            }
        }

        #[test]
        fn test_encrypted_storage_messages() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("messages.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

            // Create storage, group, and messages
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let group = create_test_group(mls_group_id.clone());
                storage.save_group(group).unwrap();

                // Save a message
                let event_id = EventId::all_zeros();
                let mut message = create_test_message(mls_group_id.clone(), event_id);
                message.content = "Test message content".to_string();
                storage.save_message(message).unwrap();
            }

            // Reopen and verify messages
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let messages = storage2.messages(&mls_group_id, None).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "Test message content");
        }

        #[test]
        fn test_encrypted_storage_welcomes() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("welcomes.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[5, 6, 7, 8]);

            // Create storage, group, and welcome
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let group = create_test_group(mls_group_id.clone());
                storage.save_group(group).unwrap();

                let event_id = EventId::all_zeros();
                let welcome = create_test_welcome(mls_group_id.clone(), event_id);
                storage.save_welcome(welcome).unwrap();
            }

            // Reopen and verify
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let welcomes = storage2.pending_welcomes(None).unwrap();
            assert_eq!(welcomes.len(), 1);
        }

        #[test]
        fn test_encrypted_storage_exporter_secrets() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("exporter_secrets.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[10, 20, 30, 40]);

            // Create storage, group, and exporter secrets for multiple epochs
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let group = Group {
                    mls_group_id: mls_group_id.clone(),
                    nostr_group_id: [0u8; 32],
                    name: "Exporter Secret Test".to_string(),
                    description: "Testing exporter secrets".to_string(),
                    admin_pubkeys: BTreeSet::new(),
                    last_message_id: None,
                    last_message_at: None,
                    last_message_processed_at: None,
                    epoch: 5,
                    state: GroupState::Active,
                    image_hash: None,
                    image_key: None,
                    image_nonce: None,
                    self_update_state: SelfUpdateState::Required,
                };
                storage.save_group(group).unwrap();

                // Save secrets for epochs 0-5
                for epoch in 0..=5u64 {
                    let secret = GroupExporterSecret {
                        mls_group_id: mls_group_id.clone(),
                        epoch,
                        secret: Secret::new([epoch as u8; 32]),
                    };
                    storage.save_group_exporter_secret(secret).unwrap();
                }
            }

            // Reopen and verify all secrets
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            for epoch in 0..=5u64 {
                let secret = storage2
                    .get_group_exporter_secret(&mls_group_id, epoch)
                    .unwrap()
                    .unwrap();
                assert_eq!(secret.epoch, epoch);
                assert_eq!(secret.secret[0], epoch as u8);
            }

            // Non-existent epoch should return None
            let missing = storage2
                .get_group_exporter_secret(&mls_group_id, 999)
                .unwrap();
            assert!(missing.is_none());
        }

        #[test]
        fn test_encrypted_storage_with_nested_directory() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir
                .path()
                .join("deep")
                .join("nested")
                .join("path")
                .join("db.sqlite");

            let config = EncryptionConfig::generate().unwrap();
            let storage = MdkSqliteStorage::new_with_key(&db_path, config);
            assert!(storage.is_ok());

            // Verify the nested directories were created
            assert!(db_path.parent().unwrap().exists());
            assert!(db_path.exists());

            // Verify the database is encrypted
            assert!(encryption::is_database_encrypted(&db_path).unwrap());
        }

        #[test]
        fn test_encrypted_unencrypted_incompatibility() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("compat_test.db");

            // First create an unencrypted database
            {
                let _storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();
            }

            // The database should NOT be encrypted
            assert!(!encryption::is_database_encrypted(&db_path).unwrap());

            // Now create an encrypted database at a different path
            let encrypted_path = temp_dir.path().join("compat_encrypted.db");
            {
                let config = EncryptionConfig::generate().unwrap();
                let _storage = MdkSqliteStorage::new_with_key(&encrypted_path, config).unwrap();
            }

            // The encrypted database SHOULD be encrypted
            assert!(encryption::is_database_encrypted(&encrypted_path).unwrap());
        }

        #[test]
        fn test_new_on_unencrypted_database_returns_correct_error() {
            // This test verifies that when MdkSqliteStorage::new() is called on an
            // existing unencrypted database (created with new_unencrypted()), the code
            // returns UnencryptedDatabaseWithEncryption rather than the misleading
            // KeyringEntryMissingForExistingDatabase error.

            // Initialize the mock keyring store for this test
            ensure_mock_store();

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("unencrypted_then_new.db");

            // Create an unencrypted database first
            {
                let _storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();
            }

            // Verify the database is unencrypted
            assert!(!encryption::is_database_encrypted(&db_path).unwrap());

            // Now try to open it with new() - should fail with UnencryptedDatabaseWithEncryption
            let result = MdkSqliteStorage::new(&db_path, "com.test.app", "test.key.id");

            assert!(result.is_err());
            match result {
                Err(Error::UnencryptedDatabaseWithEncryption) => {
                    // This is the expected error - the database was created unencrypted
                    // and we're trying to open it with the encrypted constructor
                }
                Err(Error::KeyringEntryMissingForExistingDatabase { .. }) => {
                    panic!(
                        "Got KeyringEntryMissingForExistingDatabase but should have gotten \
                         UnencryptedDatabaseWithEncryption. The database is unencrypted, not \
                         encrypted with a missing key."
                    );
                }
                Err(other) => {
                    panic!("Unexpected error: {:?}", other);
                }
                Ok(_) => {
                    panic!("Expected an error but got Ok");
                }
            }
        }

        #[test]
        fn test_new_with_key_on_unencrypted_database_returns_correct_error() {
            // This test verifies that when MdkSqliteStorage::new_with_key() is called on an
            // existing unencrypted database, the code returns UnencryptedDatabaseWithEncryption
            // rather than WrongEncryptionKey (which would be misleading).

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("unencrypted_then_new_with_key.db");

            // Create an unencrypted database first
            {
                let _storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();
            }

            // Verify the database is unencrypted
            assert!(!encryption::is_database_encrypted(&db_path).unwrap());

            // Now try to open it with new_with_key() - should fail with
            // UnencryptedDatabaseWithEncryption
            let config = EncryptionConfig::generate().unwrap();
            let result = MdkSqliteStorage::new_with_key(&db_path, config);

            assert!(result.is_err());
            match result {
                Err(Error::UnencryptedDatabaseWithEncryption) => {
                    // This is the expected error - the database was created unencrypted
                    // and we're trying to open it with an encryption key
                }
                Err(Error::WrongEncryptionKey) => {
                    panic!(
                        "Got WrongEncryptionKey but should have gotten \
                         UnencryptedDatabaseWithEncryption. The database is unencrypted, not \
                         encrypted with a different key."
                    );
                }
                Err(other) => {
                    panic!("Unexpected error: {:?}", other);
                }
                Ok(_) => {
                    panic!("Expected an error but got Ok");
                }
            }
        }

        #[test]
        fn test_encrypted_storage_large_data() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("large_data.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[99; 8]);

            // Create storage with a large message
            let large_content = "x".repeat(10_000);
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let mut group = create_test_group(mls_group_id.clone());
                group.name = "Large Data Test".to_string();
                group.description = "Testing large data".to_string();
                storage.save_group(group).unwrap();

                let event_id = EventId::all_zeros();
                let mut message = create_test_message(mls_group_id.clone(), event_id);
                message.content = large_content.clone();
                storage.save_message(message).unwrap();
            }

            // Reopen and verify
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let messages = storage2.messages(&mls_group_id, None).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, large_content);
        }

        #[test]
        fn test_encrypted_storage_concurrent_reads() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("concurrent.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[77; 8]);

            // Create and populate the database
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let mut group = create_test_group(mls_group_id.clone());
                group.name = "Concurrent Test".to_string();
                group.description = "Testing concurrent access".to_string();
                storage.save_group(group).unwrap();
            }

            // Open two connections simultaneously
            let config1 = EncryptionConfig::new(key);
            let config2 = EncryptionConfig::new(key);

            let storage1 = MdkSqliteStorage::new_with_key(&db_path, config1).unwrap();
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            // Both should be able to read
            let group1 = storage1
                .find_group_by_mls_group_id(&mls_group_id)
                .unwrap()
                .unwrap();
            let group2 = storage2
                .find_group_by_mls_group_id(&mls_group_id)
                .unwrap()
                .unwrap();

            assert_eq!(group1.name, group2.name);
        }

        #[cfg(unix)]
        #[test]
        fn test_encrypted_storage_sidecar_file_permissions() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("sidecar_test.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            // Create and use the database to trigger WAL file creation
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                // Create multiple groups to generate some WAL activity
                for i in 0..10 {
                    let mls_group_id = GroupId::from_slice(&[i; 8]);
                    let mut group = create_test_group(mls_group_id);
                    group.name = format!("Group {}", i);
                    group.description = format!("Description {}", i);
                    storage.save_group(group).unwrap();
                }
            }

            // Reopen to ensure any sidecar files exist
            let config2 = EncryptionConfig::new(key);
            let _storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            // Check main database file permissions
            let db_metadata = std::fs::metadata(&db_path).unwrap();
            let db_mode = db_metadata.permissions().mode();
            assert_eq!(
                db_mode & 0o077,
                0,
                "Database file should have owner-only permissions, got {:o}",
                db_mode & 0o777
            );

            // Check sidecar file permissions if they exist
            let sidecar_suffixes = ["-wal", "-shm", "-journal"];
            for suffix in &sidecar_suffixes {
                let sidecar_path = temp_dir.path().join(format!("sidecar_test.db{}", suffix));
                if sidecar_path.exists() {
                    let metadata = std::fs::metadata(&sidecar_path).unwrap();
                    let mode = metadata.permissions().mode();
                    assert_eq!(
                        mode & 0o077,
                        0,
                        "Sidecar file {} should have owner-only permissions, got {:o}",
                        suffix,
                        mode & 0o777
                    );
                }
            }
        }

        #[test]
        fn test_encryption_config_key_is_accessible() {
            let key = [0xDE; 32];
            let config = EncryptionConfig::new(key);

            // Verify we can access the key
            assert_eq!(config.key().len(), 32);
            assert_eq!(config.key()[0], 0xDE);
            assert_eq!(config.key()[31], 0xDE);
        }

        #[test]
        fn test_encrypted_storage_empty_group_name() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("empty_name.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[0xAB; 8]);

            // Create storage with empty name
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let mut group = create_test_group(mls_group_id.clone());
                group.name = String::new();
                group.description = String::new();
                storage.save_group(group).unwrap();
            }

            // Reopen and verify
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let group = storage2
                .find_group_by_mls_group_id(&mls_group_id)
                .unwrap()
                .unwrap();
            assert!(group.name.is_empty());
            assert!(group.description.is_empty());
        }

        #[test]
        fn test_encrypted_storage_unicode_content() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("unicode.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            let mls_group_id = GroupId::from_slice(&[0xCD; 8]);
            let unicode_content = "Hello 世界! 🎉 Ñoño مرحبا Привет 日本語 한국어 ελληνικά";

            // Create storage with unicode content
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();

                let mut group = create_test_group(mls_group_id.clone());
                group.name = "Тест группа 测试组".to_string();
                group.description = "描述 описание".to_string();
                storage.save_group(group).unwrap();

                let event_id = EventId::all_zeros();
                let mut message = create_test_message(mls_group_id.clone(), event_id);
                message.content = unicode_content.to_string();
                storage.save_message(message).unwrap();
            }

            // Reopen and verify
            let config2 = EncryptionConfig::new(key);
            let storage2 = MdkSqliteStorage::new_with_key(&db_path, config2).unwrap();

            let group = storage2
                .find_group_by_mls_group_id(&mls_group_id)
                .unwrap()
                .unwrap();
            assert_eq!(group.name, "Тест группа 测试组");
            assert_eq!(group.description, "描述 описание");

            let messages = storage2.messages(&mls_group_id, None).unwrap();
            assert_eq!(messages[0].content, unicode_content);
        }

        /// Test that opening an existing database fails when keyring entry is missing.
        ///
        /// This verifies the fix for the issue where a missing keyring entry would
        /// cause a new key to be generated instead of failing immediately.
        #[test]
        fn test_existing_db_with_missing_keyring_entry_fails() {
            ensure_mock_store();

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("missing_key_test.db");

            let service_id = "test.mdk.storage.missingkey";
            let db_key_id = "test.key.missingkeytest";

            // Clean up any existing key
            let _ = keyring::delete_db_key(service_id, db_key_id);

            // First, create an encrypted database using automatic key management
            {
                let storage = MdkSqliteStorage::new(&db_path, service_id, db_key_id);
                assert!(storage.is_ok(), "Should create database successfully");
            }

            // Verify database exists
            assert!(db_path.exists(), "Database file should exist");

            // Delete the keyring entry to simulate key loss
            keyring::delete_db_key(service_id, db_key_id).unwrap();

            // Verify keyring entry is gone
            let key_check = keyring::get_db_key(service_id, db_key_id).unwrap();
            assert!(key_check.is_none(), "Key should be deleted");

            // Now try to open the existing database - this should fail with a clear error
            // instead of generating a new key
            let result = MdkSqliteStorage::new(&db_path, service_id, db_key_id);

            assert!(result.is_err(), "Should fail when keyring entry is missing");

            match result {
                Err(error::Error::KeyringEntryMissingForExistingDatabase {
                    db_path: err_path,
                    service_id: err_service,
                    db_key_id: err_key,
                }) => {
                    assert!(
                        err_path.contains("missing_key_test.db"),
                        "Error should contain database path"
                    );
                    assert_eq!(err_service, service_id);
                    assert_eq!(err_key, db_key_id);
                }
                Err(e) => panic!(
                    "Expected KeyringEntryMissingForExistingDatabase error, got: {:?}",
                    e
                ),
                Ok(_) => panic!("Expected error but got success"),
            }

            // Verify that no new key was stored in the keyring
            let key_after = keyring::get_db_key(service_id, db_key_id).unwrap();
            assert!(
                key_after.is_none(),
                "No new key should have been stored in keyring"
            );
        }

        /// Test that creating a new database with automatic key management works.
        #[test]
        fn test_new_db_with_keyring_creates_key() {
            ensure_mock_store();

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("new_db_keyring.db");

            let service_id = "test.mdk.storage.newdb";
            let db_key_id = "test.key.newdbtest";

            // Clean up any existing key
            let _ = keyring::delete_db_key(service_id, db_key_id);

            // Verify database doesn't exist
            assert!(!db_path.exists(), "Database should not exist yet");

            // Create a new database - should succeed and create a key
            let storage = MdkSqliteStorage::new(&db_path, service_id, db_key_id);
            assert!(storage.is_ok(), "Should create database successfully");

            // Verify database exists
            assert!(db_path.exists(), "Database file should exist");

            // Verify key was stored
            let key = keyring::get_db_key(service_id, db_key_id).unwrap();
            assert!(key.is_some(), "Key should be stored in keyring");

            // Verify database is encrypted
            assert!(
                encryption::is_database_encrypted(&db_path).unwrap(),
                "Database should be encrypted"
            );

            // Clean up
            drop(storage);
            keyring::delete_db_key(service_id, db_key_id).unwrap();
        }

        /// Test that reopening a database with keyring works when the key is present.
        #[test]
        fn test_reopen_db_with_keyring_succeeds() {
            ensure_mock_store();

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("reopen_keyring.db");

            let service_id = "test.mdk.storage.reopen";
            let db_key_id = "test.key.reopentest";

            // Clean up any existing key
            let _ = keyring::delete_db_key(service_id, db_key_id);

            let mls_group_id = GroupId::from_slice(&[0xAA; 8]);

            // Create database and save a group
            {
                let storage = MdkSqliteStorage::new(&db_path, service_id, db_key_id).unwrap();

                let mut group = create_test_group(mls_group_id.clone());
                group.name = "Keyring Reopen Test".to_string();
                storage.save_group(group).unwrap();
            }

            // Reopen with the same keyring entry - should succeed
            let storage2 = MdkSqliteStorage::new(&db_path, service_id, db_key_id);
            assert!(storage2.is_ok(), "Should reopen database successfully");

            // Verify data persisted
            let storage2 = storage2.unwrap();
            let group = storage2
                .find_group_by_mls_group_id(&mls_group_id)
                .unwrap()
                .unwrap();
            assert_eq!(group.name, "Keyring Reopen Test");

            // Clean up
            drop(storage2);
            keyring::delete_db_key(service_id, db_key_id).unwrap();
        }

        /// Test concurrent access to encrypted database with same key.
        #[test]
        fn test_concurrent_encrypted_access_same_key() {
            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("concurrent_encrypted.db");

            let config = EncryptionConfig::generate().unwrap();
            let key = *config.key();

            // Create database with initial data
            {
                let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();
                let group = create_test_group(GroupId::from_slice(&[1, 2, 3, 4]));
                storage.save_group(group).unwrap();
            }

            // Spawn multiple threads that all read from the database
            let num_threads = 5;
            let handles: Vec<_> = (0..num_threads)
                .map(|_| {
                    let db_path = db_path.clone();
                    thread::spawn(move || {
                        let config = EncryptionConfig::new(key);
                        let storage = MdkSqliteStorage::new_with_key(&db_path, config).unwrap();
                        let groups = storage.all_groups().unwrap();
                        assert_eq!(groups.len(), 1);
                        groups
                    })
                })
                .collect();

            // All threads should succeed
            for handle in handles {
                let groups = handle.join().unwrap();
                assert_eq!(groups.len(), 1);
            }
        }

        /// Test multiple databases with different keys in same directory.
        #[test]
        fn test_multiple_encrypted_databases_different_keys() {
            let temp_dir = tempdir().unwrap();

            // Create multiple databases with different keys
            let db1_path = temp_dir.path().join("db1.db");
            let db2_path = temp_dir.path().join("db2.db");
            let db3_path = temp_dir.path().join("db3.db");

            let config1 = EncryptionConfig::generate().unwrap();
            let config2 = EncryptionConfig::generate().unwrap();
            let config3 = EncryptionConfig::generate().unwrap();

            let key1 = *config1.key();
            let key2 = *config2.key();
            let key3 = *config3.key();

            // Create and populate each database
            {
                let storage1 = MdkSqliteStorage::new_with_key(&db1_path, config1).unwrap();
                let mut group1 = create_test_group(GroupId::from_slice(&[1]));
                group1.name = "Database 1".to_string();
                storage1.save_group(group1).unwrap();

                let storage2 = MdkSqliteStorage::new_with_key(&db2_path, config2).unwrap();
                let mut group2 = create_test_group(GroupId::from_slice(&[2]));
                group2.name = "Database 2".to_string();
                storage2.save_group(group2).unwrap();

                let storage3 = MdkSqliteStorage::new_with_key(&db3_path, config3).unwrap();
                let mut group3 = create_test_group(GroupId::from_slice(&[3]));
                group3.name = "Database 3".to_string();
                storage3.save_group(group3).unwrap();
            }

            // Reopen each with correct key
            let config1_reopen = EncryptionConfig::new(key1);
            let config2_reopen = EncryptionConfig::new(key2);
            let config3_reopen = EncryptionConfig::new(key3);

            let storage1 = MdkSqliteStorage::new_with_key(&db1_path, config1_reopen).unwrap();
            let storage2 = MdkSqliteStorage::new_with_key(&db2_path, config2_reopen).unwrap();
            let storage3 = MdkSqliteStorage::new_with_key(&db3_path, config3_reopen).unwrap();

            // Verify each database has correct data
            let group1 = storage1
                .find_group_by_mls_group_id(&GroupId::from_slice(&[1]))
                .unwrap()
                .unwrap();
            assert_eq!(group1.name, "Database 1");

            let group2 = storage2
                .find_group_by_mls_group_id(&GroupId::from_slice(&[2]))
                .unwrap()
                .unwrap();
            assert_eq!(group2.name, "Database 2");

            let group3 = storage3
                .find_group_by_mls_group_id(&GroupId::from_slice(&[3]))
                .unwrap()
                .unwrap();
            assert_eq!(group3.name, "Database 3");

            // Verify wrong keys don't work
            let wrong_config = EncryptionConfig::new(key1);
            let result = MdkSqliteStorage::new_with_key(&db2_path, wrong_config);
            assert!(result.is_err());
        }
    }

    // ========================================
    // Migration Tests (Phase 5)
    // ========================================

    mod migration_tests {
        use super::*;

        #[test]
        fn test_fresh_database_has_all_tables() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Expected MDK tables
            let expected_mdk_tables = [
                "groups",
                "group_relays",
                "group_exporter_secrets",
                "messages",
                "processed_messages",
                "welcomes",
                "processed_welcomes",
            ];

            // Expected OpenMLS tables
            let expected_openmls_tables = [
                "openmls_group_data",
                "openmls_proposals",
                "openmls_own_leaf_nodes",
                "openmls_key_packages",
                "openmls_psks",
                "openmls_signature_keys",
                "openmls_encryption_keys",
                "openmls_epoch_key_pairs",
            ];

            storage.with_connection(|conn| {
                // Get all table names
                let mut stmt = conn
                    .prepare(
                        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                    )
                    .unwrap();
                let table_names: Vec<String> = stmt
                    .query_map([], |row| row.get(0))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();

                // Check MDK tables
                for table in &expected_mdk_tables {
                    assert!(
                        table_names.contains(&table.to_string()),
                        "Missing MDK table: {}",
                        table
                    );
                }

                // Check OpenMLS tables
                for table in &expected_openmls_tables {
                    assert!(
                        table_names.contains(&table.to_string()),
                        "Missing OpenMLS table: {}",
                        table
                    );
                }
            });
        }

        #[test]
        fn test_all_indexes_exist() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Expected indexes (actual names from schema)
            let expected_indexes = [
                "idx_groups_nostr_group_id",
                "idx_group_relays_mls_group_id",
                "idx_group_exporter_secrets_mls_group_id",
                "idx_messages_mls_group_id",
                "idx_messages_wrapper_event_id",
                "idx_messages_created_at",
                "idx_messages_pubkey",
                "idx_messages_kind",
                "idx_messages_state",
                "idx_processed_messages_message_event_id",
                "idx_processed_messages_state",
                "idx_processed_messages_processed_at",
                "idx_welcomes_mls_group_id",
                "idx_welcomes_wrapper_event_id",
                "idx_welcomes_state",
                "idx_welcomes_nostr_group_id",
                "idx_processed_welcomes_welcome_event_id",
                "idx_processed_welcomes_state",
                "idx_processed_welcomes_processed_at",
            ];

            storage.with_connection(|conn| {
                let mut stmt = conn
                    .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'")
                    .unwrap();
                let index_names: Vec<String> = stmt
                    .query_map([], |row| row.get(0))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();

                for idx in &expected_indexes {
                    assert!(
                        index_names.contains(&idx.to_string()),
                        "Missing index: {}. Found indexes: {:?}",
                        idx,
                        index_names
                    );
                }
            });
        }

        #[test]
        fn test_foreign_key_constraints_work() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            storage.with_connection(|conn| {
                // Verify foreign keys are enabled
                let fk_enabled: i32 = conn
                    .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(fk_enabled, 1, "Foreign keys should be enabled");

                // Try to insert a group_relay without a group (should fail)
                let result = conn.execute(
                    "INSERT INTO group_relays (mls_group_id, relay_url) VALUES (?, ?)",
                    rusqlite::params![vec![1u8, 2u8, 3u8, 4u8], "wss://relay.example.com"],
                );
                assert!(result.is_err(), "Should fail due to foreign key constraint");
            });
        }

        #[test]
        fn test_openmls_group_data_check_constraint() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            storage.with_connection(|conn| {
                // Valid data_type should succeed
                let valid_result = conn.execute(
                    "INSERT INTO openmls_group_data (provider_version, group_id, data_type, group_data) VALUES (?, ?, ?, ?)",
                    rusqlite::params![1, vec![1u8, 2u8, 3u8], "tree", vec![4u8, 5u8, 6u8]],
                );
                assert!(valid_result.is_ok(), "Valid data_type should succeed");

                // Invalid data_type should fail
                let invalid_result = conn.execute(
                    "INSERT INTO openmls_group_data (provider_version, group_id, data_type, group_data) VALUES (?, ?, ?, ?)",
                    rusqlite::params![1, vec![7u8, 8u8, 9u8], "invalid_type", vec![10u8, 11u8]],
                );
                assert!(
                    invalid_result.is_err(),
                    "Invalid data_type should fail CHECK constraint"
                );
            });
        }

        #[test]
        fn test_schema_matches_plan_specification() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            storage.with_connection(|conn| {
                // Check groups table has all required columns
                let groups_info: Vec<(String, String)> = conn
                    .prepare("PRAGMA table_info(groups)")
                    .unwrap()
                    .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();

                let groups_columns: Vec<&str> =
                    groups_info.iter().map(|(n, _)| n.as_str()).collect();
                assert!(groups_columns.contains(&"mls_group_id"));
                assert!(groups_columns.contains(&"nostr_group_id"));
                assert!(groups_columns.contains(&"name"));
                assert!(groups_columns.contains(&"description"));
                assert!(groups_columns.contains(&"admin_pubkeys"));
                assert!(groups_columns.contains(&"epoch"));
                assert!(groups_columns.contains(&"state"));

                // Check messages table has all required columns
                let messages_info: Vec<String> = conn
                    .prepare("PRAGMA table_info(messages)")
                    .unwrap()
                    .query_map([], |row| row.get(1))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();

                assert!(messages_info.contains(&"mls_group_id".to_string()));
                assert!(messages_info.contains(&"id".to_string()));
                assert!(messages_info.contains(&"pubkey".to_string()));
                assert!(messages_info.contains(&"kind".to_string()));
                assert!(messages_info.contains(&"created_at".to_string()));
                assert!(messages_info.contains(&"content".to_string()));
                assert!(messages_info.contains(&"wrapper_event_id".to_string()));
            });
        }
    }

    // ========================================
    // Snapshot tests
    // ========================================

    mod snapshot_tests {
        use std::collections::BTreeSet;

        use mdk_storage_traits::groups::GroupStorage;
        use mdk_storage_traits::groups::types::{
            Group, GroupExporterSecret, GroupState, SelfUpdateState,
        };
        use mdk_storage_traits::{GroupId, MdkStorageProvider, Secret};

        use super::*;

        fn create_test_group(id: u8) -> Group {
            Group {
                mls_group_id: GroupId::from_slice(&[id; 32]),
                nostr_group_id: [id; 32],
                name: format!("Test Group {}", id),
                description: format!("Description {}", id),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 0,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            }
        }

        #[test]
        fn test_snapshot_and_rollback_group_state() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Create and save a group
            let group = create_test_group(1);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Verify initial state
            let initial_group = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(initial_group.name, "Test Group 1");
            assert_eq!(initial_group.epoch, 0);

            // Create a snapshot
            storage
                .create_group_snapshot(&group_id, "snap_epoch_0")
                .unwrap();

            // Modify the group
            let mut modified_group = initial_group.clone();
            modified_group.name = "Modified Group".to_string();
            modified_group.epoch = 1;
            storage.save_group(modified_group).unwrap();

            // Verify modification
            let after_mod = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(after_mod.name, "Modified Group");
            assert_eq!(after_mod.epoch, 1);

            // Rollback to snapshot
            storage
                .rollback_group_to_snapshot(&group_id, "snap_epoch_0")
                .unwrap();

            // Verify rollback restored original state
            let after_rollback = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(after_rollback.name, "Test Group 1");
            assert_eq!(after_rollback.epoch, 0);
        }

        #[test]
        fn test_snapshot_release_without_rollback() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Create and save a group
            let group = create_test_group(2);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Create a snapshot
            storage
                .create_group_snapshot(&group_id, "snap_to_release")
                .unwrap();

            // Modify the group
            let mut modified = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            modified.name = "Modified Name".to_string();
            storage.save_group(modified).unwrap();

            // Release the snapshot (commit the changes)
            storage
                .release_group_snapshot(&group_id, "snap_to_release")
                .unwrap();

            // Verify modifications are kept
            let final_state = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(final_state.name, "Modified Name");
        }

        #[test]
        fn test_snapshot_with_exporter_secrets_rollback() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Create and save a group
            let group = create_test_group(3);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Save initial exporter secret
            let secret_0 = GroupExporterSecret {
                mls_group_id: group_id.clone(),
                epoch: 0,
                secret: Secret::new([0u8; 32]),
            };
            storage.save_group_exporter_secret(secret_0).unwrap();

            // Create snapshot
            storage
                .create_group_snapshot(&group_id, "snap_secrets")
                .unwrap();

            // Add more exporter secrets
            let secret_1 = GroupExporterSecret {
                mls_group_id: group_id.clone(),
                epoch: 1,
                secret: Secret::new([1u8; 32]),
            };
            storage.save_group_exporter_secret(secret_1).unwrap();

            // Verify new secret exists
            let secret_check = storage.get_group_exporter_secret(&group_id, 1).unwrap();
            assert!(secret_check.is_some());

            // Rollback
            storage
                .rollback_group_to_snapshot(&group_id, "snap_secrets")
                .unwrap();

            // Epoch 1 secret should be gone after rollback
            let after_rollback = storage.get_group_exporter_secret(&group_id, 1).unwrap();
            assert!(after_rollback.is_none());

            // Epoch 0 secret should still exist
            let epoch_0 = storage.get_group_exporter_secret(&group_id, 0).unwrap();
            assert!(epoch_0.is_some());
        }

        #[test]
        fn test_snapshot_isolation_between_groups() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            // Create two groups
            let group1 = create_test_group(10);
            let group2 = create_test_group(20);
            let group1_id = group1.mls_group_id.clone();
            let group2_id = group2.mls_group_id.clone();

            storage.save_group(group1).unwrap();
            storage.save_group(group2).unwrap();

            // Snapshot group1
            storage
                .create_group_snapshot(&group1_id, "snap_group1")
                .unwrap();

            // Modify both groups
            let mut mod1 = storage
                .find_group_by_mls_group_id(&group1_id)
                .unwrap()
                .unwrap();
            let mut mod2 = storage
                .find_group_by_mls_group_id(&group2_id)
                .unwrap()
                .unwrap();
            mod1.name = "Modified Group 1".to_string();
            mod2.name = "Modified Group 2".to_string();
            storage.save_group(mod1).unwrap();
            storage.save_group(mod2).unwrap();

            // Rollback group1 only
            storage
                .rollback_group_to_snapshot(&group1_id, "snap_group1")
                .unwrap();

            // Group1 should be rolled back
            let final1 = storage
                .find_group_by_mls_group_id(&group1_id)
                .unwrap()
                .unwrap();
            assert_eq!(final1.name, "Test Group 10");

            // Group2 should still have modifications
            let final2 = storage
                .find_group_by_mls_group_id(&group2_id)
                .unwrap()
                .unwrap();
            assert_eq!(final2.name, "Modified Group 2");
        }

        #[test]
        fn test_rollback_nonexistent_snapshot_returns_error() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(5);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group.clone()).unwrap();

            // Rolling back to a nonexistent snapshot should return an error
            // and NOT delete any data. This prevents accidental data loss
            // from typos in snapshot names.
            // This matches the memory storage behavior.
            let result = storage.rollback_group_to_snapshot(&group_id, "nonexistent_snap");
            assert!(
                result.is_err(),
                "Rollback to nonexistent snapshot should return an error"
            );

            // CRITICAL: Group should still exist (no data was deleted)
            let after_rollback = storage.find_group_by_mls_group_id(&group_id).unwrap();
            assert!(
                after_rollback.is_some(),
                "Group should NOT be deleted when rolling back to nonexistent snapshot"
            );
            assert_eq!(
                after_rollback.unwrap().epoch,
                group.epoch,
                "Group data should be unchanged"
            );
        }

        #[test]
        fn test_release_nonexistent_snapshot_succeeds() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(6);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Releasing a non-existent snapshot should be a no-op (idempotent)
            let result = storage.release_group_snapshot(&group_id, "nonexistent_snap");
            // Should succeed (no-op)
            assert!(result.is_ok());
        }

        #[test]
        fn test_multiple_snapshots_same_group() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(7);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Create first snapshot at epoch 0
            storage
                .create_group_snapshot(&group_id, "snap_epoch_0")
                .unwrap();

            // Modify to epoch 1
            let mut mod1 = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            mod1.epoch = 1;
            mod1.name = "Epoch 1".to_string();
            storage.save_group(mod1).unwrap();

            // Create second snapshot at epoch 1
            storage
                .create_group_snapshot(&group_id, "snap_epoch_1")
                .unwrap();

            // Modify to epoch 2
            let mut mod2 = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            mod2.epoch = 2;
            mod2.name = "Epoch 2".to_string();
            storage.save_group(mod2).unwrap();

            // Rollback to epoch 1 snapshot
            storage
                .rollback_group_to_snapshot(&group_id, "snap_epoch_1")
                .unwrap();

            let after_rollback = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(after_rollback.epoch, 1);
            assert_eq!(after_rollback.name, "Epoch 1");

            // Can still rollback further to epoch 0
            storage
                .rollback_group_to_snapshot(&group_id, "snap_epoch_0")
                .unwrap();

            let final_state = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(final_state.epoch, 0);
            assert_eq!(final_state.name, "Test Group 7");
        }

        #[test]
        fn test_list_group_snapshots_empty() {
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("list_snapshots_empty.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

            let snapshots = storage.list_group_snapshots(&group_id).unwrap();
            assert!(
                snapshots.is_empty(),
                "Should return empty list for no snapshots"
            );
        }

        #[test]
        fn test_list_group_snapshots_returns_snapshots_sorted_by_created_at() {
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("list_snapshots_sorted.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group_id = GroupId::from_slice(&[8; 32]);
            let nostr_group_id: [u8; 32] = [9; 32];

            // Create a group first
            let group = Group {
                mls_group_id: group_id.clone(),
                nostr_group_id,
                name: "Test Group".to_string(),
                description: "".to_string(),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 1,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            };
            storage.save_group(group).unwrap();

            // Create snapshots - they will have sequential timestamps
            storage
                .create_group_snapshot(&group_id, "snap_first")
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
            storage
                .create_group_snapshot(&group_id, "snap_second")
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
            storage
                .create_group_snapshot(&group_id, "snap_third")
                .unwrap();

            let result = storage.list_group_snapshots(&group_id).unwrap();

            assert_eq!(result.len(), 3);
            // Should be sorted by created_at ascending
            assert_eq!(result[0].0, "snap_first");
            assert_eq!(result[1].0, "snap_second");
            assert_eq!(result[2].0, "snap_third");
            // Verify timestamps are increasing
            assert!(result[0].1 <= result[1].1);
            assert!(result[1].1 <= result[2].1);
        }

        #[test]
        fn test_list_group_snapshots_only_returns_matching_group() {
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("list_snapshots_filtered.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group1 = GroupId::from_slice(&[1; 32]);
            let group2 = GroupId::from_slice(&[2; 32]);

            // Create groups
            let g1 = Group {
                mls_group_id: group1.clone(),
                nostr_group_id: [11; 32],
                name: "Group 1".to_string(),
                description: "".to_string(),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 1,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            };
            let g2 = Group {
                mls_group_id: group2.clone(),
                nostr_group_id: [22; 32],
                name: "Group 2".to_string(),
                ..g1.clone()
            };
            storage.save_group(g1).unwrap();
            storage.save_group(g2).unwrap();

            // Create snapshots for each group
            storage.create_group_snapshot(&group1, "snap_g1").unwrap();
            storage.create_group_snapshot(&group2, "snap_g2").unwrap();

            let result1 = storage.list_group_snapshots(&group1).unwrap();
            let result2 = storage.list_group_snapshots(&group2).unwrap();

            assert_eq!(result1.len(), 1);
            assert_eq!(result1[0].0, "snap_g1");

            assert_eq!(result2.len(), 1);
            assert_eq!(result2[0].0, "snap_g2");
        }

        #[test]
        fn test_prune_expired_snapshots_removes_old_snapshots() {
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("prune_expired.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group_id = GroupId::from_slice(&[3; 32]);

            let group = Group {
                mls_group_id: group_id.clone(),
                nostr_group_id: [33; 32],
                name: "Test Group".to_string(),
                description: "".to_string(),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 1,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            };
            storage.save_group(group).unwrap();

            // Create a snapshot
            storage
                .create_group_snapshot(&group_id, "old_snap")
                .unwrap();

            // Get the snapshot's timestamp
            let snapshots_before = storage.list_group_snapshots(&group_id).unwrap();
            assert_eq!(snapshots_before.len(), 1);
            let old_ts = snapshots_before[0].1;

            // Prune with a threshold in the future - should prune the snapshot
            let pruned = storage.prune_expired_snapshots(old_ts + 1).unwrap();
            assert_eq!(pruned, 1, "Should have pruned 1 snapshot");

            let remaining = storage.list_group_snapshots(&group_id).unwrap();
            assert!(remaining.is_empty());
        }

        #[test]
        fn test_prune_expired_snapshots_keeps_recent_snapshots() {
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("prune_keeps_recent.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group_id = GroupId::from_slice(&[4; 32]);

            let group = Group {
                mls_group_id: group_id.clone(),
                nostr_group_id: [44; 32],
                name: "Test Group".to_string(),
                description: "".to_string(),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 1,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            };
            storage.save_group(group).unwrap();

            // Create a snapshot
            storage
                .create_group_snapshot(&group_id, "recent_snap")
                .unwrap();

            // Prune with threshold 0 - should keep everything
            let pruned = storage.prune_expired_snapshots(0).unwrap();
            assert_eq!(pruned, 0, "Should have pruned 0 snapshots");

            let remaining = storage.list_group_snapshots(&group_id).unwrap();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].0, "recent_snap");
        }

        #[test]
        fn test_prune_expired_snapshots_with_cascade_delete() {
            // This test verifies that pruning removes all related snapshot data
            // (the CASCADE DELETE on the FK should handle this)
            use mdk_storage_traits::MdkStorageProvider;

            let temp_dir = tempdir().unwrap();
            let db_path = temp_dir.path().join("prune_cascade.db");
            let storage = MdkSqliteStorage::new_unencrypted(&db_path).unwrap();

            let group_id = GroupId::from_slice(&[5; 32]);

            let group = Group {
                mls_group_id: group_id.clone(),
                nostr_group_id: [55; 32],
                name: "Test Group".to_string(),
                description: "".to_string(),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 1,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            };
            storage.save_group(group).unwrap();

            // Create snapshot (this creates both group_state_snapshots header and data rows)
            storage
                .create_group_snapshot(&group_id, "to_prune")
                .unwrap();

            // Verify snapshot exists
            let before = storage.list_group_snapshots(&group_id).unwrap();
            assert_eq!(before.len(), 1);

            // Get timestamp and prune
            let ts = before[0].1;
            let pruned = storage.prune_expired_snapshots(ts + 1).unwrap();
            assert_eq!(pruned, 1);

            // Verify snapshot is completely gone
            let after = storage.list_group_snapshots(&group_id).unwrap();
            assert!(after.is_empty());

            // Attempting to rollback should fail (no snapshot exists)
            let rollback_result = storage.rollback_group_to_snapshot(&group_id, "to_prune");
            assert!(rollback_result.is_err());
        }
    }

    // ========================================
    // Snapshot tests for OpenMLS data
    // ========================================
    // These tests verify that snapshots correctly capture and restore OpenMLS
    // cryptographic state that is written through the StorageProvider trait
    // (which uses MlsCodec::serialize for group_id keys). The existing snapshot
    // tests only exercise MDK-level data (groups, relays, exporter secrets),
    // missing the fact that OpenMLS tables store group_id as MlsCodec-encoded bytes
    // while the snapshot code queries them with raw bytes from as_slice().

    mod snapshot_openmls_tests {
        use std::collections::BTreeSet;

        use mdk_storage_traits::groups::GroupStorage;
        use mdk_storage_traits::groups::types::{Group, GroupState};
        use mdk_storage_traits::mls_codec::MlsCodec;
        use mdk_storage_traits::{GroupId, MdkStorageProvider};
        use rusqlite::params;

        use super::*;

        /// Helper: create a minimal test group for snapshot tests.
        fn create_test_group(id: u8) -> Group {
            Group {
                mls_group_id: GroupId::from_slice(&[id; 32]),
                nostr_group_id: [id; 32],
                name: format!("Test Group {}", id),
                description: format!("Description {}", id),
                admin_pubkeys: BTreeSet::new(),
                last_message_id: None,
                last_message_at: None,
                last_message_processed_at: None,
                epoch: 0,
                state: GroupState::Active,
                image_hash: None,
                image_key: None,
                image_nonce: None,
                self_update_state: SelfUpdateState::Required,
            }
        }

        /// Helper: count rows in an OpenMLS table for a given group_id using
        /// MlsCodec-serialized key (the way OpenMLS actually stores them).
        fn count_openmls_rows(storage: &MdkSqliteStorage, table: &str, group_id: &GroupId) -> i64 {
            let mls_key = MlsCodec::serialize(group_id).unwrap();
            storage.with_connection(|conn| {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM {} WHERE group_id = ?", table),
                    params![mls_key],
                    |row| row.get(0),
                )
                .unwrap()
            })
        }

        /// Helper: count snapshot rows for a given table_name.
        fn count_snapshot_rows_for_table(
            storage: &MdkSqliteStorage,
            snapshot_name: &str,
            group_id: &GroupId,
            table_name: &str,
        ) -> i64 {
            let raw_bytes = group_id.as_slice().to_vec();
            storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM group_state_snapshots
                     WHERE snapshot_name = ? AND group_id = ? AND table_name = ?",
                    params![snapshot_name, raw_bytes, table_name],
                    |row| row.get(0),
                )
                .unwrap()
            })
        }

        /// Helper: insert OpenMLS data via the same code path as StorageProvider.
        ///
        /// This uses `MlsCodec::serialize()` for the group_id key, exactly as
        /// the `write_group_data`, `append_own_leaf_node`, `queue_proposal`, and
        /// `write_encryption_epoch_key_pairs` functions do in `mls_storage/mod.rs`.
        fn seed_openmls_data(storage: &MdkSqliteStorage, group_id: &GroupId) {
            let mls_key = MlsCodec::serialize(group_id).unwrap();
            storage.with_connection(|conn| {
                // openmls_group_data — written by write_group_data() via serialize_key()
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_group_data
                     (group_id, data_type, group_data, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, "group_state", b"test_crypto_state" as &[u8], 1i32],
                )
                .unwrap();

                conn.execute(
                    "INSERT OR REPLACE INTO openmls_group_data
                     (group_id, data_type, group_data, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, "tree", b"test_tree_data" as &[u8], 1i32],
                )
                .unwrap();

                // openmls_own_leaf_nodes — written by append_own_leaf_node() via serialize_key()
                conn.execute(
                    "INSERT INTO openmls_own_leaf_nodes
                     (group_id, leaf_node, provider_version)
                     VALUES (?, ?, ?)",
                    params![mls_key, b"test_leaf_node" as &[u8], 1i32],
                )
                .unwrap();

                // openmls_proposals — written by queue_proposal() via serialize_key()
                let proposal_ref = MlsCodec::serialize(&vec![10u8, 20, 30]).unwrap();
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_proposals
                     (group_id, proposal_ref, proposal, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, proposal_ref, b"test_proposal" as &[u8], 1i32],
                )
                .unwrap();

                // openmls_epoch_key_pairs — written by write_encryption_epoch_key_pairs()
                let epoch_key = MlsCodec::serialize(&5u64).unwrap();
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_epoch_key_pairs
                     (group_id, epoch_id, leaf_index, key_pairs, provider_version)
                     VALUES (?, ?, ?, ?, ?)",
                    params![mls_key, epoch_key, 0i32, b"test_key_pairs" as &[u8], 1i32],
                )
                .unwrap();
            });
        }

        /// Snapshot must capture openmls_group_data rows written via StorageProvider.
        ///
        /// The StorageProvider writes group_id using `MlsCodec::serialize()` which
        /// produces serialized bytes distinct from the raw group_id. The snapshot
        /// code queries these tables using `group_id.as_slice()` (raw bytes), so
        /// the WHERE clause never matches and zero rows are captured.
        #[test]
        fn test_snapshot_captures_openmls_group_data() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(1);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            // Write OpenMLS data the way StorageProvider does (MlsCodec keys)
            seed_openmls_data(&storage, &group_id);

            // Verify data exists in the tables
            assert_eq!(
                count_openmls_rows(&storage, "openmls_group_data", &group_id),
                2,
                "openmls_group_data should have 2 rows (group_state + tree)"
            );

            // Take a snapshot
            storage
                .create_group_snapshot(&group_id, "snap_mls")
                .unwrap();

            // Verify snapshot captured the OpenMLS group data rows
            let snap_count = count_snapshot_rows_for_table(
                &storage,
                "snap_mls",
                &group_id,
                "openmls_group_data",
            );
            assert_eq!(
                snap_count, 2,
                "Snapshot must capture openmls_group_data rows written via StorageProvider \
                 (MlsCodec-serialized group_id keys)"
            );
        }

        /// Snapshot must capture openmls_own_leaf_nodes written via StorageProvider.
        #[test]
        fn test_snapshot_captures_openmls_own_leaf_nodes() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(2);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            seed_openmls_data(&storage, &group_id);

            assert_eq!(
                count_openmls_rows(&storage, "openmls_own_leaf_nodes", &group_id),
                1,
                "openmls_own_leaf_nodes should have 1 row"
            );

            storage
                .create_group_snapshot(&group_id, "snap_leaf")
                .unwrap();

            let snap_count = count_snapshot_rows_for_table(
                &storage,
                "snap_leaf",
                &group_id,
                "openmls_own_leaf_nodes",
            );
            assert_eq!(
                snap_count, 1,
                "Snapshot must capture openmls_own_leaf_nodes rows written via \
                 StorageProvider"
            );
        }

        /// Snapshot must capture openmls_proposals written via StorageProvider.
        #[test]
        fn test_snapshot_captures_openmls_proposals() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(3);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            seed_openmls_data(&storage, &group_id);

            assert_eq!(
                count_openmls_rows(&storage, "openmls_proposals", &group_id),
                1,
                "openmls_proposals should have 1 row"
            );

            storage
                .create_group_snapshot(&group_id, "snap_prop")
                .unwrap();

            let snap_count = count_snapshot_rows_for_table(
                &storage,
                "snap_prop",
                &group_id,
                "openmls_proposals",
            );
            assert_eq!(
                snap_count, 1,
                "Snapshot must capture openmls_proposals rows written via \
                 StorageProvider"
            );
        }

        /// Snapshot must capture openmls_epoch_key_pairs written via StorageProvider.
        #[test]
        fn test_snapshot_captures_openmls_epoch_key_pairs() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(4);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            seed_openmls_data(&storage, &group_id);

            assert_eq!(
                count_openmls_rows(&storage, "openmls_epoch_key_pairs", &group_id),
                1,
                "openmls_epoch_key_pairs should have 1 row"
            );

            storage
                .create_group_snapshot(&group_id, "snap_epoch")
                .unwrap();

            let snap_count = count_snapshot_rows_for_table(
                &storage,
                "snap_epoch",
                &group_id,
                "openmls_epoch_key_pairs",
            );
            assert_eq!(
                snap_count, 1,
                "Snapshot must capture openmls_epoch_key_pairs rows written via \
                 StorageProvider"
            );
        }

        /// Rollback must restore OpenMLS group_data to pre-modification state.
        ///
        /// This simulates the MIP-03 epoch rollback flow:
        /// 1. Write MLS data at epoch N (via StorageProvider / MlsCodec)
        /// 2. Take snapshot
        /// 3. Advance MLS data to epoch N+1
        /// 4. Rollback to snapshot
        /// 5. Verify MLS data is back to epoch N state
        ///
        /// Due to the group_id encoding mismatch, the snapshot captures zero
        /// OpenMLS rows, so rollback has nothing to restore — the crypto state
        /// from epoch N+1 persists (or worse, gets deleted with nothing to
        /// replace it), creating a metadata/crypto split-brain.
        #[test]
        fn test_rollback_restores_openmls_group_data() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(5);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            let mls_key = MlsCodec::serialize(&group_id).unwrap();

            // Write epoch 5 OpenMLS state
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_group_data
                     (group_id, data_type, group_data, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, "group_state", b"epoch5_crypto" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Snapshot at epoch 5
            storage.create_group_snapshot(&group_id, "snap_e5").unwrap();

            // Advance to epoch 6 (simulate processing a commit)
            storage.with_connection(|conn| {
                conn.execute(
                    "UPDATE openmls_group_data SET group_data = ?
                     WHERE group_id = ? AND data_type = ?",
                    params![b"epoch6_crypto" as &[u8], mls_key, "group_state"],
                )
                .unwrap();
            });

            // Verify we're at epoch 6
            let crypto_before_rollback: Vec<u8> = storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ?",
                    params![mls_key, "group_state"],
                    |row| row.get(0),
                )
                .unwrap()
            });
            assert_eq!(crypto_before_rollback, b"epoch6_crypto");

            // Rollback to snapshot
            storage
                .rollback_group_to_snapshot(&group_id, "snap_e5")
                .unwrap();

            // Verify OpenMLS data was restored to epoch 5 state
            let crypto_after_rollback: Vec<u8> = storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ?",
                    params![mls_key, "group_state"],
                    |row| row.get(0),
                )
                .unwrap()
            });
            assert_eq!(
                crypto_after_rollback, b"epoch5_crypto",
                "Rollback must restore openmls_group_data to the snapshot state. \
                 If this is epoch6_crypto, the snapshot failed to capture the \
                 OpenMLS rows due to group_id encoding mismatch \
                 (as_slice vs MlsCodec::serialize)."
            );
        }

        /// Rollback must restore openmls_epoch_key_pairs to pre-modification state.
        #[test]
        fn test_rollback_restores_openmls_epoch_key_pairs() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(6);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            let mls_key = MlsCodec::serialize(&group_id).unwrap();
            let epoch_key = MlsCodec::serialize(&5u64).unwrap();

            // Write epoch 5 key pairs
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_epoch_key_pairs
                     (group_id, epoch_id, leaf_index, key_pairs, provider_version)
                     VALUES (?, ?, ?, ?, ?)",
                    params![mls_key, epoch_key, 0i32, b"epoch5_keys" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Snapshot
            storage
                .create_group_snapshot(&group_id, "snap_keys")
                .unwrap();

            // Advance key pairs to epoch 6
            storage.with_connection(|conn| {
                conn.execute(
                    "UPDATE openmls_epoch_key_pairs SET key_pairs = ?
                     WHERE group_id = ? AND epoch_id = ? AND leaf_index = ?",
                    params![b"epoch6_keys" as &[u8], mls_key, epoch_key, 0i32],
                )
                .unwrap();
            });

            // Rollback
            storage
                .rollback_group_to_snapshot(&group_id, "snap_keys")
                .unwrap();

            // Verify restoration
            let keys_after: Vec<u8> = storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT key_pairs FROM openmls_epoch_key_pairs
                     WHERE group_id = ? AND epoch_id = ? AND leaf_index = ?",
                    params![mls_key, epoch_key, 0i32],
                    |row| row.get(0),
                )
                .unwrap()
            });
            assert_eq!(
                keys_after, b"epoch5_keys",
                "Rollback must restore openmls_epoch_key_pairs to snapshot state"
            );
        }

        /// Rollback must restore openmls_own_leaf_nodes to pre-modification state.
        #[test]
        fn test_rollback_restores_openmls_own_leaf_nodes() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(7);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            let mls_key = MlsCodec::serialize(&group_id).unwrap();

            // Write initial leaf node
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT INTO openmls_own_leaf_nodes
                     (group_id, leaf_node, provider_version)
                     VALUES (?, ?, ?)",
                    params![mls_key, b"original_leaf" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Snapshot
            storage
                .create_group_snapshot(&group_id, "snap_leaf")
                .unwrap();

            // Modify: add a second leaf node (simulating post-commit state)
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT INTO openmls_own_leaf_nodes
                     (group_id, leaf_node, provider_version)
                     VALUES (?, ?, ?)",
                    params![mls_key, b"added_after_snapshot" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Verify 2 leaf nodes exist before rollback
            assert_eq!(
                count_openmls_rows(&storage, "openmls_own_leaf_nodes", &group_id),
                2
            );

            // Rollback
            storage
                .rollback_group_to_snapshot(&group_id, "snap_leaf")
                .unwrap();

            // Verify only the original leaf node remains
            let leaf_count = count_openmls_rows(&storage, "openmls_own_leaf_nodes", &group_id);
            assert_eq!(
                leaf_count, 1,
                "Rollback must restore openmls_own_leaf_nodes to snapshot state \
                 (1 leaf, not 2)"
            );

            let leaf_data: Vec<u8> = storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT leaf_node FROM openmls_own_leaf_nodes
                     WHERE group_id = ? AND provider_version = ?",
                    params![mls_key, 1i32],
                    |row| row.get(0),
                )
                .unwrap()
            });
            assert_eq!(
                leaf_data, b"original_leaf",
                "Rollback must restore the original leaf node data"
            );
        }

        /// Full MIP-03 rollback simulation: metadata and crypto state must be
        /// consistent after rollback.
        ///
        /// This is the most critical test — it proves the split-brain condition
        /// where MDK metadata (groups.epoch) gets rolled back but OpenMLS
        /// cryptographic state is left at the wrong epoch.
        #[test]
        fn test_rollback_metadata_crypto_consistency() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(8);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            let mls_key = MlsCodec::serialize(&group_id).unwrap();

            // Set up epoch 5 state: both MDK metadata and OpenMLS crypto
            {
                let mut g = storage
                    .find_group_by_mls_group_id(&group_id)
                    .unwrap()
                    .unwrap();
                g.epoch = 5;
                storage.save_group(g).unwrap();
            }
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_group_data
                     (group_id, data_type, group_data, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, "group_state", b"epoch5_state" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Snapshot at epoch 5
            storage
                .create_group_snapshot(&group_id, "snap_epoch5")
                .unwrap();

            // Advance to epoch 6 (simulate processing a commit)
            {
                let mut g = storage
                    .find_group_by_mls_group_id(&group_id)
                    .unwrap()
                    .unwrap();
                g.epoch = 6;
                storage.save_group(g).unwrap();
            }
            storage.with_connection(|conn| {
                conn.execute(
                    "UPDATE openmls_group_data SET group_data = ?
                     WHERE group_id = ? AND data_type = ?",
                    params![b"epoch6_state" as &[u8], mls_key, "group_state"],
                )
                .unwrap();
            });

            // MIP-03: detect conflict, rollback to epoch 5
            storage
                .rollback_group_to_snapshot(&group_id, "snap_epoch5")
                .unwrap();

            // Check MDK metadata
            let group_after = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                group_after.epoch, 5,
                "MDK groups.epoch should be 5 after rollback"
            );

            // Check OpenMLS crypto state
            let crypto_after: Vec<u8> = storage.with_connection(|conn| {
                conn.query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ?",
                    params![mls_key, "group_state"],
                    |row| row.get(0),
                )
                .unwrap()
            });
            assert_eq!(
                crypto_after, b"epoch5_state",
                "OpenMLS crypto state must match MDK metadata epoch after rollback. \
                 groups.epoch=5 but crypto state is still epoch6 data means \
                 split-brain: MDK thinks epoch 5, MLS engine has epoch 6 keys. \
                 Every subsequent message in this group will fail to decrypt."
            );
        }

        /// Restore DELETE path must also use correct key format for OpenMLS
        /// tables.
        ///
        /// The restore code deletes existing OpenMLS rows before re-inserting
        /// from the snapshot. If it uses raw bytes (as_slice) for the DELETE,
        /// it won't match the MlsCodec-keyed rows and they persist alongside the
        /// (hypothetically fixed) snapshot data — causing duplicate/stale state.
        #[test]
        fn test_restore_deletes_openmls_data_before_reinserting() {
            let storage = MdkSqliteStorage::new_in_memory().unwrap();

            let group = create_test_group(9);
            let group_id = group.mls_group_id.clone();
            storage.save_group(group).unwrap();

            let mls_key = MlsCodec::serialize(&group_id).unwrap();

            // Write initial OpenMLS state
            storage.with_connection(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO openmls_group_data
                     (group_id, data_type, group_data, provider_version)
                     VALUES (?, ?, ?, ?)",
                    params![mls_key, "group_state", b"initial_state" as &[u8], 1i32],
                )
                .unwrap();
            });

            // Take snapshot
            storage
                .create_group_snapshot(&group_id, "snap_initial")
                .unwrap();

            // Replace with new state
            storage.with_connection(|conn| {
                conn.execute(
                    "UPDATE openmls_group_data SET group_data = ?
                     WHERE group_id = ? AND data_type = ?",
                    params![b"modified_state" as &[u8], mls_key, "group_state"],
                )
                .unwrap();
            });

            // Rollback
            storage
                .rollback_group_to_snapshot(&group_id, "snap_initial")
                .unwrap();

            // Count how many openmls_group_data rows exist for this group.
            // If DELETE used raw bytes (missing the MlsCodec-keyed rows), the old
            // "modified_state" row persists alongside the restored
            // "initial_state".
            let row_count = count_openmls_rows(&storage, "openmls_group_data", &group_id);
            assert_eq!(
                row_count, 1,
                "After rollback, there should be exactly 1 openmls_group_data \
                 row. More than 1 means the DELETE used the wrong key format and \
                 failed to remove the stale OpenMLS row before re-inserting from \
                 snapshot."
            );
        }
    }
}
