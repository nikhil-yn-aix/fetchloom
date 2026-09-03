//! What decides whether output carries color.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

mod support;

use tempfile::TempDir;

/// The byte every styled run writes and every plain one does not.
const ESCAPE: &str = "\u{1b}[";

fn failing_run(arguments: &[&str], environment: &[(&str, &str)]) -> String {
    let temporary = TempDir::new().unwrap();
    let mut command = support::fetchloom();
    command
        .arg("get")
        .arg("nothing-is-here")
        .arg("--output")
        .arg(temporary.path().join("out"))
        .arg("--no-config")
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"));
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    assert!(!output.status.success(), "the run was supposed to fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn color_always_styles_and_color_never_does_not() {
    assert!(
        failing_run(&["--color", "always"], &[]).contains(ESCAPE),
        "--color always wrote no styling at all"
    );
    assert!(
        !failing_run(&["--color", "never"], &[]).contains(ESCAPE),
        "--color never wrote styling"
    );
}

#[test]
fn no_color_strips_styling_and_a_flag_still_overrides_it() {
    assert!(
        !failing_run(&[], &[("NO_COLOR", "1")]).contains(ESCAPE),
        "NO_COLOR was not honored"
    );
    assert!(
        failing_run(&["--color", "always"], &[("NO_COLOR", "1")]).contains(ESCAPE),
        "--color always did not override NO_COLOR, which the precedence order gives it"
    );
}

#[test]
fn a_stream_that_is_not_a_terminal_carries_no_styling() {
    assert!(
        !failing_run(&[], &[]).contains(ESCAPE),
        "a run whose stderr is a pipe wrote styling"
    );
}

#[test]
fn the_configuration_file_decides_color_when_no_flag_does() {
    let temporary = TempDir::new().unwrap();
    let config = temporary.path().join("fetchloom.toml");
    std::fs::write(&config, "color = \"always\"\n").unwrap();

    let output = support::fetchloom_reading_configuration()
        .arg("get")
        .arg("nothing-is-here")
        .arg("--output")
        .arg(temporary.path().join("out"))
        .arg("--config")
        .arg(&config)
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(ESCAPE),
        "the color setting in a configuration file did nothing"
    );
}
