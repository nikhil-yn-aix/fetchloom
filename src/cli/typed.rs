//! commands as a person types them: words quoted for a shell, a suggested command read back with
//! fetchloom's grammar, and this run's own command line, which help repeats.

use std::borrow::Cow;

use super::args;
use crate::net;

/// the words of this run's command line after the program's name, as typed, kept for help that
/// suggests a corrected command line. tests that never `run` leave it unset.
pub(super) static TYPED: std::sync::OnceLock<Vec<std::ffi::OsString>> = std::sync::OnceLock::new();

/// whether `command`, as help quotes it, is a whole command line fetchloom accepts, which a script
/// handed it as the next step can run. `fetchloom add` alone is not one.
pub(crate) fn runs(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .is_some_and(|words| args::Args::try_parse_from(words).is_ok())
}

/// whether `command`, as help quotes it, is one a script may run with no person to ask, as
/// [`args::Command::harmless`] says. one with `--yes` never is, since it answers for a person.
pub(crate) fn harmless(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| {
            !args.global.yes && args.command.is_some_and(|command| command.harmless())
        })
}

/// whether `command`, as help quotes it, may write data.lock, which a run asked to leave it as it
/// is never hands a script as the next step.
pub(crate) fn changes_lock(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| args.command.is_some_and(|command| command.writes_lock()))
}

/// the words of a suggested `command`, read through its literal quoting. none for an expansion or
/// a shell operator, which it never interprets.
fn command_words(command: &str, windows: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(delimiter) = quote {
            let powershell_single = windows && delimiter == '\'';
            if c == delimiter || (powershell_single && is_single_quote(c)) {
                match chars.next_if(|&next| powershell_single && is_single_quote(next)) {
                    Some(next) => word.get_or_insert_default().push(next),
                    None => quote = None,
                }
            } else if delimiter == '"' && c == '\\' && !windows {
                let next = *chars.peek()?;
                if matches!(next, '"' | '\\' | '$' | '\u{60}') {
                    chars.next();
                    word.get_or_insert_default().push(next);
                } else {
                    word.get_or_insert_default().push(c);
                }
            } else if delimiter == '"' && matches!(c, '$' | '\u{60}') {
                return None;
            } else {
                word.get_or_insert_default().push(c);
            }
        } else {
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    word.get_or_insert_default();
                }
                '\n' | '\r' | '|' | '&' | ';' | '<' | '>' | '$' | '\u{60}' | '(' | ')' | '*'
                | '?' | '[' | ']' => return None,
                c if c.is_whitespace() => words.extend(word.take()),
                '\\' if !windows => word.get_or_insert_default().push(chars.next()?),
                _ => word.get_or_insert_default().push(c),
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    words.extend(word);
    Some(words)
}

/// quotes a word so suggested commands preserve spaces, patterns and shell metacharacters.
pub(in crate::cli) fn shell_word(word: &str) -> Cow<'_, str> {
    shell_word_for(word, cfg!(windows))
}

/// [`shell_word`] for powershell and cmd on windows, or for bash, zsh and fish. a word starting
/// with `~` or `=`, which zsh expands, or `@`, which powershell splats, is quoted. double quotes
/// keep it literal unless it holds `$`, a backtick or a double quote, on windows a curly double
/// quote, elsewhere `!` (history) or a backslash; then it is single-quoted, each single quote
/// doubled on windows, curly ones too, and elsewhere a single quote or backslash escaped outside
/// the quotes, since fish reads a backslash inside them as an escape.
fn shell_word_for(word: &str, windows: bool) -> Cow<'_, str> {
    if !word.is_empty()
        && !word.starts_with(['~', '=', '@'])
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,~".contains(c))
    {
        return Cow::Borrowed(word);
    }
    let literal = |c: char| match c {
        '$' | '\u{60}' | '"' => false,
        '!' | '\\' => windows,
        '\u{201c}'..='\u{201e}' => !windows,
        _ => true,
    };
    if word.chars().all(literal) {
        return Cow::Owned(format!("\"{word}\""));
    }
    if windows {
        let mut quoted = String::with_capacity(word.len() + 2);
        quoted.push('\'');
        for c in word.chars() {
            if is_single_quote(c) {
                quoted.push(c);
            }
            quoted.push(c);
        }
        quoted.push('\'');
        return Cow::Owned(quoted);
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    let mut open = false;
    for c in word.chars() {
        if matches!(c, '\'' | '\\') {
            if open {
                quoted.push('\'');
                open = false;
            }
            quoted.push('\\');
        } else if !open {
            quoted.push('\'');
            open = true;
        }
        quoted.push(c);
    }
    if open {
        quoted.push('\'');
    }
    Cow::Owned(quoted)
}

