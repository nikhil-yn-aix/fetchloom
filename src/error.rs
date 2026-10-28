//! failures, in a form both people and scripts can read.

use std::fmt;

/// a stable name for each kind of failure, and the exit code it ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Usage,
    /// a question went unanswered or was answered no, so nothing happened.
    Unconfirmed,
    ReferenceInvalid,
    ReferenceUnresolved,
    /// a source answered in a way fetchloom cannot use, such as a listing it cannot read.
    SourceInvalid,
    NetworkUnreachable,
    NetworkStatus,
    NetworkRateLimited,
    NetworkRefused,
    OfflineMissing,
    IntegrityMismatch,
    AuthRequired,
    LocalIo,
    LocalNotEmpty,
    /// data.toml, data.lock or the project's folders are not in a state fetchloom can work with.
    ProjectInvalid,
    /// a check found data.lock out of date with data.toml.
    Outdated,
    Interrupted,
}

impl Kind {
    /// the name scripts match on. `-v` and `--json` show it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Unconfirmed => "unconfirmed",
            Kind::ReferenceInvalid => "reference.invalid",
            Kind::ReferenceUnresolved => "reference.unresolved",
            Kind::SourceInvalid => "source.invalid",
            Kind::NetworkUnreachable => "network.unreachable",
            Kind::NetworkStatus => "network.status",
            Kind::NetworkRateLimited => "network.rate_limited",
            Kind::NetworkRefused => "network.refused",
            Kind::OfflineMissing => "offline.missing",
            Kind::IntegrityMismatch => "integrity.mismatch",
            Kind::AuthRequired => "auth.required",
            Kind::LocalIo => "local.io",
            Kind::LocalNotEmpty => "local.not_empty",
            Kind::ProjectInvalid => "project.invalid",
            Kind::Outdated => "lock.outdated",
            Kind::Interrupted => "interrupted",
        }
    }

    /// the exit code a run ends with when this kind stops it.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Kind::Unconfirmed | Kind::Outdated => 1,
            Kind::Usage => 2,
            Kind::ReferenceInvalid | Kind::ReferenceUnresolved | Kind::SourceInvalid => 10,
            Kind::NetworkUnreachable
            | Kind::NetworkStatus
            | Kind::NetworkRateLimited
            | Kind::NetworkRefused
            | Kind::OfflineMissing => 20,
            Kind::IntegrityMismatch => 30,
            Kind::AuthRequired => 40,
            Kind::LocalIo | Kind::LocalNotEmpty | Kind::ProjectInvalid => 50,
            Kind::Interrupted => 130,
        }
    }
}

/// a failure: what happened in one sentence, supporting lines, and what to do next.
#[derive(Debug)]
pub(crate) struct Error {
    pub(crate) kind: Kind,
    pub(crate) message: String,
    pub(crate) details: Vec<String>,
    pub(crate) help: Option<String>,
}

impl Error {
    pub(crate) fn new(kind: Kind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
            details: Vec::new(),
            help: None,
        }
    }

    pub(crate) fn detail(mut self, line: impl Into<String>) -> Error {
        self.details.push(line.into());
        self
    }

    pub(crate) fn help(mut self, help: impl Into<String>) -> Error {
        self.help = Some(help.into());
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
