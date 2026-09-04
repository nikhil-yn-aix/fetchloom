//! What the two in-flight bounds admit, and in what order they report.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::tuning::{Ceilings, Controller};

fn ceilings(global: u32, per_host: u32) -> Ceilings {
    Ceilings {
        global: NonZeroU32::new(global).unwrap(),
        per_host: NonZeroU32::new(per_host).unwrap(),
    }
}

#[derive(Debug, Default)]
struct Watermark {
    now: AtomicU32,
    most: AtomicU32,
}

impl Watermark {
    fn entered(&self) {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
    }

    fn left(&self) {
        self.now.fetch_sub(1, Ordering::SeqCst);
    }

    fn most(&self) -> u32 {
        self.most.load(Ordering::SeqCst)
    }
}

fn fixed(at: u32) -> impl Fn(&str) -> Controller + Sync {
    move |_| Controller::fixed(NonZeroU32::new(at).unwrap())
}

#[test]
fn a_per_host_ceiling_holds_that_many_jobs_in_flight_for_one_host() {
    let flights = Flights::new(ceilings(8, 3), fixed(3));
    let seen = Watermark::default();
    let items: Vec<u32> = (0..16).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        seen.entered();
        std::thread::sleep(Duration::from_millis(20));
        seen.left();
        Ok::<u32, Error>(*item)
    });

    assert_eq!(
        seen.most(),
        3,
        "the per-host ceiling of three admitted a different count"
    );
    assert!(
        outcomes.failure.is_none(),
        "a job that cannot fail reported a failure"
    );
    let produced = outcomes.outputs;
    assert_eq!(
        produced, items,
        "the results did not come back in the order the items were given"
    );
}

#[test]
fn a_global_ceiling_holds_that_many_jobs_in_flight_across_every_host() {
    let flights = Flights::new(ceilings(2, 4), fixed(4));
    let seen = Watermark::default();
    let items: Vec<u32> = (0..12).collect();
    let hosts: Vec<String> = items
        .iter()
        .map(|index| format!("host-{index}.example"))
        .collect();

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        seen.entered();
        std::thread::sleep(Duration::from_millis(20));
        seen.left();
        Ok::<u32, Error>(*item)
    });

    assert_eq!(
        seen.most(),
        2,
        "the global ceiling of two admitted a different count across twelve hosts"
    );
    assert_eq!(outcomes.outputs.len(), items.len());
}

#[test]
fn one_ceiling_admits_one_job_at_a_time() {
    let flights = Flights::new(ceilings(1, 1), fixed(1));
    let seen = Watermark::default();
    let items: Vec<u32> = (0..6).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();

    flights.each(&items, &hosts, &|_: &u32| {
        seen.entered();
        std::thread::sleep(Duration::from_millis(10));
        seen.left();
        Ok::<u32, Error>(0)
    });

    assert_eq!(
        seen.most(),
        1,
        "a ceiling of one admitted more than one job"
    );
}

#[test]
fn the_failure_reported_is_the_first_in_order_however_the_jobs_finished() {
    let flights = Flights::new(ceilings(8, 8), fixed(8));
    let items: Vec<u32> = (0..8).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        if *item == 5 {
            return Err(Error::new(ErrorKind::NetworkRefused, "the later one"));
        }
        if *item == 2 {
            std::thread::sleep(Duration::from_millis(40));
            return Err(Error::new(ErrorKind::NetworkRefused, "the earlier one"));
        }
        Ok::<u32, Error>(*item)
    });
    assert_eq!(
        outcomes.outputs,
        vec![0, 1],
        "the outputs did not stop at the first failure in order"
    );
    assert_eq!(
        outcomes
            .failure
            .map(|failure| failure.next_action().to_owned()),
        Some("the earlier one".to_owned()),
        "the failure reported was not the first in order, so it moves with the clock"
    );
}

#[test]
fn nothing_after_the_first_failure_is_attempted() {
    let flights = Flights::new(ceilings(1, 1), fixed(1));
    let attempted = Arc::new(Mutex::new(Vec::new()));
    let items: Vec<u32> = (0..8).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();
    let recording = Arc::clone(&attempted);

    flights.each(&items, &hosts, &|item: &u32| {
        recording.lock().unwrap().push(*item);
        if *item == 3 {
            return Err(Error::new(ErrorKind::NetworkRefused, "stop here"));
        }
        Ok::<u32, Error>(*item)
    });

    let mut ran = attempted.lock().unwrap().clone();
    ran.sort_unstable();
    assert_eq!(
        ran,
        vec![0, 1, 2, 3],
        "work was attempted past the first failure, which a run one at a time never reaches"
    );
}
