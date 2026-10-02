//! Error types for vector-core.

use std::fmt;

/// Unified error type for all vector-core operations.
#[derive(Debug)]
pub enum VectorError {
    /// Database error
    Db(String),
    /// Nostr protocol error
    Nostr(String),
    /// Cryptographic error
    Crypto(String),
    /// Network/HTTP error
    Network(String),
    /// I/O error
    Io(std::io::Error),
    /// State not initialized (e.g., not logged in)
    NotInitialized(String),
    /// Generic error with message
    Other(String),
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VectorError::Db(msg) => write!(f, "Database error: {}", msg),
            VectorError::Nostr(msg) => write!(f, "Nostr error: {}", msg),
            VectorError::Crypto(msg) => write!(f, "Crypto error: {}", msg),
            VectorError::Network(msg) => write!(f, "Network error: {}", msg),
            VectorError::Io(err) => write!(f, "I/O error: {}", err),
            VectorError::NotInitialized(msg) => write!(f, "Not initialized: {}", msg),
            VectorError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for VectorError {}

/// Why an attachment download failed, for callers that remember failures.
#[derive(Debug)]
pub enum DownloadError {
    /// More ciphertext than the caller's cap (in bytes). Every mirror serves the same blob.
    TooLarge(usize),
    /// Every source answered, and none served bytes that decrypt or match their hash.
    Unusable(String),
    /// A source didn't answer; a later attempt may reach it.
    Unreachable(String),
    /// The caller stopped it.
    Cancelled,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::TooLarge(cap) => write!(f, "attachment exceeds the {} cap", crate::crypto::format_bytes(*cap as u64)),
            DownloadError::Unusable(msg) | DownloadError::Unreachable(msg) => write!(f, "{msg}"),
            DownloadError::Cancelled => write!(f, "Download cancelled"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<String> for VectorError {
    fn from(s: String) -> Self {
        VectorError::Other(s)
    }
}

impl From<&str> for VectorError {
    fn from(s: &str) -> Self {
        VectorError::Other(s.to_string())
    }
}

impl From<std::io::Error> for VectorError {
    fn from(err: std::io::Error) -> Self {
        VectorError::Io(err)
    }
}

impl From<rusqlite::Error> for VectorError {
    fn from(err: rusqlite::Error) -> Self {
        VectorError::Db(err.to_string())
    }
}

impl From<nostr_sdk::prelude::Error> for VectorError {
    fn from(err: nostr_sdk::prelude::Error) -> Self {
        VectorError::Nostr(err.to_string())
    }
}

impl From<reqwest::Error> for VectorError {
    fn from(err: reqwest::Error) -> Self {
        VectorError::Network(err.to_string())
    }
}

/// Convenience alias used throughout vector-core (matches src-tauri's `Result<T, String>` pattern).
pub type Result<T> = std::result::Result<T, VectorError>;

/// Convert VectorError to String for compatibility with existing code that returns Result<T, String>.
impl From<VectorError> for String {
    fn from(err: VectorError) -> String {
        err.to_string()
    }
}
