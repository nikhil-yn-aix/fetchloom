//! `plan` and `apply`, called as functions.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::path::Path;

use fetchloom_cli::command::get::run_get;
use fetchloom_cli::command::plan::{run_apply, run_plan};
use fetchloom_engine::outcome::ExitCode;
use tempfile::TempDir;

use crate::driver::Driver;

fn corpus() -> TempDir {
    let temporary = TempDir::new().unwrap();
    std::fs::write(temporary.path().join("one.bin"), b"bytes").unwrap();
    temporary
}

fn plan_of(temporary: &TempDir, extra: &[&str]) -> ExitCode {
    let object = temporary.path().join("one.bin");
    let reference = object.to_str().unwrap().to_owned();
    let cache = temporary.path().join("cache");
    let destination = temporary.path().join("destination");
    let lock = temporary.path().join("fetchloom.lock");
    let mut argv = vec![
        "--cache-dir",
        cache.to_str().unwrap(),
        "plan",
        reference.as_str(),
        "--output",
        destination.to_str().unwrap(),
        "--lock",
        lock.to_str().unwrap(),
    ];
    argv.extend_from_slice(extra);
    let driver = Driver::of(&argv);
    let transfer = driver.transfer();
    run_plan(
        &reference,
        &transfer,
        &driver.parsed,
        &driver.resolved,
        &driver.observer,
        &driver.sequence,
    )
}

fn get_once(temporary: &TempDir) {
    let object = temporary.path().join("one.bin");
    let reference = object.to_str().unwrap().to_owned();
    let cache = temporary.path().join("cache");
    let destination = temporary.path().join("destination");
    let lock = temporary.path().join("fetchloom.lock");
    let driver = Driver::of(&[
        "--cache-dir",
        cache.to_str().unwrap(),
        "get",
        reference.as_str(),
        "--output",
        destination.to_str().unwrap(),
        "--lock",
        lock.to_str().unwrap(),
    ]);
    let transfer = driver.transfer();
    let code = run_get(
        &reference,
        &transfer,
        &driver.parsed,
        &driver.resolved,
        &driver.observer,
        &driver.sequence,
    );
    assert_eq!(code, ExitCode::Success);
}

fn apply_of(temporary: &TempDir, plan_path: &Path) -> ExitCode {
    let cache = temporary.path().join("cache");
    let driver = Driver::of(&[
        "--cache-dir",
        cache.to_str().unwrap(),
        "apply",
        plan_path.to_str().unwrap(),
    ]);
    let transfer = driver.transfer();
    run_apply(
        plan_path,
        &transfer,
        &driver.parsed,
        &driver.resolved,
        &driver.observer,
        &driver.sequence,
    )
}

#[test]
fn a_plan_before_any_run_is_refused_because_nothing_is_pinned_yet() {
    let temporary = corpus();
    assert_ne!(
        plan_of(&temporary, &[]),
        ExitCode::Success,
        "a plan states resolved digests and moves no bytes to learn one"
    );
    assert!(
        !temporary.path().join("destination").exists(),
        "a refused plan writes to no destination"
    );
}

#[test]
fn a_plan_after_a_run_states_what_the_lock_pinned() {
    let temporary = corpus();
    get_once(&temporary);
    std::fs::remove_dir_all(temporary.path().join("destination")).unwrap();
    assert_eq!(plan_of(&temporary, &[]), ExitCode::Success);
    assert!(
        !temporary.path().join("destination").exists(),
        "a plan states what a run would do and writes to no destination"
    );
}

#[test]
fn force_is_refused_on_plan_because_a_plan_touches_no_destination() {
    let temporary = corpus();
    get_once(&temporary);
    assert_eq!(plan_of(&temporary, &["--force"]), ExitCode::Usage);
}

#[test]
fn adopt_is_refused_on_plan_because_a_plan_touches_no_destination() {
    let temporary = corpus();
    get_once(&temporary);
    assert_eq!(plan_of(&temporary, &["--adopt"]), ExitCode::Usage);
}

#[test]
fn applying_a_file_that_is_not_a_plan_fails_and_writes_no_destination() {
    let temporary = corpus();
    let not_a_plan = temporary.path().join("not.plan");
    std::fs::write(&not_a_plan, b"this is not a plan").unwrap();
    assert_eq!(
        apply_of(&temporary, &not_a_plan),
        ExitCode::Resolution,
        "a file that is not a plan is a document that did not resolve"
    );
    assert!(
        !temporary.path().join("destination").exists(),
        "a plan that could not be read materializes nothing"
    );
}

#[test]
fn applying_a_plan_that_is_not_there_fails() {
    let temporary = corpus();
    let missing = temporary.path().join("missing.plan");
    assert_eq!(
        apply_of(&temporary, &missing),
        ExitCode::Resolution,
        "a plan that is not there is a reference that did not resolve"
    );
}
