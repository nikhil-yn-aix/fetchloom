//! the fetchloom commands help, hints and `next:` lines name. after a run given `-C`, each names
//! the folder the run worked in, so it finds the same project from whatever folder it runs in.
//! [`shell_word`] quotes a word from data.toml, a link list or a server, so a pasted command runs
//! only what it shows.

use std::borrow::Cow;
use std::sync::OnceLock;

/// the folder a run given `-C` works in, quoted for a shell. unset without `-C`, as in tests.
static DIRECTORY: OnceLock<String> = OnceLock::new();

/// has every later [`command`] name `directory`, already quoted for a shell, after `-C`.
pub(crate) fn set_directory(directory: String) {
    let _ = DIRECTORY.set(directory);
}

/// `fetchloom` and `words`, with `-C` and the run's folder between them when the run had `-C`.
pub(crate) fn command(words: &str) -> String {
    match DIRECTORY.get() {
        Some(directory) => format!("fetchloom -C {directory} {words}"),
        None => format!("fetchloom {words}"),
    }
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
pub(crate) fn shell_word(word: &str) -> Cow<'_, str> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
