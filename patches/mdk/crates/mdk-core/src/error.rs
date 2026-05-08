// Copyright (c) 2024-2025 Jeff Gardner
// Copyright (c) 2025 Rust Nostr Developers
// Distributed under the MIT software license

//! MDK errors

use std::string::FromUtf8Error;
use std::{fmt, str};

use nostr::nips::nip44;
use nostr::types::url;
use nostr::{Kind, SignerError, event, key};
use openmls::credentials::errors::BasicCredentialError;
use openmls::error::LibraryError;
use openmls::extensions::errors::InvalidExtensionError;
use openmls::framing::errors::ProtocolMessageError;
use openmls::group::{
    AddMembersError, CommitToPendingProposalsError, CreateGroupContextExtProposalError,
    CreateMessageError, ExportSecretError, MergePendingCommitError, NewGroupError,
    ProcessMessageError, SelfUpdateError, WelcomeError,
};
use openmls::key_packages::errors::{KeyPackageNewError, KeyPackageVerifyError};
use openmls::prelude::{MlsGroupStateError, ValidationError};
use openmls_traits::types::CryptoError;

/// MDK error
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum Error {
    /// Hex error
    #[error(transparent)]
    Hex(#[from] hex::FromHexError),
    /// Keys error
    #[error(transparent)]
    Keys(#[from] key::Error),
    /// Event error
    #[error(transparent)]
    Event(#[from] event::Error),
    /// Event Builder error
    #[error(transparent)]
    EventBuilder(#[from] event::builder::Error),
    /// Nostr Signer error
    #[error(transparent)]
    Signer(#[from] SignerError),
    /// NIP44 error
    #[error(transparent)]
    NIP44(#[from] nip44::Error),
    /// Relay URL error
    #[error(transparent)]
    RelayUrl(#[from] url::Error),
    /// TLS error
    #[error(transparent)]
    Tls(#[from] tls_codec::Error),
    /// UTF8 error
    #[error(transparent)]
    Utf8(#[from] str::Utf8Error),
    /// Crypto error
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Storage error
    #[error(transparent)]
    Storage(#[from] mdk_storage_traits::MdkStorageError),
    /// Generic OpenMLS error
    #[error(transparent)]
    OpenMlsGeneric(#[from] LibraryError),
    /// Invalid extension error
    #[error(transparent)]
    InvalidExtension(#[from] InvalidExtensionError),
    /// Create message error
    #[error(transparent)]
    CreateMessage(#[from] CreateMessageError),
    /// Export secret error
    #[error(transparent)]
    ExportSecret(#[from] ExportSecretError),
    /// Basic credential error
    #[error(transparent)]
    BasicCredential(#[from] BasicCredentialError),
    /// Process message error - epoch mismatch
    #[error("Message epoch differs from the group's epoch")]
    ProcessMessageWrongEpoch(u64),
    /// Process message error - wrong group ID
    #[error("Wrong group ID")]
    ProcessMessageWrongGroupId,
    /// Process message error - use after eviction
    #[error("Use after eviction")]
    ProcessMessageUseAfterEviction,
    /// Process message error - other
    #[error("{0}")]
    ProcessMessageOther(String),
    /// Protocol message error
    #[error("{0}")]
    ProtocolMessage(String),
    /// Key package error
    #[error("{0}")]
    KeyPackage(String),
    /// Group error
    #[error("{0}")]
    Group(String),
    /// Group exporter secret not found
    #[error("group exporter secret not found")]
    GroupExporterSecretNotFound,
    /// Message error
    #[error("{0}")]
    Message(String),
    /// Cannot decrypt own message
    #[error("cannot decrypt own message")]
    CannotDecryptOwnMessage,
    /// Merge pending commit error
    #[error("{0}")]
    MergePendingCommit(String),
    /// Commit to pending proposal
    #[error("unable to commit to pending proposal")]
    CommitToPendingProposalsError,
    /// Self update error
    #[error("{0}")]
    SelfUpdate(String),
    /// Welcome error
    #[error("{0}")]
    Welcome(String),
    /// Welcome previously failed to process (retries are not supported)
    #[error("welcome previously failed to process: {0}")]
    WelcomePreviouslyFailed(String),
    /// Processed welcome not found
    #[error("processed welcome not found")]
    ProcessedWelcomeNotFound,
    /// Provider error
    #[error("{0}")]
    Provider(String),
    /// Group not found
    #[error("group not found")]
    GroupNotFound,
    /// Protocol message group ID doesn't match the current group ID
    #[error("protocol message group ID doesn't match the current group ID")]
    ProtocolGroupIdMismatch,
    /// Own leaf not found
    #[error("own leaf not found")]
    OwnLeafNotFound,
    /// Failed to load signer
    #[error("can't load signer")]
    CantLoadSigner,
    /// Invalid Welcome message
    #[error("invalid welcome message")]
    InvalidWelcomeMessage,
    /// Unexpected event
    #[error("unexpected event kind: expected={expected}, received={received}")]
    UnexpectedEvent {
        /// Expected event kind
        expected: Kind,
        /// Received event kind
        received: Kind,
    },
    /// Unexpected extension type
    #[error("Unexpected extension type")]
    UnexpectedExtensionType,
    /// Nostr group data extension not found
    #[error("Nostr group data extension not found")]
    NostrGroupDataExtensionNotFound,
    /// Message from a non-member of a group
    #[error("Message received from non-member")]
    MessageFromNonMember,
    /// Code path is not yet implemented
    #[error("{0}")]
    NotImplemented(String),
    /// Stored message not found
    #[error("stored message not found")]
    MessageNotFound,
    /// Commit message received from a non-admin
    #[error("not processing commit from non-admin")]
    CommitFromNonAdmin,
    /// Own commit pending merge
    #[error("own commit pending merge")]
    OwnCommitPending,
    /// Error when updating group context extensions
    #[error("Error when updating group context extensions {0}")]
    UpdateGroupContextExts(String),
    /// Invalid image hash length
    #[error("invalid image hash length")]
    InvalidImageHashLength,
    /// Invalid image key length
    #[error("invalid image key length")]
    InvalidImageKeyLength,
    /// Invalid image nonce length
    #[error("invalid image nonce length")]
    InvalidImageNonceLength,
    /// Invalid image upload key length
    #[error("invalid image upload key length")]
    InvalidImageUploadKeyLength,
    /// Invalid extension version
    #[error("invalid extension version: {0}")]
    InvalidExtensionVersion(u16),
    /// Extension format error
    #[error("extension format error: {0}")]
    ExtensionFormatError(String),
    /// Rumor pubkey does not match MLS sender credential
    #[error("author mismatch: rumor pubkey does not match MLS sender")]
    AuthorMismatch,
    /// Key package identity binding mismatch - credential identity doesn't match event signer
    #[error(
        "key package identity mismatch: credential identity {credential_identity} doesn't match event signer {event_signer}"
    )]
    KeyPackageIdentityMismatch {
        /// The identity claimed in the BasicCredential
        credential_identity: String,
        /// The public key that signed the event
        event_signer: String,
    },
    /// Identity change attempted in proposal or commit - MIP-00 requires immutable identity
    #[error(
        "identity change not allowed: proposal attempts to change identity from {original_identity} to {new_identity}"
    )]
    IdentityChangeNotAllowed {
        /// The original identity of the member
        original_identity: String,
        /// The new identity attempted in the proposal
        new_identity: String,
    },
    /// Rumor event is missing its ID
    #[error("rumor event is missing its ID")]
    MissingRumorEventId,
    /// Event timestamp is invalid (too far in future or past)
    #[error("event timestamp is invalid: {0}")]
    InvalidTimestamp(String),
    /// Missing required group ID tag
    #[error("missing required group ID tag (h tag)")]
    MissingGroupIdTag,
    /// Invalid group ID format in tag
    #[error("invalid group ID format: {0}")]
    InvalidGroupIdFormat(String),
    /// Multiple group ID tags found (MIP-03 requires exactly one)
    #[error("multiple group ID tags found: expected exactly one h tag, found {0}")]
    MultipleGroupIdTags(usize),
    /// Failed to create epoch snapshot for commit race resolution
    #[error("failed to create epoch snapshot: {0}")]
    SnapshotCreationFailed(String),
}

impl From<FromUtf8Error> for Error {
    fn from(e: FromUtf8Error) -> Self {
        Self::Utf8(e.utf8_error())
    }
}

impl From<ProtocolMessageError> for Error {
    fn from(e: ProtocolMessageError) -> Self {
        Self::ProtocolMessage(e.to_string())
    }
}

impl From<KeyPackageNewError> for Error {
    fn from(e: KeyPackageNewError) -> Self {
        Self::KeyPackage(e.to_string())
    }
}

impl From<KeyPackageVerifyError> for Error {
    fn from(e: KeyPackageVerifyError) -> Self {
        Self::KeyPackage(e.to_string())
    }
}

impl<T> From<NewGroupError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: NewGroupError<T>) -> Self {
        Self::Group(e.to_string())
    }
}

impl<T> From<AddMembersError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: AddMembersError<T>) -> Self {
        Self::Group(e.to_string())
    }
}

impl<T> From<MergePendingCommitError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: MergePendingCommitError<T>) -> Self {
        Self::MergePendingCommit(e.to_string())
    }
}

impl<T> From<CommitToPendingProposalsError<T>> for Error
where
    T: fmt::Display,
{
    fn from(_e: CommitToPendingProposalsError<T>) -> Self {
        Self::CommitToPendingProposalsError
    }
}

impl<T> From<SelfUpdateError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: SelfUpdateError<T>) -> Self {
        Self::SelfUpdate(e.to_string())
    }
}

impl<T> From<WelcomeError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: WelcomeError<T>) -> Self {
        Self::Welcome(e.to_string())
    }
}

impl<T> From<CreateGroupContextExtProposalError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: CreateGroupContextExtProposalError<T>) -> Self {
        Self::UpdateGroupContextExts(e.to_string())
    }
}

