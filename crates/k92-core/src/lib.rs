//! Shared building blocks for K-92: validated paths, hashes, errors, cancellation and IPC types.

pub mod cancel;
pub mod dto;
pub mod error;
pub mod hash;
pub mod relpath;

pub use cancel::CancelToken;
pub use error::{Error, ErrorDto, ErrorKind, IoContext, Result};
pub use hash::ObjectHash;
pub use relpath::{RelPath, is_windows_reserved_name};

/// Receives progress notifications. Implementations must be cheap; throttle in the UI layer.
pub type ProgressFn<'a> = &'a (dyn Fn(dto::ProgressEvent) + Sync);

/// A progress sink that discards everything.
pub fn no_progress(_: dto::ProgressEvent) {}
