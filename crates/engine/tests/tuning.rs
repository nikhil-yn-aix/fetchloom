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

use fetchloom_engine::capability::{
    Backing, CaseFolding, Normalization, Scanner, VolumeCapabilities,
};
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::timestamp::Timestamp;
use fetchloom_engine::tuning::{
    Answer, Ceilings, Controller, FIRST_PER_HOST, HostMeasurement, SUSTAINED_WINDOWS,
    TRANSFERS_CEILING, WriteRate, debt, order_candidates, resolve_io_mode,
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

fn measured(throughput: u64, time_to_first_byte_ms: u64) -> HostMeasurement {
    HostMeasurement {
        concurrency: 1,
        throughput,
        time_to_first_byte_ms,
        observed_at: Timestamp::from_epoch_seconds(0),
    }
}

fn locations(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("https://host-{index}/object"))
        .collect()
}

#[test]
fn with_no_measurements_at_all_the_order_is_exactly_the_input_order() {
    let candidates = locations(4);
    let ordered = order_candidates(&candidates, &|_location| None);
    assert_eq!(ordered, candidates);
}

#[test]
fn a_host_with_higher_recorded_throughput_sorts_before_one_with_lower() {
    let candidates = locations(2);
    let ordered = order_candidates(&candidates, &|location| {
        if location == candidates[0] {
            Some(measured(100, 50))
        } else {
            Some(measured(200, 50))
        }
    });
    assert_eq!(ordered, vec![candidates[1].clone(), candidates[0].clone()]);
}

#[test]
fn equal_throughput_sorts_by_lower_time_to_first_byte() {
    let candidates = locations(2);
    let ordered = order_candidates(&candidates, &|location| {
        if location == candidates[0] {
            Some(measured(100, 80))
        } else {
            Some(measured(100, 20))
        }
    });
    assert_eq!(ordered, vec![candidates[1].clone(), candidates[0].clone()]);
}

#[test]
fn fully_equal_measurements_keep_manifest_order() {
    let candidates = locations(3);
    let ordered = order_candidates(&candidates, &|_location| Some(measured(100, 50)));
    assert_eq!(ordered, candidates);
}

#[test]
fn an_unmeasured_candidate_never_jumps_ahead_of_a_measured_one() {
    let candidates = locations(2);
    let ordered = order_candidates(&candidates, &|location| {
        if location == candidates[0] {
            None
        } else {
            Some(measured(1, 999))
        }
    });
    assert_eq!(
        ordered,
        vec![candidates[1].clone(), candidates[0].clone()],
        "the unmeasured candidate outranked one with an actual measurement, however low"
    );
}

fn capabilities(backing: Backing, scanner: Scanner) -> VolumeCapabilities {
    VolumeCapabilities {
        case_folding: CaseFolding::Sensitive,
        normalization: Normalization::Sensitive,
        clone: false,
        sparse: false,
        symlink: false,
        hard_link: false,
        max_component_length: 255,
        max_path_length: 4096,
        backing,
        scanner,
    }
}

#[test]
fn auto_chooses_uncached_only_when_local_absent_and_the_platform_can_release() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Auto,
        &capabilities(Backing::Local, Scanner::Absent),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Uncached);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn auto_stays_buffered_on_a_network_volume() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Auto,
        &capabilities(Backing::Network, Scanner::Absent),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn auto_stays_buffered_when_a_scanner_is_present() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Auto,
        &capabilities(
            Backing::Local,
            Scanner::Present {
                name: "guard".to_owned(),
                cost_ratio: 3.0,
            },
        ),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn auto_stays_buffered_when_the_scanner_answer_is_unknown() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Auto,
        &capabilities(Backing::Local, Scanner::Unknown { cost_ratio: 1.0 }),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn auto_stays_buffered_when_the_platform_cannot_release_pages() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Auto,
        &capabilities(Backing::Local, Scanner::Absent),
        false,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn explicit_buffered_stays_buffered_whatever_the_volume_says() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Buffered,
        &capabilities(Backing::Local, Scanner::Absent),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn explicit_uncached_stays_uncached_when_the_platform_can_release() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Uncached,
        &capabilities(Backing::Network, Scanner::Absent),
        true,
        &queue,
    );
    assert_eq!(resolved, IoMode::Uncached);
    assert_eq!(queue.take(), Vec::new());
}

#[test]
fn explicit_uncached_falls_back_to_buffered_when_the_platform_cannot_release() {
    let queue = DegradeQueue::new();
    let resolved = resolve_io_mode(
        IoMode::Uncached,
        &capabilities(Backing::Local, Scanner::Absent),
        false,
        &queue,
    );
    assert_eq!(resolved, IoMode::Buffered);
    let recorded = queue.take();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].requested, "uncached");
    assert_eq!(recorded[0].used, "buffered");
    assert_eq!(
        recorded[0].reason,
        "the platform offers no way to release written pages without constraining every write to sector alignment"
    );
}

#[test]
fn a_write_rate_that_holds_up_never_answers_the_controller() {
    let mut rate = WriteRate::default();
    assert!(!rate.observed(1 << 20, Duration::from_millis(100)));
    assert!(!rate.observed(1 << 20, Duration::from_millis(100)));
    assert!(
        !rate.observed(1 << 20, Duration::from_millis(110)),
        "a tenth slower is not a collapse"
    );
}

