//! Application error type, serialized to the front end as a plain string so
//! every command can surface a clear, human-readable message.

use serde::{Serialize, Serializer};

/// All fallible operations in the core return [`AppError`]. It implements
/// `Serialize` (as its display string) so it can be returned directly from a
/// `#[tauri::command]` and rejected to the JS caller.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("no document is open with id {0}")]
    DocNotFound(u64),

    #[error("invalid argument: {0}")]
    InvalidArg(String),

    #[error("nothing to undo")]
    NothingToUndo,

    #[error("nothing to redo")]
    NothingToRedo,

    /// A long-running job observed its cancellation flag and stopped.
    #[error("operation cancelled")]
    Cancelled,

    /// A deferred operation (preview apply, scan result, …) was generated
    /// against an older document revision and must be discarded.
    #[error("stale revision: the document changed since this operation was prepared (expected revision {expected}, document is at {actual})")]
    StaleRevision { expected: u64, actual: u64 },

    /// A mutation was attempted on a document opened in indexed read-only
    /// mode (F10). Convert it to editable first.
    #[error(
        "this document is open in read-only (indexed) mode; convert it to editable to make changes"
    )]
    ReadOnly,

    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// Convenience constructor for ad-hoc validation failures.
    pub fn invalid(msg: impl Into<String>) -> Self {
        AppError::InvalidArg(msg.into())
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Result alias used throughout the core.
pub type AppResult<T> = Result<T, AppError>;
