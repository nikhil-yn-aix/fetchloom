//! what the terminal can show, decided once when the program starts.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::sync::OnceLock;

use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor};

/// what a color means. a line's subject is bold and its facts are dim, so a tone always means
/// the same thing wherever it appears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// finished work
    Done,
    /// work under way, and its bars
    Work,
    /// what needs attention
    Warn,
    /// what failed
    Fail,
    /// what a person types: commands, options, numbers to choose
    Accent,
}

/// the color of each tone. `FETCHLOOM_COLORS` changes any of them, as in
/// `done=green:work=blue:accent=#d787ff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    done: Color,
    work: Color,
    warn: Color,
    fail: Color,
    accent: Color,
}

impl Default for Palette {
    fn default() -> Palette {
        Palette {
            done: AnsiColor::Green.into(),
            work: AnsiColor::Cyan.into(),
            warn: AnsiColor::Yellow.into(),
            fail: AnsiColor::Red.into(),
            accent: AnsiColor::Cyan.into(),
        }
    }
}

impl Palette {
    pub(crate) fn color(&self, tone: Tone) -> Color {
        match tone {
            Tone::Done => self.done,
            Tone::Work => self.work,
            Tone::Warn => self.warn,
            Tone::Fail => self.fail,
            Tone::Accent => self.accent,
        }
    }

    /// reads `tone=color` entries split by `:`. an entry it cannot read keeps the default and is
    /// returned, so one typo never costs the rest.
    pub(crate) fn parse(text: &str) -> (Palette, Vec<String>) {
        let mut palette = Palette::default();
        let mut unread = Vec::new();
        for entry in text.split(':').map(str::trim).filter(|e| !e.is_empty()) {
            let parsed = entry.split_once('=').and_then(|(tone, color)| {
                let slot = match tone.trim().to_ascii_lowercase().as_str() {
                    "done" => &mut palette.done,
                    "work" => &mut palette.work,
                    "warn" => &mut palette.warn,
                    "fail" => &mut palette.fail,
                    "accent" => &mut palette.accent,
                    _ => return None,
                };
                *slot = parse_color(color.trim())?;
                Some(())
            });
            if parsed.is_none() {
                unread.push(entry.to_owned());
            }
        }
        (palette, unread)
    }
}

/// a color name, `bright-` and a name, a 256-color number, or `#rrggbb`.
fn parse_color(text: &str) -> Option<Color> {
    let text = text.to_ascii_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        let [_, r, g, b] = value.to_be_bytes();
        return Some(RgbColor(r, g, b).into());
    }
    if let Ok(number) = text.parse::<u8>() {
        return Some(Ansi256Color(number).into());
    }
    let (bright, name) = match text.strip_prefix("bright-") {
        Some(name) => (true, name),
        None => (false, text.as_str()),
    };
    let base = match name {
        "black" => AnsiColor::Black,
        "red" => AnsiColor::Red,
        "green" => AnsiColor::Green,
        "yellow" => AnsiColor::Yellow,
        "blue" => AnsiColor::Blue,
        "magenta" => AnsiColor::Magenta,
        "cyan" => AnsiColor::Cyan,
        "white" => AnsiColor::White,
        _ => return None,
    };
    Some(if bright { base.bright(true) } else { base }.into())
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

/// the color of `tone` in the palette this run uses.
pub(crate) fn color(tone: Tone) -> Color {
    PALETTE.get().copied().unwrap_or_default().color(tone)
}

/// reads `FETCHLOOM_COLORS` for this run and returns the entries it could not read.
pub(crate) fn load_palette() -> Vec<String> {
    let Some(text) = std::env::var_os("FETCHLOOM_COLORS") else {
        return Vec::new();
    };
    let (palette, unread) = Palette::parse(&text.to_string_lossy());
    let _ = PALETTE.set(palette);
    unread
}

/// when to color output, as `--color` takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// what stderr can show: color, unicode symbols, and live bars and questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a yes or no about the terminal"
)]
pub(crate) struct Style {
    pub(crate) color: bool,
    pub(crate) unicode: bool,
    /// a person watches stderr: it is a terminal, outside CI, and not a dumb one.
    pub(crate) interactive: bool,
    /// stdout and stderr are both terminals, so a person reads the answer and the story, and no
    /// script does.
    pub(crate) watched: bool,
}

impl Style {
    pub(crate) fn detect(choice: ColorChoice) -> Style {
        let terminal = std::io::stderr().is_terminal();
        let var = |name: &str| std::env::var_os(name);
        Style {
            color: wants_color(choice, terminal, &var),
            unicode: wants_unicode(terminal, &var),
            interactive: is_interactive(terminal, &var),
            watched: terminal && std::io::stdout().is_terminal(),
        }
    }

    pub(crate) fn stdout_color(choice: ColorChoice) -> bool {
        wants_color(choice, std::io::stdout().is_terminal(), &|name| {
            std::env::var_os(name)
        })
    }

    /// what ends text that was cut short.
    pub(crate) fn more(self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }
}

fn is_interactive(terminal: bool, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    terminal && var("CI").is_none_or(|v| v.is_empty()) && var("TERM").is_none_or(|v| v != "dumb")
}

