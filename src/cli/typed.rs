//! commands as a person types them: a suggested command read back with fetchloom's grammar, and
//! this run's own command line, which help repeats.

use std::borrow::Cow;

use super::args;
use crate::net;
use crate::output::Runnable;
use crate::suggested::{command_words, shell_word};

/// the words of this run's command line after the program's name, as typed, kept for help that
/// suggests a corrected command line. tests that never `run` leave it unset.
pub(super) static TYPED: std::sync::OnceLock<Vec<std::ffi::OsString>> = std::sync::OnceLock::new();

/// how `--json`'s `fix` may treat `command`, as help quotes it: none when it is no whole command
/// line fetchloom accepts.
pub(crate) fn judged(command: &str) -> Option<Runnable> {
    runs(command).then(|| Runnable {
        harmless: harmless(command),
        changes_lock: changes_lock(command),
    })
}

/// whether `command`, as help quotes it, is a whole command line fetchloom accepts, which a script
/// handed it as the next step can run. `fetchloom add` alone is not one.
fn runs(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .is_some_and(|words| args::Args::try_parse_from(words).is_ok())
}

/// whether `command`, as help quotes it, is one a script may run with no person to ask, as
/// [`args::Command::harmless`] says. one with `--yes` never is, since it answers for a person.
fn harmless(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| {
            !args.global.yes && args.command.is_some_and(|command| command.harmless())
        })
}

/// whether `command`, as help quotes it, may write data.lock, which a run asked to leave it as it
/// is never hands a script as the next step.
fn changes_lock(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| args.command.is_some_and(|command| command.writes_lock()))
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

/// this run's command line as typed, for a person to run again from a new folder inside the one
/// it ran in, so each relative path, as `./stuff` or the list `-i links.txt` reads, starts one
/// folder further up. none when the line cannot be repeated, or gave `-C`, whose own folder
/// changes where its paths are read from.
pub(in crate::cli) fn one_folder_down() -> Option<String> {
    let line = TYPED.get()?;
    let root = <args::Args as clap::CommandFactory>::command();
    // an option's value, such as a pattern after --select, is no path
    let valued = |word: &str| {
        std::iter::once(&root)
            .chain(root.get_subcommands())
            .flat_map(clap::Command::get_arguments)
            .filter(|arg| arg.get_action().takes_values())
            .any(|arg| match word.strip_prefix("--") {
                Some(long) => arg.get_long() == Some(long),
                None => {
                    word.len() == 2
                        && word.starts_with('-')
                        && arg.get_short() == word.chars().nth(1)
                }
            })
    };
    let mut words = Vec::with_capacity(line.len());
    let (mut listed_next, mut value_next) = (false, false);
    for word in line {
        let text = word.to_str()?;
        if text.starts_with('-') && !text.starts_with("--") && text.contains('C') {
            return None;
        }
        let moved = if std::mem::take(&mut listed_next) {
            up_one(text, text != "-")
        } else if std::mem::take(&mut value_next) {
            text.to_owned()
        } else if text == "-i" || text == "--input-file" {
            listed_next = true;
            text.to_owned()
        } else if valued(text) {
            value_next = true;
            text.to_owned()
        } else if let Some(list) = text.strip_prefix("--input-file=") {
            format!("--input-file={}", up_one(list, list != "-"))
        } else if let Some(list) = text.strip_prefix("-i").filter(|_| !text.starts_with("--")) {
            format!("-i{}", up_one(list, list != "-"))
        } else {
            up_one(text, false)
        };
        words.push(std::ffi::OsString::from(moved));
    }
    command_line(words.into_iter())
}

/// `word` as seen from a folder inside the one it was typed in: a relative path, as
/// [`crate::source::local::is_path`] reads one or as a `relative` list's name, one folder up.
fn up_one(word: &str, relative: bool) -> String {
    let dotted = [".", ".."].iter().any(|dots| {
        word.strip_prefix(dots)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
    });
    let separator = if word.contains('\\') && !word.contains('/') {
        '\\'
    } else {
        '/'
    };
    if dotted {
        let rest = word.strip_prefix('.').filter(|rest| !rest.starts_with('.'));
        return match rest {
            Some(rest) => format!("..{rest}"),
            None => format!("..{separator}{word}"),
        };
    }
    let absolute = std::path::Path::new(word).is_absolute() || word.starts_with(['/', '\\', '~']);
    if relative && !absolute && !word.contains(':') {
        return format!("..{separator}{word}");
    }
    word.to_owned()
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
    fn a_relative_path_starts_one_folder_further_up() {
        for (word, list, moved) in [
            ("./stuff", false, "../stuff"),
            ("../stuff", false, "../../stuff"),
            (".", false, ".."),
            ("..", false, "../.."),
            (".\\stuff", false, "..\\stuff"),
            ("links.txt", true, "../links.txt"),
            ("-", false, "-"),
            ("/data/stuff", true, "/data/stuff"),
            ("~/stuff", true, "~/stuff"),
            ("C:\\data", true, "C:\\data"),
            (
                "https://example.org/a.csv",
                false,
                "https://example.org/a.csv",
            ),
            (".hidden", false, ".hidden"),
        ] {
            assert_eq!(up_one(word, list), moved, "{word}");
        }
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
}
