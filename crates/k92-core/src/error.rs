//! One error type shared by every engine crate, plus its IPC representation.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid path {0}")]
    InvalidPath(String),
    #[error("backup data is corrupt: {0}")]
    Corrupt(String),
    #[error("not a K-92 backup: {0}")]
    NotARepository(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("another K-92 operation is using this backup drive; try again when it finishes")]
    Busy,
    #[error("operation cancelled")]
    Cancelled,
    #[error("{0}")]
    InvalidRequest(String),
}

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    pub fn io_path(action: &str, path: &Path, source: io::Error) -> Self {
        Error::Io {
            context: format!("{action} {}", path.display()),
            source,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::Io { .. } => ErrorKind::Io,
            Error::InvalidPath(_) => ErrorKind::InvalidPath,
            Error::Corrupt(_) => ErrorKind::Corrupt,
            Error::NotARepository(_) => ErrorKind::NotARepository,
            Error::Unsupported(_) => ErrorKind::Unsupported,
            Error::NotFound(_) => ErrorKind::NotFound,
            Error::Busy => ErrorKind::Busy,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::InvalidRequest(_) => ErrorKind::InvalidRequest,
        }
    }

    pub fn to_dto(&self) -> ErrorDto {
        ErrorDto {
            kind: self.kind(),
            message: self.to_string(),
        }
    }
}

/// Extension to attach a path to `io::Result`s concisely.
pub trait IoContext<T> {
    fn ctx(self, action: &str, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, action: &str, path: &Path) -> Result<T> {
        self.map_err(|e| Error::io_path(action, path, e))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorKind {
    Io,
    InvalidPath,
    Corrupt,
    NotARepository,
    Unsupported,
    NotFound,
    Busy,
    Cancelled,
    InvalidRequest,
}

/// Error as sent over IPC. Messages never contain credentials (none exist in v1).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ErrorDto {
    pub kind: ErrorKind,
    pub message: String,
}

/// Convenience for building path-bearing errors without a source error.
pub fn not_found(p: &Path) -> Error {
    Error::NotFound(p.display().to_string())
}
