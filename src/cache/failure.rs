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
    /// the failure in fetchloom's words, without the system's error number.
    #[serde(skip)]
    said: String,
    /// a link, when true, or a file someone put where the cache keeps a folder of its own.
    #[serde(skip)]
    planted: Option<bool>,
}

impl Failure {
    pub(super) fn new(path: &Path, error: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: format!("{:?}", error.kind()),
            os_code: error.raw_os_error(),
            message: error.to_string(),
            cause: error.kind(),
            said: crate::error::said(error),
            planted: error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<crate::place::NotCacheFolder>())
                .map(|refused| refused.link),
        }
    }

    /// the path and what went wrong there, naming the path once when the cause names it already.
    pub(crate) fn description(&self) -> String {
        let path = self.path.display().to_string();
        if self
            .said
            .strip_prefix(&path)
            .is_some_and(|rest| rest.starts_with(' '))
        {
            return self.said.clone();
        }
        format!("{path}: {}", self.said)
    }

    pub(crate) fn advice(&self) -> &'static str {
        // a disk check finds nothing wrong with a link or a file someone put in the cache
        match self.planted {
            Some(true) => {
                return "remove the link itself, not the folder it leads to, since fetchloom never puts one in the cache; then run the command again";
            }
            Some(false) => {
                return "move that file out of the cache, which keeps only its own folders there; then run the command again";
            }
            None => {}
        }
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

    /// a failure reads in fetchloom's words, without the system's error number, as other
    /// commands say the same failure.
    #[test]
    fn a_failure_is_described_without_the_systems_number() {
        // a file another program holds open: busy on unix, a sharing violation on windows
        let code = if cfg!(windows) { 32 } else { 16 };
        let failure = Failure::new(Path::new("object"), &io::Error::from_raw_os_error(code));
        let description = failure.description();
        assert!(!description.contains("os error"), "{description}");
        // the system's own words differ between its libraries and languages
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        assert_eq!(description, "object: device or resource busy");
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
