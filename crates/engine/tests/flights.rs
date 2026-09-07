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
use std::sync::{Arc, Condvar, Mutex};
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

const PATIENCE: Duration = Duration::from_secs(30);

const GRACE: Duration = Duration::from_millis(250);

struct Gate {
    arrived: Mutex<u32>,
    opened: Condvar,
    ceiling: u32,
    gave_up: AtomicU32,
}

impl Gate {
    fn holding(ceiling: u32) -> Self {
        Self {
            arrived: Mutex::new(0),
            opened: Condvar::new(),
            ceiling,
            gave_up: AtomicU32::new(0),
        }
    }

    fn arrive(&self) {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        if *arrived >= self.ceiling {
            self.opened.notify_all();
        }
        let (held, assembly) = self
            .opened
            .wait_timeout_while(arrived, PATIENCE, |count| *count < self.ceiling)
            .unwrap();
        if assembly.timed_out() {
            self.gave_up.fetch_max((*held).max(1), Ordering::SeqCst);
            return;
        }
        let _ = self
            .opened
            .wait_timeout_while(held, GRACE, |count| *count <= self.ceiling)
            .unwrap();
    }

    fn assembled(&self) -> Result<(), u32> {
        match self.gave_up.load(Ordering::SeqCst) {
            0 => Ok(()),
            most => Err(most),
        }
    }
}

#[derive(Default)]
struct Announcement {
    made: Mutex<bool>,
    changed: Condvar,
}

impl Announcement {
    fn make(&self) {
        *self.made.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn hear(&self) -> bool {
        let (_held, outcome) = self
            .changed
            .wait_timeout_while(self.made.lock().unwrap(), PATIENCE, |made| !*made)
            .unwrap();
        !outcome.timed_out()
    }
}

#[test]
fn a_per_host_ceiling_holds_that_many_jobs_in_flight_for_one_host() {
    let flights = Flights::new(ceilings(8, 3), fixed(3));
    let seen = Watermark::default();
    let the_ceiling = Gate::holding(3);
    let items: Vec<u32> = (0..16).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        seen.entered();
        the_ceiling.arrive();
        seen.left();
        Ok::<u32, Error>(*item)
    });

    assert_eq!(
        the_ceiling.assembled(),
        Ok(()),
        "the gate gave up before three jobs were ever in flight at once, so nothing here measures the ceiling"
    );
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
    let the_ceiling = Gate::holding(2);
    let items: Vec<u32> = (0..12).collect();
    let hosts: Vec<String> = items
        .iter()
        .map(|index| format!("host-{index}.example"))
        .collect();

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        seen.entered();
        the_ceiling.arrive();
        seen.left();
        Ok::<u32, Error>(*item)
    });

    assert_eq!(
        the_ceiling.assembled(),
        Ok(()),
        "the gate gave up before two jobs were ever in flight at once, so nothing here measures the ceiling"
    );
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
    let the_ceiling = Gate::holding(1);
    let items: Vec<u32> = (0..6).collect();
    let hosts: Vec<String> = items.iter().map(|_| "one.example".to_owned()).collect();

    flights.each(&items, &hosts, &|_: &u32| {
        seen.entered();
        the_ceiling.arrive();
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

    let the_later_one_failed = Announcement::default();
    let heard = AtomicU32::new(0);

    let outcomes = flights.each(&items, &hosts, &|item: &u32| {
        if *item == 5 {
            the_later_one_failed.make();
            return Err(Error::new(ErrorKind::NetworkRefused, "the later one"));
        }
        if *item == 2 {
            heard.store(u32::from(the_later_one_failed.hear()), Ordering::SeqCst);
            return Err(Error::new(ErrorKind::NetworkRefused, "the earlier one"));
        }
        Ok::<u32, Error>(*item)
    });
    assert_eq!(
        heard.load(Ordering::SeqCst),
        1,
        "the earlier job never heard the later one fail, so it did not finish second and this measures nothing"
    );
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
