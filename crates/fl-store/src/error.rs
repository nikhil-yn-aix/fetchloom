//! Store errors: what failed, the path it concerns, and the kind a command reports.

use std::io;
use std::path::{Path, PathBuf};

use fl_model::error::ErrorKind;

/// A failure of the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A file operation failed.
    #[error("could not {op} `{}`: {source}", path.display())]
    Io {
        /// What was being done, such as `create directory`.
        op: &'static str,
        /// The path it was done to.
        path: PathBuf,
        /// What the operating system said.
        #[source]
        source: io::Error,
    },
    /// A staged object received another number of bytes than its source promised.
    #[error("received {written} bytes where {expected} were promised")]
    Length {
        /// The bytes promised.
        expected: u64,
        /// The bytes written.
        written: u64,
    },
    /// A path that this platform's filesystem cannot hold.
    #[error("`{path}` cannot exist on this filesystem: {reason}")]
    Unrepresentable {
        /// The path inside the data directory.
        path: String,
        /// Why it cannot exist.
        reason: &'static str,
    },
    /// A parent of a destination exists as something other than a real directory.
    #[error("`{}` is not a real directory, and files are placed only in real directories", path.display())]
    NotADirectory {
        /// The parent.
        path: PathBuf,
    },
}

impl StoreError {
    /// The kind a command reports for this error.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::Length { .. } => ErrorKind::IntegrityMismatch,
            Self::Unrepresentable { .. } => ErrorKind::LocalUnrepresentable,
            Self::Io { .. } | Self::NotADirectory { .. } => ErrorKind::LocalIo,
        }
    }
}

/// Wraps an IO error from `op` on `path`.
pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StoreError {
    move |source| StoreError::Io {
        op,
        path: path.to_owned(),
        source,
    }
}
