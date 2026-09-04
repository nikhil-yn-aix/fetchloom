//! `get`, called as a function, and what it says about the lock it did not write.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use fetchloom_cli::command::get::run_get;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_faults::{Script, TestServer};
use tempfile::TempDir;

use crate::driver::Driver;

fn tree() -> TempDir {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"world").unwrap();
    temporary
}

fn run(reference: &str, temporary: &TempDir, extra: &[&str]) -> (ExitCode, Driver) {
    let destination = temporary.path().join("destination");
    let lock = temporary.path().join("fetchloom.lock");
    let cache = temporary.path().join("cache");
    let mut arguments = vec![
        "--cache-dir",
        cache.to_str().unwrap(),
        "get",
        reference,
        "--output",
        destination.to_str().unwrap(),
        "--lock",
        lock.to_str().unwrap(),
    ];
    arguments.extend_from_slice(extra);
    let driver = Driver::of(&arguments);
    let transfer = driver.transfer();
    let code = run_get(
        reference,
        &transfer,
        &driver.parsed,
        &driver.resolved,
        &driver.observer,
        &driver.sequence,
    );
    (code, driver)
}

#[test]
fn a_directory_reference_says_it_pinned_nothing_because_it_resolved_to_a_tree() {
    let temporary = tree();
    let source = temporary.path().join("source");
    let (code, driver) = run(source.to_str().unwrap(), &temporary, &[]);
    assert_eq!(code, ExitCode::Success);

    let degradation = driver
        .lock_degradation()
        .expect("a run that pins no object must say so");
    assert_eq!(degradation.used, "no lock entry at all");
    assert!(
        degradation.reason.contains("resolves to a tree"),
        "a directory resolves to a tree, and the reason must say that: {}",
        degradation.reason
    );
}

#[test]
fn a_failing_get_gives_up_no_lock_entry_and_states_no_reason_for_one() {
    let temporary = tree();
    let broken = temporary.path().join("broken.tar.gz");
    std::fs::write(&broken, b"this is not gzip").unwrap();
    let (code, driver) = run(broken.to_str().unwrap(), &temporary, &[]);
    assert_ne!(code, ExitCode::Success);

    assert_eq!(
        driver
            .lock_degradation()
            .map(|degradation| degradation.reason),
        None,
        "a run that failed gave up nothing the failure it already reports does not say, and a \
         reason naming another cause sends a reader to the wrong place"
    );
}

#[test]
fn an_object_reference_writes_a_lock_entry_and_degrades_nothing_about_it() {
    let temporary = tree();
    let object = temporary.path().join("one.bin");
    std::fs::write(&object, b"bytes").unwrap();
    let (code, driver) = run(object.to_str().unwrap(), &temporary, &[]);
    assert_eq!(code, ExitCode::Success);

    assert!(
        driver.lock_degradation().is_none(),
        "an object resolves to an object, so nothing about the lock was given up"
    );
    assert!(
        temporary.path().join("fetchloom.lock").exists(),
        "an unlocked run that resolved an object records what it resolved"
    );
}

#[test]
fn an_object_whose_interop_digest_the_cache_lost_says_that_is_why_it_pinned_nothing() {
    let temporary = tree();
    let bytes = vec![7u8; 2 * 1024 * 1024];
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let reference = format!("{}/large.bin", server.origin());

    let (code, _first) = run(&reference, &temporary, &[]);
    assert_eq!(code, ExitCode::Success);

    let records = temporary.path().join("cache").join("meta").join("object");
    let mut removed = 0;
    for entry in std::fs::read_dir(&records).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
        removed += 1;
    }
    assert!(
        removed > 0,
        "an object above the pack threshold is stored loose and recorded"
    );
    std::fs::remove_dir_all(temporary.path().join("destination")).unwrap();

    let (code, second) = run(&reference, &temporary, &["--verify", "never", "--locked"]);
    assert_eq!(code, ExitCode::Success);
    let degradation = second
        .lock_degradation()
        .expect("a run that pinned nothing must say why");
    assert!(
        degradation.reason.contains("interop digest"),
        "the cache lost the interop digest, and that is the reason to state: {}",
        degradation.reason
    );
}
