//! One volume, many callers at once, one answer.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use rustix as _;

mod support;

use std::sync::{Arc, Barrier};

use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

#[test]
fn every_caller_racing_one_volume_is_given_the_same_answer() {
    let scratch = support::scratch();
    let platform = Arc::new(NativePlatform::new(Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    )));
    let callers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .max(4)
        * 4;
    let gate = Arc::new(Barrier::new(callers));

    let answers: Vec<_> = (0..callers)
        .map(|_| {
            let platform = Arc::clone(&platform);
            let gate = Arc::clone(&gate);
            let directory = scratch.path().to_path_buf();
            std::thread::spawn(move || {
                gate.wait();
                platform.volume_capabilities(&directory).unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();

    let first = answers.first().unwrap();
    for (index, answer) in answers.iter().enumerate() {
        assert_eq!(
            answer, first,
            "caller {index} was given a different answer for the same volume than the first caller"
        );
    }
}
