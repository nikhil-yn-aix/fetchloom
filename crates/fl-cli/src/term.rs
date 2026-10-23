//! What the terminal can show: whether each stream is a terminal, whether to color, whether
//! symbols beyond ASCII print, and the styles and symbols that follow from it.

use std::io::{self, IsTerminal};

use anstyle::{AnsiColor, Effects};

use crate::args::Color;

/// What the streams of this process are attached to and what they can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Terminal {
    /// stderr is a terminal.
    pub(crate) stderr: bool,
    /// stdin is a terminal, so a question can be answered.
    pub(crate) stdin: bool,
    /// How the story on stderr is styled.
    pub(crate) style: Style,
}

impl Terminal {
    /// Detects the streams of this process under `--color` and the environment.
    pub(crate) fn detect(choice: Color) -> Self {
        Self::from_parts(
            choice,
            |name| std::env::var(name).ok(),
            io::stderr().is_terminal(),
            io::stdin().is_terminal(),
        )
    }

    /// The terminal for `choice`, variables read through `env`, and whether stderr and stdin
    /// are terminals.
    pub(crate) fn from_parts(
        choice: Color,
        env: impl Fn(&str) -> Option<String>,
        stderr: bool,
        stdin: bool,
    ) -> Self {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        let dumb = env("TERM").is_some_and(|term| term == "dumb");
        let color = match choice {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => {
                set("CLICOLOR_FORCE")
                    || (stderr
                        && !set("NO_COLOR")
                        && !dumb
                        && env("CLICOLOR").is_none_or(|value| value != "0"))
            }
        };
        let unicode = !set("NO_COLOR") && !dumb && (!stderr || shows_unicode(&env));
        Self {
            stderr,
            stdin,
            style: Style { color, unicode },
        }
    }
}

fn shows_unicode(env: &impl Fn(&str) -> Option<String>) -> bool {
    if cfg!(windows) {
        env("WT_SESSION").is_some()
            || env("TERM_PROGRAM").is_some()
            || env("ConEmuANSI").is_some_and(|value| value == "ON")
            || env("TERM").is_some_and(|term| term.starts_with("xterm") || term == "alacritty")
    } else {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|name| env(name).filter(|value| !value.is_empty()))
            .is_some_and(|locale| {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            })
    }
}

/// What a piece of text means, which picks its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Something finished well.
    Done,
    /// Something to look at that did not stop the command.
    Warn,
    /// Something that stopped the command.
    Error,
    /// Detail that matters less.
    Dim,
}

/// How text is styled on stderr and which symbols stand for what.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Style {
    color: bool,
    unicode: bool,
}

#[cfg(test)]
impl Style {
    /// No color and ASCII only.
    pub(crate) const PLAIN: Self = Self {
        color: false,
        unicode: false,
    };

    /// No color, every symbol as itself.
    pub(crate) const UNICODE: Self = Self {
        color: false,
        unicode: true,
    };
}

impl Style {
    /// Whether text is colored.
    pub(crate) const fn color(self) -> bool {
        self.color
    }

    /// Whether symbols beyond ASCII print as themselves.
    pub(crate) const fn unicode(self) -> bool {
        self.unicode
    }

    /// `text` in the color of `tone`.
    pub(crate) fn paint(self, tone: Tone, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        let style = match tone {
            Tone::Done => AnsiColor::Green.on_default().effects(Effects::BOLD),
            Tone::Warn => AnsiColor::Yellow.on_default().effects(Effects::BOLD),
            Tone::Error => AnsiColor::Red.on_default().effects(Effects::BOLD),
            Tone::Dim => anstyle::Style::new().effects(Effects::DIMMED),
        };
        format!("{style}{text}{style:#}")
    }

    /// A line starting with `verb`, right aligned in the color of `tone`, then `rest`.
    pub(crate) fn verb(self, verb: &str, tone: Tone, rest: &str) -> String {
        format!("{} {rest}\n", self.paint(tone, &format!("{verb:>11}")))
    }

