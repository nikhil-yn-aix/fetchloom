//! Contract tests over when progress appears and which view is used.

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use tempfile as _;
use toml as _;

use fetchloom_cli::surface::DisplayMode;
use fetchloom_cli::terminal::{Streams, resolve_display};

mod support;

use support::FakeEnvironment;

const TERMINAL: Streams = Streams {
    stdout: true,
    stderr: true,
    stdin: true,
};

const PIPED: Streams = Streams {
    stdout: false,
    stderr: false,
    stdin: false,
};

#[test]
fn progress_is_suppressed_when_the_error_stream_is_not_a_terminal() {
    let resolved = resolve_display(
        DisplayMode::Plain,
        false,
        PIPED,
        &FakeEnvironment::default(),
    );
    assert_eq!(resolved.mode, DisplayMode::None);
}

#[test]
fn a_terminal_keeps_the_plain_view() {
    let resolved = resolve_display(
        DisplayMode::Plain,
        false,
        TERMINAL,
        &FakeEnvironment::default(),
    );
    assert_eq!(resolved.mode, DisplayMode::Plain);
    assert!(resolved.reason.is_none());
}

#[test]
fn a_dumb_terminal_is_forced_to_the_plain_view() {
    let resolved = resolve_display(
        DisplayMode::Live,
        false,
        TERMINAL,
        &FakeEnvironment::with(&[("TERM", "dumb")]),
    );
    assert_eq!(resolved.mode, DisplayMode::Plain);
    assert_eq!(resolved.requested, Some(DisplayMode::Live));
    assert!(resolved.reason.is_some());
}

#[test]
fn continuous_integration_is_forced_to_the_plain_view() {
    let resolved = resolve_display(
        DisplayMode::Live,
        false,
        TERMINAL,
        &FakeEnvironment::with(&[("CI", "true")]),
    );
    assert_eq!(resolved.mode, DisplayMode::Plain);
    assert!(resolved.reason.is_some());
}

#[test]
fn the_live_view_falls_back_and_says_why() {
    let resolved = resolve_display(
        DisplayMode::Live,
        false,
        TERMINAL,
        &FakeEnvironment::default(),
    );
    assert_eq!(resolved.mode, DisplayMode::Plain);
    assert_eq!(resolved.requested, Some(DisplayMode::Live));
    assert!(
        resolved.reason.is_some(),
        "a fallback must name what was used and why"
    );
}

#[test]
fn quiet_suppresses_progress_even_on_a_terminal() {
    let resolved = resolve_display(
        DisplayMode::Plain,
        true,
        TERMINAL,
        &FakeEnvironment::default(),
    );
    assert_eq!(resolved.mode, DisplayMode::None);
}

#[test]
fn asking_for_no_progress_is_not_a_degradation() {
    let resolved = resolve_display(
        DisplayMode::None,
        false,
        TERMINAL,
        &FakeEnvironment::default(),
    );
    assert_eq!(resolved.mode, DisplayMode::None);
    assert!(resolved.reason.is_none());
}

#[test]
fn a_prompt_needs_both_input_and_error_to_be_terminals() {
    assert!(TERMINAL.can_prompt());
    assert!(!PIPED.can_prompt());
    assert!(
        !Streams {
            stdout: true,
            stderr: true,
            stdin: false,
        }
        .can_prompt()
    );
    assert!(
        !Streams {
            stdout: true,
            stderr: false,
            stdin: true,
        }
        .can_prompt()
    );
}
