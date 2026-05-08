//! Error types for MDK storage operations

use thiserror::Error;

/// Error type for MDK storage operations.
///
/// This error type is used as the associated `Error` type for the OpenMLS
/// `StorageProvider` trait implementation, enabling unified error handling
/// across MLS and MDK storage operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MdkStorageError {
    /// Database operation failed
    #[error("database error: {0}")]
    Database(String),

    /// Serialization failed
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Deserialization failed
    #[error("deserialization error: {0}")]
    Deserialization(String),

    /// Requested item was not found
    #[error("not found: {0}")]
    NotFound(String),

    /// Other error
    #[error("error: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdk_storage_error_display() {
        let err = MdkStorageError::Database("connection failed".to_string());
        assert_eq!(err.to_string(), "database error: connection failed");

        let err = MdkStorageError::Serialization("invalid json".to_string());
        assert_eq!(err.to_string(), "serialization error: invalid json");

        let err = MdkStorageError::Deserialization("parse error".to_string());
        assert_eq!(err.to_string(), "deserialization error: parse error");

        let err = MdkStorageError::NotFound("key package".to_string());
        assert_eq!(err.to_string(), "not found: key package");

        let err = MdkStorageError::Other("unexpected error".to_string());
        assert_eq!(err.to_string(), "error: unexpected error");
    }

    #[test]
    fn test_mdk_storage_error_debug() {
        let err = MdkStorageError::Database("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("Database"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_mdk_storage_error_is_error() {
        let err: Box<dyn std::error::Error> =
            Box::new(MdkStorageError::Database("test".to_string()));
        assert!(err.to_string().contains("database error"));
    }
}