/// whether powershell reads `c` as a single quote, as it reads the curly ones.
fn is_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}'..='\u{201b}')
}

/// the typed command line for the moved project, without `-C` and its folder, attached or not.
pub(super) fn moved_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let command = <args::Args as clap::CommandFactory>::command();
    let mut kept = Vec::new();
    let mut positional = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            positional = true;
        }
        if !positional
            && let Some(word) = arg.to_str()
            && word.starts_with('-')
            && !word.starts_with("--")
            && let Some(at) = word.find('C')
            && word[1..at].chars().all(|short| {
                command
                    .get_arguments()
                    .any(|arg| arg.get_short() == Some(short) && !arg.get_action().takes_values())
            })
        {
            if at > 1 {
                kept.push(std::ffi::OsString::from(&word[..at]));
            }
            if at + 1 == word.len() {
                args.next();
            }
        } else {
            kept.push(arg);
        }
    }
    command_line(kept.into_iter())
}

/// the command line as typed, ready to copy, for help that says to run it again. none when an
/// argument holds a link with a query, a fragment or a login, or is not unicode.
pub(super) fn command_line(args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let mut line = String::from("fetchloom");
    for arg in args {
        let arg = arg.into_string().ok()?;
        if may_hold_secret(&arg) {
            return None;
        }
        line.push(' ');
        line.push_str(&shell_word(&arg));
    }
    Some(line)
}

/// whether `arg` holds a link with a query, a fragment or a login, which may carry a secret under
/// a name no list knows, as dropbox's `rlkey`, even after `=` or a short option or percent-encoded.
fn may_hold_secret(arg: &str) -> bool {
    if matches!(net::mask_query(arg), Cow::Owned(_)) {
        return true;
    }
    let decoded;
    let arg = if !arg.contains(':') && arg.contains('%') {
        decoded = percent_encoding::percent_decode_str(arg).decode_utf8_lossy();
        decoded.as_ref()
    } else {
        arg
    };
    let value = arg
        .starts_with('-')
        .then(|| arg.find('=').map(|at| at + 1))
        .flatten();
    let schemes = arg.match_indices("://").map(|(at, _)| {
        let run = arg[..at]
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || "+-.".contains(*c)))
            .map_or(0, |(before, c)| before + c.len_utf8());
        // a scheme starts with a letter, so `-ihttps://` holds the link `ihttps://`
        arg[run..at]
            .find(|c: char| c.is_ascii_alphabetic())
            .map_or(at, |letter| run + letter)
    });
    std::iter::once(0).chain(value).chain(schemes).any(|start| {
        url::Url::parse(&arg[start..]).is_ok_and(|url| {
            url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
        })
    })
}

/// the command line as typed with the words `typed` changed to `meant`, so a flag such as
/// `--dry-run` stays in what a person pastes, or `instead` when that line cannot be repeated.
pub(in crate::cli) fn meant(typed: &[&str], meant: &[&str], instead: String) -> String {
    TYPED
        .get()
        .and_then(|line| retyped(line, typed, meant))
        .unwrap_or(instead)
}

