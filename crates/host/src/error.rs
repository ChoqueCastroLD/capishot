//! The one error type every host call returns.

use photocraft_engine::EngineError;

use crate::DocumentId;

/// Why a host call failed. Nothing in this crate panics on bad input: every failure is one of
/// these, and the document is left as it was unless the variant says otherwise.
///
/// `#[non_exhaustive]`: new variants may be added in a minor release; match with a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No command has this id (see [`crate::Host::commands`]).
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    /// The command exists but can't run now (no document, no selection, a job holds the
    /// document…); `reason` says which precondition failed.
    #[error("command `{command}` is not available right now: {reason}")]
    Disabled {
        /// The command id.
        command: String,
        /// The precondition that failed.
        reason: String,
    },
    /// The params don't fit the command (wrong type, out of range, missing key).
    #[error("invalid parameters for `{command}`: {message}")]
    BadParams {
        /// The command id (or the host call's name).
        command: String,
        /// What is wrong.
        message: String,
    },
    /// There is no active document (a call without a document needs one).
    #[error("no active document")]
    NoActiveDocument,
    /// No open document has this id.
    #[error("no open document {}", .0.0)]
    NoDocument(DocumentId),
    /// No layer with this id (session id or uid) in the document.
    #[error("no such layer {0}")]
    NoLayer(String),
    /// A background job was cancelled; nothing changed.
    #[error("cancelled")]
    Cancelled,
    /// A file could not be read or written (decode/encode failure, unsupported format).
    #[error("{0}")]
    File(String),
    /// Anything else the engine reported (the message is meant for people).
    #[error("{0}")]
    Engine(String),
}

/// `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl From<EngineError> for Error {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::UnknownCommand(c) => Error::UnknownCommand(c),
            EngineError::Disabled(command, reason) => Error::Disabled { command, reason },
            EngineError::BadParams { cmd, msg } => Error::BadParams { command: cmd, message: msg },
            EngineError::NoDocument => Error::NoActiveDocument,
            EngineError::NoLayer(id) => Error::NoLayer(id.0.to_string()),
            e @ EngineError::UnknownLayer { .. } => Error::NoLayer(e.to_string()),
            EngineError::Cancelled => Error::Cancelled,
            EngineError::Other(m) => Error::Engine(m),
        }
    }
}

impl From<photocraft_io::IoError> for Error {
    fn from(e: photocraft_io::IoError) -> Self {
        match e {
            photocraft_io::IoError::Cancelled => Error::Cancelled,
            e => Error::File(e.to_string()),
        }
    }
}

pub(crate) fn bad(command: &str, message: impl Into<String>) -> Error {
    Error::BadParams { command: command.into(), message: message.into() }
}
