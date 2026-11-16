//! the fetchloom commands help, hints and `next:` lines name. after a run given `-C`, each names
//! the folder the run worked in, so it finds the same project from whatever folder it runs in.
//! [`shell_word`] quotes a word from data.toml, a link list or a server, so a pasted command runs
//! only what it shows. each command written here is kept, so `--json`'s fix is only ever one of
//! them, never one a name from a project file or a server spells out between backticks.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock, PoisonError};

/// the folder a run given `-C` works in, quoted for a shell. unset without `-C`, as in tests.
/// it is the process's own: `-C` moves the whole process into that folder before any command
/// runs, and every hint the run prints names it, however deep the code that writes the hint.
static DIRECTORY: OnceLock<String> = OnceLock::new();

/// every command this run wrote for a person to run next, as [`written`] keeps them.
static WRITTEN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// has every later [`command`] name `directory`, already quoted for a shell, after `-C`. set once
/// at start; a second set is a bug, which debug builds stop at.
pub(crate) fn set_directory(directory: String) {
    let first = DIRECTORY.set(directory).is_ok();
    debug_assert!(first, "the folder hints name is set once, at start");
}

/// `fetchloom` and `words`, with `-C` and the run's folder between them when the run had `-C`.
pub(crate) fn command(words: &str) -> String {
    written(match DIRECTORY.get() {
        Some(directory) => format!("fetchloom -C {directory} {words}"),
        None => format!("fetchloom {words}"),
    })
}

/// `fetchloom` and `words`, for a command that reads no project, as `auth` and `help` do, so it
/// runs the same from any folder.
pub(crate) fn anywhere(words: &str) -> String {
    written(format!("fetchloom {words}"))
}

/// `command`, a whole command line fetchloom made from words it quoted, kept as one this run
/// wrote, which [`is_written`] knows from then on.
pub(crate) fn written(command: String) -> String {
    WRITTEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(command.clone());
    command
}

/// whether this run wrote `command` through [`written`]. text in a name, a path or a value only
/// spells one out, so help that quotes such text never makes it one.
pub(crate) fn is_written(command: &str) -> bool {
    WRITTEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(command)
}

/// the words of a suggested `command`, read through its literal quoting. none for an expansion or
/// a shell operator, which it never interprets.
pub(crate) fn command_words(command: &str, windows: bool) -> Option<Vec<String>> {
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
            } else if delimiter == '"' && c == '\\' && windows {
                // a program reads backslashes before a double quote as escapes, two for one
                let mut run = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    run += 1;
                }
                let closing = chars.peek() == Some(&'"');
                if closing && run % 2 == 1 {
                    // powershell ends the string there, a program reads on: never a command
                    return None;
                }
                let kept = if closing { run / 2 } else { run };
                word.get_or_insert_default()
                    .extend(std::iter::repeat_n('\\', kept));
            } else if delimiter == '"'
                && (matches!(c, '$' | '\u{60}')
                    || (windows && matches!(c, '\u{201c}'..='\u{201e}')))
            {
                // powershell ends a double-quoted string at a curly double quote too
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
                // powershell starts a quoted string at a curly quote too
                '\u{2018}'..='\u{201e}' if windows => return None,
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

/// quotes a word so suggested commands preserve spaces, patterns and shell metacharacters. on
/// windows a word no quoting keeps literal in both powershell and cmd is withheld, as
/// [`withheld`] tells.
pub(crate) fn shell_word(word: &str) -> Cow<'_, str> {
    shell_word_for(word, cfg!(windows))
}

/// [`shell_word`] for a folder, which a withheld word stands for as `<folder>`.
pub(crate) fn shell_folder(word: &str) -> Cow<'_, str> {
    if word.contains([OPEN, CLOSE]) || (cfg!(windows) && !safe_on_windows(word)) {
        return Cow::Owned(withhold(FOLDER, word));
    }
    shell_word(word)
}

/// every word withheld so far, between its marks, as [`withhold`] wrote it: marks a name in a
/// listing or a project file holds outside a command are text, which [`withheld`] leaves be.
static SPANS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// `word` between the marks of a word withheld from a command, as a `kind` of word, kept so
/// [`withheld`] knows the marks for its own. marks the word holds are written out, as `\u{f8e0}`,
/// so none ends its marks early.
fn withhold(kind: char, word: &str) -> String {
    let word = word
        .replace(OPEN, &OPEN.escape_default().to_string())
        .replace(CLOSE, &CLOSE.escape_default().to_string());
    let span = format!("{OPEN}{kind}{word}{CLOSE}");
    SPANS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(span.clone());
    span
}