/// Convert ProcessMessageError to our structured error variants
impl<T> From<ProcessMessageError<T>> for Error
where
    T: fmt::Display,
{
    fn from(e: ProcessMessageError<T>) -> Self {
        match e {
            ProcessMessageError::ValidationError(validation_error) => match validation_error {
                ValidationError::WrongGroupId => Self::ProcessMessageWrongGroupId,
                ValidationError::CannotDecryptOwnMessage => Self::CannotDecryptOwnMessage,
                _ => Self::ProcessMessageOther(validation_error.to_string()),
            },
            ProcessMessageError::GroupStateError(group_state_error) => match group_state_error {
                MlsGroupStateError::UseAfterEviction => Self::ProcessMessageUseAfterEviction,
                _ => Self::ProcessMessageOther(group_state_error.to_string()),
            },
            _ => Self::ProcessMessageOther(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Kind;

    /// Test that all error variants can be constructed and display correctly
    #[test]
    fn test_error_display_messages() {
        // Test simple message errors
        let error = Error::ProcessMessageWrongEpoch(5);
        assert_eq!(
            error.to_string(),
            "Message epoch differs from the group's epoch"
        );

        let error = Error::ProcessMessageWrongGroupId;
        assert_eq!(error.to_string(), "Wrong group ID");

        let error = Error::ProcessMessageUseAfterEviction;
        assert_eq!(error.to_string(), "Use after eviction");

        let error = Error::ProcessMessageOther("custom error".to_string());
        assert_eq!(error.to_string(), "custom error");

        let error = Error::ProtocolMessage("protocol error".to_string());
        assert_eq!(error.to_string(), "protocol error");

        let error = Error::KeyPackage("key package error".to_string());
        assert_eq!(error.to_string(), "key package error");

        let error = Error::Group("group error".to_string());
        assert_eq!(error.to_string(), "group error");

        let error = Error::GroupExporterSecretNotFound;
        assert_eq!(error.to_string(), "group exporter secret not found");

        let error = Error::Message("message error".to_string());
        assert_eq!(error.to_string(), "message error");

        let error = Error::CannotDecryptOwnMessage;
        assert_eq!(error.to_string(), "cannot decrypt own message");

        let error = Error::MergePendingCommit("merge error".to_string());
        assert_eq!(error.to_string(), "merge error");

        let error = Error::CommitToPendingProposalsError;
        assert_eq!(error.to_string(), "unable to commit to pending proposal");

        let error = Error::SelfUpdate("self update error".to_string());
        assert_eq!(error.to_string(), "self update error");

        let error = Error::Welcome("welcome error".to_string());
        assert_eq!(error.to_string(), "welcome error");

        let error = Error::WelcomePreviouslyFailed("original error reason".to_string());
        assert_eq!(
            error.to_string(),
            "welcome previously failed to process: original error reason"
        );

        let error = Error::ProcessedWelcomeNotFound;
        assert_eq!(error.to_string(), "processed welcome not found");

        let error = Error::Provider("provider error".to_string());
        assert_eq!(error.to_string(), "provider error");

        let error = Error::GroupNotFound;
        assert_eq!(error.to_string(), "group not found");

        let error = Error::ProtocolGroupIdMismatch;
        assert_eq!(
            error.to_string(),
            "protocol message group ID doesn't match the current group ID"
        );

        let error = Error::OwnLeafNotFound;
        assert_eq!(error.to_string(), "own leaf not found");

        let error = Error::CantLoadSigner;
        assert_eq!(error.to_string(), "can't load signer");

        let error = Error::InvalidWelcomeMessage;
        assert_eq!(error.to_string(), "invalid welcome message");

        let error = Error::UnexpectedExtensionType;
        assert_eq!(error.to_string(), "Unexpected extension type");

        let error = Error::NostrGroupDataExtensionNotFound;
        assert_eq!(error.to_string(), "Nostr group data extension not found");

        let error = Error::MessageFromNonMember;
        assert_eq!(error.to_string(), "Message received from non-member");

        let error = Error::NotImplemented("feature X".to_string());
        assert_eq!(error.to_string(), "feature X");

        let error = Error::MessageNotFound;
        assert_eq!(error.to_string(), "stored message not found");

        let error = Error::CommitFromNonAdmin;
        assert_eq!(error.to_string(), "not processing commit from non-admin");

        let error = Error::UpdateGroupContextExts("context error".to_string());
        assert_eq!(
            error.to_string(),
            "Error when updating group context extensions context error"
        );

        let error = Error::InvalidImageHashLength;
        assert_eq!(error.to_string(), "invalid image hash length");

        let error = Error::InvalidImageKeyLength;
        assert_eq!(error.to_string(), "invalid image key length");

        let error = Error::InvalidImageNonceLength;
        assert_eq!(error.to_string(), "invalid image nonce length");

        let error = Error::InvalidImageUploadKeyLength;
        assert_eq!(error.to_string(), "invalid image upload key length");

        let error = Error::InvalidExtensionVersion(99);
        assert_eq!(error.to_string(), "invalid extension version: 99");

        let error = Error::AuthorMismatch;
        assert_eq!(
            error.to_string(),
            "author mismatch: rumor pubkey does not match MLS sender"
        );

        let error = Error::MissingRumorEventId;
        assert_eq!(error.to_string(), "rumor event is missing its ID");
    }

    /// Test UnexpectedEvent error variant with Kind values
    #[test]
    fn test_unexpected_event_error() {
        let error = Error::UnexpectedEvent {
            expected: Kind::MlsGroupMessage,
            received: Kind::TextNote,
        };

        let msg = error.to_string();
        assert!(msg.contains("unexpected event kind"));
        assert!(msg.contains("expected="));
        assert!(msg.contains("received="));
    }

    /// Test KeyPackageIdentityMismatch error variant
    #[test]
    fn test_key_package_identity_mismatch_error() {
        let error = Error::KeyPackageIdentityMismatch {
            credential_identity: "abc123".to_string(),
            event_signer: "def456".to_string(),
        };

        let msg = error.to_string();
        assert!(msg.contains("key package identity mismatch"));
        assert!(msg.contains("abc123"));
        assert!(msg.contains("def456"));
    }

    /// Test IdentityChangeNotAllowed error variant
    #[test]
    fn test_identity_change_not_allowed_error() {
        let error = Error::IdentityChangeNotAllowed {
            original_identity: "original_id".to_string(),
            new_identity: "new_id".to_string(),
        };

        let msg = error.to_string();
        assert!(msg.contains("identity change not allowed"));
        assert!(msg.contains("original_id"));
        assert!(msg.contains("new_id"));
    }

    /// Test error equality (PartialEq implementation)
    #[test]
    fn test_error_equality() {
        let error1 = Error::GroupNotFound;
        let error2 = Error::GroupNotFound;
        let error3 = Error::OwnLeafNotFound;

        assert_eq!(error1, error2);
        assert_ne!(error1, error3);

        let error1 = Error::Message("test".to_string());
        let error2 = Error::Message("test".to_string());
        let error3 = Error::Message("different".to_string());

        assert_eq!(error1, error2);
        assert_ne!(error1, error3);
    }

    /// Test From<FromUtf8Error> conversion
    #[test]
    fn test_from_utf8_error_conversion() {
        let invalid_bytes = vec![0xff, 0xfe];
        let utf8_result = String::from_utf8(invalid_bytes);
        assert!(utf8_result.is_err());

        let error: Error = utf8_result.unwrap_err().into();
        assert!(matches!(error, Error::Utf8(_)));
    }

    /// Test Debug implementation
    #[test]
    fn test_error_debug() {
        let error = Error::GroupNotFound;
        let debug_str = format!("{:?}", error);
        assert!(debug_str.contains("GroupNotFound"));

        let error = Error::UnexpectedEvent {
            expected: Kind::MlsGroupMessage,
            received: Kind::TextNote,
        };
        let debug_str = format!("{:?}", error);
        assert!(debug_str.contains("UnexpectedEvent"));
    }

    /// Test ProcessMessageWrongEpoch error variant preserves epoch for internal use
    #[test]
    fn test_process_message_wrong_epoch() {
        // Epoch value is preserved for internal rollback logic but not exposed in message
        let error = Error::ProcessMessageWrongEpoch(42);
        assert_eq!(
            error.to_string(),
            "Message epoch differs from the group's epoch"
        );

        // Different epoch values produce same message (epoch used internally only)
        let error2 = Error::ProcessMessageWrongEpoch(100);
        assert_eq!(error.to_string(), error2.to_string());
    }

    /// Test OwnCommitPending error variant
    #[test]
    fn test_own_commit_pending() {
        let error = Error::OwnCommitPending;
        assert_eq!(error.to_string(), "own commit pending merge");
    }

    /// Test SnapshotCreationFailed error variant
    #[test]
    fn test_snapshot_creation_failed() {
        let error = Error::SnapshotCreationFailed("storage unavailable".to_string());
        assert_eq!(
            error.to_string(),
            "failed to create epoch snapshot: storage unavailable"
        );
    }

    /// Test Storage error conversion
    #[test]
    fn test_storage_error_conversion() {
        use mdk_storage_traits::MdkStorageError;

        let storage_error = MdkStorageError::NotFound("group not found".to_string());
        let error: Error = storage_error.into();

        assert!(matches!(error, Error::Storage(_)));
        let msg = error.to_string();
        assert!(msg.contains("not found"));
    }
}
