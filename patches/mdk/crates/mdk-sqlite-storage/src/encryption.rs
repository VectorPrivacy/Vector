//! SQLCipher encryption support for SQLite storage.
//!
//! This module provides encryption configuration and utilities for SQLCipher-encrypted
//! SQLite databases. All databases created with [`crate::MdkSqliteStorage::new`] or
//! [`crate::MdkSqliteStorage::new_with_key`] are encrypted using SQLCipher with a 256-bit AES key.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use mdk_storage_traits::Secret;
use rusqlite::Connection;

use crate::error::Error;

/// Configuration for database encryption using SQLCipher.
///
/// This struct holds the 32-byte (256-bit) encryption key used to encrypt/decrypt
/// the SQLite database. The key should be generated using a cryptographically
/// secure random number generator and stored securely (e.g., in the platform's
/// secure storage such as Keychain on iOS/macOS or Keystore on Android).
///
/// # Security
///
/// - Never log or expose the encryption key
/// - Store keys in platform-specific secure storage
/// - Use a unique key per database
#[derive(Clone)]
pub struct EncryptionConfig {
    /// The 32-byte (256-bit) encryption key for SQLCipher.
    key: Secret<[u8; 32]>,
}

impl EncryptionConfig {
    /// Creates a new encryption configuration with the provided key.
    ///
    /// # Arguments
    ///
    /// * `key` - A 32-byte (256-bit) encryption key.
    ///
    /// # Example
    ///
    /// ```
    /// use mdk_sqlite_storage::encryption::EncryptionConfig;
    ///
    /// let key = [0u8; 32]; // In production, use a securely generated key
    /// let config = EncryptionConfig::new(key);
    /// ```
    #[must_use]
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: Secret::new(key),
        }
    }

    /// Creates a new encryption configuration from a byte slice.
    ///
    /// # Arguments
    ///
    /// * `key` - A byte slice that must be exactly 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is not exactly 32 bytes.
    pub fn from_slice(key: &[u8]) -> Result<Self, Error> {
        let key: [u8; 32] = key
            .try_into()
            .map_err(|_| Error::InvalidKeyLength(key.len()))?;
        Ok(Self {
            key: Secret::new(key),
        })
    }

    /// Generates a new random encryption key.
    ///
    /// This function uses `getrandom` for cryptographically secure random number
    /// generation. The generated key should be stored in secure storage for later use.
    ///
    /// # Errors
    ///
    /// Returns an error if random number generation fails.
    pub fn generate() -> Result<Self, Error> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| Error::KeyGeneration(e.to_string()))?;
        Ok(Self {
            key: Secret::new(key),
        })
    }

    /// Returns a reference to the encryption key.
    #[must_use]
    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    /// Formats the key as a SQLCipher-compatible hex string.
    ///
    /// SQLCipher expects a raw key in the format: `x'<64-char-hex-string>'`
    fn to_sqlcipher_key(&self) -> String {
        format!("x'{}'", hex::encode(self.key.as_ref()))
    }
}

// Implement Debug without exposing the key
impl fmt::Debug for EncryptionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptionConfig")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// Applies SQLCipher encryption to a connection.
///
/// This function configures a SQLite connection for SQLCipher encryption by:
/// 1. Setting the encryption key (PRAGMA key) - must be first operation
/// 2. Pinning SQLCipher 4.x compatibility mode
/// 3. Forcing in-memory temporary storage to avoid plaintext temp file spill
/// 4. Validating the key by reading from sqlite_master
///
/// # Arguments
///
/// * `conn` - The SQLite connection to configure
/// * `config` - The encryption configuration containing the key
///
/// # Errors
///
/// Returns an error if:
/// - The key is incorrect for an existing database
/// - The database is corrupted
/// - The database is not encrypted but a key was provided
pub fn apply_encryption(conn: &Connection, config: &EncryptionConfig) -> Result<(), Error> {
    let key = config.to_sqlcipher_key();

    // PRAGMA key MUST be the first operation on the connection
    conn.execute_batch(&format!("PRAGMA key = \"{key}\";"))?;

    // Pin SQLCipher 4.x defaults to prevent issues with future SQLCipher upgrades
    conn.execute_batch("PRAGMA cipher_compatibility = 4;")?;

    // Force in-memory temporary storage to prevent plaintext temp file spill
    conn.execute_batch("PRAGMA temp_store = MEMORY;")?;

    // Validate the key by attempting to read from the database
    // This will fail if the key is wrong or the database is not encrypted
    validate_encryption_key(conn)?;

    Ok(())
}

