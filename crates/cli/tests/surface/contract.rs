//! Contract tests that drive the real binary and assert what it produces.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::Output;

use crate::support;
use fetchloom_faults::{Reply, Script, TestServer};

use tempfile::TempDir;

fn run(arguments: &[&str]) -> Output {
    let cache = TempDir::new().unwrap();
    support::fetchloom()
        .current_dir(scratch())
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache.path())
        .output()
        .unwrap()
}

fn run_reading_configuration(directory: &Path, arguments: &[&str]) -> Output {
    let cache = TempDir::new().unwrap();
    support::fetchloom_reading_configuration()
        .current_dir(directory)
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache.path())
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
fn a_destination_holding_a_foreign_entry_exits_sixty_and_names_it() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let destination = temporary.path().join("destination");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("nobody-asked-for-this.txt"), b"stray").unwrap();

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
    assert!(
        stderr.contains("nobody-asked-for-this.txt"),
        "the foreign path was not named: {stderr}"
    );
    assert!(
        !destination.join("a.txt").is_file(),
        "the run staged an entry even though it stopped before staging anything"
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
fn get_materializes_a_reference_naming_one_file() {
    let temporary = corpus();
    let source = temporary.path().join("source").join("a.txt");
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        got.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&got.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&got.stdout).expect("the result is JSON");

    assert_eq!(result["entries"].as_u64(), Some(1));
    assert_eq!(result["bytes"].as_u64(), Some(5));
    assert!(
        destination.join("a.txt").is_file(),
        "a reference naming one file materialized {:?}",
        std::fs::read_dir(&destination).map(|entries| entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>())
    );
    assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"hello");

    let verified = run(&["verify", destination.to_str().unwrap(), "--json"]);
    assert_eq!(verified.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        body["tree"].as_str().unwrap(),
        result["tree"].as_str().unwrap()
    );
}

#[test]
fn get_on_a_file_url_names_one_file_the_same_way_a_path_does() {
    let temporary = corpus();
    let source = temporary.path().join("source").join("a.txt");
    let by_path = temporary.path().join("by-path");
    let by_url = temporary.path().join("by-url");

    let url = format!(
        "file:///{}",
        source.display().to_string().replace('\\', "/")
    );
    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        by_path.to_str().unwrap(),
        "--json",
    ]);
    let second = run(&["get", &url, "--output", by_url.to_str().unwrap(), "--json"]);

    assert_eq!(first.status.code(), Some(0));
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let one: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let two: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(one["tree"], two["tree"], "the two spellings disagreed");
}

