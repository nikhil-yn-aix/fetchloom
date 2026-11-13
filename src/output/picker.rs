//! drawing and key handling for a dataset choice.

use anstyle::Style as Paint;
use console::{Key, Term};

use super::{Style, accent, paint, plain};
use crate::error::{Error, Kind};

pub(super) fn choose(
    question: &str,
    options: &[String],
    footer: &str,
    style: Style,
) -> Result<Option<usize>, Error> {
    if options.is_empty() {
        return Err(cancelled());
    }
    let terminal = Term::stderr();
    let escapes = escapes();
    let _ = terminal.hide_cursor();
    let mut cursor = 0;
    let mut shown = 0;
    let draw = |cursor: usize, shown: &mut usize| {
        let width = usize::from(terminal.size().1);
        let lines = frame(question, options, footer, cursor, style, width);
        show(&terminal, &lines, shown, width, escapes);
    };
    draw(cursor, &mut shown);
    let answer = loop {
        let previous = cursor;
        match selection_key(terminal.read_key_raw(), &mut cursor, options.len()) {
            Ok(None) => {
                if previous != cursor {
                    draw(cursor, &mut shown);
                }
            }
            answer => break answer,
        }
    };
    wipe(&terminal, shown, escapes);
    let _ = terminal.show_cursor();
    answer
}

/// a choice for a person who reads a line at a time, as with a screen reader: the options listed
/// once with their numbers, then the number typed. nothing is drawn over again.
pub(super) fn numbered(
    question: &str,
    options: &[String],
    footer: &str,
    style: Style,
) -> Result<Option<usize>, Error> {
    if options.is_empty() {
        return Err(cancelled());
    }
    let terminal = Term::stderr();
    let width = terminal
        .size_checked()
        .map_or(usize::MAX, |(_, columns)| usize::from(columns));
    let bold = paint(style.color, Paint::new().bold());
    let dim = paint(style.color, Paint::new().dimmed());
    let _ = terminal.write_line(&format!("  {bold}{}{bold:#}", plain(question)));
    for line in listed(options, usize::MAX, style, width) {
        let _ = terminal.write_line(&line);
    }
    if !footer.is_empty() {
        let _ = terminal.write_line(&format!("    {dim}{}{dim:#}", plain(footer)));
    }
    let count = options.len();
    loop {
        let _ = terminal.write_str(&format!(
            "  type a number from 1 to {count} and press enter, or q to cancel: "
        ));
        let typed = super::typed_line(
            || terminal.read_key_raw(),
            |edit| super::echo(&terminal, edit, style.interactive),
        );
        // keys read one by one bring no line break of their own
        let _ = terminal.write_line("");
        let line = match typed {
            Ok(line) => line,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err(Error::new(Kind::Interrupted, "stopped").cancelled("the choice"));
            }
            Err(_) => return Err(cancelled()),
        };
        match picked(&line, count) {
            Picked::One(index) => return Ok(Some(index)),
            Picked::Cancel => return Err(cancelled().cancelled("the choice")),
            Picked::Again => {}
        }
    }
}

/// what a line typed at a numbered choice picks.
#[derive(Debug, PartialEq, Eq)]
enum Picked {
    /// the option at this index.
    One(usize),
    /// none, as q or n says.
    Cancel,
    /// nothing it can read, such as a number out of range, so the choice is asked again.
    Again,
}

/// `line`, typed at a numbered choice of `count` options: a number from 1 to `count`, or q or n
/// in either case to cancel, as at the picker.
fn picked(line: &str, count: usize) -> Picked {
    let line = line.trim();
    if line.eq_ignore_ascii_case("q") || line.eq_ignore_ascii_case("n") {
        return Picked::Cancel;
    }
    match line.parse::<usize>() {
        Ok(number) if (1..=count).contains(&number) => Picked::One(number - 1),
        _ => Picked::Again,
    }
}

