//! what the terminal can show, decided once when the program starts.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::sync::OnceLock;

use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor};

/// what a color means, the same wherever it appears.
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
/// `done=green:work=blue:accent=#d787ff`, or picks a whole theme with `theme=light`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    done: Color,
    work: Color,
    warn: Color,
    fail: Color,
    accent: Color,
}

/// a set of colors that belong together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Theme {
    /// the terminal's own sixteen colors, which follow whatever theme it has
    Ansi,
    /// a sage green for work and for what a person types, a leaf green for what finished, amber
    /// and red, for a dark background: the docs site's colors, which `python/test_site.py` holds
    /// these to
    Dark,
    /// the same colors, deeper, for a light background
    Light,
}

impl Theme {
    fn palette(self) -> Palette {
        let rgb = |r, g, b| Color::from(RgbColor(r, g, b));
        match self {
            Theme::Ansi => Palette {
                done: AnsiColor::Green.into(),
                work: AnsiColor::Cyan.into(),
                warn: AnsiColor::Yellow.into(),
                fail: AnsiColor::Red.into(),
                accent: AnsiColor::Cyan.into(),
            },
            Theme::Dark => Palette {
                done: rgb(0xa6, 0xcd, 0x6c),
                work: rgb(0x97, 0xcf, 0xa8),
                warn: rgb(0xe3, 0xb2, 0x57),
                fail: rgb(0xf1, 0x78, 0x86),
                accent: rgb(0x97, 0xcf, 0xa8),
            },
            Theme::Light => Palette {
                done: rgb(0x36, 0x76, 0x27),
                work: rgb(0x29, 0x6a, 0x42),
                warn: rgb(0x89, 0x59, 0x00),
                fail: rgb(0xb2, 0x26, 0x3a),
                accent: rgb(0x29, 0x6a, 0x42),
            },
        }
    }

    /// the theme a terminal suits: the dark one where it draws any color and its background is
    /// known or assumed dark, the light one on a light background, and its own colors elsewhere.
    pub(crate) fn detect(var: &dyn Fn(&str) -> Option<OsString>) -> Theme {
        if !full_color(var) {
            return Theme::Ansi;
        }
        // COLORFGBG is "foreground;background" in the sixteen colors; 7 and 15 are white
        let light = var("COLORFGBG")
            .and_then(|value| value.into_string().ok())
            .and_then(|value| value.rsplit(';').next().map(str::to_owned))
            .is_some_and(|background| matches!(background.as_str(), "7" | "15"));
        if light { Theme::Light } else { Theme::Dark }
    }
}

/// terminals that draw any of sixteen million colors rather than a fixed sixteen.
fn full_color(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    let program = var("TERM_PROGRAM").unwrap_or_default();
    var("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit")
        || var("WT_SESSION").is_some()
        || ["vscode", "iTerm.app", "WezTerm", "ghostty"]
            .iter()
            .any(|name| program == *name)
}