#[test]
fn a_reference_naming_nothing_fails_as_unresolved_rather_than_as_unreadable() {
    let temporary = corpus();
    let missing = temporary.path().join("source").join("absent.txt");
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        missing.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(got.status.code(), Some(10));
    let body: serde_json::Value = serde_json::from_slice(&got.stdout).unwrap();
    assert_eq!(body["kind"].as_str(), Some("reference.unresolved"));
    let action = body["next_action"].as_str().unwrap();
    assert!(
        !action.contains("readable"),
        "a missing name was reported as a permission problem: {action}"
    );
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
fn a_listing_with_a_link_outside_the_prefix_reports_it_skipped_between_start_and_end() {
    let temporary = TempDir::new().unwrap();
    let server =
        TestServer::start(
            Script::serving(b"object".to_vec()).replying(vec![Reply::Listing {
                format: fetchloom_faults::IndexFormat::GeneratedHtml,
            }]),
        )
        .unwrap();
    let location = format!("{}/set/", server.origin());
    let destination = temporary.path().join("destination");
    let events_path = temporary.path().join("events.ndjson");

    let output = run(&[
        "get",
        &location,
        "--output",
        destination.to_str().unwrap(),
        "--events",
        events_path.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let body = std::fs::read_to_string(&events_path).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let start = events
        .iter()
        .position(|event| event["event"] == "listing.start")
        .expect("no listing.start event");
    let end = events
        .iter()
        .position(|event| event["event"] == "listing.end")
        .expect("no listing.end event");
    let skipped = events
        .iter()
        .position(|event| event["event"] == "listing.skipped")
        .expect("a listing with a link outside the prefix did not report listing.skipped");

    assert!(
        start < skipped && skipped < end,
        "listing.skipped must land between listing.start and listing.end: {events:?}"
    );
    assert_eq!(events[skipped]["count"], 1);
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
    let unexpected: Vec<&serde_json::Value> = degradations
        .iter()
        .filter(|event| {
            let requested = event["requested"].as_str().unwrap_or_default();
            requested != "the mode each file carries"
                && !requested.starts_with("a lock pinning what")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "a run on a healthy volume claimed a degradation it should not have: {unexpected:?}"
    );
    assert_eq!(
        degradations.len(),
        2,
        "a walk of a filesystem tree reads no mode and pins no object, and must say each once:          {degradations:?}"
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

    let output = run_reading_configuration(temporary.path(), &["explain"]);
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
        stdout.starts_with("threads = ") && stdout.contains("(measured)"),
        "a setting chosen by measurement must say the measurement chose it: {stdout}"
    );
}

#[test]
fn a_malformed_configuration_file_stops_the_run() {
    let temporary = TempDir::new().unwrap();
    let named = temporary.path().join("bad.toml");
    std::fs::write(&named, "nonsense = 1\n").unwrap();

    let output =
        run_reading_configuration(scratch(), &["--config", named.to_str().unwrap(), "explain"]);
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
fn the_surface_holds_every_command_the_contract_names() {
    let output = run(&["--help"]);
    let help = String::from_utf8_lossy(&output.stdout);
    for present in [
        "get",
        "init",
        "plan",
        "apply",
        "verify",
        "repair",
        "cache",
        "watch",
        "completions",
        "explain",
        "doctor",
        "why",
    ] {
        assert!(help.contains(present), "{present} is missing from {help}");
    }
}

#[test]
fn every_contracted_command_acts_rather_than_being_a_usage_error() {
    for named in ["doctor", "why", "init", "watch"] {
        let output = run(&[named, "--help"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{named} is present and cannot act: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn every_contracted_global_flag_is_accepted() {
    for flag in ["--color=never", "--no-hints", "--verbose"] {
        let output = run(&["explain", flag]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{flag} was refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_credential_never_reaches_any_stream() {
    let temporary = corpus();
    let source = temporary.path().join("source");
    let events = temporary.path().join("events.ndjson");
    let secret = "super-secret-token-value";

    let output = support::fetchloom()
        .current_dir(scratch())
        .args([
            "get",
            &format!("https://user:{secret}@example.invalid/data.tar?sig={secret}"),
            "--offline",
            "--events",
            events.to_str().unwrap(),
        ])
        .current_dir(temporary.path())
        .env("FETCHLOOM_TOKEN_EXAMPLE_INVALID", secret)
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

#[test]
fn get_over_http_materializes_the_object_it_was_pointed_at() {
    let bytes: Vec<u8> = (0..4096u32)
        .map(|value| u8::try_from(value % 256).unwrap_or(0))
        .collect();
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let temporary = TempDir::new().unwrap();
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        &format!("{}/object.bin", server.origin()),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(
        got.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&got.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&got.stdout).unwrap();
    assert_eq!(result["entries"].as_u64(), Some(1));
    assert_eq!(result["bytes"].as_u64(), Some(4096));
    assert_eq!(
        std::fs::read(destination.join("object.bin")).unwrap(),
        bytes
    );
}

#[test]
fn get_over_http_reports_a_terminal_status_as_a_network_failure() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 404,
        retry_after: None,
    }]))
    .unwrap();
    let temporary = TempDir::new().unwrap();
    let destination = temporary.path().join("destination");

    let got = run(&[
        "get",
        &format!("{}/object.bin", server.origin()),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(got.status.code(), Some(20));
    let body: serde_json::Value = serde_json::from_slice(&got.stdout).unwrap();
    assert_eq!(body["kind"].as_str(), Some("network.status"));
    assert!(
        !destination.exists(),
        "a failed transfer left a destination"
    );
}

#[test]
fn get_over_http_resumes_a_second_run_from_what_the_first_left() {
    let bytes = vec![7u8; 256 * 1024];
    let script = Script::serving(bytes.clone())
        .tagged(vec!["\"one\"".to_owned()])
        .replying(vec![Reply::ClosedMidBody { after: 8 * 1024 }; 5]);
    let server = TestServer::start(script).unwrap();
    let temporary = TempDir::new().unwrap();
    let destination = temporary.path().join("destination");
    let cache = TempDir::new().unwrap();
    let location = format!("{}/object.bin", server.origin());

    let first = support::fetchloom()
        .current_dir(scratch())
        .args(["get", &location, "--output", destination.to_str().unwrap()])
        .env("FETCHLOOM_CACHE_DIR", cache.path())
        .output()
        .unwrap();
    assert_ne!(
        first.status.code(),
        Some(0),
        "the first run did not exit non-zero after its retries were spent"
    );
    assert!(
        !destination.exists(),
        "a destination appeared after a run that never finished a transfer"
    );

    let second = support::fetchloom()
        .current_dir(scratch())
        .args([
            "get",
            &location,
            "--output",
            destination.to_str().unwrap(),
            "--events",
            "-",
        ])
        .env("FETCHLOOM_CACHE_DIR", cache.path())
        .output()
        .unwrap();
    assert_eq!(
        second.status.code(),
        Some(0),
        "second: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let events = String::from_utf8_lossy(&second.stdout);
    let resume = events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "transfer.resume");
    assert!(
        resume.is_some(),
        "the second run reported no transfer.resume event: {events}"
    );
    let resume = resume.unwrap();
    assert_eq!(resume["rung"].as_str(), Some("strong_validator"));
    assert!(
        resume["bytes_kept"].as_u64().unwrap_or(0) > 0,
        "the second run kept nothing from the first: {events}"
    );

    assert_eq!(
        std::fs::read(destination.join("object.bin")).unwrap(),
        bytes
    );
}

fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

#[test]
fn a_failure_says_what_to_do_first_and_names_the_kind_and_the_code_second() {
    let output = run(&["get", "./definitely-missing-path"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('{'))
        .collect();
    assert_eq!(lines.len(), 2, "stderr was {stderr}");
    assert!(
        !lines[0].contains("reference.unresolved"),
        "the first line spent itself on the kind rather than the action: {stderr}"
    );
    assert_eq!(
        lines[1], "reference.unresolved, exit 10",
        "stderr was {stderr}"
    );
}