/// [`meant`]'s line from `line`: the words `typed` change where they stand last, alone or beside
/// an `=`, as in `--limt=5`. none when they stand nowhere or the line cannot be repeated.
fn retyped(line: &[std::ffi::OsString], typed: &[&str], meant: &[&str]) -> Option<String> {
    let mut line = line.to_vec();
    let count = typed.len();
    let alone = (0..=line.len().checked_sub(count)?).rev().find(|&at| {
        line[at..at + count]
            .iter()
            .zip(typed)
            .all(|(arg, word)| arg == *word)
    });
    if let Some(at) = alone {
        line.splice(at..at + count, meant.iter().map(Into::into));
    } else {
        let ([word], [replacement]) = (typed, meant) else {
            return None;
        };
        let (at, changed) = line.iter().enumerate().rev().find_map(|(at, arg)| {
            let changed = match arg.to_str()?.split_once('=') {
                Some((option, value)) if option == *word && word.starts_with('-') => {
                    format!("{replacement}={value}")
                }
                Some((option, value)) if value == *word && option.starts_with('-') => {
                    format!("{option}={replacement}")
                }
                _ => return None,
            };
            Some((at, changed))
        })?;
        line[at] = changed.into();
    }
    command_line(line.into_iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_commands_keep_other_option_values_and_literal_arguments() {
        let line = |args: &[&str]| moved_command(args.iter().map(std::ffi::OsString::from));
        assert_eq!(
            line(&[
                "-C",
                "old",
                "get",
                "https://example.org/data.csv",
                "-oCaseStudy"
            ])
            .as_deref(),
            Some("fetchloom get https://example.org/data.csv -oCaseStudy")
        );
        assert_eq!(
            line(&["-qvCold", "sync", "--offline"]).as_deref(),
            Some("fetchloom -qv sync --offline")
        );
        assert_eq!(
            line(&["-C", "old", "get", "--", "-CaseStudy"]).as_deref(),
            Some("fetchloom get -- -CaseStudy")
        );
        assert_eq!(
            line(&[
                "-C",
                "old",
                "get",
                "https://example.org/data.csv?token=secret"
            ]),
            None
        );
    }

    #[test]
    fn a_guess_keeps_every_other_word_of_the_command_line() {
        let line = |args: &[&str], typed: &[&str], meant: &[&str]| {
            let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
            retyped(&args, typed, meant)
        };
        for (args, typed, meant, expected) in [
            (
                &["remove", "--dry-run", "irs"][..],
                &["irs"][..],
                &["iris"][..],
                "fetchloom remove --dry-run iris",
            ),
            (
                &["-C", "irs", "remove", "irs", "-n"],
                &["irs"],
                &["iris"],
                "fetchloom -C irs remove iris -n",
            ),
            (
                &["lock", "--upgrade=irs"],
                &["irs"],
                &["iris"],
                "fetchloom lock --upgrade=iris",
            ),
            (
                &["search", "iris", "--limt=5"],
                &["--limt"],
                &["--limit"],
                "fetchloom search iris --limit=5",
            ),
            (
                &["help", "cache", "gx", "--json"],
                &["cache", "gx"],
                &["cache", "gc"],
                "fetchloom help cache gc --json",
            ),
            (
                &["verify", "-q"],
                &["verify"],
                &["cache", "verify"],
                "fetchloom cache verify -q",
            ),
            (
                &["add", "fasion mnist", "--select", "*.csv"],
                &["fasion mnist"],
                &["fashion mnist"],
                "fetchloom add \"fashion mnist\" --select \"*.csv\"",
            ),
            (
                &["-q", "Search", "iris", "--source", "hf"],
                &["Search"],
                &["search"],
                "fetchloom -q search iris --source hf",
            ),
            (
                &["ad", "iris", "--dry-run"],
                &["ad"],
                &["add"],
                "fetchloom add iris --dry-run",
            ),
            (
                &["add", "--yse", "zenodo:3608990"],
                &["--yse"],
                &["--yes"],
                "fetchloom add --yes zenodo:3608990",
            ),
            (
                &["cache", "verfiy", "--json"],
                &["verfiy"],
                &["verify"],
                "fetchloom cache verify --json",
            ),
        ] {
            assert_eq!(
                line(args, typed, meant).as_deref(),
                Some(expected),
                "{args:?}"
            );
        }
        assert_eq!(line(&["remove", "iris"], &["irs"], &["iris"]), None);
        assert_eq!(
            line(
                &["serch", "https://example.org/a.csv?token=abc"],
                &["serch"],
                &["search"]
            ),
            None
        );
        assert_eq!(
            meant(&["irs"], &["iris"], "fetchloom sync iris".to_owned()),
            "fetchloom sync iris",
            "without the line typed, the command alone"
        );
    }

    #[test]
    fn the_command_line_is_named_as_typed_unless_it_holds_a_secret() {
        let line = |args: &[&str]| command_line(args.iter().map(std::ffi::OsString::from));
        assert_eq!(
            line(&["add", "zenodo:3608990", "--select", "*.csv"]).as_deref(),
            Some("fetchloom add zenodo:3608990 --select \"*.csv\"")
        );
        assert_eq!(
            line(&["-C", "my project", "sync"]).as_deref(),
            Some("fetchloom -C \"my project\" sync")
        );
        assert_eq!(line(&["get", "https://example.org/a.csv?token=abc"]), None);
        assert_eq!(
            line(&["get", "https://user:secret@example.org/a.csv"]),
            None
        );
        // a name no list knows may hold a secret too, as dropbox's rlkey does
        for link in [
            "https://example.org/a.csv?rlkey=abc",
            "https://example.org/a.csv?download=1",
            "https://example.org/a.csv#abc",
            "s3://key:abc@bucket/a.csv",
            "https%3A%2F%2Fexample.org%2Fa.csv%3Fsv%3Dabc",
        ] {
            assert_eq!(line(&["get", link]), None, "{link}");
        }
        assert_eq!(
            line(&["--mirror=https://lab.example.org/m?code=abc", "sync"]),
            None,
            "a link given as an option's value"
        );
        assert_eq!(
            line(&["-ihttps://example.org/list.txt#abc", "get"]),
            None,
            "a link attached to a short option"
        );
        assert_eq!(
            line(&["get", "https://example.org/a.csv", "-o", "a#1.csv"]).as_deref(),
            Some("fetchloom get https://example.org/a.csv -o \"a#1.csv\"")
        );
        assert_eq!(
            line(&["add", "hf:org/name@main"]).as_deref(),
            Some("fetchloom add hf:org/name@main")
        );
    }

    #[test]
    fn a_word_a_shell_would_split_or_expand_is_quoted() {
        assert_eq!(shell_word("zenodo:3608990"), "zenodo:3608990");
        assert_eq!(
            shell_word("https://example.org/a.csv?download=1&v=2"),
            "\"https://example.org/a.csv?download=1&v=2\""
        );
        assert_eq!(shell_word("*.csv"), "\"*.csv\"");
        assert_eq!(shell_word("my data"), "\"my data\"");
    }

    /// a word a shell would expand or split, quoted for bash, zsh and fish, then for powershell,
    /// and for cmd but with `$`, a backtick or a double quote, which nothing keeps literal in both.
    const QUOTED: [(&str, &str, &str); 13] = [
        ("hi!.csv", "'hi!.csv'", "\"hi!.csv\""),
        ("$notes", "'$notes'", "'$notes'"),
        ("a`b`", "'a`b`'", "'a`b`'"),
        ("ada's data", "\"ada's data\"", "\"ada's data\""),
        ("it's $5!", "'it'\\''s $5!'", "'it''s $5!'"),
        ("my data", "\"my data\"", "\"my data\""),
        ("données é.csv", "\"données é.csv\"", "\"données é.csv\""),
        (r"data\notes", r"'data'\\'notes'", r#""data\notes""#),
        ("a\"b", "'a\"b'", "'a\"b'"),
        ("~notes.csv", "\"~notes.csv\"", "\"~notes.csv\""),
        ("=notes.csv", "\"=notes.csv\"", "\"=notes.csv\""),
        ("@notes.csv", "\"@notes.csv\"", "\"@notes.csv\""),
        (
            "ada’s “best” data",
            "\"ada’s “best” data\"",
            "'ada’’s “best” data'",
        ),
    ];

    #[test]
    fn suggested_commands_keep_shell_metacharacters_literal() {
        for (word, posix, windows) in QUOTED {
            for (on_windows, quoted) in [(false, posix), (true, windows)] {
                assert_eq!(shell_word_for(word, on_windows), quoted, "{word}");
                let line = format!("fetchloom get {quoted}");
                assert_eq!(
                    command_words(&line, on_windows),
                    Some(vec![
                        "fetchloom".to_owned(),
                        "get".to_owned(),
                        word.to_owned()
                    ]),
                    "{line}"
                );
            }
        }
    }

    /// interactive bash and zsh expand `!` inside double quotes from their history. powershell
    /// reads the same wherever it runs, so it gets windows' quoting.
    #[cfg(unix)]
    #[test]
    fn a_quoted_word_reads_back_whole_in_each_shell() {
        let words: Vec<&str> = QUOTED.iter().map(|(word, _, _)| *word).collect();
        let printf = "printf '%s\\n'";
        for (shell, windows, print) in [
            (
                &["bash", "--norc", "--noprofile", "-i", "-s"][..],
                false,
                printf,
            ),
            (&["sh", "-s"], false, printf),
            (&["zsh", "-f", "-i", "-s"], false, printf),
            (&["fish", "--no-config"], false, printf),
            (
                &["pwsh", "-NoProfile", "-NonInteractive", "-Command", "-"],
                true,
                "Write-Output",
            ),
        ] {
            let line: Vec<String> = words
                .iter()
                .map(|word| shell_word_for(word, windows).into_owned())
                .collect();
            let script = format!("{print} {}\n", line.join(" "));
            let started = std::process::Command::new(shell[0])
                .args(&shell[1..])
                .env("HISTFILE", "/dev/null")
                .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
                .env("TERM", "dumb")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            let mut child = match started {
                Ok(child) => child,
                Err(error) => {
                    let reason = format!("{} cannot start: {error}", shell[0]);
                    crate::test_skips::skip(shell[0], &reason);
                    continue;
                }
            };
            std::io::Write::write_all(child.stdin.as_mut().unwrap(), script.as_bytes()).unwrap();
            drop(child.stdin.take());
            let output = child.wait_with_output().unwrap();
            let told = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                told.lines().collect::<Vec<_>>(),
                words,
                "{}: {script}",
                shell[0]
            );
        }
    }

    #[test]
    fn quoted_retry_commands_are_runnable() {
        for command in [
            r#"fetchloom -C "my project" status"#,
            "fetchloom -C 'my project' list",
            "fetchloom status",
        ] {
            assert!(runs(command), "{command}");
        }
        assert!(!runs("fetchloom -C 'an unfinished quote status"));
    }
    #[test]
    fn retry_words_preserve_literal_quoting() {
        for windows in [false, true] {
            assert_eq!(
                command_words(r#"fetchloom -C "my project" status"#, windows).unwrap(),
                ["fetchloom", "-C", "my project", "status"],
            );
            assert_eq!(
                command_words("fetchloom -C '' status", windows).unwrap(),
                ["fetchloom", "-C", "", "status"],
            );
            for command in [
                "fetchloom get a | fetchloom get b",
                "fetchloom get $dataset",
                "fetchloom get a; fetchloom get b",
                "fetchloom get a\nfetchloom get b",
                "fetchloom -C \"$directory\" status",
            ] {
                assert!(command_words(command, windows).is_none(), "{command}");
            }
        }
        assert_eq!(
            command_words(r#"fetchloom -C 'ada'"'"'s project' status"#, false).unwrap(),
            ["fetchloom", "-C", "ada's project", "status"],
        );
        assert_eq!(
            command_words("fetchloom -C 'ada''s project' status", true).unwrap(),
            ["fetchloom", "-C", "ada's project", "status"],
        );
    }
}