/// Validates that the encryption key is correct by attempting to read from the database.
///
/// SQLCipher doesn't always error immediately on PRAGMA key if the key is wrong.
/// We need to actually try to read from the database to verify the key.
fn validate_encryption_key(conn: &Connection) -> Result<(), Error> {
    match conn.query_row("SELECT count(*) FROM sqlite_master;", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ffi::ErrorCode::NotADatabase =>
        {
            // This error typically means wrong key or not an encrypted database
            Err(Error::WrongEncryptionKey)
        }
        Err(e) => Err(e.into()),
    }
}

/// Checks if a database file appears to be encrypted.
///
/// SQLCipher-encrypted databases have a different file header than plain SQLite.
/// Plain SQLite databases start with "SQLite format 3\0" (16 bytes).
/// Encrypted databases will have random-looking bytes at the start.
///
/// # Arguments
///
/// * `path` - Path to the database file
///
/// # Returns
///
/// - `Ok(true)` if the file exists and appears encrypted
/// - `Ok(false)` if the file exists and appears unencrypted (plain SQLite)
/// - `Ok(false)` if the file doesn't exist (new database)
/// - `Err` if there's an I/O error reading the file
pub fn is_database_encrypted<P>(path: P) -> Result<bool, Error>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        // New database, not encrypted yet
        return Ok(false);
    }

    let mut file = File::open(path)?;
    let mut header = [0u8; 16];

    match file.read_exact(&mut header) {
        Ok(()) => {
            // Plain SQLite header: "SQLite format 3\0"
            const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
            Ok(header != *SQLITE_HEADER)
        }
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
            // File is too small to have a valid header - treat as unencrypted/new
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_config_new() {
        let key = [0x42u8; 32];
        let config = EncryptionConfig::new(key);
        assert_eq!(config.key(), &key);
    }

    #[test]
    fn test_encryption_config_from_slice() {
        let key = vec![0x42u8; 32];
        let config = EncryptionConfig::from_slice(&key).unwrap();
        assert_eq!(config.key(), key.as_slice());
    }

    #[test]
    fn test_encryption_config_from_slice_invalid_length() {
        let short_key = vec![0x42u8; 16];
        let result = EncryptionConfig::from_slice(&short_key);
        assert!(matches!(result, Err(Error::InvalidKeyLength(16))));

        let long_key = vec![0x42u8; 64];
        let result = EncryptionConfig::from_slice(&long_key);
        assert!(matches!(result, Err(Error::InvalidKeyLength(64))));
    }

    #[test]
    fn test_encryption_config_from_slice_empty() {
        let empty_key: Vec<u8> = vec![];
        let result = EncryptionConfig::from_slice(&empty_key);
        assert!(matches!(result, Err(Error::InvalidKeyLength(0))));
    }

    #[test]
    fn test_encryption_config_debug_redacts_key() {
        let key = [0x42u8; 32];
        let config = EncryptionConfig::new(key);
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("REDACTED"));
        assert!(!debug_str.contains("42"));
    }

    #[test]
    fn test_encryption_config_generate() {
        let config1 = EncryptionConfig::generate().unwrap();
        let config2 = EncryptionConfig::generate().unwrap();

        // Keys should be different (with overwhelming probability)
        assert_ne!(config1.key(), config2.key());

        // Keys should be 32 bytes
        assert_eq!(config1.key().len(), 32);
    }

    #[test]
    fn test_encryption_config_clone() {
        let key = [0x42u8; 32];
        let config1 = EncryptionConfig::new(key);
        let config2 = config1.clone();
        assert_eq!(config1.key(), config2.key());
    }

    #[test]
    fn test_to_sqlcipher_key_format() {
        let key = [0x00u8; 32];
        let config = EncryptionConfig::new(key);
        let sqlcipher_key = config.to_sqlcipher_key();

        // Should be x'<64 hex chars>'
        assert!(sqlcipher_key.starts_with("x'"));
        assert!(sqlcipher_key.ends_with('\''));
        assert_eq!(sqlcipher_key.len(), 2 + 64 + 1); // x' + 64 hex chars + '
    }

    #[test]
    fn test_to_sqlcipher_key_format_nonzero() {
        // Test with a known non-zero key to verify hex encoding
        let mut key = [0u8; 32];
        key[0] = 0xAB;
        key[31] = 0xCD;
        let config = EncryptionConfig::new(key);
        let sqlcipher_key = config.to_sqlcipher_key();

        // Verify the hex encoding is correct
        assert!(sqlcipher_key.starts_with("x'ab"));
        assert!(sqlcipher_key.ends_with("cd'"));
    }

    #[test]
    fn test_is_database_encrypted_nonexistent() {
        let result = is_database_encrypted("/nonexistent/path/db.sqlite");
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_is_database_encrypted_empty_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("empty.db");

        // Create an empty file
        std::fs::File::create(&db_path).unwrap();

        // Empty file should be treated as unencrypted/new
        let result = is_database_encrypted(&db_path);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_is_database_encrypted_small_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("small.db");

        // Create a file smaller than the SQLite header (16 bytes)
        std::fs::write(&db_path, b"too small").unwrap();

        // File too small to have a valid header should be treated as unencrypted/new
        let result = is_database_encrypted(&db_path);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_is_database_encrypted_unencrypted_sqlite() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("plain.db");

        // Create an unencrypted SQLite database
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER);")
            .unwrap();
        drop(conn);

        // Plain SQLite database should not be detected as encrypted
        let result = is_database_encrypted(&db_path);
        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_is_database_encrypted_encrypted_sqlite() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("encrypted.db");

        // Create an encrypted SQLite database
        let config = EncryptionConfig::generate().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        apply_encryption(&conn, &config).unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER);")
            .unwrap();
        drop(conn);

        // Encrypted database should be detected as encrypted
        let result = is_database_encrypted(&db_path);
        assert!(matches!(result, Ok(true)));
    }

    #[test]
    fn test_apply_encryption_new_database() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("new_encrypted.db");

        let config = EncryptionConfig::generate().unwrap();
        let conn = Connection::open(&db_path).unwrap();

        // Apply encryption should succeed on new database
        let result = apply_encryption(&conn, &config);
        assert!(result.is_ok());

        // Should be able to create tables and use the database
        conn.execute_batch("CREATE TABLE test (id INTEGER);")
            .unwrap();
        conn.execute("INSERT INTO test VALUES (42)", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_apply_encryption_reopen_correct_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("reopen.db");

        let config = EncryptionConfig::generate().unwrap();
        let key = *config.key();

        // Create and populate the database
        {
            let conn = Connection::open(&db_path).unwrap();
            apply_encryption(&conn, &config).unwrap();
            conn.execute_batch("CREATE TABLE test (id INTEGER);")
                .unwrap();
            conn.execute("INSERT INTO test VALUES (123)", []).unwrap();
        }

        // Reopen with the same key
        let config2 = EncryptionConfig::new(key);
        let conn2 = Connection::open(&db_path).unwrap();
        let result = apply_encryption(&conn2, &config2);
        assert!(result.is_ok());

        // Verify data is still there
        let value: i64 = conn2
            .query_row("SELECT id FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 123);
    }

    #[test]
    fn test_apply_encryption_wrong_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("wrong_key.db");

        // Create with key1
        let config1 = EncryptionConfig::generate().unwrap();
        {
            let conn = Connection::open(&db_path).unwrap();
            apply_encryption(&conn, &config1).unwrap();
            conn.execute_batch("CREATE TABLE test (id INTEGER);")
                .unwrap();
        }

        // Try to open with key2
        let config2 = EncryptionConfig::generate().unwrap();
        let conn2 = Connection::open(&db_path).unwrap();
        let result = apply_encryption(&conn2, &config2);

        assert!(result.is_err());
        assert!(matches!(result, Err(Error::WrongEncryptionKey)));
    }

    #[test]
    fn test_apply_encryption_on_plain_database_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("plain_then_encrypt.db");

        // Create an unencrypted database with data
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE test (id INTEGER);")
                .unwrap();
            conn.execute("INSERT INTO test VALUES (1)", []).unwrap();
        }

        // Try to open with encryption
        let config = EncryptionConfig::generate().unwrap();
        let conn2 = Connection::open(&db_path).unwrap();
        let result = apply_encryption(&conn2, &config);

        // Should fail because database is not encrypted
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_encryption_key_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("validate.db");

        let config = EncryptionConfig::generate().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        apply_encryption(&conn, &config).unwrap();

        // Create a table so we have something to validate against
        conn.execute_batch("CREATE TABLE test (id INTEGER);")
            .unwrap();

        // Key validation should succeed
        let result = validate_encryption_key(&conn);
        assert!(result.is_ok());
    }

    #[test]
    fn test_encryption_persists_across_connections() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("persist.db");

        let config = EncryptionConfig::generate().unwrap();
        let key = *config.key();

        // Create database and add data through multiple operations
        {
            let conn = Connection::open(&db_path).unwrap();
            apply_encryption(&conn, &config).unwrap();
            conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);")
                .unwrap();
            conn.execute("INSERT INTO users (name) VALUES ('Alice')", [])
                .unwrap();
        }

        // Open again and add more data
        {
            let config2 = EncryptionConfig::new(key);
            let conn = Connection::open(&db_path).unwrap();
            apply_encryption(&conn, &config2).unwrap();
            conn.execute("INSERT INTO users (name) VALUES ('Bob')", [])
                .unwrap();
        }

        // Final verification
        let config3 = EncryptionConfig::new(key);
        let conn = Connection::open(&db_path).unwrap();
        apply_encryption(&conn, &config3).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Verify specific names
        let names: Vec<String> = conn
            .prepare("SELECT name FROM users ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, vec!["Alice", "Bob"]);
    }

    #[test]
    fn test_encrypted_database_binary_data() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("binary.db");

        let config = EncryptionConfig::generate().unwrap();
        let key = *config.key();

        // Store binary data
        let binary_data: Vec<u8> = (0..=255).collect();

        {
            let conn = Connection::open(&db_path).unwrap();
            apply_encryption(&conn, &config).unwrap();
            conn.execute_batch("CREATE TABLE blobs (data BLOB);")
                .unwrap();
            conn.execute("INSERT INTO blobs VALUES (?)", [&binary_data])
                .unwrap();
        }

        // Retrieve and verify
        let config2 = EncryptionConfig::new(key);
        let conn = Connection::open(&db_path).unwrap();
        apply_encryption(&conn, &config2).unwrap();

        let retrieved: Vec<u8> = conn
            .query_row("SELECT data FROM blobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retrieved, binary_data);
    }

    #[test]
    fn test_apply_encryption_on_corrupted_database() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("corrupted.db");

        // Create a file that looks like a database but is corrupted
        std::fs::write(&db_path, b"corrupted database content").unwrap();

        let config = EncryptionConfig::generate().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        let result = apply_encryption(&conn, &config);

        // Should fail because the file is corrupted
        assert!(result.is_err());
    }

    #[test]
    fn test_is_database_encrypted_with_partial_write() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("partial.db");

        // Create a file that's partially written (less than 16 bytes)
        std::fs::write(&db_path, b"partial").unwrap();

        // Should not be detected as encrypted (too small)
        let result = is_database_encrypted(&db_path).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_encryption_config_generate_produces_unique_keys() {
        // Generate many keys and verify they're all different
        let keys: Vec<_> = (0..100)
            .map(|_| *EncryptionConfig::generate().unwrap().key())
            .collect();

        // Check that all keys are unique
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(
                    keys[i], keys[j],
                    "Generated keys should be unique (with overwhelming probability)"
                );
            }
        }
    }
}
