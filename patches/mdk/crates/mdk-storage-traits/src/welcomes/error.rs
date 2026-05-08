//! Error types for the welcomes module

use std::fmt;

/// Error types for the welcomes module
#[derive(Debug)]
pub enum WelcomeError {
    /// Invalid parameters
    InvalidParameters(String),
    /// Database error
    DatabaseError(String),
}

impl std::error::Error for WelcomeError {}

impl fmt::Display for WelcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters(message) => write!(f, "Invalid parameters: {}", message),
            Self::DatabaseError(message) => write!(f, "Database error: {}", message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_welcome_error_display_invalid_parameters() {
        let err = WelcomeError::InvalidParameters("invalid welcome data".to_string());
        assert_eq!(err.to_string(), "Invalid parameters: invalid welcome data");
    }

    #[test]
    fn test_welcome_error_display_database_error() {
        let err = WelcomeError::DatabaseError("welcome not found".to_string());
        assert_eq!(err.to_string(), "Database error: welcome not found");
    }

    #[test]
    fn test_welcome_error_debug() {
        let err = WelcomeError::InvalidParameters("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InvalidParameters"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_welcome_error_is_error() {
        let err: Box<dyn std::error::Error> =
            Box::new(WelcomeError::DatabaseError("test".to_string()));
        assert!(err.to_string().contains("Database error"));
    }
}
