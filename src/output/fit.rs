//! lines as wide as the terminal they go to: a story line wrapped between words under its own
//! text, and an example in help continued across lines as a shell reads it.

use crate::units::columns;

/// what separates an example's command from its comment in help.
const COMMENT: &str = "    # ";

/// what ends a line of a command a shell reads on from the next: a backslash in posix shells,
/// a backtick in powershell.
pub(super) const MARK: &str = if cfg!(windows) { " `" } else { " \\" };

/// `text` with each line wider than `width` wrapped between words, colors kept, going on under its
/// own text as [`hanging`] finds it. a command in backticks is never split, since a shell would run
/// a split command's first line alone: one too wide for the room under that text starts at the
/// first column, where it fits whole or the terminal wraps it. a name in double quotes too wide
/// for that room wraps at its own spaces, and a word too wide for it, as a link or a path, is cut
/// at the edge, each going on under the text, never leaving a label such as `help:` alone.
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
    for (at, column) in breaks {
        let (offset, _) = shown[at];
        fitted.push_str(&line[from..offset]);
        fitted.push('\n');
        fitted.push_str(&" ".repeat(column));
        // the spaces the break replaces, the run of them where there are several
        from = offset;
        while line[from..].starts_with(' ') {
            from += 1;
        }
    }
    fitted.push_str(&line[from..]);
    fitted
}

/// the fewest columns a line keeps for the start of a word too wide for any line, which otherwise
/// starts on the next line.
const ROOM: usize = 12;

/// the indexes in `shown`, the characters a line shows, of the spaces it breaks at to fit
/// `width`, or of the character a word too wide for a line is cut before, each with the column
/// the line after it starts at: `indent`, or the first column for a command too wide for the room
/// past it. a number keeps its unit.
fn breaks(shown: &[(usize, char)], indent: usize, width: usize) -> Vec<(usize, usize)> {
    let room = width - indent;
    let breakable = spanning(shown, indent, room);
    let after = widths_after(shown, &breakable);
    let ticked = ticked(shown);
    let mut breaks = Vec::new();
    let mut column = 0;
    // the last space a line may break at, and the column it stands at
    let mut last: Option<(usize, usize)> = None;
    // where the line being laid out starts in `shown`
    let mut line = 0;
    for (at, &(_, c)) in shown.iter().enumerate() {
        if breakable[at] {
            last = Some((at, column));
        }
        let wide = console::measure_text_width(c.encode_utf8(&mut [0; 4]));
        column += wide;
        if column <= width || c == ' ' {
            continue;
        }
        if ticked[at] {
            // a command moves whole to the next line, its punctuation with it, under the text, or
            // as far left as it needs to fit whole; one wider than a line runs on there, and the
            // terminal wraps it, since a shell runs the first line of a split command alone
            match last.take() {
                Some((space, before)) => {
                    let unit = after[space];
                    let start = if unit <= room || unit >= width {
                        indent
                    } else {
                        width - unit
                    };
                    breaks.push((space, start));
                    column = start + column - before - 1;
                    line = space + 1;
                }
                None => column -= width,
            }
            continue;
        }
        // a break at the space that ends a label would leave the label alone on its line
        if let Some((space, before)) = last.take()
            && before >= indent
            && (after[space] <= room || width.saturating_sub(before + 1) < ROOM)
        {
            breaks.push((space, indent));
            column = indent + column - before - 1;
            line = space + 1;
            if column <= width {
                continue;
            }
        }
        // a word too wide for a line, as a long path or link, is cut at the edge, after the last
        // `/` before it where it has one, so a name in it stays whole. the cut looks only in that
        // word, past any command in it, which is never cut
        let word = (line..at)
            .rev()
            .find(|&before| shown[before].1 == ' ' || ticked[before])
            .map_or(line, |before| before + 1);
        let cut = (word + 1..at)
            .rev()
            .find(|&before| shown[before - 1].1 == '/' && shown[before].1 != '/')
            .unwrap_or(at);
        breaks.push((cut, indent));
        column = indent
            + shown[cut..=at]
                .iter()
                .map(|&(_, c)| console::measure_text_width(c.encode_utf8(&mut [0; 4])))
                .sum::<usize>();
        line = cut;
    }
    breaks
}

