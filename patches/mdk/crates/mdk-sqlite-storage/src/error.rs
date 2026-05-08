//! Error types for the SQLite storage implementation.

/// Error type for SQLite storage operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite database error
    #[error("Database error: {0}")]
    Database(String),
    /// Error from rusqlite
    #[error("SQLite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    /// Error during database migration
    #[error("Migration error: {0}")]
    Refinery(#[from] refinery::Error),
    /// Error from OpenMLS
    #[error("OpenMLS error: {0}")]
    OpenMls(String),
    /// Input validation error
    #[error("{field_name} exceeds maximum length of {max_size} bytes (got {actual_size} bytes)")]
    Validation {
        /// Name of the field that failed validation
        field_name: String,
        /// Maximum allowed size/length in bytes
        max_size: usize,
        /// Actual size/length in bytes
        actual_size: usize,
    },

    // Encryption-related errors
    /// Database encryption key has invalid length (expected 32 bytes)
    #[error("Invalid encryption key length: expected 32 bytes, got {0} bytes")]
    InvalidKeyLength(usize),

    /// Wrong encryption key provided for existing database
    #[error("Wrong encryption key: database cannot be decrypted with the provided key")]
    WrongEncryptionKey,

    /// Attempted to open an unencrypted database with encryption enabled
    #[error(
        "Cannot open unencrypted database with encryption: database was created without encryption"
    )]
    UnencryptedDatabaseWithEncryption,

    /// Failed to generate random key
    #[error("Failed to generate encryption key: {0}")]
    KeyGeneration(String),

    /// File permission error
    #[error("File permission error: {0}")]
    FilePermission(String),

    // Keyring-related errors
    /// Keyring operation failed
    #[error("Keyring error: {0}")]
    Keyring(String),

    /// Keyring store not initialized
    ///
    /// The host application must initialize a platform-specific keyring store
    /// before using encrypted storage. See the MDK documentation for platform-specific
    /// setup instructions.
    #[error(
        "Keyring store not initialized. The host application must call keyring_core::set_default_store() with a platform-specific store before using encrypted storage. Details: {0}"
    )]
    KeyringNotInitialized(String),

    /// Keyring entry missing for existing database
    ///
    /// The database file exists but the encryption key is not in the keyring.
    /// This can happen if the keyring was cleared, the key was deleted, or the
    /// database was copied from another machine. The database cannot be opened
    /// without the original encryption key.
    #[error(
        "Database exists at '{db_path}' but no encryption key found in keyring (service='{service_id}', key='{db_key_id}'). The database cannot be opened without the original encryption key. If the key was lost, the database data is unrecoverable."
    )]
    KeyringEntryMissingForExistingDatabase {
        /// Path to the database file
        db_path: String,
        /// Service identifier used for keyring lookup
        service_id: String,
        /// Key identifier used for keyring lookup
        db_key_id: String,
    },
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Database(format!("IO error: {}", e))
    }
}

impl From<Error> for rusqlite::Error {
    fn from(err: Error) -> Self {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err.to_string(),
            )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_database() {
        let err = Error::Database("connection failed".to_string());
        assert!(err.to_string().contains("connection failed"));
        assert!(err.to_string().contains("Database error"));
    }

    #[test]
    fn test_error_display_openmls() {
        let err = Error::OpenMls("key generation failed".to_string());
        assert_eq!(err.to_string(), "OpenMLS error: key generation failed");
    }

    #[test]
    fn test_error_display_invalid_key_length() {
        let err = Error::InvalidKeyLength(16);
        let msg = err.to_string();
        assert!(msg.contains("16"));
        assert!(msg.contains("32"));
    }

    #[test]
    fn test_error_display_wrong_encryption_key() {
        let err = Error::WrongEncryptionKey;
        let msg = err.to_string();
        assert!(msg.contains("Wrong encryption key"));
    }

    #[test]
    fn test_error_display_unencrypted_database_with_encryption() {
        let err = Error::UnencryptedDatabaseWithEncryption;
        let msg = err.to_string();
        assert!(msg.contains("unencrypted database"));
    }

    #[test]
    fn test_error_display_key_generation() {
        let err = Error::KeyGeneration("entropy failure".to_string());
        let msg = err.to_string();
        assert!(msg.contains("entropy failure"));
        assert!(msg.contains("generate encryption key"));
    }

    #[test]
    fn test_error_display_file_permission() {
        let err = Error::FilePermission("access denied".to_string());
        let msg = err.to_string();
        assert!(msg.contains("access denied"));
        assert!(msg.contains("permission"));
    }

    #[test]
    fn test_error_display_keyring() {
        let err = Error::Keyring("keychain locked".to_string());
        let msg = err.to_string();
        assert!(msg.contains("keychain locked"));
    }

    #[test]
    fn test_error_display_keyring_not_initialized() {
        let err = Error::KeyringNotInitialized("no store configured".to_string());
        let msg = err.to_string();
        assert!(msg.contains("not initialized"));
        assert!(msg.contains("set_default_store"));
    }

    #[test]
    fn test_error_display_validation() {
        let err = Error::Validation {
            field_name: "name".to_string(),
            max_size: 100,
            actual_size: 150,
        };
        let msg = err.to_string();
        assert!(msg.contains("name"));
        assert!(msg.contains("100"));
        assert!(msg.contains("150"));
    }

    #[test]
    fn test_error_debug() {
        let err = Error::Database("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("Database"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        match err {
            Error::Database(msg) => assert!(msg.contains("file not found")),
            _ => panic!("Expected Database error variant"),
        }
    }

    #[test]
    fn test_error_from_rusqlite_error() {
        let rusqlite_err = rusqlite::Error::InvalidQuery;
        let err: Error = rusqlite_err.into();

        match err {
            Error::Rusqlite(_) => {}
            _ => panic!("Expected Rusqlite variant"),
        }
    }

    #[test]
    fn test_error_into_rusqlite_error() {
        let err = Error::WrongEncryptionKey;
        let rusqlite_err: rusqlite::Error = err.into();
        // Verify it converts to a rusqlite error (the specific type is less important)
        let msg = rusqlite_err.to_string();
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_error_to_rusqlite_error() {
        let err = Error::Database("test error".to_string());
        let rusqlite_err: rusqlite::Error = err.into();

        match rusqlite_err {
            rusqlite::Error::FromSqlConversionFailure(_, _, _) => {}
            _ => panic!("Expected FromSqlConversionFailure variant"),
        }
    }

    #[test]
    fn test_error_debug_format() {
        let err = Error::InvalidKeyLength(24);
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InvalidKeyLength"));
        assert!(debug_str.contains("24"));
    }

    #[test]
    fn test_error_display_keyring_entry_missing_for_existing_database() {
        let err = Error::KeyringEntryMissingForExistingDatabase {
            db_path: "/path/to/db.sqlite".to_string(),
            service_id: "com.example.app".to_string(),
            db_key_id: "mdk.db.key".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("/path/to/db.sqlite"));
        assert!(msg.contains("com.example.app"));
        assert!(msg.contains("mdk.db.key"));
        assert!(msg.contains("no encryption key found"));
        assert!(msg.contains("unrecoverable"));
    }

    #[test]
    fn test_validation_error_fields() {
        let err = Error::Validation {
            field_name: "description".to_string(),
            max_size: 1024,
            actual_size: 2048,
        };

        if let Error::Validation {
            field_name,
            max_size,
            actual_size,
        } = err
        {
            assert_eq!(field_name, "description");
            assert_eq!(max_size, 1024);
            assert_eq!(actual_size, 2048);
        } else {
            panic!("Expected Validation variant");
        }
    }
}
