//! What the tuning decisions do, stated as the contract states them.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::tuning::{
    Answer, Ceilings, Controller, FIRST_PER_HOST, TRANSFERS_CEILING, debt,
};

fn budget(threads: usize) -> ThreadBudget {
    ThreadBudget::resolve(NonZeroUsize::new(threads).unwrap(), None)
}

fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

#[test]
fn the_global_ceiling_is_the_thread_budget_bounded_by_the_transfers_ceiling() {
    let small = Ceilings::resolve(budget(2), &Limits::default(), None, None, false);
    let large = Ceilings::resolve(budget(64), &Limits::default(), None, None, false);
    assert_eq!(small.global.get(), 2);
    assert_eq!(large.global.get(), TRANSFERS_CEILING);
}

#[test]
fn politeness_bounds_one_host_and_only_aggressive_raises_it() {
    let limits = Limits::default();
    let polite = Ceilings::resolve(budget(64), &limits, None, None, false);
    let raised = Ceilings::resolve(budget(64), &limits, None, None, true);
    assert_eq!(
        polite.per_host.get(),
        u32::try_from(limits.connections_per_host).unwrap()
    );
    assert_eq!(raised.per_host.get(), raised.global.get());
}

#[test]
fn a_user_ceiling_never_raises_a_politeness_ceiling() {
    let limits = Limits::default();
    let asked = Ceilings::resolve(budget(64), &limits, None, Some(count(100)), false);
    assert_eq!(
        asked.per_host.get(),
        u32::try_from(limits.connections_per_host).unwrap(),
        "a per-host ceiling above politeness was honored"
    );
}

#[test]
fn a_user_ceiling_below_what_was_measured_is_what_is_used() {
    let asked = Ceilings::resolve(budget(64), &Limits::default(), Some(count(1)), None, false);
    assert_eq!(asked.global.get(), 1);
    assert_eq!(asked.per_host.get(), 1);
}

#[test]
fn a_host_nothing_is_recorded_for_starts_at_the_first_count() {
    let controller = Controller::start(None, count(4));
    assert_eq!(controller.permitted(), FIRST_PER_HOST);
}

#[test]
fn a_recorded_count_is_where_the_next_run_starts_and_never_passes_the_ceiling() {
    assert_eq!(Controller::start(Some(3), count(4)).permitted(), 3);
    assert_eq!(Controller::start(Some(99), count(4)).permitted(), 4);
    assert_eq!(Controller::start(Some(0), count(4)).permitted(), 1);
}

#[test]
fn a_clean_transfer_adds_one_and_stops_at_the_ceiling() {
    let mut controller = Controller::start(Some(1), count(3));
    controller.answered(Answer::Clean);
    assert_eq!(controller.permitted(), 2);
    controller.answered(Answer::Clean);
    controller.answered(Answer::Clean);
    assert_eq!(controller.permitted(), 3);
}

#[test]
fn a_rate_limit_halves_the_count_at_once_and_never_reaches_zero() {
    let mut controller = Controller::start(Some(8), count(8));
    controller.answered(Answer::RateLimited);
    assert_eq!(controller.permitted(), 4);
    controller.answered(Answer::RateLimited);
    controller.answered(Answer::RateLimited);
    controller.answered(Answer::RateLimited);
    assert_eq!(controller.permitted(), 1);
}

#[test]
fn a_failure_that_is_not_a_rate_limit_steps_down_by_one() {
    let mut controller = Controller::start(Some(4), count(8));
    controller.answered(Answer::Faltered);
    assert_eq!(controller.permitted(), 3);
}

#[test]
fn a_fixed_controller_never_moves_whatever_the_host_answers() {
    let mut controller = Controller::fixed(count(2));
    for answer in [Answer::Clean, Answer::RateLimited, Answer::Faltered] {
        controller.answered(answer);
        assert_eq!(controller.permitted(), 2);
    }
}

#[test]
fn a_rate_owes_time_only_when_the_bytes_ran_ahead_of_the_clock() {
    assert_eq!(
        debt(1000, 2000, Duration::from_secs(1)),
        Duration::from_secs(1)
    );
    assert_eq!(debt(1000, 500, Duration::from_secs(1)), Duration::ZERO);
    assert_eq!(debt(0, 5000, Duration::ZERO), Duration::ZERO);
}

#[test]
fn a_rate_is_read_in_one_form_and_no_other() {
    assert_eq!(
        "1024".parse::<Bandwidth>().unwrap().bytes_per_second(),
        1024
    );
    assert_eq!("1k".parse::<Bandwidth>().unwrap().bytes_per_second(), 1024);
    assert_eq!(
        "2M".parse::<Bandwidth>().unwrap().bytes_per_second(),
        2 * 1024 * 1024
    );
    for refused in ["0", "", "1kb", "1/s", "-1", "1.5k", "k"] {
        assert!(
            refused.parse::<Bandwidth>().is_err(),
            "{refused} was read as a rate"
        );
    }
}
