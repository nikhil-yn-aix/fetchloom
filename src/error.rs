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
    ReferenceAmbiguous,
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
    /// data.toml or data.lock changed while a command that writes them ran, so it wrote neither.
    ProjectChanged,
    /// a check found data.lock out of date with data.toml.
    Outdated,
    /// a strict check found data the run asked for not ready to use: a file kept with other bytes
    /// than data.lock pins, one not in place yet, a recipe's result not here, or a dataset
    /// data.lock does not pin as data.toml asks.
    NotReady,
    /// data.lock does not pin what a recipe in data.toml makes now, which only `fetchloom build`
    /// can pin, since it runs the project's code.
    BuildNeeded,
    /// a recipe's step could not start, failed, or wrote what a result cannot hold.
    BuildFailed,
    /// a step built again from the same inputs, command and code made other files than
    /// data.lock pins for them.
    BuildDiffers,
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
            Kind::ReferenceAmbiguous => "reference.ambiguous",
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
            Kind::ProjectChanged => "project.changed",
            Kind::Outdated => "lock.outdated",
            Kind::NotReady => "data.not_ready",
            Kind::BuildNeeded => "build.needed",
            Kind::BuildFailed => "build.failed",
            Kind::BuildDiffers => "build.differs",
            Kind::Interrupted => "interrupted",
        }
    }

    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Kind::Unconfirmed | Kind::Outdated | Kind::NotReady => 1,
            Kind::Usage => 2,
            Kind::ReferenceInvalid
            | Kind::ReferenceUnresolved
            | Kind::ReferenceAmbiguous
            | Kind::SourceInvalid => 10,
            Kind::NetworkUnreachable
            | Kind::NetworkStatus
            | Kind::NetworkRateLimited
            | Kind::NetworkRefused
            | Kind::OfflineMissing => 20,
            Kind::IntegrityMismatch => 30,
            Kind::AuthRequired => 40,
            Kind::LocalIo | Kind::LocalNotEmpty | Kind::ProjectInvalid | Kind::ProjectChanged => 50,
            Kind::BuildNeeded | Kind::BuildFailed | Kind::BuildDiffers => 60,
            Kind::Interrupted => 130,
        }
    }
}

/// one failed download, kept whole for scripts when the human summary is shortened.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct FileFailure {
    pub(crate) file: String,
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) details: Vec<String>,
    pub(crate) help: Option<String>,
}

/// a failure: what happened in one sentence, supporting lines, and what to do next.
#[derive(Clone, Debug)]
pub(crate) struct Error {
    pub(crate) kind: Kind,
    pub(crate) message: String,
    pub(crate) details: Vec<String>,
    pub(crate) help: Option<Box<str>>,
    /// a command's partial result, kept beside the error. boxed, since most errors carry none
    pub(crate) result: Option<Box<serde_json::Value>>,
    pub(crate) failures: Box<[FileFailure]>,
    /// the account that would let the request through, so a person can sign in and retry.
    pub(crate) need: Option<Box<crate::auth::Need>>,
    /// the source showed no dataset of the name asked: none exists, or none this machine may
    /// see, so the name may be what is wrong.
    pub(crate) unseen: bool,
    /// what the person cancelled, such as "the download of x.zip", when they ended a question
    /// on purpose. a person reads that as a plain line, not as an error; `--json` and the exit
    /// code report the error as before.
    pub(crate) cancelled: Option<Box<str>>,
}

