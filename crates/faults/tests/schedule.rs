//! What the schedule does to an operation before the platform underneath runs
//! it.

use std::time::{Duration, Instant};

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_faults::{Faults, Operation};

#[test]
fn an_operation_with_no_rule_is_neither_delayed_nor_failed() {
    let faults = Faults::new();
    let started = Instant::now();
    assert!(faults.check(Operation::Flush).is_none());
    assert!(started.elapsed() < Duration::from_millis(50));
}

#[test]
fn a_scheduled_delay_is_charged_before_the_operation_runs() {
    let faults = Faults::new();
    faults.delay(Operation::Flush, Duration::from_millis(120));

    let started = Instant::now();
    assert!(faults.check(Operation::Flush).is_none());
    let waited = started.elapsed();

    assert!(
        waited >= Duration::from_millis(120),
        "a scheduled delay of 120 milliseconds charged {waited:?}"
    );
}

#[test]
fn a_delay_charges_every_call_and_a_failure_still_fails() {
    let faults = Faults::new();
    faults.delay(Operation::Preallocate, Duration::from_millis(40));
    faults.fail(
        Operation::Preallocate,
        1,
        1,
        Error::new(ErrorKind::CacheCorrupt, "the scheduled failure"),
    );

    let started = Instant::now();
    assert!(faults.check(Operation::Preallocate).is_none());
    assert!(faults.check(Operation::Preallocate).is_some());
    let waited = started.elapsed();

    assert!(
        waited >= Duration::from_millis(80),
        "two calls against a 40 millisecond delay charged {waited:?}"
    );
}

#[test]
fn a_delay_on_one_operation_leaves_another_alone() {
    let faults = Faults::new();
    faults.delay(Operation::FreeSpace, Duration::from_millis(200));

    let started = Instant::now();
    assert!(faults.check(Operation::Flush).is_none());
    assert!(started.elapsed() < Duration::from_millis(50));
}