impl Default for Palette {
    fn default() -> Palette {
        Theme::Ansi.palette()
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
    pub(crate) fn parse(text: &str, theme: Theme) -> (Palette, Vec<String>) {
        let entries: Vec<&str> = text
            .split(':')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .collect();
        let mut unread = Vec::new();
        // a theme entry sets the base the other entries change, wherever it is written
        let mut theme = theme;
        for entry in &entries {
            if let Some((key, value)) = entry.split_once('=')
                && key.trim().eq_ignore_ascii_case("theme")
            {
                match value.trim().to_ascii_lowercase().as_str() {
                    "ansi" => theme = Theme::Ansi,
                    "dark" => theme = Theme::Dark,
                    "light" => theme = Theme::Light,
                    _ => unread.push((*entry).to_owned()),
                }
            }
        }
        let mut palette = theme.palette();
        for entry in entries {
            if entry
                .split_once('=')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("theme"))
            {
                continue;
            }
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

/// whether stdout takes color, as the run's color choice decided it for that stream.
static STDOUT_COLOR: OnceLock<bool> = OnceLock::new();

/// the color of `tone` in the palette this run uses.
pub(crate) fn color(tone: Tone) -> Color {
    PALETTE.get().copied().unwrap_or_default().color(tone)
}

/// picks this run's palette from the terminal and `FETCHLOOM_COLORS`, and returns the entries it
/// could not read.
pub(crate) fn load_palette() -> Vec<String> {
    // unit tests compare escape codes, which must not follow the terminal that runs them;
    // `Theme::detect` and `Palette::parse` have tests of their own
    if cfg!(test) {
        return Vec::new();
    }
    let var = |name: &str| std::env::var_os(name);
    let theme = Theme::detect(&var);
    let text = var("FETCHLOOM_COLORS").unwrap_or_default();
    let (palette, unread) = Palette::parse(&text.to_string_lossy(), theme);
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
    /// the terminal turns OSC 8 sequences into links a person can click.
    pub(crate) links: bool,
    /// a person at a terminal asked with `FETCHLOOM_ACCESSIBLE` for what a screen reader reads
    /// well: questions on plain lines, a choice by its number, and nothing drawn over again.
    pub(crate) accessible: bool,
}

impl Style {
    pub(crate) fn detect(choice: ColorChoice) -> Style {
        let _ = STDOUT_COLOR.set(Style::stdout_color(choice));
        let terminal = std::io::stderr().is_terminal();
        let var = |name: &str| std::env::var_os(name);
        Style {
            color: wants_color(choice, terminal, &var),
            unicode: wants_unicode(terminal, &var),
            interactive: is_interactive(terminal, &var),
            watched: terminal && std::io::stdout().is_terminal() && !by_agent(&var),
            links: is_interactive(terminal, &var) && makes_links(&var),
            accessible: is_accessible(terminal, &var),
        }
    }

    pub(crate) fn stdout_color(choice: ColorChoice) -> bool {
        wants_color(choice, std::io::stdout().is_terminal(), &|name| {
            std::env::var_os(name)
        })
    }

    /// whether stdout takes color, as [`Style::detect`] decided for the run, or not before it
    /// ran, as in tests.
    pub(crate) fn stdout_colored() -> bool {
        STDOUT_COLOR.get().copied().unwrap_or(false)
    }

    /// what ends text that was cut short.
    pub(crate) fn more(self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }

    /// what parts the facts on a line.
    pub(crate) fn dot(self) -> &'static str {
        if self.unicode { "\u{b7}" } else { "-" }
    }
}

fn is_interactive(terminal: bool, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    terminal && !marks("CI", var) && var("TERM").is_none_or(|v| v != "dumb") && !by_agent(var)
}

/// whether `FETCHLOOM_ACCESSIBLE` is on at a terminal, `TERM=dumb` included, outside CI and
/// coding agents, which a question would leave waiting.
fn is_accessible(terminal: bool, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    terminal
        && crate::machine::is_on(var, "FETCHLOOM_ACCESSIBLE")
        && !marks("CI", var)
        && !by_agent(var)
}

/// whether `name`, a variable a CI service or a coding agent sets, marks this run: any value but
/// empty, false or 0, so a service's own word counts too, as drone's `CI=drone` does.
fn marks(name: &str, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    !matches!(
        crate::machine::switched(name, var(name).as_deref()),
        Ok(None | Some(false))
    )
}

/// variables coding agents set on the commands they run, often in a real terminal where a question
/// would wait forever. ones a person's own terminal sets too, as cursor's trace id, are left out.
const AGENT_VARIABLES: [&str; 10] = [
    "AI_AGENT",
    "VSCODE_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "GEMINI_CLI",
    "CURSOR_AGENT",
    "OPENCODE",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
];

/// whether a coding agent runs this command, which then gets no questions, colors or animation.
pub(crate) fn by_agent(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    AGENT_VARIABLES.iter().any(|name| marks(name, var))
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
    let is = |name: &str, value: &str| var(name).is_some_and(|v| v == value);
    let non_empty = |name: &str| var(name).is_some_and(|v| !v.is_empty());
    if non_empty("NO_COLOR") {
        return false;
    }
    // CLICOLOR_FORCE asks for color where none would be, as into a pipe
    if non_empty("CLICOLOR_FORCE") && !is("CLICOLOR_FORCE", "0") {
        return true;
    }
    if !terminal {
        return false;
    }
    if is("CLICOLOR", "0") || is("TERM", "dumb") || marks("CI", var) || by_agent(var) {
        return false;
    }
    terminal
}

/// terminals known to draw OSC 8 links. there is no way to ask a terminal, so anything else,
/// including a multiplexer that may drop the sequence, gets the plain address.
fn makes_links(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    if var("TMUX").is_some() || var("STY").is_some() {
        return false;
    }
    let program = var("TERM_PROGRAM").unwrap_or_default();
    let vte = var("VTE_VERSION")
        .and_then(|value| value.to_str().and_then(|text| text.parse::<u32>().ok()))
        .is_some_and(|version| version >= 5000);
    var("WT_SESSION").is_some()
        || ["vscode", "iTerm.app", "WezTerm", "ghostty"]
            .iter()
            .any(|name| program == *name)
        || vte
        || var("KONSOLE_VERSION").is_some()
        || var("TERM").is_some_and(|term| term == "xterm-kitty")
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
        assert!(
            auto(&[("CLICOLOR_FORCE", "1")], false),
            "forced into a pipe too"
        );
        assert!(!auto(&[("CLICOLOR_FORCE", "0")], false));
        assert!(!auto(&[("CLICOLOR", "0")], true));
        assert!(!auto(&[("TERM", "dumb")], true));
        assert!(!auto(&[("CI", "true")], true));
        assert!(auto(&[("CI", "0")], true), "CI=0 is no CI");
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
        assert!(!is_interactive(true, &env(&[("TERM", "dumb")])));
        // a service's own word, as drone's, marks CI too
        for value in ["true", "1", "drone"] {
            assert!(!is_interactive(true, &env(&[("CI", value)])), "{value}");
        }
        for value in ["false", "0", " FALSE ", ""] {
            assert!(is_interactive(true, &env(&[("CI", value)])), "{value:?}");
        }
    }

    #[test]
    fn accessible_asks_a_person_at_a_terminal_but_never_ci_or_an_agent() {
        let asked = ("FETCHLOOM_ACCESSIBLE", "1");
        assert!(is_accessible(true, &env(&[asked])));
        assert!(is_accessible(true, &env(&[asked, ("TERM", "dumb")])));
        assert!(is_accessible(
            true,
            &env(&[("FETCHLOOM_ACCESSIBLE", " True ")])
        ));
        assert!(is_accessible(true, &env(&[asked, ("CI", "false")])));
        assert!(!is_accessible(false, &env(&[asked])));
        assert!(!is_accessible(true, &env(&[asked, ("CI", "true")])));
        assert!(!is_accessible(
            true,
            &env(&[("FETCHLOOM_ACCESSIBLE", "yes")])
        ));
        assert!(!is_accessible(true, &env(&[asked, ("CLAUDECODE", "1")])));
        assert!(!is_accessible(true, &env(&[("FETCHLOOM_ACCESSIBLE", "0")])));
        assert!(!is_accessible(true, &env(&[])));
    }

    #[test]
    fn a_coding_agent_gets_no_questions_colors_or_animation() {
        for name in ["VSCODE_AGENT", "CLAUDECODE", "AI_AGENT", "CODEX_SANDBOX"] {
            assert!(!is_interactive(true, &env(&[(name, "1")])), "{name}");
            assert!(
                !wants_color(ColorChoice::Auto, true, &env(&[(name, "1")])),
                "{name}"
            );
        }
        assert!(is_interactive(true, &env(&[("CLAUDECODE", "0")])));
        assert!(is_interactive(true, &env(&[("CURSOR_TRACE_ID", "x")])));
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
        let (palette, unread) =
            Palette::parse("done=blue : accent=#D787FF:warn=214::", Theme::Ansi);
        assert_eq!(unread, Vec::<String>::new());
        assert_eq!(palette.color(Tone::Done), AnsiColor::Blue.into());
        assert_eq!(
            palette.color(Tone::Accent),
            RgbColor(0xd7, 0x87, 0xff).into()
        );
        assert_eq!(palette.color(Tone::Warn), Ansi256Color(214).into());
        assert_eq!(palette.color(Tone::Work), AnsiColor::Cyan.into());
        assert_eq!(palette.color(Tone::Fail), AnsiColor::Red.into());
        let (palette, _) = Palette::parse("fail=bright-magenta", Theme::Ansi);
        assert_eq!(palette.color(Tone::Fail), AnsiColor::BrightMagenta.into());
    }

    #[test]
    fn full_color_terminals_get_a_full_color_theme_and_others_their_own() {
        assert_eq!(Theme::detect(&env(&[])), Theme::Ansi);
        assert_eq!(
            Theme::detect(&env(&[("COLORTERM", "truecolor")])),
            Theme::Dark
        );
        assert_eq!(Theme::detect(&env(&[("WT_SESSION", "x")])), Theme::Dark);
        assert_eq!(
            Theme::detect(&env(&[("TERM_PROGRAM", "vscode"), ("COLORFGBG", "0;15")])),
            Theme::Light
        );
        let (palette, unread) = Palette::parse("theme=light:accent=cyan", Theme::Dark);
        assert_eq!(unread, Vec::<String>::new());
        assert_eq!(palette.color(Tone::Accent), AnsiColor::Cyan.into());
        assert_eq!(
            palette.color(Tone::Done),
            Theme::Light.palette().color(Tone::Done)
        );
        let (palette, unread) = Palette::parse("theme=sepia", Theme::Dark);
        assert_eq!(unread, ["theme=sepia"]);
        assert_eq!(palette, Theme::Dark.palette());
    }

    #[test]
    fn a_palette_keeps_the_default_for_each_entry_it_cannot_read() {
        let (palette, unread) = Palette::parse(
            "done=teal:shiny=red:work=#12345:accent=256:fail:warn=magenta",
            Theme::Ansi,
        );
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
