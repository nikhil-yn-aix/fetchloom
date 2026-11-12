//! a program that prints AWS keys: a profile's `credential_process`, or `aws configure
//! export-credentials` for a profile that signs in to IAM identity center or takes on a role,
//! which fetchloom leaves to the AWS CLI. its words are split as the AWS CLI splits them, never
//! by a shell, and it is found by a full path, never through a folder PATH names relatively,
//! which would be whatever folder fetchloom runs in. it runs as [`process`] runs a program, and
//! what it prints is read as version 1 JSON.

use std::iter::repeat_n;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::auth::process::{self, PATIENCE, Ran};
use crate::error::{Error, Kind};
use crate::net::sign::Keys;
use crate::source::Var;
use crate::stop::Stop;

/// what is wrong with an `Expiration` fetchloom cannot read.
const NO_TIME: &str = "its Expiration is not a time such as 2026-11-12T12:00:00Z";

/// keys a program printed, and when they expire, none for keys that never do.
#[derive(Debug)]
pub(crate) struct Issued {
    pub(crate) keys: Keys,
    pub(crate) expires: Option<SystemTime>,
}

/// a program that prints the keys of one AWS profile.
pub(crate) struct Program {
    /// the program, by its full path.
    pub(super) path: PathBuf,
    pub(super) args: Vec<String>,
    profile: String,
    pub(super) by: By,
    /// how long it may run.
    patience: Duration,
}

/// which program it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum By {
    /// the profile's own `credential_process`.
    Process,
    /// the AWS CLI, for a profile that signs in to IAM identity center when `signs_in`, else takes
    /// on a role.
    Cli { signs_in: bool },
}

