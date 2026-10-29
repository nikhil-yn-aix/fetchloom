//! failures, in a form both people and scripts can read.

use std::{fmt, io};

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
#[derive(Clone, Debug)]
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
            help: (kind == Kind::NetworkStatus).then(|| {
                "check the link in a browser, or ask its owner for a direct download link"
                    .to_owned()
            }),
        }
    }

    /// a failure of the disk or the system: what fetchloom was doing, the system's reason, and
    /// what to do when that reason is one a person can fix.
    pub(crate) fn io(doing: impl fmt::Display, err: &io::Error) -> Error {
        let error = Error::new(Kind::LocalIo, format!("{doing}: {err}"));
        match advice(err) {
            Some(help) => error.help(help),
            None => error,
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

    /// the help, unless the error has one already.
    pub(crate) fn or_help(self, help: impl Into<String>) -> Error {
        if self.help.is_some() {
            self
        } else {
            self.help(help)
        }
    }
}

/// what a person can do about an io error, when its cause is one they can change.
pub(crate) fn advice(err: &io::Error) -> Option<&'static str> {
    // windows refuses a file another program opened without sharing it, with a sharing or a
    // lock violation
    if cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33)) {
        return Some("close the program using the file, then run the command again");
    }
    match err.kind() {
        // windows also says access is denied when it cannot replace or delete a file that
        // another program has open
        io::ErrorKind::PermissionDenied if cfg!(windows) => Some(
            "close any program that has it open and check its permissions, then run the command again",
        ),
        io::ErrorKind::PermissionDenied => {
            Some("check its permissions, then run the command again")
        }
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            Some("free some space on that disk, then run the command again")
        }
        _ => None,
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_a_person_can_fix_come_with_help() {
        let full = Error::io(
            "could not write data.toml",
            &io::ErrorKind::StorageFull.into(),
        );
        assert_eq!(full.kind, Kind::LocalIo);
        assert!(
            full.message.starts_with("could not write data.toml: "),
            "{}",
            full.message
        );
        assert_eq!(
            full.help.as_deref(),
            Some("free some space on that disk, then run the command again")
        );
        let missing = Error::io("could not read x", &io::ErrorKind::NotFound.into());
        assert_eq!(missing.help, None);
        assert_eq!(
            missing.or_help("fallback").help.as_deref(),
            Some("fallback")
        );
        assert_eq!(
            full.or_help("fallback").help.as_deref(),
            Some("free some space on that disk, then run the command again")
        );
    }

    #[test]
    fn the_codes_each_platform_uses_are_known() {
        let codes: &[(i32, &str)] = if cfg!(windows) {
            &[
                (32, "close the program using the file"),
                (33, "close the program using the file"),
                (5, "check its permissions"),
                (112, "free some space"),
            ]
        } else {
            &[
                (1, "check its permissions"),
                (13, "check its permissions"),
                (28, "free some space"),
            ]
        };
        for (code, help) in codes {
            let advice = advice(&io::Error::from_raw_os_error(*code));
            assert!(
                advice.is_some_and(|text| text.contains(help)),
                "{code}: {advice:?}"
            );
        }
    }
}
