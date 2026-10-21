//! What stops a command: an error kind, one sentence, and the next step to take.

use std::fmt;
use std::io;
use std::path::Path;

use fl_model::error::ErrorKind;
use fl_model::event::Problem;
use fl_model::lock::LockError;
use fl_model::manifest::ManifestError;
use fl_sources::SourceError;
use fl_store::error::StoreError;
use fl_transfer::error::TransferError;

/// A failure as a command reports it.
#[derive(Clone, Debug)]
pub(crate) struct Failure(pub(crate) Problem);

impl Failure {
    /// A failure of `kind` saying `message`.
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self(Problem {
            kind,
            message: message.into(),
            help: None,
            subject: None,
        })
    }

    /// Adds the next step to take.
    #[must_use]
    pub(crate) fn help(mut self, help: impl Into<String>) -> Self {
        self.0.help = Some(help.into());
        self
    }

    /// Names the path or dataset the failure concerns.
    #[must_use]
    pub(crate) fn subject(mut self, subject: impl Into<String>) -> Self {
        self.0.subject = Some(subject.into());
        self
    }

    /// A file operation on `path` that failed.
    pub(crate) fn io(op: &str, path: &Path, error: &io::Error) -> Self {
        Self::new(
            ErrorKind::LocalIo,
            format!("could not {op} `{}`: {error}", path.display()),
        )
    }

    /// The kind.
    pub(crate) const fn kind(&self) -> ErrorKind {
        self.0.kind
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.message)
    }
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        let kind = error.kind();
        let failure = Self::new(kind, error.to_string());
        match &error {
            SourceError::Selection(_) => {
                failure.help("patterns match the whole path inside the dataset, try `**/NAME`")
            }
            SourceError::Unresolved { .. } => {
                failure.help("check the identifier on the repository's own site")
            }
            SourceError::NoSource { .. } => {
                failure.help("references look like zenodo:3608990 or https://host/file.csv")
            }
            SourceError::Transfer(transfer) => with_network_help(failure, transfer),
            _ => failure,
        }
    }
}

impl From<TransferError> for Failure {
    fn from(error: TransferError) -> Self {
        let failure = Self::new(error.kind(), error.to_string());
        with_network_help(failure, &error)
    }
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

impl From<ManifestError> for Failure {
    fn from(error: ManifestError) -> Self {
        let failure =
            Self::new(error.kind(), format!("data.toml: {}", error.message)).subject("data.toml");
        match error.help {
            Some(help) => failure.help(help),
            None => failure,
        }
    }
}

impl From<LockError> for Failure {
    fn from(error: LockError) -> Self {
        Self::new(error.kind(), format!("data.lock: {error}"))
            .subject("data.lock")
            .help("delete data.lock and run `fl lock` to write it again")
    }
}

fn with_network_help(failure: Failure, error: &TransferError) -> Failure {
    match error.kind().exit_code() {
        20 => failure.help("check the connection, then run the same command again"),
        _ => failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_kind_message_help_and_subject() {
        let failure = Failure::new(ErrorKind::Usage, "no such dataset")
            .help("run `fl status`")
            .subject("eeg");
        assert_eq!(failure.kind(), ErrorKind::Usage);
        assert_eq!(failure.to_string(), "no such dataset");
        assert_eq!(failure.0.help.as_deref(), Some("run `fl status`"));
        assert_eq!(failure.0.subject.as_deref(), Some("eeg"));
    }

    #[test]
    fn an_io_failure_names_the_operation_and_path() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        let failure = Failure::io("read", Path::new("data.toml"), &error);
        assert_eq!(failure.kind(), ErrorKind::LocalIo);
        assert!(
            failure
                .to_string()
                .starts_with("could not read `data.toml`: ")
        );
    }
}
