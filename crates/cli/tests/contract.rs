//! Contract tests that drive the real binary and assert what it produces.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

use clap as _;
use clap_complete as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use serde as _;
use toml as _;

use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn run(arguments: &[&str]) -> Output {
    Command::new(binary())
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn run_in(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(binary())
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn corpus() -> TempDir {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::create_dir_all(source.join("empty-directory")).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    std::fs::write(source.join("zero.txt"), b"").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"world").unwrap();
    temporary
}

#[test]
fn a_usage_error_exits_two() {
    let output = run(&["definitely-not-a-command"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_reference_that_resolves_to_nothing_exits_ten() {
    let output = run(&["get", "./definitely-missing-path"]);
    assert_eq!(output.status.code(), Some(10));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reference.unresolved"),
        "stderr was {stderr}"
    );
}

#[test]
fn a_network_reference_while_offline_is_a_policy_failure_exiting_forty() {
    let output = run(&["get", "--offline", "https://example.invalid/data.tar"]);
    assert_eq!(output.status.code(), Some(40));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("policy.offline"), "stderr was {stderr}");
}

#[test]
fn a_destination_that_already_exists_exits_sixty() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let destination = temporary.path().join("destination");
    std::fs::create_dir_all(&destination).unwrap();

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(60));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("destination.foreign"),
        "stderr was {stderr}"
    );
}

#[test]
fn a_run_with_nothing_to_do_exits_zero() {
    let output = run(&["explain"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn get_materializes_a_tree_and_verify_reproduces_its_digest() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(got.status.code(), Some(0));
    let result: serde_json::Value =
        serde_json::from_slice(&got.stdout).expect("the result is JSON");
    let materialized = result["tree"].as_str().unwrap().to_owned();

    assert!(destination.join("a.txt").is_file());
    assert!(destination.join("nested").join("b.txt").is_file());
    assert!(destination.join("empty-directory").is_dir());
    assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"hello");

    let verified = run(&["verify", destination.to_str().unwrap(), "--json"]);
    assert_eq!(verified.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(body["tree"].as_str().unwrap(), materialized);
}

#[test]
fn the_source_and_the_destination_have_the_same_tree_digest() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(got.status.code(), Some(0));

    let of_source = run(&["verify", source.to_str().unwrap(), "--json"]);
    let of_destination = run(&["verify", destination.to_str().unwrap(), "--json"]);
    let source_tree: serde_json::Value = serde_json::from_slice(&of_source.stdout).unwrap();
    let destination_tree: serde_json::Value =
        serde_json::from_slice(&of_destination.stdout).unwrap();
    assert_eq!(source_tree["tree"], destination_tree["tree"]);
}

#[test]
fn a_repeated_run_produces_the_same_tree_digest() {
    let temporary = corpus();
    let source = temporary.path().join("source");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        temporary.path().join("one").to_str().unwrap(),
        "--json",
    ]);
    let second = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        temporary.path().join("two").to_str().unwrap(),
        "--json",
    ]);
    let one: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let two: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(one["tree"], two["tree"]);
}

#[test]
fn a_failed_run_publishes_nothing() {
    let temporary = corpus();
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        temporary.path().join("missing").to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_ne!(output.status.code(), Some(0));
    assert!(!destination.exists(), "the destination must not exist");
}

#[test]
fn progress_is_not_written_when_the_error_stream_is_not_a_terminal() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        temporary.path().join("destination").to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("entries"),
        "progress reached a stream that is not a terminal: {stderr}"
    );
}

#[test]
fn the_event_stream_is_newline_delimited_json_with_a_monotonic_sequence() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let events = temporary.path().join("events.ndjson");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        temporary.path().join("destination").to_str().unwrap(),
        "--events",
        events.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0));

    let body = std::fs::read_to_string(&events).unwrap();
    let mut previous = None;
    let mut names = Vec::new();
    for line in body.lines() {
        let event: serde_json::Value = serde_json::from_str(line).expect("every line is JSON");
        let sequence = event["seq"].as_u64().unwrap();
        if let Some(previous) = previous {
            assert!(sequence > previous, "the sequence must increase");
        }
        previous = Some(sequence);
        names.push(event["event"].as_str().unwrap().to_owned());
    }
    assert!(names.contains(&"run.start".to_owned()));
    assert!(names.contains(&"publish.commit".to_owned()));
    assert!(names.contains(&"run.end".to_owned()));
}