impl Program {
    /// the `credential_process` `text` of `profile`: its words as the AWS CLI splits them, by posix
    /// shell quoting, or on windows by the rules windows programs read their command line with,
    /// with nothing a shell does, such as variables, `~` or pipes. the program is a full path, or
    /// a name found in a folder on PATH.
    ///
    /// # Errors
    ///
    /// `usage` for a quote that never closes, no program, or a program named by a relative path;
    /// `auth.required` for a name no folder on PATH holds.
    pub(super) fn process(text: &str, profile: &str, var: Var<'_>) -> Result<Program, Error> {
        let named = format!("the credential_process of the AWS profile {profile}");
        let words = words(text, cfg!(windows)).ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!("{named} has a quote that never closes"),
            )
            .help("close the quote in credential_process, in ~/.aws/config")
        })?;
        let Some((first, args)) = words.split_first().filter(|(first, _)| !first.is_empty()) else {
            return Err(Error::new(Kind::Usage, format!("{named} names no program"))
                .help("give the program's full path in credential_process, in ~/.aws/config"));
        };
        let written = Path::new(first);
        let path = if written.is_absolute() {
            written.to_path_buf()
        } else if written
            .parent()
            .is_some_and(|parent| parent.as_os_str().is_empty())
        {
            installed(first, var).ok_or_else(|| {
                Error::new(
                    Kind::AuthRequired,
                    format!("{named} names {first}, which no folder on PATH holds"),
                )
                .help(format!(
                    "install {first}, or give its full path in credential_process"
                ))
            })?
        } else {
            return Err(Error::new(
                Kind::Usage,
                format!("{named} names the program {first} by a relative path"),
            )
            .help("give its full path in credential_process, since a relative one would be taken from whatever folder fetchloom runs in"));
        };
        Ok(Program {
            path,
            args: args.to_vec(),
            profile: profile.to_owned(),
            by: By::Process,
            patience: PATIENCE,
        })
    }

    /// `aws configure export-credentials` for `profile`, which signs in to IAM identity center
    /// when `signs_in`, else takes on a role, with the AWS CLI a folder on PATH holds.
    ///
    /// # Errors
    ///
    /// `auth.required` when no folder on PATH holds the AWS CLI.
    pub(super) fn cli(profile: &str, signs_in: bool, var: Var<'_>) -> Result<Program, Error> {
        let Some(path) = installed("aws", var) else {
            let (does, help) = if signs_in {
                (
                    "signs in to IAM identity center",
                    format!(
                        "install the AWS CLI, version 2.9 or later, and sign in with `aws sso login --profile {profile}`"
                    ),
                )
            } else {
                (
                    "takes on a role",
                    "install the AWS CLI, version 2.9 or later".to_owned(),
                )
            };
            return Err(Error::new(
                Kind::AuthRequired,
                format!(
                    "the AWS profile {profile} {does}, which fetchloom leaves to the AWS CLI, and no folder on PATH holds the AWS CLI"
                ),
            )
            .help(help));
        };
        let args = [
            "configure",
            "export-credentials",
            "--profile",
            profile,
            "--format",
            "process",
        ];
        Ok(Program {
            path,
            args: args.map(str::to_owned).to_vec(),
            profile: profile.to_owned(),
            by: By::Cli { signs_in },
            patience: PATIENCE,
        })
    }

    /// runs the program and reads the keys it printed, which must still hold at `now`.
    ///
    /// # Errors
    ///
    /// `interrupted` once a stop is asked; `auth.required` when the program could not start,
    /// failed, ran past its time, printed too much or no keys fetchloom reads, or keys that had
    /// expired by `now`.
    pub(crate) async fn run(&self, now: SystemTime, stop: &Stop) -> Result<Issued, Error> {
        tracing::debug!(program = %self.path.display(), profile = %self.profile, "asking a program for AWS keys");
        let printed = process::printed(&self.path, &self.args, self.patience, stop)
            .await
            .map_err(|ran| self.failed(ran))?;
        let issued = read(&printed).map_err(|wrong| {
            Error::new(
                Kind::AuthRequired,
                format!(
                    "{} printed no keys fetchloom can read: {wrong}",
                    self.named()
                ),
            )
            .help(match self.by {
                By::Process => "a credential_process prints JSON with Version 1, AccessKeyId and SecretAccessKey, and SessionToken and Expiration for keys that expire",
                By::Cli { .. } => "update the AWS CLI to version 2.9 or later",
            })
        })?;
        if let Some(expires) = issued.expires.filter(|expires| *expires <= now) {
            let help = match self.by {
                By::Cli { signs_in: true } => format!(
                    "sign in again with `aws sso login --profile {}`, and check this machine's clock",
                    self.profile
                ),
                _ => "check that the program gives keys that hold, and this machine's clock"
                    .to_owned(),
            };
            return Err(Error::new(
                Kind::AuthRequired,
                format!(
                    "the keys {} gave expired at {}",
                    self.named(),
                    crate::project::lock::timestamp(expires)
                ),
            )
            .help(help));
        }
        tracing::debug!(key = %issued.keys.shown(), expires = ?issued.expires.map(crate::project::lock::timestamp), "the program gave AWS keys");
        Ok(issued)
    }

    /// how messages name the program.
    fn named(&self) -> String {
        match self.by {
            By::Process => format!("the credential_process of the AWS profile {}", self.profile),
            By::Cli { .. } => format!(
                "`aws configure export-credentials --profile {}`",
                self.profile
            ),
        }
    }

    /// the error for a run that went wrong, which says nothing the program printed.
    fn failed(&self, ran: Ran) -> Error {
        let named = self.named();
        let Some(message) = ran.said(&named, "keys", &self.path, self.patience) else {
            return Stop::stopped();
        };
        let help = match self.by {
            By::Process => "run the credential_process yourself to see what went wrong; fetchloom shows nothing it prints, since that can hold keys".to_owned(),
            By::Cli { signs_in: true } => format!(
                "sign in again with `aws sso login --profile {0}`, or run `aws configure export-credentials --profile {0}` yourself to see what went wrong",
                self.profile
            ),
            By::Cli { signs_in: false } => format!(
                "run `aws configure export-credentials --profile {}` yourself to see what went wrong",
                self.profile
            ),
        };
        Error::new(Kind::AuthRequired, message).help(help)
    }
}