impl Error {
    pub(crate) fn new(kind: Kind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
            details: Vec::new(),
            result: None,
            failures: Box::default(),
            need: None,
            unseen: false,
            cancelled: None,
            help: (kind == Kind::NetworkStatus).then(|| {
                "check the link in a browser, then run the same command again, or ask its owner for a direct download link"
                    .into()
            }),
        }
    }

    /// a failure of the disk or the system: what fetchloom was doing, the system's reason, and
    /// what to do when that reason is one a person can fix.
    pub(crate) fn io(doing: impl fmt::Display, err: &io::Error) -> Error {
        let error = Error::new(Kind::LocalIo, format!("{doing}: {}", said(err)));
        match advice(err) {
            Some(help) => error.help(help),
            None => error,
        }
    }

    pub(crate) fn with_result(mut self, result: serde_json::Value) -> Error {
        self.result = Some(Box::new(result));
        self
    }

    /// the error with `report`, what its command found or did so far, so `--json` still gives
    /// it. a report that cannot be written as JSON leaves the error as it was.
    pub(crate) fn with_report(self, report: &impl serde::Serialize) -> Error {
        match serde_json::to_value(report) {
            Ok(value) => self.with_result(value),
            Err(_) => self,
        }
    }

    pub(crate) fn detail(mut self, line: impl Into<String>) -> Error {
        self.details.push(line.into());
        self
    }

    /// the error with the first `most` of `lines` as details, then a line saying how many more
    /// there are.
    pub(crate) fn details_up_to(mut self, lines: &[impl AsRef<str>], most: usize) -> Error {
        for line in lines.iter().take(most) {
            self.details.push(line.as_ref().to_owned());
        }
        if lines.len() > most {
            self.details.push(format!(
                "and {} more",
                crate::units::grouped((lines.len() - most) as u64)
            ));
        }
        self
    }

    /// marks the error as a refusal `account` could lift, after accepting `terms` where a source
    /// asks for that too.
    pub(crate) fn needs(mut self, account: crate::auth::Account, terms: Option<String>) -> Error {
        self.need = Some(Box::new(crate::auth::Need {
            account,
            terms,
            maybe_missing: None,
        }));
        self
    }

    /// marks the error as a refusal `account` could lift, from a source that refuses a name that
    /// does not exist as it refuses a private one, so the name may be what is wrong: its `page`
    /// shows whether it is.
    pub(crate) fn needs_or_missing(mut self, account: crate::auth::Account, page: String) -> Error {
        self.need = Some(Box::new(crate::auth::Need {
            account,
            terms: None,
            maybe_missing: Some(page),
        }));
        self.unseen()
    }

    /// marks the error as a source's answer that it shows no dataset of the name asked, which
    /// may be a typo.
    pub(crate) fn unseen(mut self) -> Error {
        self.unseen = true;
        self
    }

    pub(crate) fn help(mut self, help: impl Into<String>) -> Error {
        self.help = Some(help.into().into_boxed_str());
        self
    }

    /// marks the error as a cancel the person chose, of `what`.
    pub(crate) fn cancelled(mut self, what: impl Into<String>) -> Error {
        self.cancelled = Some(what.into().into_boxed_str());
        self
    }

    /// names what a cancelled question was about, once the caller knows it. any other error
    /// stays as it is.
    pub(crate) fn cancelling(self, what: impl Into<String>) -> Error {
        if self.cancelled.is_some() {
            self.cancelled(what)
        } else {
            self
        }
    }

    /// says that a cancel left everything as it was, once the caller knows that nothing changed
    /// before it. any other error stays as it is.
    pub(crate) fn unchanged(mut self) -> Error {
        if let Some(what) = self.cancelled.take() {
            self.cancelled = Some(format!("{what}, so nothing changed").into_boxed_str());
        }
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

    /// the help with `command` in place of "the command" or "the same command" it says to run,
    /// so a person or a script knows exactly what to run.
    pub(crate) fn naming(mut self, command: &str) -> Error {
        if let Some(help) = &mut self.help {
            let named = format!("run `{command}`");
            *help = help
                .replace("run the same command", &named)
                .replace("run the command", &named)
                .into_boxed_str();
        }
        self
    }
}

/// the native filesystem failure inside an archive reader's wrappers, if any.
pub(crate) fn io_cause<'e>(
    mut cause: &'e (dyn std::error::Error + 'static),
) -> Option<&'e io::Error> {
    loop {
        if let Some(error) = cause.downcast_ref::<io::Error>() {
            if error.raw_os_error().is_some() {
                return Some(error);
            }
            cause = error.get_ref()?;
        } else {
            cause = cause.source()?;
        }
    }
}