fn wants_color(
    choice: ColorChoice,
    terminal: bool,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> bool {
    match choice {
        ColorChoice::Always => return true,
        ColorChoice::Never => return false,
        ColorChoice::Auto => {}
    }
    if !terminal {
        return false;
    }
    let is = |name: &str, value: &str| var(name).is_some_and(|v| v == value);
    let non_empty = |name: &str| var(name).is_some_and(|v| !v.is_empty());
    if non_empty("NO_COLOR") {
        return false;
    }
    if non_empty("CLICOLOR_FORCE") && !is("CLICOLOR_FORCE", "0") {
        return true;
    }
    if is("CLICOLOR", "0") || is("TERM", "dumb") || non_empty("CI") {
        return false;
    }
    terminal
}

fn wants_unicode(terminal: bool, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    if var("TERM").is_some_and(|v| v == "dumb") {
        return false;
    }
    if cfg!(windows) && terminal {
        // the legacy windows console draws many symbols as empty boxes. windows terminal,
        // vs code and conemu set one of these variables and draw them properly.
        return ["WT_SESSION", "TERM_PROGRAM", "ConEmuANSI"]
            .iter()
            .any(|name| var(name).is_some());
    }
    // the locale says which characters the terminal can draw: the first of these that is set
    // wins, as in C programs. none set keeps unicode, as most terminals draw it.
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()));
    locale.is_none_or(|locale| {
        let locale = locale.to_string_lossy().to_ascii_lowercase();
        locale.contains("utf-8") || locale.contains("utf8")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_flag_beats_every_variable() {
        let var = env(&[("NO_COLOR", "1")]);
        assert!(wants_color(ColorChoice::Always, false, &var));
        let var = env(&[("CLICOLOR_FORCE", "1")]);
        assert!(!wants_color(ColorChoice::Never, true, &var));
    }

    #[test]
    fn auto_follows_the_variables_in_order() {
        let auto = |pairs: &[(&str, &str)], terminal| {
            wants_color(ColorChoice::Auto, terminal, &env(pairs))
        };
        assert!(auto(&[], true));
        assert!(!auto(&[], false));
        assert!(!auto(&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")], true));
        assert!(
            auto(&[("NO_COLOR", "")], true),
            "an empty NO_COLOR is ignored"
        );
        assert!(auto(&[("CLICOLOR_FORCE", "1")], true));
        assert!(!auto(&[("CLICOLOR_FORCE", "1")], false));
        assert!(!auto(&[("CLICOLOR_FORCE", "0")], false));
        assert!(!auto(&[("CLICOLOR", "0")], true));
        assert!(!auto(&[("TERM", "dumb")], true));
        assert!(!auto(&[("CI", "true")], true));
    }

    #[test]
    fn unicode_falls_back_on_dumb_terminals() {
        assert!(!wants_unicode(true, &env(&[("TERM", "dumb")])));
        assert!(
            wants_unicode(false, &env(&[])),
            "files and pipes keep unicode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unicode_follows_the_locale() {
        assert!(wants_unicode(true, &env(&[("LANG", "en_US.UTF-8")])));
        assert!(wants_unicode(true, &env(&[("LANG", "C.utf8")])));
        assert!(!wants_unicode(true, &env(&[("LANG", "C")])));
        assert!(!wants_unicode(
            true,
            &env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")])
        ));
        assert!(wants_unicode(
            true,
            &env(&[("LC_ALL", ""), ("LANG", "en_US.UTF-8")])
        ));
    }

    #[test]
    fn bars_and_questions_need_a_person_at_a_terminal() {
        assert!(is_interactive(true, &env(&[])));
        assert!(!is_interactive(false, &env(&[])));
        assert!(!is_interactive(true, &env(&[("CI", "true")])));
        assert!(!is_interactive(true, &env(&[("TERM", "dumb")])));
    }

    #[cfg(windows)]
    #[test]
    fn unicode_on_windows_needs_a_modern_terminal() {
        assert!(!wants_unicode(true, &env(&[])));
        assert!(wants_unicode(true, &env(&[("WT_SESSION", "x")])));
        assert!(wants_unicode(true, &env(&[("TERM_PROGRAM", "vscode")])));
    }

    #[test]
    fn a_palette_changes_only_the_tones_it_names() {
        let (palette, unread) = Palette::parse("done=blue : accent=#D787FF:warn=214::");
        assert_eq!(unread, Vec::<String>::new());
        assert_eq!(palette.color(Tone::Done), AnsiColor::Blue.into());
        assert_eq!(
            palette.color(Tone::Accent),
            RgbColor(0xd7, 0x87, 0xff).into()
        );
        assert_eq!(palette.color(Tone::Warn), Ansi256Color(214).into());
        assert_eq!(palette.color(Tone::Work), AnsiColor::Cyan.into());
        assert_eq!(palette.color(Tone::Fail), AnsiColor::Red.into());
        let (palette, _) = Palette::parse("fail=bright-magenta");
        assert_eq!(palette.color(Tone::Fail), AnsiColor::BrightMagenta.into());
    }

    #[test]
    fn a_palette_keeps_the_default_for_each_entry_it_cannot_read() {
        let (palette, unread) =
            Palette::parse("done=teal:shiny=red:work=#12345:accent=256:fail:warn=magenta");
        assert_eq!(
            unread,
            [
                "done=teal",
                "shiny=red",
                "work=#12345",
                "accent=256",
                "fail"
            ]
        );
        assert_eq!(palette.color(Tone::Warn), AnsiColor::Magenta.into());
        assert_eq!(
            Palette {
                warn: AnsiColor::Yellow.into(),
                ..palette
            },
            Palette::default()
        );
    }
}
