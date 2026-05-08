//! Error types for the messages module

use std::fmt;

/// Error types for the messages module
#[derive(Debug)]
pub enum MessageError {
    /// Invalid parameters
    InvalidParameters(String),
    /// Database error
    DatabaseError(String),
    /// Message not found or not in expected state
    NotFound,
}

impl std::error::Error for MessageError {}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters(message) => write!(f, "Invalid parameters: {}", message),
            Self::DatabaseError(message) => write!(f, "Database error: {}", message),
            Self::NotFound => write!(f, "Message not found or not in expected state"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_error_display_invalid_parameters() {
        let err = MessageError::InvalidParameters("missing field".to_string());
        assert_eq!(err.to_string(), "Invalid parameters: missing field");
    }

    #[test]
    fn test_message_error_display_database_error() {
        let err = MessageError::DatabaseError("connection lost".to_string());
        assert_eq!(err.to_string(), "Database error: connection lost");
    }

    #[test]
    fn test_message_error_debug() {
        let err = MessageError::InvalidParameters("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InvalidParameters"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_message_error_is_error() {
        let err: Box<dyn std::error::Error> =
            Box::new(MessageError::DatabaseError("test".to_string()));
        assert!(err.to_string().contains("Database error"));
    }
}
