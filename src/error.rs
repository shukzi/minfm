use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MinfmError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("protected path: {0}")]
    ProtectedPath(PathBuf),
    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("source and destination overlap: {0}")]
    PathOverlap(PathBuf),
    #[error("invalid trash metadata: {0}")]
    InvalidTrashInfo(PathBuf),
    #[error("operation was cancelled")]
    Cancelled,
    #[error("incorrect passphrase")]
    IncorrectPassphrase,
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, MinfmError>;

pub fn io_error(context: impl Into<String>, source: std::io::Error) -> MinfmError {
    MinfmError::Io {
        context: context.into(),
        source,
    }
}