/// the first word withheld from a command that `text` starts with, between its marks.
fn span_at(text: &str) -> Option<&str> {
    let end = text.strip_prefix(OPEN)?.find(CLOSE)? + OPEN.len_utf8() + CLOSE.len_utf8();
    let span = &text[..end];
    SPANS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(span)
        .then_some(span)
}

/// `text` with the marks of a withheld word that no command here wrote around it written out, as
/// `\u{f8e0}`, so a line shows a name holding them as it is.
pub(crate) fn marks_written_out(text: &str) -> Cow<'_, str> {
    if !text.contains([OPEN, CLOSE]) {
        return Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if matches!(c, OPEN | CLOSE) {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    Cow::Owned(shown)
}

/// the marks around a word withheld from a command on windows, which [`withheld`] turns into a
/// placeholder and a line naming the word: private-use characters no text fetchloom prints holds.
const OPEN: char = '\u{f8e0}';
const CLOSE: char = '\u{f8e1}';
/// after [`OPEN`], the kind of word withheld, which names its placeholder.
const FILE: char = 'f';
const FOLDER: char = 'd';
const NAME: char = 'n';

/// whether `word` may go into a command a person pastes on windows: none of the characters cmd
/// acts on even inside quotes, `& | < > ^ % !`, nor those powershell's quoting cannot keep, `$`, a
/// backtick or a double quote, straight or curly.
pub(crate) fn safe_on_windows(word: &str) -> bool {
    !word.contains([
        '&', '|', '<', '>', '^', '%', '!', '$', '\u{60}', '"', '\u{201c}', '\u{201d}', '\u{201e}',
        OPEN, CLOSE,
    ])
}

/// `text` with each word [`shell_word`] withheld named by a placeholder, `<file>`, `<folder>` or
/// `<name>`, in double quotes, so no shell reads it as a redirection from a file of its name, and
/// `--json`'s fix never offers it, and a line for each saying which name it stands for. marks a
/// name holds outside a command stay as they are.
pub(crate) fn withheld(text: &str) -> (Cow<'_, str>, Vec<String>) {
    if !text.contains(OPEN) {
        return (Cow::Borrowed(text), Vec::new());
    }
    let mut shown = String::with_capacity(text.len());
    let mut named: Vec<(&str, String)> = Vec::new();
    let mut lines = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        shown.push_str(&rest[..start]);
        let Some(span) = span_at(&rest[start..]) else {
            shown.push(OPEN);
            rest = &rest[start + OPEN.len_utf8()..];
            continue;
        };
        let after = &rest[start + OPEN.len_utf8()..];
        let end = span.len() - OPEN.len_utf8() - CLOSE.len_utf8();
        let mut marked = after[..end].chars();
        let base = match marked.next() {
            Some(FILE) => "file",
            Some(FOLDER) => "folder",
            _ => "name",
        };
        let word = marked.as_str();
        let seen = named
            .iter()
            .find(|(seen, _)| *seen == word)
            .map(|(_, placeholder)| placeholder.clone());
        let placeholder = seen.unwrap_or_else(|| {
            // one the text holds already, as a help told before does, names another word; each
            // taken one needs a `<`, so one past their count is free
            let taken = text.matches('<').count() + named.len();
            let placeholder = (1..=taken + 1)
                .map(|count| match count {
                    1 => format!("<{base}>"),
                    count => format!("<{base} {count}>"),
                })
                .find(|placeholder| {
                    !text.contains(placeholder.as_str())
                        && named.iter().all(|(_, taken)| taken != placeholder)
                })
                .unwrap_or_default();
            lines.push(format!(
                "{placeholder} is {word}, which no command here can hold safely"
            ));
            named.push((word, placeholder.clone()));
            placeholder
        });
        let _ = write!(shown, "\"{placeholder}\"");
        rest = &after[end + CLOSE.len_utf8()..];
    }
    shown.push_str(rest);
    (Cow::Owned(shown), lines)
}

