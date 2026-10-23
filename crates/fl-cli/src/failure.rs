//! What stops a command: an error kind, one sentence, and the next step to take.

use std::fmt;
use std::io;
use std::ops::Range;
use std::path::Path;

use fl_model::error::ErrorKind;
use fl_model::event::Problem;
use fl_model::lock::LockError;
use fl_model::manifest::ManifestError;
use fl_sources::SourceError;
use fl_store::error::StoreError;
use fl_transfer::error::TransferError;

/// A failure as a command reports it, and the place in a file that caused it, when one did.
#[derive(Clone, Debug)]
pub(crate) struct Failure(pub(crate) Problem, pub(crate) Option<Box<Snippet>>);

/// A place in a file, shown under an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snippet {
    /// The file as the user knows it, such as `data.toml`.
    pub(crate) file: String,
    /// Its text.
    pub(crate) text: String,
    /// The bytes of `text` at fault.
    pub(crate) span: Range<usize>,
}

impl Failure {
    /// A failure of `kind` saying `message`.
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self(
            Problem {
                kind,
                message: message.into(),
                help: None,
                subject: None,
            },
            None,
        )
    }

    /// Points at `span` of `text`, the file the user knows as `file`.
    #[must_use]
    pub(crate) fn snippet(mut self, file: &str, text: &str, span: Range<usize>) -> Self {
        self.1 = Some(Box::new(Snippet {
            file: file.to_owned(),
            text: text.to_owned(),
            span,
        }));
        self
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

/// Where problems with fetchloom itself are reported.
pub(crate) const ISSUES: &str = "https://github.com/nikhil-yn-aix/fetchloom/issues";

impl Failure {
    /// The problem as reported, with a `help:` line even when the code that failed gave none:
    /// then it says what to check, by the kind of failure.
    pub(crate) fn into_problem(self) -> Problem {
        let mut problem = self.0;
        if problem.help.is_none() {
            problem.help = Some(fallback(problem.kind).to_owned());
        }
        problem
    }
}

fn fallback(kind: ErrorKind) -> &'static str {
    match kind.exit_code() {
        1 => "run `fl status` to see what differs",
        2 => "run `fl --help` to see what each command takes",
        10..=12 => "check the reference and its filters in data.toml",
        20 => "check the connection, then run the same command again",
        30 => "run the same command again; if it repeats, the source changed what it serves",
        40 => "check the key configured for this host",
        50 => "check the files and folders the message names",
        _ => "run the same command with -v to see more",
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.message)
    }
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        let failure = Self::new(error.kind(), error.to_string());
        match &error {
            SourceError::Selection(_) => {
                failure.help("patterns match the whole path inside the dataset, try `**/NAME`")
            }
            SourceError::Unresolved { .. } => {
                failure.help("check the identifier on the repository's own site")
            }
            SourceError::NoSource { .. } | SourceError::Invalid { .. } => failure.help(
                "references look like zenodo:3608990, 10.5281/zenodo.3608990 or https://host/file.csv",
            ),
            SourceError::NoLanding { .. } => failure.help(
                "add the files by their download links instead, like `fl add https://host/file.csv`",
            ),
            SourceError::Record { .. } | SourceError::Body { .. } => failure.help(format!(
                "the repository answered in a shape fetchloom does not know; please report it at {ISSUES}"
            )),
            SourceError::Collision { .. } => failure.help(
                "this disk cannot hold both names; leave one out with `--exclude PATH`",
            ),
            SourceError::Terms { .. } => failure.help(
                "accept the terms on that page, then set HF_TOKEN, or FETCHLOOM_TOKEN_ and the host, to your key",
            ),
            SourceError::Local { .. } => failure.help("check that the path exists and can be read"),
            SourceError::Adapter { .. } => {
                failure.help("fix the adapter file it names, or move it out of the sources folder")
            }
            SourceError::Transfer(transfer) => transfer_help(failure, transfer),
        }
    }
}

impl From<TransferError> for Failure {
    fn from(error: TransferError) -> Self {
        let failure = Self::new(error.kind(), error.to_string());
        transfer_help(failure, &error)
    }
}

fn transfer_help(failure: Failure, error: &TransferError) -> Failure {
    match error {
        TransferError::Status { .. } => failure
            .help("check the address in a browser; if it works there, try again in a while"),
        TransferError::RateLimited { .. } => {
            failure.help("the host wants a pause; run the same command again later")
        }
        TransferError::Refused { .. } => failure.help(
            "fetchloom only contacts private addresses you type yourself, as in `fl add URL`",
        ),
        TransferError::Insecure { .. } => failure.help("use an https:// address"),
        TransferError::Protocol { .. } => failure.help(format!(
            "the server broke the rules of HTTP; try again later, and report it at {ISSUES} if it repeats"
        )),
        TransferError::AuthRequired { .. } => failure.help(
            "give fetchloom a key: set FETCHLOOM_TOKEN_ and the host, in capitals with `_` for dots, to it",
        ),
        TransferError::AuthRejected { .. } => {
            failure.help("check that the key is current and allowed to read this dataset")
        }
        TransferError::Credentials(_) => failure.help("fix the credentials file it names"),
        TransferError::Offline { .. } => failure.help("run the same command without --offline"),
        TransferError::Mismatch { .. } | TransferError::Length { .. } => failure.help(
            "run the same command again; if it repeats, the source changed what it serves",
        ),
        TransferError::Sink { .. } => {
            failure.help("check that the disk holding the store has free space")
        }
        TransferError::Setup(_) => failure.help("check the system's certificate store and proxy settings"),
        _ => failure.help("check the connection, then run the same command again"),
    }
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        let failure = Self::new(error.kind(), error.to_string());
        match &error {
            StoreError::Unrepresentable { .. } => {
                failure.help("leave the file out with `--exclude PATH` on `fl add`")
            }
            StoreError::Missing { .. } => failure.help("run `fl sync` to download it again"),
            StoreError::NotADirectory { .. } => {
                failure.help("move that file out of the way, then run the same command again")
            }
            _ => failure.help("check the free space and permissions of the path it names"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reported_problem_has_help_by_its_kind_when_none_was_given() {
        let given = Failure::new(ErrorKind::Usage, "x").help("do y");
        assert_eq!(given.into_problem().help.as_deref(), Some("do y"));
        let kinds = [
            ErrorKind::LockStale,
            ErrorKind::Usage,
            ErrorKind::ReferenceUnresolved,
            ErrorKind::NetworkStatus,
            ErrorKind::IntegrityDrift,
            ErrorKind::AuthRequired,
            ErrorKind::LocalIo,
            ErrorKind::StepFailed,
        ];
        let helps: std::collections::BTreeSet<String> = kinds
            .iter()
            .map(|kind| Failure::new(*kind, "x").into_problem().help.unwrap())
            .collect();
        assert_eq!(helps.len(), kinds.len());
    }

    #[test]
    fn source_transfer_and_store_errors_carry_help() {
        let unreachable = TransferError::Offline {
            url: fl_transfer::url::SafeUrl::parse("https://h/x").unwrap(),
        };
        assert!(
            Failure::from(unreachable)
                .0
                .help
                .unwrap()
                .contains("--offline")
        );
        let missing = StoreError::Missing {
            digest: fl_model::digest::Digest::Blake3([0; 32]),
        };
        assert!(Failure::from(missing).0.help.unwrap().contains("fl sync"));
        let adapter = SourceError::NoSource {
            scheme: "nope".to_owned(),
        };
        assert!(
            Failure::from(adapter)
                .0
                .help
                .unwrap()
                .contains("zenodo:3608990")
        );
    }

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