#[test]
fn a_fallback_is_reported_as_a_degradation_naming_what_was_used() {
    let temporary = corpus();
    let events = temporary.path().join("events.ndjson");

    run(&[
        "verify",
        temporary.path().join("source").to_str().unwrap(),
        "--display",
        "live",
        "--events",
        events.to_str().unwrap(),
    ]);

    let body = std::fs::read_to_string(&events).unwrap();
    let degrade = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "degrade")
        .expect("a view this build does not render must emit degrade");

    assert!(
        degrade["requested"]
            .as_str()
            .is_some_and(|it| !it.is_empty()),
        "a degradation must name what was requested: {degrade}"
    );
    assert!(
        degrade["used"].as_str().is_some_and(|it| !it.is_empty()),
        "a degradation must name what was used: {degrade}"
    );
    assert!(
        degrade["reason"].as_str().is_some_and(|it| !it.is_empty()),
        "a degradation must name why: {degrade}"
    );
}

#[test]
fn a_run_that_degrades_nothing_reports_no_degradation() {
    let temporary = corpus();
    let events = temporary.path().join("events.ndjson");

    let output = run(&[
        "get",
        temporary.path().join("source").to_str().unwrap(),
        "--output",
        temporary.path().join("destination").to_str().unwrap(),
        "--events",
        events.to_str().unwrap(),
        "--no-cache",
    ]);
    assert_eq!(output.status.code(), Some(0));

    let body = std::fs::read_to_string(&events).unwrap();
    let degradations: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "degrade")
        .collect();
    assert!(
        degradations.is_empty(),
        "a run on a healthy volume claimed a degradation: {degradations:?}"
    );
}

#[test]
fn no_display_mode_changes_the_result() {
    let temporary = corpus();
    let source = temporary.path().join("source");

    let mut digests = Vec::new();
    for (index, mode) in ["plain", "none"].iter().enumerate() {
        let destination = temporary.path().join(format!("destination{index}"));
        let output = run(&[
            "get",
            source.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
            "--display",
            mode,
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(0));
        let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        digests.push(body["tree"].as_str().unwrap().to_owned());
    }
    assert_eq!(digests[0], digests[1]);
}

#[test]
fn explain_reports_the_level_that_supplied_each_value() {
    let temporary = TempDir::new().unwrap();
    std::fs::write(temporary.path().join("fetchloom.toml"), "threads = 3\n").unwrap();

    let output = run_in(temporary.path(), &["explain"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project config"), "stdout was {stdout}");
    assert!(stdout.contains("default"), "stdout was {stdout}");
    assert!(stdout.contains("fetchloom.toml"), "stdout was {stdout}");
}

#[test]
fn explain_reports_one_named_setting() {
    let output = run(&["explain", "threads"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("threads = ? (default)"),
        "a value nothing supplied must be shown as a question mark: {stdout}"
    );
}

#[test]
fn a_malformed_configuration_file_stops_the_run() {
    let temporary = TempDir::new().unwrap();
    let named = temporary.path().join("bad.toml");
    std::fs::write(&named, "nonsense = 1\n").unwrap();

    let output = run(&["--config", named.to_str().unwrap(), "explain"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn completions_are_written_for_every_shell() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let output = run(&["completions", shell]);
        assert_eq!(output.status.code(), Some(0), "{shell} did not succeed");
        assert!(
            !output.stdout.is_empty(),
            "{shell} produced no completion script"
        );
    }
}

#[test]
fn the_surface_holds_exactly_the_commands_this_build_performs() {
    let output = run(&["--help"]);
    let help = String::from_utf8_lossy(&output.stdout);
    for present in ["get", "verify", "completions", "explain", "cache"] {
        assert!(help.contains(present), "{present} is missing from {help}");
    }
    for absent in ["init", "plan", "apply", "repair", "watch", "doctor", "why"] {
        assert!(
            !help.contains(absent),
            "{absent} is in the surface and performs nothing: {help}"
        );
    }
}

#[test]
fn a_command_this_build_does_not_perform_is_not_accepted() {
    for absent in ["init", "plan", "apply", "repair", "watch", "doctor", "why"] {
        let output = run(&[absent]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{absent} was accepted by the parser"
        );
    }
}

#[test]
fn a_flag_this_build_does_not_act_on_is_not_accepted() {
    for absent in ["--verbose", "--color=never", "--no-hints"] {
        let output = run(&["explain", absent]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{absent} was accepted and does nothing"
        );
    }
}

#[test]
fn a_credential_never_reaches_any_stream() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let events = temporary.path().join("events.ndjson");
    let secret = "super-secret-token-value";

    let output = Command::new(binary())
        .args([
            "get",
            &format!("https://user:{secret}@example.invalid/data.tar?sig={secret}"),
            "--offline",
            "--events",
            events.to_str().unwrap(),
        ])
        .current_dir(temporary.path())
        .env("FETCHLOOM_TOKEN_EXAMPLE_INVALID", secret)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let body = std::fs::read_to_string(&events).unwrap_or_default();

    assert!(!stdout.contains(secret), "the secret reached stdout");
    assert!(!stderr.contains(secret), "the secret reached stderr");
    assert!(
        !body.contains(secret),
        "the secret reached the event stream"
    );
    let _ = source;
}