#[test]
fn a_write_rate_that_collapses_answers_the_controller_once_per_window() {
    let mut rate = WriteRate::default();
    assert!(!rate.observed(1 << 20, Duration::from_millis(100)));
    assert!(
        rate.observed(1 << 20, Duration::from_millis(1000)),
        "a tenth of the rate the run had been sustaining was not a collapse"
    );
}

#[test]
fn a_write_rate_that_recovers_stops_answering() {
    let mut rate = WriteRate::default();
    assert!(!rate.observed(1 << 20, Duration::from_millis(100)));
    assert!(rate.observed(1 << 20, Duration::from_millis(1000)));
    assert!(
        !rate.observed(1 << 20, Duration::from_millis(100)),
        "a recovered volume kept answering"
    );
}

#[test]
fn the_first_window_is_never_a_collapse_because_nothing_was_sustained_yet() {
    let mut rate = WriteRate::default();
    assert!(!rate.observed(1, Duration::from_secs(60)));
}

#[test]
fn a_collapsed_write_rate_lowers_what_the_controller_permits() {
    let ceiling = NonZeroU32::new(8).unwrap();
    let mut controller = Controller::start(Some(4), ceiling);
    let before = controller.permitted();
    controller.answered(Answer::Faltered);
    assert!(
        controller.permitted() < before,
        "a volume that collapsed did not reduce what is in flight"
    );
}

#[test]
fn a_collapsed_write_rate_never_raises_the_ceiling() {
    let ceiling = NonZeroU32::new(2).unwrap();
    let mut controller = Controller::start(Some(2), ceiling);
    for _ in 0..10 {
        controller.answered(Answer::Clean);
    }
    assert_eq!(
        controller.permitted(),
        2,
        "clean answers pushed past the ceiling a measurement may not raise"
    );
}

#[test]
fn a_rate_the_page_cache_absorbed_once_does_not_condemn_every_window_after_it() {
    let mut rate = WriteRate::default();
    assert!(
        !rate.observed(64 << 20, Duration::from_millis(1)),
        "the first window is never a collapse"
    );
    let mut collapses = 0;
    for _ in 0..64 {
        if rate.observed(1 << 20, Duration::from_millis(4)) {
            collapses += 1;
        }
    }
    assert!(
        collapses <= SUSTAINED_WINDOWS,
        "a volume writing at one steady rate answered the controller {collapses} times, so a \
         peak the page cache absorbed once condemns every honest window after it"
    );
}

#[test]
fn writes_shorter_than_one_window_are_gathered_rather_than_judged_one_at_a_time() {
    let mut rate = WriteRate::default();
    for _ in 0..8 {
        assert!(
            !rate.observed(1 << 13, Duration::from_micros(10)),
            "a write shorter than one window was judged on its own"
        );
    }
    let mut slow = WriteRate::default();
    assert!(!slow.observed(1 << 20, Duration::from_millis(100)));
    for _ in 0..15 {
        assert!(
            !slow.observed(1 << 16, Duration::from_millis(100)),
            "half a window of a stalled volume was judged before the window was full"
        );
    }
    assert!(
        slow.observed(1 << 16, Duration::from_millis(100)),
        "a full window a volume took sixteen times longer over was not read as a collapse"
    );
}

/// A window's worth of bytes, which is the least a host must deliver at a count
/// before that count's rate says anything.
const A_WINDOW: u64 = fetchloom_engine::tuning::WINDOW_BYTES;

#[test]
fn a_count_that_did_not_deliver_more_is_given_up_and_never_reached_again() {
    let mut controller = Controller::start(Some(1), count(8));
    controller.delivered(A_WINDOW, Duration::from_millis(100));
    controller.answered(Answer::Clean);
    assert_eq!(
        controller.permitted(),
        2,
        "a host that answered cleanly was not given a second stream"
    );

    controller.delivered(A_WINDOW, Duration::from_millis(200));
    controller.answered(Answer::Clean);
    assert_eq!(
        controller.permitted(),
        1,
        "a second stream that delivered no more was kept"
    );

    controller.delivered(A_WINDOW, Duration::from_millis(50));
    controller.answered(Answer::Clean);
    assert_eq!(
        controller.permitted(),
        1,
        "a count measured not to help was reached again"
    );
}

#[test]
fn a_count_that_delivered_more_is_kept_and_the_next_one_is_tried() {
    let mut controller = Controller::start(Some(1), count(8));
    controller.delivered(A_WINDOW, Duration::from_millis(200));
    controller.answered(Answer::Clean);
    assert_eq!(controller.permitted(), 2);

    controller.delivered(A_WINDOW, Duration::from_millis(100));
    controller.answered(Answer::Clean);
    assert_eq!(
        controller.permitted(),
        3,
        "a count that delivered twice as much was not built on"
    );
}

#[test]
fn a_host_that_has_delivered_less_than_a_window_rises_on_a_clean_answer() {
    let mut controller = Controller::start(Some(1), count(8));
    controller.delivered(A_WINDOW / 4, Duration::from_millis(100));
    controller.answered(Answer::Clean);
    assert_eq!(
        controller.permitted(),
        2,
        "a run that has measured nothing yet refused to measure whether a second stream helps"
    );
}