    /// The arrow from a reference to what it resolved to.
    pub(crate) const fn arrow(self) -> &'static str {
        if self.unicode { "\u{2192}" } else { "->" }
    }

    /// The separator between facts on one line.
    pub(crate) const fn dot(self) -> &'static str {
        if self.unicode { "\u{b7}" } else { "-" }
    }

    /// The mark of a run that ended well.
    pub(crate) const fn check(self) -> &'static str {
        if self.unicode { "\u{2713}" } else { "ok" }
    }

    /// The mark of a run that failed.
    pub(crate) const fn cross(self) -> &'static str {
        if self.unicode { "\u{2717}" } else { "x" }
    }

    /// The gutter of a source snippet.
    pub(crate) const fn gutter(self) -> &'static str {
        if self.unicode { "\u{2502}" } else { "|" }
    }

    /// The corner that opens a source snippet.
    pub(crate) const fn corner(self) -> &'static str {
        if self.unicode {
            "\u{250c}\u{2500}"
        } else {
            "--"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn unicode_terminal() -> [(&'static str, &'static str); 2] {
        [("WT_SESSION", "1"), ("LANG", "en_US.UTF-8")]
    }

    #[test]
    fn color_follows_the_flag_then_the_environment_then_the_stream() {
        let env = unicode_terminal();
        let auto = |pairs: &[(&str, &str)], stderr| {
            Terminal::from_parts(Color::Auto, with(pairs), stderr, true)
                .style
                .color()
        };
        assert!(auto(&env, true));
        assert!(!auto(&env, false));
        assert!(!auto(&[("NO_COLOR", "1")], true));
        assert!(auto(&[("NO_COLOR", "")], true));
        assert!(!auto(&[("TERM", "dumb")], true));
        assert!(!auto(&[("CLICOLOR", "0")], true));
        assert!(auto(&[("CLICOLOR_FORCE", "1")], false));
        assert!(
            Terminal::from_parts(Color::Always, with(&[("NO_COLOR", "1")]), false, false)
                .style
                .color()
        );
        assert!(
            !Terminal::from_parts(Color::Never, with(&env), true, true)
                .style
                .color()
        );
    }

    #[test]
    fn symbols_fall_back_to_ascii_when_the_terminal_cannot_show_them() {
        let unicode = |pairs: &[(&str, &str)], stderr| {
            Terminal::from_parts(Color::Auto, with(pairs), stderr, false)
                .style
                .unicode()
        };
        assert!(unicode(&unicode_terminal(), true));
        assert!(unicode(&[], false));
        assert!(!unicode(&[], true));
        assert!(!unicode(&[("NO_COLOR", "1"), ("WT_SESSION", "1")], false));
        assert!(!unicode(&[("TERM", "dumb"), ("LANG", "C.UTF-8")], false));
        if cfg!(windows) {
            assert!(unicode(&[("TERM_PROGRAM", "vscode")], true));
            assert!(unicode(&[("ConEmuANSI", "ON")], true));
            assert!(unicode(&[("TERM", "xterm-256color")], true));
        } else {
            assert!(unicode(&[("LC_ALL", "C.utf8")], true));
            assert!(!unicode(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")], true));
        }
    }

    #[test]
    fn plain_style_is_ascii_without_escape_codes() {
        let plain = Style::PLAIN;
        assert_eq!(plain.paint(Tone::Error, "x"), "x");
        assert_eq!(
            plain.verb("Locked", Tone::Done, "1 file"),
            "     Locked 1 file\n"
        );
        assert_eq!(
            [
                plain.arrow(),
                plain.dot(),
                plain.check(),
                plain.cross(),
                plain.gutter(),
                plain.corner()
            ],
            ["->", "-", "ok", "x", "|", "--"]
        );
        let fancy = Style::UNICODE;
        assert_eq!(
            [fancy.arrow(), fancy.dot(), fancy.check(), fancy.cross()],
            ["\u{2192}", "\u{b7}", "\u{2713}", "\u{2717}"]
        );
    }

    #[test]
    fn color_wraps_text_in_one_escape_code_and_resets_it() {
        let style = Terminal::from_parts(Color::Always, with(&[]), true, true).style;
        assert!(style.color());
        assert_eq!(
            style.paint(Tone::Done, "ok"),
            "\u{1b}[1m\u{1b}[32mok\u{1b}[0m"
        );
        assert_eq!(style.paint(Tone::Dim, "d"), "\u{1b}[2md\u{1b}[0m");
        for tone in [Tone::Warn, Tone::Error] {
            assert_ne!(style.paint(tone, "t"), "t");
        }
    }
}
