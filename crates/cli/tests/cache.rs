//! Contract tests over the cache commands and over a run that uses one.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

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
use flate2 as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn run(cache: &Path, arguments: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn source(under: &Path) -> std::path::PathBuf {
    let source = under.join("source");
    std::fs::create_dir_all(source.join("inner")).unwrap();
    std::fs::write(source.join("one.txt"), b"the first file").unwrap();
    std::fs::write(source.join("inner").join("two.txt"), b"the second file").unwrap();
    source
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn events(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
}

fn recorded(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_second_identical_run_is_a_cache_hit() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());

    let first_events = scratch.path().join("first.ndjson");
    let first = run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("first").to_str().unwrap(),
            "--events",
            first_events.to_str().unwrap(),
        ],
    );
    assert!(first.status.success(), "{}", events(&first));
    assert!(
        recorded(&first_events).contains("cache.miss"),
        "the first run did not miss: {}",
        recorded(&first_events)
    );

    let second_events = scratch.path().join("second.ndjson");
    let second = run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("second").to_str().unwrap(),
            "--events",
            second_events.to_str().unwrap(),
        ],
    );
    assert!(second.status.success(), "{}", events(&second));
    assert!(
        recorded(&second_events).contains("cache.hit"),
        "the second run did not hit: {}",
        recorded(&second_events)
    );
    assert!(
        !recorded(&second_events).contains("cache.miss"),
        "the second run transferred into the cache again: {}",
        recorded(&second_events)
    );
    assert_eq!(
        text(&first).split_whitespace().next(),
        text(&second).split_whitespace().next(),
        "two runs of one source produced two tree digests"
    );
}

#[test]
fn a_run_asked_for_no_cache_retains_nothing() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());

    let done = run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
            "--no-cache",
        ],
    );
    assert!(done.status.success());
    assert!(
        !cache.exists(),
        "a run asked for no cache created one anyway"
    );
}

#[test]
fn status_counts_what_a_run_put_in_the_cache() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());

    run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
        ],
    );
    let status = run(&cache, &["cache", "status", "--json"]);
    assert!(status.status.success());
    let body: serde_json::Value = serde_json::from_str(text(&status).trim()).unwrap();
    assert_eq!(body["objects"], 2);
    assert_eq!(body["quarantined"], 0);
}

#[test]
fn ls_prints_the_digests_pin_takes() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());

    run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
        ],
    );
    let listed = run(&cache, &["cache", "ls"]);
    let first = text(&listed)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next().map(str::to_owned))
        .expect("cache ls printed nothing");

    let pinned = run(&cache, &["cache", "pin", &first]);
    assert!(pinned.status.success(), "{}", events(&pinned));

    let pruned = run(&cache, &["cache", "prune", "--json"]);
    assert!(pruned.status.success());
    let again = run(&cache, &["cache", "ls"]);
    assert!(
        text(&again).contains(&first),
        "a pinned object was not listed after a prune"
    );

    let unpinned = run(&cache, &["cache", "unpin", &first]);
    assert!(unpinned.status.success(), "{}", events(&unpinned));
}

#[test]
fn pin_refuses_something_that_is_not_a_digest() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let refused = run(&cache, &["cache", "pin", "not-a-digest"]);
    assert_eq!(refused.status.code(), Some(80));
}

#[test]
fn clear_refuses_to_act_without_an_answer() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());
    run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
        ],
    );

    let refused = run(&cache, &["cache", "clear"]);
    assert_eq!(refused.status.code(), Some(40));
    assert!(
        cache.join("objects").exists(),
        "clear removed the cache anyway"
    );

    let cleared = run(&cache, &["cache", "clear", "--yes"]);
    assert!(cleared.status.success(), "{}", events(&cleared));
    assert!(!cache.exists(), "clear left the cache behind");
}

#[test]
fn a_format_written_by_another_build_stops_a_run() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());
    run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("first").to_str().unwrap(),
        ],
    );
    std::fs::write(
        cache.join("format"),
        "blake3:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();

    let refused = run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("second").to_str().unwrap(),
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(80),
        "a format mismatch did not stop the run: {}",
        events(&refused)
    );
    assert!(
        events(&refused).contains("cache clear"),
        "the failure does not name the fix: {}",
        events(&refused)
    );
    assert!(
        !scratch.path().join("second").exists(),
        "a run that failed on its cache materialized anyway"
    );
}

