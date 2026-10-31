//! drawing and key handling for a dataset choice.

use anstyle::Style as Paint;
use console::{Key, Term};

use super::{ACCENT, Style, paint, plain};
use crate::error::{Error, Kind};

pub(super) fn choose(
    question: &str,
    options: &[String],
    style: Style,
) -> Result<Option<usize>, Error> {
    if options.is_empty() {
        return Err(cancelled());
    }
    let terminal = Term::stderr();
    let mut cursor = 0;
    draw(&terminal, question, options, cursor, style);
    let answer = loop {
        let previous = cursor;
        match selection_key(terminal.read_key_raw(), &mut cursor, options.len()) {
            Ok(None) => {
                if previous != cursor {
                    clear(&terminal, options.len() + 1);
                    draw(&terminal, question, options, cursor, style);
                }
            }
            answer => break answer,
        }
    };
    clear(&terminal, options.len() + 1);
    answer
}

fn draw(terminal: &Term, question: &str, options: &[String], cursor: usize, style: Style) {
    let width = usize::from(terminal.size().1);
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
    let _ = terminal.write_line(&format!("  {bold}{question}{bold:#}  {dim}{keys}{dim:#}"));
    for (index, option) in options.iter().enumerate() {
        let _ = terminal.write_line(&row(option, index, cursor, style, width));
    }
}

fn clear(terminal: &Term, lines: usize) {
    for _ in 0..lines {
        let _ = terminal.move_cursor_up(1);
        let _ = terminal.clear_line();
    }
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
        Key::CtrlC => return Err(Error::new(Kind::Interrupted, "stopped")),
        Key::Escape | Key::Char('q' | 'n') => return Err(cancelled()),
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
        .help("run it again with one of the references above, or with a link or an id")
}

/// one option: the reference, then its title and facts, dim unless the cursor is on it. the
/// reference is never cut; the rest gives way on a narrow terminal.
fn row(option: &str, index: usize, cursor: usize, style: Style, width: usize) -> String {
    let text = plain(option);
    let (reference, rest) = text.split_once("  ").unwrap_or((&text, ""));
    let rest = rest.trim_start();
    // the padding that lines the titles up, kept apart from the bold reference
    let column = text.chars().count() - rest.chars().count();
    let pad = " ".repeat(column.saturating_sub(reference.chars().count() + 2));
    let room = width.saturating_sub(column + 7);
    let more = if style.unicode { "\u{2026}" } else { "..." };
    let rest = if room < 8 {
        String::new()
    } else {
        console::truncate_str(rest, room, more).into_owned()
    };
    let rest = super::shown(&rest, style.unicode);
    let number = index + 1;
    let bold = paint(style.color, Paint::new().bold());
    let dim = paint(style.color, Paint::new().dimmed());
    if index == cursor {
        let marker = if style.unicode { "\u{203a}" } else { ">" };
        let cyan = paint(style.color, ACCENT.bold());
        format!("  {cyan}{marker} {number}{cyan:#} {bold}{reference}{bold:#}{pad}  {rest}")
            .trim_end()
            .to_owned()
    } else {
        format!("    {dim}{number}{dim:#} {reference}{pad}  {dim}{rest}{dim:#}")
            .trim_end()
            .to_owned()
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

    #[test]
    fn cancellation_and_eof_stop_without_choosing() {
        for key in [
            Ok(Key::Escape),
            Ok(Key::Char('q')),
            Ok(Key::Char('n')),
            Err(std::io::ErrorKind::UnexpectedEof.into()),
        ] {
            let error = selection_key(key, &mut 0, 3).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(error.message, "nothing was chosen");
            assert!(error.help.unwrap().contains("references above"));
        }
    }

    #[test]
    fn ctrl_c_and_interrupted_reads_stop_as_an_interruption() {
        for key in [Ok(Key::CtrlC), Err(std::io::ErrorKind::Interrupted.into())] {
            assert_eq!(
                selection_key(key, &mut 0, 3).unwrap_err().kind,
                Kind::Interrupted
            );
        }
    }

    #[test]
    fn rows_highlight_the_cursor_and_escape_untrusted_titles() {
        let selected = row("iris", 0, 0, style(false, false), 80);
        assert_eq!(selected, "  > 1 iris");
        assert_eq!(row("other", 1, 0, style(false, true), 80), "    2 other");
        let colored = row("iris", 0, 0, style(true, true), 80);
        assert!(colored.contains('\u{203a}'));
        assert!(colored.contains("\x1b["));
        assert!(colored.contains("iris"));
        let unsafe_text = row("iris\x1b[2J\nnext", 0, 0, style(false, false), 80);
        assert!(!unsafe_text.contains(char::is_control));
        assert!(unsafe_text.contains("\\u{1b}"));
        let narrow = row(
            "zenodo:1  a long title for a narrow terminal",
            0,
            0,
            style(false, false),
            30,
        );
        assert!(console::measure_text_width(&narrow) <= 30, "{narrow}");
        assert!(narrow.contains("zenodo:1"), "the reference is never cut");
        let tight = row(
            "kaggle:owner/a-very-long-dataset-name  title",
            1,
            0,
            style(false, false),
            30,
        );
        assert!(
            tight.contains("kaggle:owner/a-very-long-dataset-name"),
            "{tight}"
        );
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
                output.choose("which one?", &["iris".to_owned()]).unwrap(),
                None
            );
        }
    }
}