/// `text` with each word [`shell_word`] withheld given back as it is, for a name shown where no
/// command holds it, as in a listing.
pub(crate) fn unmarked(text: &str) -> Cow<'_, str> {
    if !text.contains(OPEN) {
        return Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        shown.push_str(&rest[..start]);
        if let Some(span) = span_at(&rest[start..]) {
            let mut word = span[OPEN.len_utf8()..span.len() - CLOSE.len_utf8()].chars();
            word.next();
            shown.push_str(word.as_str());
            rest = &rest[start + span.len()..];
        } else {
            shown.push(OPEN);
            rest = &rest[start + OPEN.len_utf8()..];
        }
    }
    shown.push_str(rest);
    Cow::Owned(shown)
}

/// [`shell_word`] for powershell and cmd on windows, or for bash, zsh and fish. a word starting
/// with `~` or `=`, which zsh expands, `@`, which powershell splats, or `%`, as fish's `%self`,
/// is quoted, as is one holding `~` after an `=`, which bash expands as in an assignment. on
/// windows a word [`safe_on_windows`] is double-quoted, which powershell and cmd both read as
/// written, and so is one holding a comma, which powershell 5.1 reads as a list and joins again
/// with its numbers rewritten, or starting with one `-` and holding a dot, where powershell splits
/// it; a backslash ending it is doubled inside the quotes, since a program reads `\"` as a quote.
/// any other is withheld between marks for [`withheld`]. elsewhere double
/// quotes keep a word literal unless it holds `$`, a backtick, a double quote, `!` (history) or a
/// backslash; then it is single-quoted, a single quote or backslash escaped outside the quotes,
/// since fish reads a backslash inside them as an escape.
pub(crate) fn shell_word_for(word: &str, windows: bool) -> Cow<'_, str> {
    // a word holding the marks is withheld everywhere: held raw, the marks would be read as a
    // withheld word's, and any other character in their place names another file
    if word.contains([OPEN, CLOSE]) || (windows && !safe_on_windows(word)) {
        // a path to a file, not a host or a link, which only look like one
        let file = word.contains(['/', '\\'])
            && !word.contains("://")
            && std::path::Path::new(word).extension().is_some();
        return Cow::Owned(withhold(if file { FILE } else { NAME }, word));
    }
    let assigned_home = word
        .split_once('=')
        .is_some_and(|(_, value)| value.contains('~'));
    let quoted_anyway = word.is_empty()
        || word.starts_with(['~', '=', '@', '%'])
        || assigned_home
        || (windows && splits_in_powershell(word));
    if !quoted_anyway
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || "-_./:@%+=~".contains(c) || (c == ',' && !windows)
        })
    {
        return Cow::Borrowed(word);
    }
    if windows {
        let ending = word.len() - word.trim_end_matches('\\').len();
        return Cow::Owned(format!("\"{word}{}\"", "\\".repeat(ending)));
    }
    let literal = |c: char| !matches!(c, '$' | '\u{60}' | '"' | '!' | '\\');
    if word.chars().all(literal) {
        return Cow::Owned(format!("\"{word}\""));
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

/// whether powershell would pass a program the bare `word` as two: one starting with one `-`
/// ends at its first dot, as `-ox.part1` does, which reaches 7-Zip as `-ox` and `.part1`.
pub(crate) fn splits_in_powershell(word: &str) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word.contains('.')
}