#[test]
fn a_cache_that_cannot_be_created_degrades_and_the_run_completes() {
    let scratch = TempDir::new().unwrap();
    let blocker = scratch.path().join("blocked");
    std::fs::write(&blocker, b"a file where a cache directory would go").unwrap();
    let source = source(scratch.path());

    let done = run(
        &blocker,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
            "--events",
            "-",
        ],
    );
    assert!(
        done.status.success(),
        "an unusable cache stopped the run: {}",
        events(&done)
    );
    assert!(
        events(&done).contains("degrade"),
        "an unusable cache did not say so: {}",
        events(&done)
    );
    assert!(scratch.path().join("out").exists());
}

#[test]
fn verify_quarantines_an_object_that_changed_on_disk() {
    let scratch = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let source = source(scratch.path());
    run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            scratch.path().join("out").to_str().unwrap(),
        ],
    );

    let object = std::fs::read_dir(cache.join("objects"))
        .unwrap()
        .flatten()
        .next()
        .expect("the cache holds no object")
        .path();
    let mut permissions = std::fs::metadata(&object).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "damaging an object needs the permissions the platform gives a new file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&object, permissions).unwrap();
    std::fs::write(&object, b"something else entirely").unwrap();

    let verified = run(&cache, &["cache", "verify", "--json"]);
    assert_eq!(verified.status.code(), Some(80));
    let body: serde_json::Value = serde_json::from_str(text(&verified).trim()).unwrap();
    assert_eq!(body["quarantined"].as_array().map(Vec::len), Some(1));
    let quarantined: Vec<String> = std::fs::read_dir(cache.join("quarantine"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let name = object.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        quarantined.contains(&name),
        "the object was not quarantined: {quarantined:?}"
    );
    assert!(
        quarantined.contains(&format!("{name}.diagnosis")),
        "no diagnosis travelled beside the quarantined object: {quarantined:?}"
    );

    let diagnosis: serde_json::Value = serde_json::from_slice(
        &std::fs::read(cache.join("quarantine").join(format!("{name}.diagnosis"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        diagnosis["localized"], "no_tree_stored",
        "an object below the outboard threshold has no tree, and the diagnosis has to say so          rather than leaving an empty damaged list to be read as no damage"
    );
    assert!(
        diagnosis["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("repair")),
        "the diagnosis does not name the command that fetches the bytes again"
    );
}

#[test]
fn a_cache_that_fills_part_way_through_degrades_and_the_run_completes() {
    let named = std::env::var_os("FETCHLOOM_TEST_SMALL_VOLUMES");
    assert!(
        !(named.is_none() && std::env::var_os("FETCHLOOM_VERIFY_VOLUMES").is_some()),
        "FETCHLOOM_TEST_SMALL_VOLUMES is unset in a verification run that builds this filesystem"
    );
    let Some(small) = named.and_then(|named| std::env::split_paths(&named).next()) else {
        return;
    };
    let scratch = TempDir::new_in(&small).unwrap();
    let elsewhere = TempDir::new().unwrap();
    let cache = scratch.path().join("cache");

    let source = elsewhere.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    for index in 0..64 {
        std::fs::write(
            source.join(format!("file-{index}.bin")),
            vec![b'a'; 1 << 20],
        )
        .unwrap();
    }

    let done = run(
        &cache,
        &[
            "get",
            source.to_str().unwrap(),
            "--output",
            elsewhere.path().join("out").to_str().unwrap(),
            "--events",
            "-",
        ],
    );
    assert!(
        done.status.success(),
        "a cache that filled stopped the run: {}",
        events(&done)
    );
    assert!(
        events(&done).contains("degrade"),
        "a cache that filled did not say so: {}",
        events(&done)
    );
    assert!(
        elsewhere.path().join("out").exists(),
        "the destination was not materialized"
    );
}

/// The directory every command in this file runs in.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}