/// the keys a program printed, in version 1 of the form AWS gives `credential_process`, or what
/// is wrong with them, which never repeats what was printed.
fn read(printed: &[u8]) -> Result<Issued, &'static str> {
    let value: Value = serde_json::from_slice(printed).map_err(|_| "it is not JSON")?;
    let object = value.as_object().ok_or("it is not a JSON object")?;
    if object.get("Version").and_then(Value::as_u64) != Some(1) {
        return Err("its Version is not 1");
    }
    let text = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let id = text("AccessKeyId").ok_or("it has no AccessKeyId")?;
    let secret = text("SecretAccessKey").ok_or("it has no SecretAccessKey")?;
    let token = match object.get("SessionToken") {
        None | Some(Value::Null) => None,
        Some(Value::String(token)) => Some(token.clone()).filter(|token| !token.is_empty()),
        Some(_) => return Err("its SessionToken is not text"),
    };
    let expires = match object.get("Expiration") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(instant(text).ok_or(NO_TIME)?),
        Some(_) => return Err(NO_TIME),
    };
    Ok(Issued {
        keys: Keys { id, secret, token },
        expires,
    })
}

/// the moment an RFC 3339 time names, such as `2026-11-12T12:00:00Z` or
/// `2026-11-12T13:00:00.5+01:00`. a time without its offset from UTC names no moment.
fn instant(text: &str) -> Option<SystemTime> {
    let parsed: toml::value::Datetime = text.trim().parse().ok()?;
    let (date, time, offset) = (parsed.date?, parsed.time?, parsed.offset?);
    let offset = match offset {
        toml::value::Offset::Z => 0,
        toml::value::Offset::Custom { minutes } => i64::from(minutes) * 60,
    };
    let seconds = days(
        i64::from(date.year),
        i64::from(date.month),
        i64::from(date.day),
    ) * 86_400
        + i64::from(time.hour) * 3600
        + i64::from(time.minute) * 60
        + i64::from(time.second.unwrap_or(0))
        - offset;
    // a time before 1970 has long passed, as the epoch has
    let since = Duration::new(
        u64::try_from(seconds).unwrap_or(0),
        time.nanosecond.unwrap_or(0),
    );
    UNIX_EPOCH.checked_add(since)
}

/// days from 1970-01-01 to a date, as in Howard Hinnant's chrono algorithms.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// the words of a command line as the AWS CLI splits a `credential_process`: by posix shell
/// quoting, or on `windows` by the rules windows programs read their command line with. none
/// for a quote that never closes, or a posix line that ends in a lone `\`.
fn words(text: &str, windows: bool) -> Option<Vec<String>> {
    if windows {
        windows_words(text)
    } else {
        posix_words(text)
    }
}

/// words as python's `shlex.split` gives them: single quotes keep everything, double quotes keep
/// everything but `\"` and `\\`, and a `\` outside quotes keeps the next character.
fn posix_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

/// words as the microsoft C runtime reads a command line: spaces and tabs part words outside
/// double quotes, and backslashes are kept as they are unless they come before a double quote,
/// where each pair is one backslash and one left over makes the quote a character.
fn windows_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    let mut slashes = 0;
    for c in text.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                let word = word.get_or_insert_with(String::new);
                word.extend(repeat_n('\\', slashes / 2));
                if slashes % 2 == 1 {
                    word.push('"');
                } else {
                    quoted = !quoted;
                }
                slashes = 0;
            }
            ' ' | '\t' if !quoted => {
                if slashes > 0 {
                    word.get_or_insert_with(String::new)
                        .extend(repeat_n('\\', slashes));
                    slashes = 0;
                }
                words.extend(word.take());
            }
            c => {
                let word = word.get_or_insert_with(String::new);
                word.extend(repeat_n('\\', slashes));
                slashes = 0;
                word.push(c);
            }
        }
    }
    if quoted {
        return None;
    }
    if slashes > 0 {
        word.get_or_insert_with(String::new)
            .extend(repeat_n('\\', slashes));
    }
    words.extend(word);
    Some(words)
}

