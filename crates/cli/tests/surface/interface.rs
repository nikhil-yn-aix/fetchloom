//! Contract tests for the flags and commands that make up the interface.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::Output;

use crate::support;

use tempfile::TempDir;

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
    let mut command = support::fetchloom();
    command
        .current_dir(directory)
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache);
    command.output().unwrap()
}

fn events_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn names_of(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| {
            value
                .get("event")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn a_log_level_never_changes_the_event_stream() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));

    let mut streams = Vec::new();
    for (index, level) in ["error", "info", "debug"].into_iter().enumerate() {
        let cache = temporary.path().join(format!("cache{index}"));
        let events = temporary.path().join(format!("events{index}.ndjson"));
        let output = support::fetchloom()
            .current_dir(temporary.path())
            .args([
                "get",
                &source,
                "--output",
                &format!("out{index}"),
                "--events",
                &events.display().to_string(),
            ])
            .env("FETCHLOOM_CACHE_DIR", &cache)
            .env("FETCHLOOM_LOG", level)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{level} did not succeed");
        streams.push(names_of(&events_of(&events)));
    }

    assert_eq!(
        streams[0], streams[1],
        "the error level carries different events from the info level"
    );
    assert_eq!(
        streams[1], streams[2],
        "the debug level carries different events from the info level"
    );
    assert!(
        streams[0].contains(&"run.start".to_owned()),
        "the stream carried no events at all"
    );
}

#[test]
fn the_default_level_renders_the_run_and_debug_renders_every_event() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));

    let quiet = fetch(
        temporary.path(),
        &temporary.path().join("cache-quiet"),
        &["get", &source, "--output", "quiet"],
    );
    let loud = fetch(
        temporary.path(),
        &temporary.path().join("cache-loud"),
        &["get", &source, "--output", "loud", "--verbose"],
    );

    let quiet_lines = String::from_utf8_lossy(&quiet.stderr).lines().count();
    let loud_text = String::from_utf8_lossy(&loud.stderr).into_owned();
    let loud_lines = loud_text.lines().count();

    assert!(
        loud_lines > quiet_lines,
        "--verbose rendered no more than the default: {loud_lines} against {quiet_lines}"
    );
    for named in ["publish.commit", "resolve.start"] {
        assert!(
            loud_text.contains(named),
            "the debug level rendered no {named}: {loud_text}"
        );
    }
}

#[test]
fn a_level_past_the_highest_is_clamped_and_says_so() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));
    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &source,
            "--output",
            "out",
            "--verbose",
            "--verbose",
            "--verbose",
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        text.contains("debug") && text.contains("clamped"),
        "a level past the highest was raised without saying it was clamped: {text}"
    );
}

#[test]
fn the_log_level_variable_names_a_level_and_a_wrong_one_is_refused() {
    let temporary = corpus();
    let source = located(&temporary.path().join("source"));

    let accepted = support::fetchloom()
        .current_dir(temporary.path())
        .args(["get", &source, "--output", "ok"])
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache-a"))
        .env("FETCHLOOM_LOG", "debug")
        .output()
        .unwrap();
    assert_eq!(accepted.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&accepted.stderr).contains("publish.commit"),
        "FETCHLOOM_LOG=debug rendered nothing extra"
    );

    let refused = support::fetchloom()
        .current_dir(temporary.path())
        .args(["get", &source, "--output", "no"])
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache-b"))
        .env("FETCHLOOM_LOG", "chatty")
        .output()
        .unwrap();
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a level this build does not take was accepted"
    );
}

#[test]
fn explain_reports_the_log_level_and_where_it_came_from() {
    let temporary = TempDir::new().unwrap();
    let output = support::fetchloom()
        .current_dir(temporary.path())
        .args(["explain", "log", "--json", "--no-config"])
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
        .env("FETCHLOOM_LOG", "error")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains("error") && text.contains("environment"),
        "explain did not report the log level and its origin: {text}"
    );
}

#[test]
fn the_retry_and_timeout_flags_are_reported_as_effective_settings() {
    let temporary = TempDir::new().unwrap();
    let output = support::fetchloom()
        .current_dir(temporary.path())
        .args(["explain", "--json", "--no-config"])
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    for key in ["retries", "timeout"] {
        assert!(text.contains(key), "explain reports no {key}: {text}");
    }
}

#[test]
fn a_retry_ceiling_bounds_the_attempts_a_transient_failure_makes() {
    let temporary = corpus();
    let mut script = fetchloom_faults::Script::serving(b"the object".to_vec());
    script.then = fetchloom_faults::Reply::Status {
        code: 503,
        retry_after: None,
    };
    let server = fetchloom_faults::TestServer::start(script).unwrap();
    let location = format!("{}/object.bin", server.origin());

    let output = fetch(
        temporary.path(),
        &temporary.path().join("cache"),
        &[
            "get",
            &location,
            "--output",
            "out",
            "--retries",
            "1",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(20));
    let issued = server.received().len();
    assert!(
        issued <= 4,
        "--retries 1 issued {issued} requests where the default five would issue more"
    );
}

#[test]
fn repair_refuses_every_flag_it_could_never_act_on() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    for (flag, value) in [
        ("--output", Some("elsewhere")),
        ("--select", Some("**/*.txt")),
        ("--exclude", Some("**/*.bin")),
        ("--layout", Some("flatten:1")),
        ("--no-extract", None),
        ("--force", None),
        ("--adopt", None),
    ] {
        let mut command = support::fetchloom();
        command
            .current_dir(temporary.path())
            .env("FETCHLOOM_CACHE_DIR", &cache)
            .args(["repair", "https://example.invalid/x.tar", flag]);
        if let Some(value) = value {
            command.arg(value);
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "repair accepted {flag}, which materializes nothing and cannot act on it: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
