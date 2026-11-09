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
    #[error(
        "not enough space on the backup drive: this backup needs about {}, but only {} is free. \
         Free up space on the drive or choose a larger one; files already backed up are kept",
        fmt_bytes(*needed),
        fmt_bytes(*available)
    )]
    InsufficientSpace { needed: u64, available: u64 },
}

/// Human-readable size in binary units with one decimal ("1.5 GB"), as Windows Explorer shows it.
pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} bytes");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
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
            Error::InsufficientSpace { .. } => ErrorKind::InsufficientSpace,
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
    InsufficientSpace,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_bytes_uses_binary_units() {
        assert_eq!(fmt_bytes(0), "0 bytes");
        assert_eq!(fmt_bytes(1023), "1023 bytes");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(500 * 1024 * 1024 * 1024), "500.0 GB");
    }

    #[test]
    fn insufficient_space_message_is_readable() {
        let e = Error::InsufficientSpace {
            needed: 3 * 1024 * 1024 * 1024,
            available: 1024 * 1024 * 1024,
        };
        assert_eq!(e.kind(), ErrorKind::InsufficientSpace);
        let m = e.to_string();
        assert!(m.contains("3.0 GB") && m.contains("1.0 GB"), "{m}");
    }
}