/// the first file named `name` this user may run, `name.exe` on windows, in a folder PATH names
/// by its full path. a folder named relatively, such as `.`, is never searched: it would be
/// whatever folder fetchloom runs in, which a cloned project could fill.
fn installed(name: &str, var: Var<'_>) -> Option<PathBuf> {
    let file = if cfg!(windows) && Path::new(name).extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    crate::machine::installed(&var("PATH")?, &file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_as_the_aws_cli_splits_them() {
        for (text, posix, windows) in [
            (
                "keys --as lab",
                Some(vec!["keys", "--as", "lab"]),
                Some(vec!["keys", "--as", "lab"]),
            ),
            (
                r#""/opt/my keys/print" --as "two words""#,
                Some(vec!["/opt/my keys/print", "--as", "two words"]),
                Some(vec!["/opt/my keys/print", "--as", "two words"]),
            ),
            (
                r"'two words' a\ b $HOME",
                Some(vec!["two words", "a b", "$HOME"]),
                Some(vec!["'two", "words'", r"a\", "b", "$HOME"]),
            ),
            (
                r#""C:\Program Files\keys.exe" """#,
                Some(vec![r"C:\Program Files\keys.exe", ""]),
                Some(vec![r"C:\Program Files\keys.exe", ""]),
            ),
            (
                r#"C:\tools\keys.exe a\"b"#,
                Some(vec!["C:toolskeys.exe", "a\"b"]),
                Some(vec![r"C:\tools\keys.exe", "a\"b"]),
            ),
            (
                r#""a \"b\" \\ \$""#,
                Some(vec![r#"a "b" \ \$"#]),
                Some(vec![r#"a "b" \\ \$"#]),
            ),
            (r#""never closed"#, None, None),
            (r"ends\", None, Some(vec![r"ends\"])),
            ("  ", Some(vec![]), Some(vec![])),
        ] {
            assert_eq!(
                words(text, false),
                posix.map(|words| words.into_iter().map(str::to_owned).collect()),
                "{text}"
            );
            assert_eq!(
                words(text, true),
                windows.map(|words| words.into_iter().map(str::to_owned).collect()),
                "{text}"
            );
        }
    }

    #[test]
    fn printed_keys_are_read_and_a_fault_never_repeats_what_was_printed() {
        let issued = read(
            br#"{"Version": 1, "AccessKeyId": "AKIAPRINTED", "SecretAccessKey": "printed-secret", "SessionToken": "printed-session", "Expiration": "2026-11-12T13:00:00+01:00", "AccountId": "123456789012"}"#,
        )
        .unwrap();
        assert_eq!(
            issued.keys,
            Keys {
                id: "AKIAPRINTED".to_owned(),
                secret: "printed-secret".to_owned(),
                token: Some("printed-session".to_owned()),
            }
        );
        assert_eq!(
            issued
                .expires
                .map(crate::project::lock::timestamp)
                .as_deref(),
            Some("2026-11-12T12:00:00Z")
        );
        let lasting = read(
            br#"{"Version": 1, "AccessKeyId": "AKIALASTING", "SecretAccessKey": "printed-secret", "SessionToken": "", "Expiration": null}"#,
        )
        .unwrap();
        assert_eq!((lasting.keys.token, lasting.expires), (None, None));
        for (printed, wrong) in [
            ("printed-secret", "it is not JSON"),
            (r#"["printed-secret"]"#, "it is not a JSON object"),
            (
                r#"{"Version": "1", "AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret"}"#,
                "its Version is not 1",
            ),
            (
                r#"{"AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret"}"#,
                "its Version is not 1",
            ),
            (
                r#"{"Version": 1, "SecretAccessKey": "printed-secret"}"#,
                "it has no AccessKeyId",
            ),
            (
                r#"{"Version": 1, "AccessKeyId": "AKIA", "SecretAccessKey": ""}"#,
                "it has no SecretAccessKey",
            ),
            (
                r#"{"Version": 1, "AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret", "SessionToken": 7}"#,
                "its SessionToken is not text",
            ),
            (
                r#"{"Version": 1, "AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret", "Expiration": "printed-secret"}"#,
                NO_TIME,
            ),
            (
                r#"{"Version": 1, "AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret", "Expiration": "2026-11-12T12:00:00"}"#,
                NO_TIME,
            ),
            (
                r#"{"Version": 1, "AccessKeyId": "AKIA", "SecretAccessKey": "printed-secret", "Expiration": 1800000000}"#,
                NO_TIME,
            ),
        ] {
            assert_eq!(read(printed.as_bytes()).err(), Some(wrong), "{printed}");
        }
    }

    #[test]
    fn an_rfc_3339_time_names_its_moment() {
        let utc = |text| instant(text).map(crate::project::lock::timestamp);
        for (text, moment) in [
            ("2026-11-12T12:00:00Z", "2026-11-12T12:00:00Z"),
            ("2026-11-12T13:30:00+01:30", "2026-11-12T12:00:00Z"),
            ("2026-11-12 07:00:00-05:00", "2026-11-12T12:00:00Z"),
            ("2024-02-29T23:59:59.999999+00:00", "2024-02-29T23:59:59Z"),
            ("1999-12-31T23:00:00-01:00", "2000-01-01T00:00:00Z"),
        ] {
            assert_eq!(utc(text).as_deref(), Some(moment), "{text}");
        }
        assert_eq!(
            instant("2026-11-12T12:00:00.25Z"),
            instant("2026-11-12T12:00:00Z").map(|moment| moment + Duration::from_millis(250))
        );
        for text in ["2026-11-12T12:00:00", "2026-11-12", "12:00:00Z", "soon", ""] {
            assert_eq!(instant(text), None, "{text}");
        }
    }

    /// the `credential_process` of the profile `lab`: `script` run by sh, which may take
    /// `patience`.
    #[cfg(unix)]
    fn script(folder: &Path, script: &str, patience: Duration) -> Program {
        use std::os::unix::fs::PermissionsExt;
        let path = folder.join("keys");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Program {
            path,
            args: Vec::new(),
            profile: "lab".to_owned(),
            by: By::Process,
            patience,
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_hangs_prints_too_much_or_cannot_start_is_ended_and_named() {
        use crate::source::recorded::block_on;
        let folder = tempfile::tempdir().unwrap();
        let named = "the credential_process of the AWS profile lab";
        let missing = folder.path().join("missing");
        for (body, patience, message) in [
            (
                "printf 'printed-secret'\nexec sleep 30",
                Duration::from_millis(50),
                format!("{named} gave no keys within 50 ms, so fetchloom ended it"),
            ),
            (
                "while :; do printf 'printed-secret '; done",
                PATIENCE,
                format!("{named} printed more than 64 KiB"),
            ),
            (
                "printf 'printed-secret'\nexit 3",
                PATIENCE,
                format!("{named} exited with code 3"),
            ),
        ] {
            let program = script(folder.path(), body, patience);
            let error = block_on(program.run(UNIX_EPOCH, &Stop::default())).unwrap_err();
            assert_eq!(error.kind, Kind::AuthRequired, "{body}");
            assert_eq!(error.message, message, "{body}");
            assert!(!error.help.unwrap_or_default().contains("printed-secret"));
        }
        let mut program = script(folder.path(), "", PATIENCE);
        program.path = missing;
        let error = block_on(program.run(UNIX_EPOCH, &Stop::default())).unwrap_err();
        assert_eq!(
            error.message,
            format!(
                "{named} could not start {}: it does not exist",
                program.path.display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stop_ends_a_program_or_keeps_it_from_starting() {
        use crate::source::recorded::block_on;
        let folder = tempfile::tempdir().unwrap();
        let started = folder.path().join("started");
        let made = std::process::Command::new("mkfifo")
            .arg(&started)
            .status()
            .unwrap();
        assert!(made.success());
        let program = script(
            folder.path(),
            &format!("printf started > '{}'\nexec sleep 30", started.display()),
            PATIENCE,
        );
        let stop = std::sync::Arc::new(Stop::default());
        let asking = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                // reading the pipe waits until the program has opened it and written to it
                std::fs::read(&started).unwrap();
                stop.ask();
            })
        };
        let error = block_on(program.run(UNIX_EPOCH, &stop)).unwrap_err();
        asking.join().unwrap();
        assert_eq!(error.kind, Kind::Interrupted);
        let ran = folder.path().join("ran");
        let program = script(folder.path(), &format!(": > '{}'", ran.display()), PATIENCE);
        let error = block_on(program.run(UNIX_EPOCH, &stop)).unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(!ran.exists());
    }
}