/// whether powershell reads `c` as a single quote, as it reads the curly ones.
fn is_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}'..='\u{201b}')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_word_a_shell_would_split_or_expand_is_quoted() {
        for windows in [false, true] {
            assert_eq!(shell_word_for("zenodo:3608990", windows), "zenodo:3608990");
            assert_eq!(shell_word_for("*.csv", windows), "\"*.csv\"");
            assert_eq!(shell_word_for("my data", windows), "\"my data\"");
        }
        assert_eq!(
            shell_word_for("https://example.org/a.csv?download=1&v=2", false),
            "\"https://example.org/a.csv?download=1&v=2\""
        );
    }

    /// a word a shell would expand or split, quoted for bash, zsh and fish, then for powershell
    /// and cmd on windows: double quotes, which both read as written, or none for a word holding
    /// what cmd acts on or powershell's quoting cannot keep, which a command never holds there.
    pub(crate) const QUOTED: [(&str, &str, Option<&str>); 20] = [
        ("hi!.csv", "'hi!.csv'", None),
        ("$notes", "'$notes'", None),
        ("a`b`", "'a`b`'", None),
        ("ada's data", "\"ada's data\"", Some("\"ada's data\"")),
        ("it's $5!", "'it'\\''s $5!'", None),
        ("my data", "\"my data\"", Some("\"my data\"")),
        (
            "données é.csv",
            "\"données é.csv\"",
            Some("\"données é.csv\""),
        ),
        (r"data\notes", r"'data'\\'notes'", Some(r#""data\notes""#)),
        ("a\"b", "'a\"b'", None),
        ("~notes.csv", "\"~notes.csv\"", Some("\"~notes.csv\"")),
        ("=notes.csv", "\"=notes.csv\"", Some("\"=notes.csv\"")),
        ("@notes.csv", "\"@notes.csv\"", Some("\"@notes.csv\"")),
        ("ada’s “best” data", "\"ada’s “best” data\"", None),
        ("ada’s data", "\"ada’s data\"", Some("\"ada’s data\"")),
        (
            "notes$ & mkdir PWNED-arch & x.csv",
            "'notes$ & mkdir PWNED-arch & x.csv'",
            None,
        ),
        ("a%PATH%b.csv", "a%PATH%b.csv", None),
        // powershell 5.1 reads a bare `x,1kb` as x and 1024 and passes on `x,1024`
        ("x,1kb", "x,1kb", Some("\"x,1kb\"")),
        // bash expands `~` after the `=` of a word shaped like an assignment, fish expands %self
        ("k=~", "\"k=~\"", Some("\"k=~\"")),
        ("%self", "\"%self\"", None),
        // powershell splits a bare word starting with one `-` at its first dot
        ("-ox.part1.rar", "-ox.part1.rar", Some("\"-ox.part1.rar\"")),
    ];

    #[test]
    fn suggested_commands_keep_shell_metacharacters_literal() {
        for (word, posix, windows) in QUOTED {
            let Some(windows) = windows else {
                continue;
            };
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

    /// cmd hands a program its line, which reads a backslash before a double quote as an
    /// escape, as powershell 5.1 does for a word it quotes for the program itself: a word ending
    /// in a backslash keeps it only doubled inside the quotes.
    #[test]
    fn a_trailing_backslash_stays_in_its_word_on_windows() {
        for (word, quoted) in [
            (r"D:\", r#""D:\\""#),
            (r"D:\my data\", r#""D:\my data\\""#),
            (r"a\b\\", r#""a\b\\\\""#),
        ] {
            assert_eq!(shell_word_for(word, true), quoted, "{word}");
            let line = format!("fetchloom -C {quoted} status");
            assert_eq!(
                command_words(&line, true),
                Some(vec![
                    "fetchloom".to_owned(),
                    "-C".to_owned(),
                    word.to_owned(),
                    "status".to_owned()
                ]),
                "{line}"
            );
        }
    }

    /// interactive bash and zsh expand `!` inside double quotes from their history. powershell
    /// reads the same wherever it runs, so it gets windows' quoting. powershell's start takes most
    /// of its time, half a second to seconds where each other shell takes milliseconds.
    #[cfg(unix)]
    #[test]
    fn a_quoted_word_reads_back_whole_in_each_shell() {
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
            // a word withheld on windows is never in a command there
            let words: Vec<&str> = QUOTED
                .iter()
                .filter(|(_, _, quoted)| !windows || quoted.is_some())
                .map(|(word, _, _)| *word)
                .collect();
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

    /// on windows a word holding what cmd acts on even inside quotes, or what powershell's quoting
    /// cannot keep, is never in a command: the command names a placeholder no shell runs, and a
    /// line says which name it stands for, the same name the same placeholder.
    #[test]
    fn a_word_no_windows_shell_keeps_literal_is_named_apart() {
        for (word, _, windows) in QUOTED {
            assert_eq!(
                shell_word_for(word, true).contains(OPEN),
                windows.is_none(),
                "{word}"
            );
        }
        let file = "data/corpus/notes$ & mkdir PWNED-arch & x.csv";
        let folder = r"C:\Users\ada\p$ & mkdir PWNED-cmd7 & q";
        let help = format!(
            "run `fetchloom -C {} exclude {}`, then `fetchloom -C {} sync`; or `fetchloom get {}`",
            withhold(FOLDER, folder),
            shell_word_for(file, true),
            withhold(FOLDER, folder),
            shell_word_for("100%done", true),
        );
        let (shown, lines) = withheld(&help);
        assert_eq!(
            shown,
            r#"run `fetchloom -C "<folder>" exclude "<file>"`, then `fetchloom -C "<folder>" sync`; or `fetchloom get "<name>"`"#
        );
        assert_eq!(
            lines,
            [
                format!("<folder> is {folder}, which no command here can hold safely"),
                format!("<file> is {file}, which no command here can hold safely"),
                "<name> is 100%done, which no command here can hold safely".to_owned(),
            ]
        );
        assert_eq!(withheld("run `fetchloom sync`").0, "run `fetchloom sync`");
        let again = format!(
            "{shown}; then run `fetchloom path {}`",
            shell_word_for("data/a&b.csv", true)
        );
        assert_eq!(
            withheld(&again),
            (
                Cow::Owned(format!("{shown}; then run `fetchloom path \"<file 2>\"`")),
                vec!["<file 2> is data/a&b.csv, which no command here can hold safely".to_owned()]
            )
        );
        assert_eq!(
            unmarked(&format!("of {}", shell_word_for(file, true))),
            format!("of {file}")
        );
    }

    /// a name holding the marks fetchloom puts around a withheld word, as a crafted data.lock
    /// path or archive member can, is withheld on every system: held raw, its marks would be read
    /// as a withheld word's, and with others in their place it names another file. nothing of it
    /// follows the placeholder, and its line writes the marks out.
    #[test]
    fn a_name_holding_the_marks_is_withheld_whole() {
        let crafted = format!("data/a{CLOSE} & mkdir PWNED & {OPEN}fb.csv");
        let written = r"data/a\u{f8e1} & mkdir PWNED & \u{f8e0}fb.csv";
        for windows in [false, true] {
            let command = format!(
                "run `fetchloom exclude {}`",
                shell_word_for(&crafted, windows)
            );
            assert_eq!(
                withheld(&command),
                (
                    Cow::Owned(r#"run `fetchloom exclude "<file>"`"#.to_owned()),
                    vec![format!(
                        "<file> is {written}, which no command here can hold safely"
                    )]
                ),
                "{windows}"
            );
        }
        assert_eq!(
            withheld(&format!("in {}", shell_folder(&format!("a{OPEN}b")))),
            (
                Cow::Owned(r#"in "<folder>""#.to_owned()),
                vec![r"<folder> is a\u{f8e0}b, which no command here can hold safely".to_owned()]
            )
        );
    }

    /// marks a name holds where no command quotes it, as a warning's message does, are text: the
    /// line names the file as it is, with no placeholder, and an opening mark there never takes in
    /// the command quoted after it.
    #[test]
    fn marks_outside_a_command_stay_text() {
        let message = format!("put back data/z/c{OPEN}d{CLOSE} z.csv, which was deleted or moved");
        assert_eq!(
            withheld(&message),
            (Cow::Borrowed(message.as_str()), Vec::new())
        );
        assert_eq!(unmarked(&message), message);
        let help = format!(
            "bring data/a{OPEN}b back: run `fetchloom exclude {}`",
            shell_word_for("x&y.csv", true)
        );
        assert_eq!(
            withheld(&help),
            (
                Cow::Owned(format!(
                    "bring data/a{OPEN}b back: run `fetchloom exclude \"<name>\"`"
                )),
                vec!["<name> is x&y.csv, which no command here can hold safely".to_owned()]
            )
        );
        assert_eq!(
            marks_written_out(&message),
            r"put back data/z/c\u{f8e0}d\u{f8e1} z.csv, which was deleted or moved"
        );
    }

    /// two names in one command, one holding the opening mark and the other the closing one, as
    /// tracked data.lock.d parts can, are never joined across their quotes: each is named apart.
    #[test]
    fn marks_in_two_names_never_join_their_quotes() {
        let names = [
            format!("data.lock.d/a{OPEN}b.aaaaaaaaaaaa.tsv.zst"),
            format!("data.lock.d/c{CLOSE}$(mkdir PWNED)\"'.bbbbbbbbbbbb.tsv.zst"),
        ];
        let words: Vec<Cow<'_, str>> = names
            .iter()
            .map(|name| shell_word_for(name, false))
            .collect();
        let help = format!("run `git rm {} {}`, then commit", words[0], words[1]);
        assert_eq!(
            withheld(&help),
            (
                Cow::Owned(r#"run `git rm "<file>" "<file 2>"`, then commit"#.to_owned()),
                vec![
                    r"<file> is data.lock.d/a\u{f8e0}b.aaaaaaaaaaaa.tsv.zst, which no command here can hold safely".to_owned(),
                    r#"<file 2> is data.lock.d/c\u{f8e1}$(mkdir PWNED)"'.bbbbbbbbbbbb.tsv.zst, which no command here can hold safely"#.to_owned(),
                ]
            )
        );
    }

    /// a placeholder stands where a name was, as `exclude "<file>" after` shows it, and a shell
    /// reads it as a word, never as a redirection from a file of its name into the word after it.
    #[cfg(unix)]
    #[test]
    fn a_placeholder_redirects_nothing_in_a_unix_shell() {
        let folder = tempfile::tempdir().unwrap();
        let there = ["file", "folder", "name"];
        for name in there {
            std::fs::write(folder.path().join(name), "mine").unwrap();
        }
        let crafted = format!("data/a{OPEN}b.csv");
        let printed = format!("printf '%s\\n' {} after", shell_word(&crafted));
        let (line, _) = withheld(&printed);
        for shell in [
            &["bash", "--norc", "--noprofile", "-c"][..],
            &["sh", "-c"],
            &["zsh", "-f", "-c"],
            &["fish", "--no-config", "-c"],
        ] {
            let ran = std::process::Command::new(shell[0])
                .args(&shell[1..])
                .arg(line.as_ref())
                .current_dir(folder.path())
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            let Ok(ran) = ran else {
                crate::test_skips::skip(shell[0], &format!("{} cannot start", shell[0]));
                continue;
            };
            let told = String::from_utf8_lossy(&ran.stdout);
            assert_eq!(told, "<file>\nafter\n", "{}: {line}", shell[0]);
            let mut made: Vec<_> = std::fs::read_dir(folder.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            made.sort();
            assert_eq!(made, there, "{}: {line}", shell[0]);
        }
    }

    /// each word's command as fetchloom prints it on windows, pasted into powershell and into cmd,
    /// runs nothing it does not show: no folder appears from a name that tries to make one, a
    /// placeholder redirects nothing in cmd though files of its name are there, and powershell
    /// reads a word fetchloom quotes back whole.
    #[cfg(windows)]
    #[test]
    fn a_printed_command_runs_nothing_more_in_powershell_or_cmd() {
        let folder = tempfile::tempdir().unwrap();
        let there = ["file", "folder", "name"];
        for name in there {
            std::fs::write(folder.path().join(name), "mine").unwrap();
        }
        let tries = [
            "p$ & mkdir PWNED-cmd7 & q",
            "x | mkdir PWNED-pipe",
            "a^& mkdir PWNED-caret",
            "%COMSPEC% & mkdir PWNED-percent",
            "b > PWNED-redirect.txt",
        ];
        let words = QUOTED
            .iter()
            .map(|(word, _, quoted)| (*word, quoted.is_some()))
            .chain(tries.iter().map(|word| (*word, false)));
        for (word, quoted) in words {
            let printed = format!("echo {} after", shell_word_for(word, true));
            let line = withheld(&printed).0.into_owned();
            let shells: [(&str, &[&str]); 2] = [
                (
                    "powershell.exe",
                    &["-NoProfile", "-NonInteractive", "-Command"],
                ),
                ("cmd.exe", &["/d", "/c"]),
            ];
            for (shell, flags) in shells {
                let output = std::process::Command::new(shell)
                    .args(flags)
                    .arg(&line)
                    .current_dir(folder.path())
                    .stdin(std::process::Stdio::null())
                    .output()
                    .unwrap();
                if shell == "powershell.exe" && quoted {
                    let told = String::from_utf8_lossy(&output.stdout);
                    assert_eq!(told.lines().next(), Some(word), "{shell}: {line}");
                }
            }
            let mut made: Vec<_> = std::fs::read_dir(folder.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            made.sort();
            assert_eq!(made, there, "{word}: {line}");
            for name in there {
                let kept = std::fs::read_to_string(folder.path().join(name)).unwrap();
                assert_eq!(kept, "mine", "{word}: {line}");
            }
        }
    }

    /// powershell reads a curly quote as a straight one, so a command holding one outside single
    /// quotes is never read as words there.
    #[test]
    fn a_curly_quote_powershell_reads_as_a_quote_is_never_read_as_text() {
        for command in [
            "fetchloom get \"a\u{201c};calc;\u{201c}\"",
            "fetchloom get a\u{2019};calc;\u{2018}",
            "fetchloom get \u{201c}a b\u{201d}",
        ] {
            assert_eq!(command_words(command, true), None, "{command}");
        }
        assert_eq!(
            command_words("fetchloom get 'a\u{201c}b'", true).unwrap(),
            ["fetchloom", "get", "a\u{201c}b"],
        );
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
