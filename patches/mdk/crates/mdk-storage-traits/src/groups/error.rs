//! Error types for the groups module

use std::fmt;

/// Invalid group state
#[derive(Debug, PartialEq, Eq)]
pub enum InvalidGroupState {
    /// Group has no admins
    NoAdmins,
    /// Group has no relays
    NoRelays,
}

impl fmt::Display for InvalidGroupState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdmins => write!(f, "group has no admins"),
            Self::NoRelays => write!(f, "group has no relays"),
        }
    }
}

/// Error types for the groups module
#[derive(Debug)]
pub enum GroupError {
    /// Invalid parameters
    InvalidParameters(String),
    /// Database error
    DatabaseError(String),
    /// Invalid state
    InvalidState(InvalidGroupState),
}

impl std::error::Error for GroupError {}

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters(message) => write!(f, "Invalid parameters: {}", message),
            Self::DatabaseError(message) => write!(f, "Database error: {}", message),
            Self::InvalidState(state) => write!(f, "Invalid state: {state}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_group_state_display() {
        assert_eq!(
            InvalidGroupState::NoAdmins.to_string(),
            "group has no admins"
        );
        assert_eq!(
            InvalidGroupState::NoRelays.to_string(),
            "group has no relays"
        );
    }

    #[test]
    fn test_invalid_group_state_equality() {
        assert_eq!(InvalidGroupState::NoAdmins, InvalidGroupState::NoAdmins);
        assert_eq!(InvalidGroupState::NoRelays, InvalidGroupState::NoRelays);
        assert_ne!(InvalidGroupState::NoAdmins, InvalidGroupState::NoRelays);
    }

    #[test]
    fn test_invalid_group_state_debug() {
        assert_eq!(format!("{:?}", InvalidGroupState::NoAdmins), "NoAdmins");
        assert_eq!(format!("{:?}", InvalidGroupState::NoRelays), "NoRelays");
    }

    #[test]
    fn test_group_error_display() {
        let err = GroupError::InvalidParameters("test param".to_string());
        assert_eq!(err.to_string(), "Invalid parameters: test param");

        let err = GroupError::DatabaseError("db failed".to_string());
        assert_eq!(err.to_string(), "Database error: db failed");

        let err = GroupError::InvalidState(InvalidGroupState::NoAdmins);
        assert_eq!(err.to_string(), "Invalid state: group has no admins");

        let err = GroupError::InvalidState(InvalidGroupState::NoRelays);
        assert_eq!(err.to_string(), "Invalid state: group has no relays");
    }

    #[test]
    fn test_group_error_debug() {
        let err = GroupError::InvalidParameters("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InvalidParameters"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_group_error_is_error() {
        let err: Box<dyn std::error::Error> =
            Box::new(GroupError::InvalidParameters("test".to_string()));
        assert!(err.to_string().contains("Invalid parameters"));
    }
}
