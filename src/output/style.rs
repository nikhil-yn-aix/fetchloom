//! what the terminal can show, decided once when the program starts.

use std::ffi::OsString;
use std::io::IsTerminal;

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
pub(crate) struct Style {
    pub(crate) color: bool,
    pub(crate) unicode: bool,
    /// a person watches stderr: it is a terminal, outside CI, and not a dumb one.
    pub(crate) interactive: bool,
}

impl Style {
    pub(crate) fn detect(choice: ColorChoice) -> Style {
        let terminal = std::io::stderr().is_terminal();
        let var = |name: &str| std::env::var_os(name);
        Style {
            color: wants_color(choice, terminal, &var),
            unicode: wants_unicode(terminal, &var),
            interactive: is_interactive(terminal, &var),
        }
    }

    pub(crate) fn failed(self) -> &'static str {
        if self.unicode { "✗" } else { "[x]" }
    }

    pub(crate) fn done(self) -> &'static str {
        if self.unicode { "✓" } else { "[ok]" }
    }

    pub(crate) fn warned(self) -> &'static str {
        if self.unicode { "!" } else { "[!]" }
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
    true
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
        assert!(auto(&[("CLICOLOR_FORCE", "1")], false));
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
}