/// whether stderr reads cursor escape codes. a windows console reads them only once asked to;
/// when that fails, console's own cursor calls still work.
fn escapes() -> bool {
    #[cfg(windows)]
    {
        anstyle_query::windows::enable_ansi_colors() == Some(true)
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// draws `lines` over the `shown` rows drawn before, in one write: clearing and drawing in
/// separate writes is what makes a picker flicker.
fn show(terminal: &Term, lines: &[String], shown: &mut usize, width: usize, escapes: bool) {
    if escapes {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        let _ = terminal.write_str(&(back(*shown) + &text));
        let _ = terminal.flush();
    } else {
        wipe(terminal, *shown, false);
        for line in lines {
            let _ = terminal.write_line(line);
        }
    }
    *shown = lines.iter().map(|line| rows(line, width)).sum();
}

/// moves up to the first of `shown` rows and clears from there to the end of the screen.
fn wipe(terminal: &Term, shown: usize, escapes: bool) {
    if escapes {
        let _ = terminal.write_str(&back(shown));
        let _ = terminal.flush();
    } else {
        let _ = terminal.clear_last_lines(shown);
    }
}

fn back(shown: usize) -> String {
    if shown == 0 {
        String::new()
    } else {
        format!("\x1b[{shown}A\r\x1b[0J")
    }
}

/// the screen rows a line takes, since a line wider than the terminal wraps, and an option's
/// facts can take a line of their own.
fn rows(line: &str, width: usize) -> usize {
    line.split('\n')
        .map(|part| {
            console::measure_text_width(part)
                .div_ceil(width.max(1))
                .max(1)
        })
        .sum()
}
/// the picker's lines: the question with its keys, a row for each option, then the footer.
fn frame(
    question: &str,
    options: &[String],
    footer: &str,
    cursor: usize,
    style: Style,
    width: usize,
) -> Vec<String> {
    let bold = paint(style.color, Paint::new().bold());
    let dim = paint(style.color, Paint::new().dimmed());
    let question = plain(question);
    let room = width.saturating_sub(question.chars().count() + 4);
    // the longest legend that fits, so cancel stays visible on a narrow terminal
    let legends: [&str; 3] = if style.unicode {
        [
            "\u{2191}\u{2193} move \u{b7} 1-9 or enter choose \u{b7} q cancel",
            "\u{2191}\u{2193} enter \u{b7} q cancel",
            "q cancels",
        ]
    } else {
        [
            "up/down move, 1-9 or enter choose, q cancel",
            "up/down, enter, q cancel",
            "q cancels",
        ]
    };
    let keys = legends
        .iter()
        .find(|legend| legend.chars().count() <= room)
        .copied()
        .unwrap_or("");
    let mut lines = vec![format!("  {bold}{question}{bold:#}  {dim}{keys}{dim:#}")];
    lines.extend(listed(options, cursor, style, width));
    if !footer.is_empty() {
        let more = if style.unicode { "\u{2026}" } else { "..." };
        let footer = console::truncate_str(footer, width.saturating_sub(5), more);
        lines.push(format!("    {dim}{}{dim:#}", plain(&footer)));
    }
    lines
}

/// a row for each option, the one at `cursor` marked.
fn listed(options: &[String], cursor: usize, style: Style, width: usize) -> Vec<String> {
    let column = column(options);
    options
        .iter()
        .enumerate()
        .map(|(index, option)| row(option, (index, options.len()), cursor, style, width, column))
        .collect()
}

/// the columns before a reference in a picker of `count` options: the cursor's mark, then the
/// option's number, padded to the widest so every row lines up.
pub(crate) fn lead(count: usize) -> usize {
    5 + count.to_string().len()
}

/// where the titles start: after the widest reference, so a row with a blank title keeps its
/// columns.
fn column(options: &[String]) -> usize {
    options
        .iter()
        .map(|option| reference(option).chars().count() + 2)
        .max()
        .unwrap_or_default()
        .min(super::REFERENCE_COLUMN + 2)
}

fn reference(option: &str) -> &str {
    option.split(' ').next().unwrap_or_default()
}

fn selection_key(
    key: std::io::Result<Key>,
    cursor: &mut usize,
    count: usize,
) -> Result<Option<usize>, Error> {
    let key = match key {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Key::CtrlC,
        Err(_) => return Err(cancelled()),
    };
    match key {
        Key::CtrlC => return Err(Error::new(Kind::Interrupted, "stopped").cancelled("the choice")),
        Key::Escape | Key::Char('q' | 'n') => return Err(cancelled().cancelled("the choice")),
        Key::ArrowUp | Key::Char('k') => *cursor = cursor.saturating_sub(1),
        Key::ArrowDown | Key::Char('j') => *cursor = (*cursor + 1).min(count.saturating_sub(1)),
        Key::Enter if *cursor < count => return Ok(Some(*cursor)),
        Key::Char(digit @ '1'..='9') => {
            let index = digit as usize - '1' as usize;
            return Ok((index < count).then_some(index));
        }
        _ => {}
    }
    Ok(None)
}

fn cancelled() -> Error {
    Error::new(Kind::Usage, "nothing was chosen")
        .help("run it again to choose, or give a link or an id instead")
}

/// one option: its number of `count`, its reference, then from `column` its title and facts, dim
/// unless the cursor is on it. the reference is never cut; the rest gives way on a narrow
/// terminal, or goes on the line under it when it fits only there.
fn row(
    option: &str,
    (index, count): (usize, usize),
    cursor: usize,
    style: Style,
    width: usize,
    column: usize,
) -> String {
    let text = plain(option);
    let reference = reference(&text);
    let long = reference.chars().count() + 2 > column;
    let rest: String = text
        .chars()
        .skip(column.max(reference.chars().count()))
        .collect();
    let rest = if long { rest.trim() } else { rest.trim_end() };
    // the padding that lines the titles up, kept apart from the bold reference
    let pad = " ".repeat(column.saturating_sub(reference.chars().count() + 2));
    let lead = lead(count);
    let room = width.saturating_sub(column.max(reference.chars().count() + 2) + lead + 1);
    let more = if style.unicode { "\u{2026}" } else { "..." };
    let wide = console::measure_text_width(rest);
    let below = !rest.is_empty() && wide > room && wide <= width.saturating_sub(lead + 1);
    let beside = if below || room < 8 {
        String::new()
    } else {
        console::truncate_str(rest, room, more).into_owned()
    };
    let beside = super::shown(&beside, style.unicode);
    let number = format!("{:>1$}", index + 1, count.to_string().len());
    let bold = paint(style.color, Paint::new().bold());
    let dim = paint(style.color, Paint::new().dimmed());
    let line = if index == cursor {
        let marker = if style.unicode { "\u{203a}" } else { ">" };
        let cyan = paint(style.color, accent().bold());
        format!("  {cyan}{marker} {number}{cyan:#} {bold}{reference}{bold:#}{pad}  {beside}")
            .trim_end()
            .to_owned()
    } else {
        format!("    {dim}{number}{dim:#} {reference}{pad}  {dim}{beside}{dim:#}")
            .trim_end()
            .to_owned()
    };
    if below {
        let rest = super::shown(rest, style.unicode);
        format!("{line}\n{:lead$}{dim}{rest}{dim:#}", "")
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Kind;
    use crate::output::{Level, Output};
    use console::Key;

    fn style(color: bool, unicode: bool) -> Style {
        Style {
            color,
            unicode,
            interactive: true,
            watched: true,
            links: false,
            accessible: false,
        }
    }

    #[test]
    fn arrows_and_jk_move_the_cursor_and_enter_selects_it() {
        let mut cursor = 0;
        for (key, expected) in [
            (Key::ArrowUp, 0),
            (Key::ArrowDown, 1),
            (Key::Char('j'), 2),
            (Key::ArrowDown, 2),
            (Key::Char('k'), 1),
        ] {
            assert_eq!(selection_key(Ok(key), &mut cursor, 3).unwrap(), None);
            assert_eq!(cursor, expected);
        }
        assert_eq!(
            selection_key(Ok(Key::Enter), &mut cursor, 3).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn a_typed_number_picks_its_option_and_anything_else_asks_again() {
        for (line, picked_one) in [
            ("2", Picked::One(1)),
            (" 12 ", Picked::One(11)),
            ("13", Picked::Again),
            ("0", Picked::Again),
            ("", Picked::Again),
            ("two", Picked::Again),
            ("Q", Picked::Cancel),
            ("n", Picked::Cancel),
        ] {
            assert_eq!(picked(line, 12), picked_one, "{line:?}");
        }
    }

    #[test]
    fn digits_select_only_existing_options() {
        let mut cursor = 0;
        for (key, expected) in [
            ('1', Some(0)),
            ('3', Some(2)),
            ('9', None),
            ('0', None),
            ('x', None),
        ] {
            assert_eq!(
                selection_key(Ok(Key::Char(key)), &mut cursor, 3).unwrap(),
                expected
            );
        }
        assert_eq!(
            selection_key(Ok(Key::Char('9')), &mut cursor, 9).unwrap(),
            Some(8)
        );
    }

    /// q, esc and n are a cancel the person chose; a terminal that closed is not.
    #[test]
    fn cancellation_and_eof_stop_without_choosing() {
        for (key, chosen) in [
            (Ok(Key::Escape), true),
            (Ok(Key::Char('q')), true),
            (Ok(Key::Char('n')), true),
            (Err(std::io::ErrorKind::UnexpectedEof.into()), false),
        ] {
            let error = selection_key(key, &mut 0, 3).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(error.message, "nothing was chosen");
            assert!(error.help.unwrap().contains("give a link or an id instead"));
            assert_eq!(error.cancelled.is_some(), chosen);
        }
    }

    #[test]
    fn ctrl_c_and_interrupted_reads_stop_as_an_interruption() {
        for key in [Ok(Key::CtrlC), Err(std::io::ErrorKind::Interrupted.into())] {
            let error = selection_key(key, &mut 0, 3).unwrap_err();
            assert_eq!(error.kind, Kind::Interrupted);
            assert_eq!(error.cancelled.as_deref(), Some("the choice"));
        }
    }

    #[test]
    fn a_line_wider_than_the_terminal_counts_every_row_it_wraps_onto() {
        assert_eq!(rows("", 80), 1);
        assert_eq!(rows(&"x".repeat(80), 80), 1);
        assert_eq!(rows(&"x".repeat(81), 80), 2);
        assert_eq!(rows("\x1b[1mbold\x1b[0m", 4), 1);
        assert_eq!(rows("abc", 0), 3);
        assert_eq!(back(0), "");
        assert_eq!(back(3), "\x1b[3A\r\x1b[0J");
    }

    #[test]
    fn a_frame_is_the_question_then_one_line_per_option_then_the_footer() {
        let options = ["a   one".to_owned(), "bb  two".to_owned()];
        let lines = frame("which?", &options, "more: x", 1, style(false, false), 80);
        assert_eq!(
            lines,
            [
                "  which?  up/down move, 1-9 or enter choose, q cancel",
                "    1 a   one",
                "  > 2 bb  two",
                "    more: x",
            ]
        );
        assert_eq!(
            frame("which?", &options, "", 0, style(false, false), 80).len(),
            3
        );
        assert!(lines.iter().all(|line| !line.contains('\n')));
    }

    #[test]
    fn rows_highlight_the_cursor_and_escape_untrusted_titles() {
        let selected = row("iris", (0, 2), 0, style(false, false), 80, 6);
        assert_eq!(selected, "  > 1 iris");
        assert_eq!(
            row("other", (1, 2), 0, style(false, true), 80, 7),
            "    2 other"
        );
        let colored = row("iris", (0, 2), 0, style(true, true), 80, 6);
        assert!(colored.contains('\u{203a}'));
        assert!(colored.contains("\x1b["));
        assert!(colored.contains("iris"));
        let unsafe_text = row("iris\x1b[2J\nnext", (0, 2), 0, style(false, false), 80, 6);
        assert!(!unsafe_text.contains(char::is_control));
        assert!(unsafe_text.contains("\\u{1b}"));
        let narrow = row(
            "zenodo:1  a long title for a narrow terminal",
            (0, 2),
            0,
            style(false, false),
            30,
            10,
        );
        assert!(console::measure_text_width(&narrow) <= 30, "{narrow}");
        assert!(narrow.contains("zenodo:1"), "the reference is never cut");
        let tight = row(
            "kaggle:owner/a-very-long-dataset-name  title",
            (1, 2),
            0,
            style(false, false),
            30,
            39,
        );
        assert!(
            tight.contains("kaggle:owner/a-very-long-dataset-name"),
            "{tight}"
        );
    }

    /// one long name runs long on its own row, with its title and facts whole on the line under
    /// it, and every other row keeps its title and facts.
    #[test]
    fn a_long_reference_does_not_push_the_other_rows_off_the_terminal() {
        let long = format!("hf:datasets/{}", "x".repeat(80));
        let options = vec![
            format!(
                "{:48}  Palmer Penguins  18.5 KiB  12k downloads",
                "hf:datasets/SIH/palmer-penguins"
            ),
            format!("{long}  Huberman  90.0 KiB  2 downloads"),
        ];
        let lines = frame("which one?", &options, "", 0, style(false, true), 120);
        assert!(
            lines[1].ends_with("Palmer Penguins  18.5 KiB  12k downloads"),
            "{}",
            lines[1]
        );
        assert_eq!(
            lines[2],
            format!("    2 {long}\n      Huberman  90.0 KiB  2 downloads")
        );
    }

    /// rows 10 to 12 pad no wider than rows 1 to 9: every number ends in one column, so every
    /// reference and title starts in one, and a cut title still leaves the last column spare.
    #[test]
    fn numbers_pad_to_the_widest_so_every_row_lines_up() {
        let options: Vec<String> = (1..=12)
            .map(|n| format!("{:9}  Iris {n}", format!("zenodo:{n}")))
            .collect();
        for cursor in [0, 11] {
            let lines = frame("which one?", &options, "", cursor, style(false, false), 80);
            let rows = &lines[1..];
            let starts = |text: &str| -> Vec<Option<usize>> {
                rows.iter().map(|line| line.find(text)).collect()
            };
            assert_eq!(starts("zenodo:"), [Some(lead(12)); 12], "{rows:#?}");
            assert_eq!(starts("Iris"), [Some(18); 12], "{rows:#?}");
            assert_eq!(rows[1], "     2 zenodo:2   Iris 2");
            assert_eq!(rows[9], "    10 zenodo:10  Iris 10");
        }
        let long: Vec<String> = (1..=12)
            .map(|n| {
                format!(
                    "{:9}  a title long enough to be cut at forty columns",
                    format!("zenodo:{n}")
                )
            })
            .collect();
        for line in frame("which one?", &long, "", 0, style(false, false), 40) {
            assert!(console::measure_text_width(&line) < 40, "{line:?}");
        }
    }

    #[test]
    fn a_blank_title_keeps_the_facts_in_their_column() {
        let titled = row(
            "zenodo:1   Iris  3.6 KiB",
            (0, 2),
            1,
            style(false, false),
            80,
            11,
        );
        let blank = row(
            "zenodo:22        5.0 KiB",
            (1, 2),
            0,
            style(false, false),
            80,
            11,
        );
        assert_eq!(titled, "    1 zenodo:1   Iris  3.6 KiB");
        assert_eq!(blank, "    2 zenodo:22        5.0 KiB");
    }

    #[test]
    fn unattended_and_json_output_never_choose() {
        for (interactive, json) in [(false, false), (true, true)] {
            let output = Output::new(
                Style {
                    interactive,
                    ..style(false, false)
                },
                Level::Normal,
                json,
                false,
            );
            assert_eq!(
                output
                    .choose("which one?", &["iris".to_owned()], "")
                    .unwrap(),
                None
            );
        }
    }
}
