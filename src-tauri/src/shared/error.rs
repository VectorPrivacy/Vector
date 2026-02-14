//! Error handling utilities to reduce duplicate `.map_err()` patterns.
//!
//! This module provides extension traits that add convenient methods for
//! adding context to errors throughout the codebase.

/// Extension trait for adding context to Result types.
///
/// # Example
/// ```rust
/// use crate::shared::ResultExt;
///
/// fn example() -> Result<(), String> {
///     some_operation().context("Failed to perform operation")?;
///     Ok(())
/// }
/// ```
pub trait ResultExt<T, E> {
    /// Add context to an error, converting it to a String.
    fn context(self, msg: &str) -> Result<T, String>;

    /// Add context with a closure for lazy evaluation.
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ResultExt<T, E> for Result<T, E> {
    fn context(self, msg: &str) -> Result<T, String> {
        self.map_err(|e| format!("{}: {}", msg, e))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T, String> {
        self.map_err(|e| format!("{}: {}", f(), e))
    }
}

/// Extension trait for Option types to convert to Result with context.
pub trait OptionExt<T> {
    /// Convert Option to Result with an error message.
    fn ok_or_context(self, msg: &str) -> Result<T, String>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_context(self, msg: &str) -> Result<T, String> {
        self.ok_or_else(|| msg.to_string())
    }
}
