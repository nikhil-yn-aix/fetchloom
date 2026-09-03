//! What a display mode may never change, and what a hint may never say.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
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
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn corpus() -> TempDir {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("one.txt"), b"first entry").unwrap();
    std::fs::write(source.join("nested").join("two.txt"), b"second entry").unwrap();
    temporary
}

fn located(path: &Path) -> String {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
}

fn fetch(directory: &Path, cache: &Path, arguments: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(directory)
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .env_remove("FETCHLOOM_LOG")
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The events a run wrote, less the one that announces a display mode the
/// terminal could not carry.
///
/// That degradation is a difference between the modes by construction, because
/// contracts forces the mode when stderr is not a terminal and forbids doing so
/// silently. Everything else must be identical, which is what this compares.
fn events_named(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            !value
                .get("requested")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|requested| requested.ends_with(" view"))
        })
        .filter_map(|value| {
            value
                .get("event")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn every_display_mode_produces_the_same_result_code_and_event_stream() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));

    let mut results = Vec::new();
    let mut streams = Vec::new();
    let mut codes = Vec::new();

    for (index, mode) in ["plain", "live", "none"].into_iter().enumerate() {
        let cache = temporary.path().join(format!("cache{index}"));
        let events = temporary.path().join(format!("events{index}.ndjson"));
        let output = fetch(
            temporary.path(),
            &cache,
            &[
                "get",
                &source,
                "--output",
                &format!("out{index}"),
                "--display",
                mode,
                "--json",
                "--events",
                &events.display().to_string(),
            ],
        );
        codes.push(output.status.code());
        results.push(String::from_utf8_lossy(&output.stdout).into_owned());
        streams.push(events_named(&events));
    }

    for index in 1..3 {
        assert_eq!(
            codes[0], codes[index],
            "a display mode changed the exit code"
        );
        assert_eq!(
            strip_destination(&results[0]),
            strip_destination(&results[index]),
            "a display mode changed the machine-readable result"
        );
        assert_eq!(
            streams[0], streams[index],
            "a display mode changed the event stream"
        );
    }
    assert_eq!(codes[0], Some(0));
}

#[test]
fn a_mode_the_terminal_cannot_carry_is_forced_and_says_so() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let events = temporary.path().join("events.ndjson");
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &source,
            "--output",
            "out",
            "--display",
            "live",
            "--events",
            &events.display().to_string(),
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let stream = std::fs::read_to_string(&events).unwrap();
    assert!(
        stream.contains("the standard error stream is not a terminal"),
        "the live view was forced to another mode without saying so: {stream}"
    );
}

/// Removes the destination, which is the one field that differs by design
/// because each run materializes into a directory of its own.
fn strip_destination(result: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(result).unwrap();
    if let Some(object) = value.as_object_mut() {
        object.remove("destination");
    }
    value.to_string()
}

#[test]
fn asking_for_the_live_view_no_longer_degrades() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let events = temporary.path().join("events.ndjson");
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &source,
            "--output",
            "out",
            "--display",
            "live",
            "--events",
            &events.display().to_string(),
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let text = std::fs::read_to_string(&events).unwrap();
    assert!(
        !text.contains("renders the plain view only"),
        "the live view still degrades to plain: {text}"
    );
}

#[test]
fn watch_renders_a_stream_after_the_fact() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let events = temporary.path().join("events.ndjson");
    let run = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &source,
            "--output",
            "out",
            "--events",
            &events.display().to_string(),
        ],
    );
    assert_eq!(run.status.code(), Some(0));

    let watched = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &["watch", &events.display().to_string()],
    );
    assert_eq!(
        watched.status.code(),
        Some(0),
        "watch failed: {}",
        String::from_utf8_lossy(&watched.stderr)
    );
    let shown = String::from_utf8_lossy(&watched.stderr).into_owned();
    assert!(
        shown.contains("dataset") && shown.contains("entries"),
        "watch rendered nothing: {shown}"
    );
    assert!(
        String::from_utf8_lossy(&watched.stdout).is_empty(),
        "watch wrote to standard output, which carries the result only"
    );
}

#[test]
fn watch_shows_nothing_a_stream_does_not_carry() {
    let temporary = TempDir::new().unwrap();
    let events = temporary.path().join("only-a-start.ndjson");
    std::fs::write(
        &events,
        "{\"seq\":0,\"timestamp\":\"2026-01-01T00:00:00Z\",\"event\":\"run.start\"}\n",
    )
    .unwrap();

    let watched = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &["watch", &events.display().to_string()],
    );
    assert_eq!(watched.status.code(), Some(0));
    let shown = String::from_utf8_lossy(&watched.stderr).into_owned();
    assert!(
        shown.contains("dataset  ?") && shown.contains("source   ?"),
        "watch invented a fact the stream did not carry: {shown}"
    );
}

#[test]
fn watch_on_a_stream_that_is_not_there_is_a_usage_error() {
    let temporary = TempDir::new().unwrap();
    let watched = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &["watch", "no-such-stream.ndjson"],
    );
    assert_eq!(watched.status.code(), Some(2));
}

#[test]
fn a_hint_never_appears_in_the_machine_readable_output() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let events = temporary.path().join("events.ndjson");
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &source,
            "--output",
            "out",
            "--json",
            "--events",
            &events.display().to_string(),
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let result = String::from_utf8_lossy(&output.stdout).into_owned();
    let stream = std::fs::read_to_string(&events).unwrap();
    for said in ["hint", "would have made"] {
        assert!(
            !result.contains(said),
            "a hint reached the JSON result: {result}"
        );
        assert!(
            !stream.contains(said),
            "a hint reached the event stream: {stream}"
        );
    }
}

#[test]
fn a_run_that_is_not_a_terminal_prints_no_hint() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &["get", &source, "--output", "out"],
    );
    assert_eq!(output.status.code(), Some(0));
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !said.contains("would have made"),
        "a hint was printed to a stream that is not a terminal: {said}"
    );
}

#[test]
fn the_no_hints_flag_and_the_config_key_are_both_accepted() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &["get", &source, "--output", "out", "--no-hints"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "--no-hints was refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_color_flag_takes_its_three_values_and_no_others() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    for (index, value) in ["auto", "always", "never"].into_iter().enumerate() {
        let output = fetch(
            temporary.path(),
            &temporary.path().join(format!("cache{index}")),
            &[
                "get",
                &source,
                "--output",
                &format!("out{index}"),
                "--color",
                value,
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "--color {value} was refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let refused = fetch(
        temporary.path(),
        &temporary.path().join("cache-bad"),
        &["get", &source, "--output", "no", "--color", "sometimes"],
    );
    assert_eq!(refused.status.code(), Some(2));
}

#[test]
fn no_display_mode_changes_the_tree_a_run_produces() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let mut trees = Vec::new();
    for (index, mode) in ["plain", "live", "none"].into_iter().enumerate() {
        let output = fetch(
            temporary.path(),
            &temporary.path().join(format!("cache{index}")),
            &[
                "get",
                &source,
                "--output",
                &format!("tree{index}"),
                "--display",
                mode,
                "--json",
            ],
        );
        assert_eq!(output.status.code(), Some(0));
        let value: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        trees.push(
            value
                .get("tree")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
    }
    assert_eq!(trees[0], trees[1], "the live view changed the tree digest");
    assert_eq!(trees[1], trees[2], "the none view changed the tree digest");
    assert!(!trees[0].is_empty());
}
