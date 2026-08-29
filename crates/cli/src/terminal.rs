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
    ///
    /// A prompt appears only when standard input and standard error are both
    /// terminals. Otherwise a required prompt is a policy failure.
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
///
/// Takes the mode that was asked for, whether progress was suppressed, what the
/// streams are attached to, and somewhere to read the environment from. The
/// mode is forced to plain or none when standard error is not a terminal, when
/// the terminal cannot address the cursor, or when a continuous integration
/// environment is detected. Returns the mode used and, when it differs from the
/// one requested, the reason, so the caller can report the degradation.
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
        (None, DisplayMode::Live) => ForcedDisplay {
            mode: DisplayMode::Plain,
            requested: Some(DisplayMode::Live),
            reason: Some("this build renders the plain view only".to_owned()),
        },
        (None, mode) => ForcedDisplay {
            mode,
            requested: None,
            reason: None,
        },
    }
}
