//! lines as wide as the terminal they go to: a story line wrapped between words under its own
//! text, and an example in help continued across lines as a shell reads it.

/// what separates an example's command from its comment in help.
const COMMENT: &str = "    # ";

/// what ends a line of a command a shell reads on from the next: a backslash in posix shells,
/// a backtick in powershell.
pub(super) const MARK: &str = if cfg!(windows) { " `" } else { " \\" };

/// `text` with each of its lines wider than `width` columns wrapped between words, its colors
/// kept. a line goes on under its own text: past a label such as `error:` or `help:`, past the
/// verb column of a step, or at its own indent. a command in backticks or a name in double quotes
/// is never split, and a word wider than the rest of a line goes whole onto a line of its own,
/// where only the terminal can wrap it.
pub(super) fn fit(text: &str, width: usize) -> String {
    text.split('\n')
        .map(|line| fit_line(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fit_line(line: &str, width: usize) -> String {
    if console::measure_text_width(line) <= width {
        return line.to_owned();
    }
    let shown = visible(line);
    let plain: String = shown.iter().map(|(_, c)| *c).collect();
    let indent = hanging(&plain);
    if indent * 2 >= width {
        return line.to_owned();
    }
    let breaks = breaks(&shown, indent, width);
    if breaks.is_empty() {
        return line.to_owned();
    }
    let mut fitted = String::with_capacity(line.len() + breaks.len() * (indent + 1));
    let mut from = 0;
    for at in breaks {
        let (offset, _) = shown[at];
        fitted.push_str(&line[from..offset]);
        fitted.push('\n');
        fitted.push_str(&" ".repeat(indent));
        // the spaces the break replaces, the run of them where there are several
        from = offset;
        while line[from..].starts_with(' ') {
            from += 1;
        }
    }
    fitted.push_str(&line[from..]);
    fitted
}

/// the indexes in `shown`, the characters a line shows, of the spaces it breaks at to fit
/// `width`, each line after the first going on at `indent`. a number keeps its unit.
fn breaks(shown: &[(usize, char)], indent: usize, width: usize) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut column = 0;
    // the last space a line may break at, and the column it stands at
    let mut last: Option<(usize, usize)> = None;
    let (mut ticked, mut quoted) = (false, false);
    // whether the word before this character is a number, such as the 2.0 of 2.0 MiB
    let mut number = false;
    for (at, &(_, c)) in shown.iter().enumerate() {
        match c {
            '`' => ticked = !ticked,
            '"' if !ticked => quoted = !quoted,
            ' ' if !ticked
                && !quoted
                && !number
                && column > indent
                && at > 0
                && shown[at - 1].1 != ' ' =>
            {
                last = Some((at, column));
            }
            _ => {}
        }
        number = match c {
            '0'..='9' => at == 0 || shown[at - 1].1 == ' ' || number,
            '.' | ',' => number,
            _ => false,
        };
        column += console::measure_text_width(c.encode_utf8(&mut [0; 4]));
        if column > width
            && c != ' '
            && let Some((space, before)) = last.take()
        {
            breaks.push(space);
            column = indent + column - before - 1;
        }
    }
    breaks
}

/// each character `line` shows, with where it starts in `line`, leaving out the escapes that
/// color it or make a link.
fn visible(line: &str) -> Vec<(usize, char)> {
    let mut shown = Vec::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '\u{1b}' {
            shown.push((at, c));
            continue;
        }
        match chars.next() {
            // a color or a cursor move, ended by a letter or another final byte
            Some((_, '[')) => {
                for (_, c) in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // a link, ended by a bell or by an escape and a backslash
            Some((_, ']')) => {
                while let Some((_, c)) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek().is_some_and(|(_, c)| *c == '\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    shown
}

/// the column a line goes on at once wrapped: past its label, as `error: ` or `  help: `, or
/// past the verb column of a step, which is 11 wide, else at its own indent.
fn hanging(plain: &str) -> usize {
    let indent = plain.len() - plain.trim_start_matches(' ').len();
    let head: Vec<char> = plain.chars().take(12).collect();
    let verb: String = head.iter().take(11).collect();
    if head.len() == 12
        && head[11] == ' '
        && !verb.trim().is_empty()
        && !verb.trim().contains([' ', ':'])
    {
        return 12;
    }
    let rest = &plain[indent..];
    match rest.split_once(' ') {
        Some((label, _))
            if label.len() > 1
                && label.ends_with(':')
                && label[..label.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_alphabetic()) =>
        {
            indent + label.len() + 1
        }
        _ => indent,
    }
}

/// a line of help's examples as lines `width` wide: its comment on lines of its own below the
/// command, when `below` asks it of the whole block, and a command too long for a line broken
/// between its words.
pub(super) fn example(line: &str, width: usize, below: bool) -> Vec<String> {
    let (command, comment) = match line.split_once(COMMENT) {
        Some((command, comment)) if below => (command, Some(comment)),
        _ => (line, None),
    };
    let mut lines = continued(command, width);
    if let Some(comment) = comment {
        lines.extend(commented(comment, width));
    }
    lines
}

/// whether an example of help's `examples`, the lines of one block, has a comment that does not
/// fit beside its command, which then puts every comment in the block below its command.
pub(super) fn comments_below<'a>(
    examples: impl IntoIterator<Item = &'a str>,
    width: usize,
) -> bool {
    examples
        .into_iter()
        .any(|line| line.contains(COMMENT) && line.chars().count() > width)
}

/// `command`, a line of help two spaces in, broken between its words where it is wider than
/// `width`, each line but the last ending in the [`MARK`] that continues it in a shell, the rest
/// two spaces further in. an option keeps its value, and a word in quotes stays whole.
fn continued(command: &str, width: usize) -> Vec<String> {
    if command.chars().count() <= width {
        return vec![command.to_owned()];
    }
    let indent = command.len() - command.trim_start_matches(' ').len();
    let mut lines = Vec::new();
    let mut current = " ".repeat(indent);
    for unit in units(command.trim_start_matches(' ')) {
        let room = current.chars().count() + 1 + unit.chars().count() + MARK.len();
        if current.trim().is_empty() {
            current.push_str(&unit);
        } else if room > width {
            lines.push(format!("{current}{MARK}"));
            current = format!("{}{unit}", " ".repeat(indent + 2));
        } else {
            current.push(' ');
            current.push_str(&unit);
        }
    }
    lines.push(current);
    lines
}

/// the words of `command` a line breaks between: each word, a quoted one whole, an option with
/// the value after it, and a redirect or a pipe with what follows it.
fn units(command: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(open), c) if c == open => {
                quote = None;
                word.push(c);
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                word.push(c);
            }
            (None, ' ') => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut units: Vec<String> = Vec::new();
    for word in words {
        match units.last_mut() {
            Some(last)
                if matches!(last.as_str(), ">" | ">>" | "<" | "|")
                    || last.starts_with('-')
                        && !last.contains([' ', '='])
                        && !word.starts_with('-') =>
            {
                last.push(' ');
                last.push_str(&word);
            }
            _ => units.push(word),
        }
    }
    units
}

/// an example's comment on lines of its own, as wide as `width`, each one a comment.
fn commented(comment: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = COMMENT.to_owned();
    for word in comment.split(' ').filter(|word| !word.is_empty()) {
        if current.len() > COMMENT.len()
            && current.chars().count() + 1 + word.chars().count() > width
        {
            lines.push(std::mem::replace(&mut current, COMMENT.to_owned()));
        }
        if current.len() > COMMENT.len() {
            current.push(' ');
        }
        current.push_str(word);
    }
    lines.push(current);
    lines
}

#[cfg(test)]
mod tests {
    use super::fit;

    /// a line goes on past its label or verb column, a command in backticks stays whole, colors
    /// count for nothing, and a word wider than a line goes whole onto a line of its own.
    #[test]
    fn a_story_line_wraps_between_words_under_its_own_text() {
        assert_eq!(
            fit(
                "error: downloading dumb-prompt.bin needs 1.0 KiB, and fetchloom cannot ask you here",
                60
            ),
            "error: downloading dumb-prompt.bin needs 1.0 KiB, and\n       fetchloom cannot ask you here"
        );
        assert_eq!(
            fit(
                " Downloaded R04-progress-dumb-long-download-name.bin · 2.0 MiB in 4.0 s",
                60
            ),
            " Downloaded R04-progress-dumb-long-download-name.bin ·\n            2.0 MiB in 4.0 s"
        );
        assert_eq!(
            fit(
                "  next: to pin this dataset in a project, run `fetchloom add zenodo:3608990` there",
                60
            ),
            "  next: to pin this dataset in a project, run\n        `fetchloom add zenodo:3608990` there"
        );
        let red = "\u{1b}[1m\u{1b}[31merror:\u{1b}[0m";
        assert_eq!(
            fit(
                &format!(
                    "{red} data.toml has a key fetchloom does not know: \"selectt\" in datasets.iris"
                ),
                60
            ),
            format!(
                "{red} data.toml has a key fetchloom does not know:\n       \"selectt\" in datasets.iris"
            )
        );
        let link = format!("https://example.org/{}", "a".repeat(60));
        assert_eq!(
            fit(&format!("  help: open {link} in a browser"), 40),
            format!("  help: open\n        {link}\n        in a browser")
        );
        assert_eq!(fit("short\n  also short", 60), "short\n  also short");
    }
}