/// the first words of the commands fetchloom suggests. a backtick before one and a space opens
/// a command whatever backticks came before it on the line, as one in a file's name may.
const PROGRAMS: &[&str] = &[
    "fetchloom",
    "fl",
    "git",
    "sudo",
    "chmod",
    "chown",
    "mkdir",
    "cd",
    "cp",
    "mv",
    "rm",
    "az",
    "gcloud",
    "aws",
    "hf",
    "kaggle",
    "winget",
    "unzip",
    "umask",
    "xz",
    "zstd",
    "tar",
    "sha256sum",
    "rsync",
    "echo",
    "ulimit",
    "export",
    "setx",
    "set",
    "id",
    "curl",
    "copy",
    "move",
    "unrar",
    "unar",
    "zip",
    "gzip",
    "Expand-Archive",
    "brew",
    "kill",
    "docker",
    "uname",
    "security",
    "md5",
    "add",
    "remove",
    "exclude",
    "drop",
    "sync",
    "lock",
    "status",
    "audit",
    "diff",
    "list",
    "path",
    "push",
    "cache",
    "settings",
    "get",
    "search",
    "auth",
    "completions",
    "help",
];

/// whether each of `shown` belongs to a command in backticks, the backticks with it: between two
/// backticks paired in order, or from a backtick a program's name and a space follow, as in
/// `` `fetchloom sync` ``, to the next backtick or the end of the line. a backtick that came with
/// a name, as in a file ``a`b.csv``, can only make more of a line a command, never end one early.
fn spans(shown: &[(usize, char)]) -> Vec<bool> {
    let mut inside = false;
    let mut spans: Vec<bool> = shown
        .iter()
        .map(|&(_, c)| {
            if c == '`' {
                inside = !inside;
                return true;
            }
            inside
        })
        .collect();
    for (at, &(_, c)) in shown.iter().enumerate() {
        if c == '`' && opens(&shown[at + 1..]) {
            let end = shown[at + 1..]
                .iter()
                .position(|&(_, c)| c == '`')
                .map_or(shown.len() - 1, |next| at + 1 + next);
            spans[at..=end].fill(true);
        }
    }
    spans
}

/// whether `rest`, what follows a backtick, starts with a program's name and a space, or with
/// `$env:`, as a powershell command does.
fn opens(rest: &[(usize, char)]) -> bool {
    let word: String = rest
        .iter()
        .map(|&(_, c)| c)
        .take_while(|&c| c != ' ' && c != '`')
        .collect();
    word.starts_with("$env:")
        || (PROGRAMS.contains(&word.as_str())
            && rest
                .get(word.chars().count())
                .is_some_and(|&(_, c)| c == ' '))
}

/// [`spans`], with the punctuation right after a command's closing backtick, as in
/// `` `fetchloom sync`, ``, which goes with it.
fn ticked(shown: &[(usize, char)]) -> Vec<bool> {
    let mut ticked = spans(shown);
    let mut closed = false;
    for at in 0..shown.len() {
        let c = shown[at].1;
        if c == '`' && ticked[at] {
            closed = !ticked.get(at + 1).copied().unwrap_or_default();
            continue;
        }
        closed &= c.is_ascii_punctuation();
        ticked[at] |= closed;
    }
    ticked
}

