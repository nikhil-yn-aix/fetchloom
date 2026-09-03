//! What the streams are attached to, and what that forces.

use std::io::IsTerminal;

use crate::settings::Environment;
use crate::surface::DisplayMode;

/// What each stream is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Streams {
    /// Whether the standard output stream is a terminal.
    pub stdout: bool,
    /// Whether the standard error stream is a terminal.
    pub stderr: bool,
    /// Whether the standard input stream is a terminal.
    pub stdin: bool,
}

impl Streams {
    /// Reads what the process's own streams are attached to.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            stdout: std::io::stdout().is_terminal(),
            stderr: std::io::stderr().is_terminal(),
            stdin: std::io::stdin().is_terminal(),
        }
    }

    /// Reports whether a prompt may be shown.
    #[must_use]
    pub fn can_prompt(self) -> bool {
        self.stdin && self.stderr
    }
}

/// Reports whether the run is inside a continuous integration environment.
#[must_use]
pub fn continuous_integration(environment: &dyn Environment) -> bool {
    environment.get("CI").is_some()
}

/// Reports whether the terminal cannot address the cursor.
#[must_use]
pub fn dumb_terminal(environment: &dyn Environment) -> bool {
    environment.get("TERM").as_deref() == Some("dumb")
}

/// Why a display mode was not the one that was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForcedDisplay {
    /// The mode actually used.
    pub mode: DisplayMode,
    /// The mode that was asked for, when it differed.
    pub requested: Option<DisplayMode>,
    /// Why the mode changed, when it changed.
    pub reason: Option<String>,
}

/// Decides which display mode a run actually uses.
#[must_use]
pub fn resolve_display(
    requested: DisplayMode,
    quiet: bool,
    streams: Streams,
    environment: &dyn Environment,
) -> ForcedDisplay {
    if quiet {
        return ForcedDisplay {
            mode: DisplayMode::None,
            requested: (requested != DisplayMode::None).then_some(requested),
            reason: (requested != DisplayMode::None).then(|| "progress was suppressed".to_owned()),
        };
    }
    if requested == DisplayMode::None {
        return ForcedDisplay {
            mode: DisplayMode::None,
            requested: None,
            reason: None,
        };
    }

    if !streams.stderr {
        return ForcedDisplay {
            mode: DisplayMode::None,
            requested: (requested == DisplayMode::Live).then_some(DisplayMode::Live),
            reason: (requested == DisplayMode::Live)
                .then(|| "the standard error stream is not a terminal".to_owned()),
        };
    }

    let forced = if dumb_terminal(environment) {
        Some("the terminal cannot address the cursor")
    } else if continuous_integration(environment) {
        Some("a continuous integration environment was detected")
    } else {
        None
    };

    match (forced, requested) {
        (Some(reason), DisplayMode::Live) => ForcedDisplay {
            mode: DisplayMode::Plain,
            requested: Some(DisplayMode::Live),
            reason: Some(reason.to_owned()),
        },
        (Some(_), _) => ForcedDisplay {
            mode: DisplayMode::Plain,
            requested: None,
            reason: None,
        },
        (None, mode) => ForcedDisplay {
            mode,
            requested: None,
            reason: None,
        },
    }
}

/// Decides whether output carries color, which is a presentation choice and
/// never a fact about the run.
#[must_use]
pub fn resolve_color(
    requested: Option<crate::surface::ColorChoice>,
    streams: Streams,
    environment: &dyn Environment,
) -> bool {
    match requested {
        Some(crate::surface::ColorChoice::Always) => true,
        Some(crate::surface::ColorChoice::Never) => false,
        Some(crate::surface::ColorChoice::Auto) | None => {
            environment.get("NO_COLOR").is_none() && streams.stderr && !dumb_terminal(environment)
        }
    }
}

/// Reports whether a hint may be printed at all, which contracts settles before
/// any hint is chosen.
#[must_use]
pub fn hints_permitted(
    disabled: bool,
    streams: Streams,
    environment: &dyn Environment,
) -> bool {
    !disabled && streams.stderr && streams.stdin && !continuous_integration(environment)
}

#[cfg(test)]
mod tests {
    use super::{Streams, hints_permitted, resolve_color};
    use crate::settings::Environment;
    use crate::surface::ColorChoice;

    struct Fixed(Vec<(String, String)>);

    impl Environment for Fixed {
        fn get(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(held, _)| held == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn nothing() -> Fixed {
        Fixed(Vec::new())
    }

    fn terminal() -> Streams {
        Streams {
            stdout: true,
            stderr: true,
            stdin: true,
        }
    }

    fn piped() -> Streams {
        Streams {
            stdout: false,
            stderr: false,
            stdin: false,
        }
    }

    #[test]
    fn color_is_forced_on_and_off_by_the_flag_whatever_the_stream_is() {
        assert!(resolve_color(Some(ColorChoice::Always), piped(), &nothing()));
        assert!(!resolve_color(
            Some(ColorChoice::Never),
            terminal(),
            &nothing()
        ));
    }

    #[test]
    fn no_color_disables_color_when_nothing_asked_for_it() {
        let set = Fixed(vec![("NO_COLOR".to_owned(), "1".to_owned())]);
        assert!(!resolve_color(None, terminal(), &set));
        assert!(!resolve_color(Some(ColorChoice::Auto), terminal(), &set));
    }

    #[test]
    fn no_color_does_not_override_an_explicit_request() {
        let set = Fixed(vec![("NO_COLOR".to_owned(), "1".to_owned())]);
        assert!(resolve_color(Some(ColorChoice::Always), terminal(), &set));
    }

    #[test]
    fn a_stream_that_is_not_a_terminal_carries_no_color_by_itself() {
        assert!(!resolve_color(None, piped(), &nothing()));
        assert!(resolve_color(None, terminal(), &nothing()));
    }

    #[test]
    fn a_dumb_terminal_carries_no_color() {
        let dumb = Fixed(vec![("TERM".to_owned(), "dumb".to_owned())]);
        assert!(!resolve_color(None, terminal(), &dumb));
    }

    #[test]
    fn hints_are_suppressed_by_the_flag_by_a_pipe_and_by_continuous_integration() {
        assert!(hints_permitted(false, terminal(), &nothing()));
        assert!(!hints_permitted(true, terminal(), &nothing()));
        assert!(!hints_permitted(false, piped(), &nothing()));
        let building = Fixed(vec![("CI".to_owned(), "1".to_owned())]);
        assert!(!hints_permitted(false, terminal(), &building));
    }
}
