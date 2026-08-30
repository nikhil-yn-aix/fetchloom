//! Contract tests over the deterministic work counters.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use sha2 as _;
use toml as _;

use std::sync::Arc;

use fetchloom_engine::work::{Work, WorkCounter};

#[test]
fn a_fresh_counter_has_counted_nothing() {
    let counter = WorkCounter::new();
    assert_eq!(
        counter.taken(),
        Work {
            bytes_read: 0,
            bytes_written: 0,
            requests: 0,
            file_operations: 0,
        }
    );
}

#[test]
fn each_kind_of_work_is_counted_separately() {
    let counter = WorkCounter::new();
    counter.read_bytes(7);
    counter.wrote_bytes(11);
    counter.issued_request();
    counter.issued_request();
    counter.touched_file();
    counter.touched_file();
    counter.touched_file();
    assert_eq!(
        counter.taken(),
        Work {
            bytes_read: 7,
            bytes_written: 11,
            requests: 2,
            file_operations: 3,
        }
    );
}

#[test]
fn work_counted_from_two_threads_sums() {
    let counter = Arc::new(WorkCounter::new());
    let mut hands = Vec::new();
    for _ in 0..4 {
        let counter = Arc::clone(&counter);
        hands.push(std::thread::spawn(move || {
            for _ in 0..1000 {
                counter.read_bytes(1);
                counter.wrote_bytes(2);
                counter.issued_request();
                counter.touched_file();
            }
        }));
    }
    for hand in hands {
        hand.join().unwrap();
    }
    assert_eq!(
        counter.taken(),
        Work {
            bytes_read: 4000,
            bytes_written: 8000,
            requests: 4000,
            file_operations: 4000,
        }
    );
}

#[test]
fn a_snapshot_carries_the_four_key_names_the_contract_states() {
    let counter = WorkCounter::new();
    counter.read_bytes(1);
    let json = serde_json::to_value(counter.taken()).unwrap();
    let object = json.as_object().unwrap();
    assert_eq!(object.len(), 4);
    assert!(object.contains_key("bytes_read"));
    assert!(object.contains_key("bytes_written"));
    assert!(object.contains_key("requests"));
    assert!(object.contains_key("file_operations"));
}

#[test]
fn reading_a_snapshot_does_not_reset_the_counter() {
    let counter = WorkCounter::new();
    counter.read_bytes(5);
    let first = counter.taken();
    let second = counter.taken();
    assert_eq!(first, second);
}
