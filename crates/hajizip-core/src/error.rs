//! Unified error type for the core library.

use crate::extract::SafetyLimits;

/// The result type used throughout `hajizip-core`.
pub type Result<T> = std::result::Result<T, Error>;

/// The unified error type for all core operations.
///
/// This is a library error type and part of the public API, so callers can
/// match on specific failure modes. It is derived with `thiserror`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The archive format is not recognized or supported.
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// A specific feature (e.g. a compression method) is not supported yet.
    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),

    /// The archive appears to be corrupt or malformed.
    #[error("corrupt archive: {0}")]
    CorruptArchive(String),

    /// An entry path failed validation (e.g. parent-directory traversal).
    #[error("invalid entry path: {0}")]
    InvalidPath(String),

    /// A password is required to proceed.
    #[error("password required")]
    PasswordRequired,

    /// The supplied password was incorrect.
    #[error("wrong password")]
    WrongPassword,

    /// A safety limit was exceeded (zip-bomb / traversal protection).
    #[error("safety limit exceeded: {0:?}")]
    LimitExceeded(SafetyLimits),

    /// The operation was cancelled by the user.
    #[error("operation cancelled")]
    Cancelled,
}
