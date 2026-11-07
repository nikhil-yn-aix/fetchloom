//! filesystem failures retained by cache inspection and cleanup.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Kind};

/// one failed cache operation, with its path and original filesystem cause.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Failure {
    pub(crate) path: PathBuf,
    kind: String,
    os_code: Option<i32>,
    message: String,
    #[serde(skip)]
    cause: io::ErrorKind,
}

impl Failure {
    pub(super) fn new(path: &Path, error: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: format!("{:?}", error.kind()),
            os_code: error.raw_os_error(),
            message: error.to_string(),
            cause: error.kind(),
        }
    }

    pub(crate) fn description(&self) -> String {
        format!("{}: {}", self.path.display(), self.message)
    }

    pub(crate) fn advice(&self) -> &'static str {
        let error = self
            .os_code
            .map_or_else(|| self.cause.into(), io::Error::from_raw_os_error);
        crate::error::advice(&error).unwrap_or(match self.cause {
            io::ErrorKind::ReadOnlyFilesystem => {
                "restore write access to this filesystem, then run the command again"
            }
            io::ErrorKind::ResourceBusy | io::ErrorKind::WouldBlock => {
                "close programs using this path, then run the command again"
            }
            _ => "check the storage device and filesystem for errors, then run the command again",
        })
    }

    pub(crate) fn error(&self, action: &str) -> Error {
        Error::new(Kind::LocalIo, format!("{action} {}", self.description())).help(self.advice())
    }
}

pub(super) fn sort(failures: &mut Vec<Failure>) {
    failures.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.kind.cmp(&right.kind))
            .then(left.message.cmp(&right.message))
    });
    failures.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_advice_matches_the_io_cause() {
        for (kind, message, advice) in [
            (
                io::ErrorKind::Other,
                "injected storage error",
                "storage device",
            ),
            (
                io::ErrorKind::PermissionDenied,
                "injected access error",
                "permissions",
            ),
            (
                io::ErrorKind::StorageFull,
                "injected full disk",
                "free some space",
            ),
        ] {
            let failure = Failure::new(Path::new("object"), &io::Error::new(kind, message));
            let report = serde_json::to_value(&failure).unwrap();
            assert_eq!(report["message"], message);
            assert_eq!(report["kind"], format!("{kind:?}"));
            let error = failure.error("cannot remove");
            assert!(error.message.contains(message));
            assert!(error.help.unwrap().contains(advice));
        }
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_failures_retain_the_os_error_code() {
        let error: io::Error = rustix::io::Errno::IO.into();
        let failure = Failure::new(Path::new("object"), &error);
        let report = serde_json::to_value(&failure).unwrap();
        assert_eq!(report["os_code"], rustix::io::Errno::IO.raw_os_error());
        assert_eq!(report["message"], error.to_string());
        assert!(!failure.advice().contains("permission"));
    }
}