/// [`breakable`], with the spaces inside a name in double quotes too wide for `room`, the columns
/// a line has past its indent, which wraps at them.
fn spanning(shown: &[(usize, char)], indent: usize, room: usize) -> Vec<bool> {
    let mut breakable = breakable(shown, indent);
    let after = widths_after(shown, &breakable);
    let first = shown
        .iter()
        .take_while(|(at, _)| !breakable.get(*at).copied().unwrap_or_default())
        .map(|(_, c)| console::measure_text_width(c.encode_utf8(&mut [0; 4])))
        .sum::<usize>();
    let mut wide = first > room;
    let ticked = spans(shown);
    let mut quoted = false;
    for (at, &(_, c)) in shown.iter().enumerate() {
        match c {
            '"' if !ticked[at] => quoted = !quoted,
            ' ' if breakable[at] => wide = after[at] > room,
            ' ' if wide && quoted && !ticked[at] && at > 0 && shown[at - 1].1 != ' ' => {
                breakable[at] = true;
            }
            _ => {}
        }
    }
    breakable
}

/// whether each of `shown` is a space a line may break at: the one that ends the label or verb
/// before `indent`, or one past it, the first of a run of spaces, outside backticks and double
/// quotes, and not between a number and its unit.
fn breakable(shown: &[(usize, char)], indent: usize) -> Vec<bool> {
    let mut breakable = vec![false; shown.len()];
    let mut column = 0;
    let ticked = spans(shown);
    let mut quoted = false;
    // whether the word before this character is a number, such as the 2.0 of 2.0 MiB
    let mut number = false;
    for (at, &(_, c)) in shown.iter().enumerate() {
        match c {
            '"' if !ticked[at] => quoted = !quoted,
            // the space that ends a label or a verb too, so a command too wide to follow it
            // there goes on the next line rather than past the edge
            ' ' if !ticked[at]
                && !quoted
                && !number
                && column + 1 >= indent
                && at > 0
                && shown[at - 1].1 != ' ' =>
            {
                breakable[at] = true;
            }
            _ => {}
        }
        number = match c {
            '0'..='9' => at == 0 || shown[at - 1].1 == ' ' || number,
            '.' | ',' => number,
            _ => false,
        };
        column += console::measure_text_width(c.encode_utf8(&mut [0; 4]));
    }
    breakable
}

/// for each space a line may break at, the columns of what would start the line after it: the
/// characters up to the next such space, without the spaces the break takes.
fn widths_after(shown: &[(usize, char)], breakable: &[bool]) -> Vec<usize> {
    let mut after = vec![0; shown.len()];
    // read from the end, the spaces seen since the last character count once one comes before them
    let (mut word, mut spaces) = (0, 0);
    for (at, &(_, c)) in shown.iter().enumerate().rev() {
        if breakable[at] {
            after[at] = word;
            (word, spaces) = (0, 0);
        } else if c == ' ' {
            spaces += 1;
        } else {
            word += spaces + console::measure_text_width(c.encode_utf8(&mut [0; 4]));
            spaces = 0;
        }
    }
    after
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
/// past the verb column of a step, which is 11 wide, else at its own indent. a verb of two words,
/// as `Put back` or `Signed in`, starts with a capital, as no line of prose here does.
fn hanging(plain: &str) -> usize {
    let indent = plain.len() - plain.trim_start_matches(' ').len();
    let head: Vec<char> = plain.chars().take(12).collect();
    let verb: String = head.iter().take(11).collect();
    let word = verb.trim();
    let words: Vec<&str> = word.split(' ').collect();
    let two_words = words.len() == 2
        && word.starts_with(|c: char| c.is_ascii_uppercase())
        && verb.ends_with(|c: char| c != ' ')
        && words
            .iter()
            .all(|word| !word.is_empty() && word.chars().all(char::is_alphabetic));
    if head.len() == 12
        && head[11] == ' '
        && !word.is_empty()
        && (!word.contains([' ', ':']) || two_words)
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

/// a line of help's examples as lines `width` wide: its comment below the command when `below`
/// asks it of the whole block, and a command too long for a line broken between its words.
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

/// the column past the widest command of an examples block, `lines`, that has a comment, where
/// [`align`] puts every comment of the block, so they read as one column.
pub(super) fn comment_column<'a>(lines: impl IntoIterator<Item = &'a str>) -> usize {
    lines
        .into_iter()
        .filter_map(|line| line.split_once(COMMENT))
        .map(|(command, _)| columns(command))
        .max()
        .unwrap_or_default()
}

/// `line` of an examples block with its comment, if any, at `column`, as [`comment_column`] finds
/// it for the block.
pub(super) fn align(line: &str, column: usize) -> String {
    match line.split_once(COMMENT) {
        Some((command, comment)) => format!(
            "{command}{}{COMMENT}{comment}",
            " ".repeat(column.saturating_sub(columns(command)))
        ),
        None => line.to_owned(),
    }
}

/// whether an example of help's `examples`, the lines of one block, has a comment that does not
/// fit beside its command, which then puts every comment in the block below its command.
pub(super) fn comments_below<'a>(
    examples: impl IntoIterator<Item = &'a str>,
    width: usize,
) -> bool {
    examples
        .into_iter()
        .any(|line| line.contains(COMMENT) && columns(line) > width)
}