/// what went wrong on the machine in plain words, without the system's own phrasing and its
/// error number, which mean little to a person.
pub(crate) fn said(err: &io::Error) -> String {
    // an error fetchloom built carries its own words, such as the file it concerns
    let bare = err.raw_os_error().is_some();
    match err.kind() {
        io::ErrorKind::NotFound if bare => "it does not exist".to_owned(),
        io::ErrorKind::PermissionDenied if bare => "permission was denied".to_owned(),
        io::ErrorKind::NotADirectory if bare => "it is a file, not a folder".to_owned(),
        io::ErrorKind::IsADirectory if bare => "it is a folder, not a file".to_owned(),
        io::ErrorKind::StorageFull if bare => "the disk is full".to_owned(),
        _ => {
            let text = err.to_string();
            let text = match text.rfind(" (os error ") {
                Some(at) if text.ends_with(')') => text[..at].to_owned(),
                _ => text,
            };
            let text = text.trim_end_matches('.');
            let mut chars = text.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_lowercase().chain(chars).collect()
            })
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

    /// every kind, listed once. a kind missing from the list fails to compile, since the match
    /// in it must name them all.
    macro_rules! every_kind {
        ($($kind:ident),* $(,)?) => {{
            fn listed(kind: Kind) {
                match kind {
                    $(Kind::$kind)|* => {}
                }
            }
            let _ = listed;
            [$(Kind::$kind),*]
        }};
    }

    /// the kinds and codes exit-codes.md gives: each cell of backticked names after the code, in
    /// the table that starts with `header`.
    fn documented(header: &str) -> Vec<(Option<u8>, String)> {
        let docs = include_str!("../docs/user/reference/exit-codes.md");
        let mut found = Vec::new();
        for row in docs
            .lines()
            .skip_while(|line| !line.starts_with(header))
            .skip(2)
            .take_while(|line| line.starts_with("| "))
        {
            let cells: Vec<&str> = row.split('|').map(str::trim).collect();
            let code = cells[1].parse::<u8>().ok();
            let names = if code.is_some() { cells[2] } else { cells[1] };
            for name in names.split('`').skip(1).step_by(2) {
                found.push((code, name.to_owned()));
            }
        }
        found
    }

    /// the exit codes page names every kind with the code it ends with, and a next step for each,
    /// and names no kind that does not exist.
    #[test]
    fn every_kind_is_documented_with_its_exit_code_and_next_step() {
        let kinds = every_kind![
            Usage,
            Unconfirmed,
            ReferenceInvalid,
            ReferenceUnresolved,
            ReferenceAmbiguous,
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
            ProjectInvalid,
            ProjectChanged,
            Outdated,
            NotReady,
            BuildNeeded,
            BuildFailed,
            BuildDiffers,
            Interrupted,
        ];
        let mut coded: Vec<(Option<u8>, String)> = kinds
            .iter()
            .map(|kind| (Some(kind.exit_code()), kind.name().to_owned()))
            .collect();
        coded.sort();
        let mut by_code = documented("| exit |");
        by_code.sort();
        assert_eq!(by_code, coded);
        let mut named: Vec<String> = kinds.iter().map(|kind| kind.name().to_owned()).collect();
        named.sort();
        let mut steps: Vec<String> = documented("| kind |")
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        steps.sort();
        assert_eq!(steps, named);
    }

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

    /// a question names what was cancelled once its caller knows it, and leaves the message
    /// scripts read as it was. an error that is no cancel stays one.
    #[test]
    fn only_a_cancel_takes_the_name_of_what_was_cancelled() {
        let failed = Error::new(Kind::LocalIo, "x").cancelling("the download of x");
        assert_eq!(failed.cancelled, None);
        let cancel = Error::new(Kind::Interrupted, "stopped")
            .cancelled("the question")
            .cancelling("the download of x");
        assert_eq!(cancel.cancelled.as_deref(), Some("the download of x"));
        assert_eq!(cancel.message, "stopped");
        assert_eq!(cancel.kind.exit_code(), 130);
    }

    /// a cancel says that nothing changed once its caller knows so, and keeps the message scripts
    /// read. an error that is no cancel stays as it is.
    #[test]
    fn a_cancel_says_nothing_changed_once_its_caller_knows_so() {
        let cancel = Error::new(Kind::Unconfirmed, "did not download x")
            .cancelled("the download of x (2.0 GiB)")
            .unchanged();
        assert_eq!(
            cancel.cancelled.as_deref(),
            Some("the download of x (2.0 GiB), so nothing changed")
        );
        assert_eq!(cancel.message, "did not download x");
        let failed = Error::new(Kind::LocalIo, "x").unchanged();
        assert_eq!((failed.cancelled, failed.message.as_str()), (None, "x"));
    }

    /// a failed command's report rides along as its `--json` result, and one that cannot be
    /// written as JSON, such as a map keyed by pairs, leaves the error as it was.
    #[test]
    fn a_report_rides_along_as_the_result_when_json_can_hold_it() {
        use std::collections::BTreeMap;
        let cases = [
            (BTreeMap::new(), Some(serde_json::json!({}))),
            (BTreeMap::from([((1, 2), "a pair as a key")]), None),
        ];
        for (report, result) in cases {
            let error = Error::new(Kind::Unconfirmed, "needs a yes").with_report(&report);
            assert_eq!(error.result.as_deref(), result.as_ref(), "{report:?}");
            assert_eq!(
                (error.kind, error.message.as_str()),
                (Kind::Unconfirmed, "needs a yes")
            );
        }
    }

    /// an error names the first lines it is given and counts the rest, with thousands apart.
    #[test]
    fn details_name_the_first_lines_and_count_the_rest() {
        let lines: Vec<String> = (1..=1_204).map(|n| format!("file {n}")).collect();
        let cases: [(usize, usize, &[&str]); 4] = [
            (0, 5, &[]),
            (5, 5, &["file 1", "file 2", "file 3", "file 4", "file 5"]),
            (
                6,
                5,
                &[
                    "file 1",
                    "file 2",
                    "file 3",
                    "file 4",
                    "file 5",
                    "and 1 more",
                ],
            ),
            (1_204, 2, &["file 1", "file 2", "and 1,202 more"]),
        ];
        for (given, most, details) in cases {
            let error = Error::new(Kind::OfflineMissing, "x").details_up_to(&lines[..given], most);
            assert_eq!(
                error.details, details,
                "{given} lines, at most {most} named"
            );
        }
    }

    #[test]
    fn help_to_run_the_command_again_names_it() {
        let named = |help: &str| {
            Error::new(Kind::LocalIo, "x")
                .help(help)
                .naming("fetchloom sync")
                .help
        };
        assert_eq!(
            named("close the program using the file, then run the command again").as_deref(),
            Some("close the program using the file, then run `fetchloom sync` again")
        );
        assert_eq!(
            named("run the same command again later").as_deref(),
            Some("run `fetchloom sync` again later")
        );
        assert_eq!(
            named("run it again without --offline").as_deref(),
            Some("run it again without --offline")
        );
        assert_eq!(
            Error::new(Kind::LocalIo, "x").naming("fetchloom sync").help,
            None
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

#[cfg(test)]
mod io_cause_tests {
    use super::*;

    #[test]
    fn native_io_causes_survive_decoder_wrappers() {
        let native = io::Error::from_raw_os_error(5);
        assert_eq!(io_cause(&native).and_then(io::Error::raw_os_error), Some(5));
        let wrapped = io::Error::other(io::Error::other(native));
        let zipped = zip::result::ZipError::Io(wrapped);
        assert_eq!(io_cause(&zipped).and_then(io::Error::raw_os_error), Some(5));
        let corrupt = io::Error::new(io::ErrorKind::InvalidData, "bad archive");
        assert!(io_cause(&corrupt).is_none());
        assert!(io_cause(&std::fmt::Error).is_none());
    }
}