/// `command`, a help line two spaces in, broken between words where wider than `width`, each line
/// but the last ending in [`MARK`], the rest further in. an option keeps its value, a quote whole.
fn continued(command: &str, width: usize) -> Vec<String> {
    if columns(command) <= width {
        return vec![command.to_owned()];
    }
    let indent = command.len() - command.trim_start_matches(' ').len();
    let mut lines = Vec::new();
    let mut current = " ".repeat(indent);
    for unit in units(command.trim_start_matches(' ')) {
        let room = columns(&current) + 1 + columns(&unit) + MARK.len();
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
        if current.len() > COMMENT.len() && columns(&current) + 1 + columns(word) > width {
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

    /// a command in backticks too wide to follow its label on the first line goes on the next,
    /// whole, under the text or as far left as it needs to fit, never at the first column, and
    /// never split, since a shell would run a split command's first line alone. its punctuation
    /// stays with it, so no line starts with a comma.
    #[test]
    fn a_command_too_wide_beside_its_label_goes_on_the_next_line() {
        assert_eq!(
            fit(
                "  next: `fetchloom add kaggle:uciml/iris` puts the first match in a project",
                40
            ),
            "  next:\n       `fetchloom add kaggle:uciml/iris`\n        puts the first match in a\n        project"
        );
        assert_eq!(
            fit(
                "  help: `fetchloom sync --force data/iris/iris.csv` discards your edit",
                40
            ),
            "  help:\n        `fetchloom sync --force data/iris/iris.csv` discards your edit",
            "wider than the line, the command runs on, and the terminal wraps it"
        );
        let mirror = "/datadisk/fl-perf/qa-ux/p8-gate/homes/newcomer/mirror";
        assert_eq!(
            fit(
                &format!(
                    "  next: teammates run `fetchloom settings set mirrors {mirror}`, plus any mirrors they already use"
                ),
                80
            ),
            format!(
                "  next: teammates run\n        `fetchloom settings set mirrors {mirror}`, plus any mirrors they already use"
            )
        );
    }

    /// a step whose verb is two words goes on under its text, as one of one word does, while a
    /// line of prose that happens to have a space at the verb column's edge keeps its indent.
    #[test]
    fn a_two_word_verb_wraps_under_its_text() {
        assert_eq!(
            fit(
                "   Put back data/iris-species/Iris.csv, which was deleted or moved, since data.lock pins it",
                80
            ),
            "   Put back data/iris-species/Iris.csv, which was deleted or moved, since\n            data.lock pins it"
        );
        assert_eq!(
            fit(
                "  Signed in to 127.0.0.1:18871 with a token · saved to ~/.config/fetchloom/credentials.json",
                60
            ),
            "  Signed in to 127.0.0.1:18871 with a token · saved to\n            ~/.config/fetchloom/credentials.json"
        );
        assert_eq!(
            fit("hello world and some more words to wrap here", 30),
            "hello world and some more\nwords to wrap here"
        );
    }

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
            format!(
                "  help: open {}\n        {}\n        {} in\n        a browser",
                &link[..20],
                &link[20..52],
                &link[52..]
            )
        );
        assert_eq!(fit("short\n  also short", 60), "short\n  also short");
    }

    /// a path or a link too wide for the room past the indent once ran on at the first column, as
    /// the terminal wrapped it: it is cut at the edge where it stands and goes on under the text,
    /// and one that fits a line under the text moves there whole. a command too wide for that
    /// room still starts at the first column, whole, where it fits or the terminal wraps it.
    #[test]
    fn a_word_too_wide_for_a_line_goes_on_under_its_text() {
        let path = "bart/sub-01/func/sub-01_task-balloonanalogrisktask_run-01_bold.nii.gz";
        assert_eq!(
            fit(&format!("      Would fetch {path} · 45.1 MiB"), 80),
            format!(
                "      Would fetch {}\n            {} · 45.1 MiB",
                &path[..17],
                &path[17..]
            )
        );
        let command =
            "`fetchloom settings set data_links /datadisk/lab/p5-adversary/work/victim/dir`";
        assert_eq!(
            fit(
                &format!(
                    "  help: if you made it to keep data on another disk, list that folder with {command} first"
                ),
                80
            ),
            format!(
                "  help: if you made it to keep data on another disk, list that folder with\n  {command}\n        first"
            )
        );
        let link = "http://127.0.0.1:18871/slow/6000000/3/clean/train-00001-of-00004-5852b56a2bd28fd9.parquet";
        assert_eq!(
            fit(&format!("  shard · {link} · 1 file · 5.7 MiB"), 80),
            format!(
                "  shard · {}\n  {} · 1 file · 5.7 MiB",
                &link[..44],
                &link[44..]
            )
        );
        let fits = "bart/sub-01/func/sub-01_task-balloonanalogrisktask_run-01_events.tsv";
        assert_eq!(
            fit(&format!("      Would fetch {fits} · 8.4 KiB"), 80),
            format!("      Would fetch\n            {fits}\n            · 8.4 KiB"),
            "a path that fits past the indent stays there"
        );
    }

    /// a cut for a word too wide for a line looks only in that word, never back into a command
    /// before it, and punctuation right after a command goes with it: each command here stays
    /// on one line at 80 columns, where the terminal wraps one wider than a line.
    #[test]
    fn a_cut_never_lands_inside_a_command() {
        let iris = format!("`fetchloom sync --force data/iris/{}`", "n".repeat(37));
        let path = format!("/home/ada/{}/fetchloom", "c".repeat(118 - 20));
        let chown = format!("`sudo chown -R $(id -u):$(id -g) {path}`");
        let checkout = format!(
            "`git -C /home/ada/projects/study-2026a checkout HEAD -- data.lock.d/{}.toml`",
            "d".repeat(63)
        );
        for (line, command) in [
            (
                format!("  help: {iris}, then run `fetchloom status`"),
                &iris,
            ),
            (
                format!(
                    "  help: root made it, as a container run as root does: take the cache back with {chown}, then run the command again"
                ),
                &chown,
            ),
            (
                format!(
                    "  help: bring it back from our side's commit with {checkout}, then run the same command again"
                ),
                &checkout,
            ),
        ] {
            let fitted = fit(&line, 80);
            assert!(
                fitted
                    .lines()
                    .any(|shown| shown.contains(&format!("{command},"))),
                "{fitted}"
            );
        }
        // a word with no `/` that crosses the edge after a command is cut at the edge
        let fitted = fit(
            &format!(
                "  help: run `sudo chown -R me /srv/cache` then{}",
                "x".repeat(71)
            ),
            80,
        );
        assert!(fitted.contains("`sudo chown -R me /srv/cache`"), "{fitted}");
        // a space ending exactly at the edge, before a word wider than the room past the label
        let edge = format!("  help: {} {}", "a".repeat(12), "b".repeat(30));
        assert!(fit(&edge, 20).contains(&"b".repeat(12)));
    }
}
