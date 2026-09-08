//! The latch that refuses a request at the point it would be issued, whatever
//! resolved to the location and whichever adapter would send it.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to read this file is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::network;

#[test]
fn a_run_that_forbade_the_network_refuses_every_location() {
    assert!(
        network::allowed("https://example.invalid/object").is_ok(),
        "a run that forbade nothing refused a request"
    );

    network::forbid();

    for location in [
        "https://example.invalid/object",
        "https://huggingface.co/api/datasets/org/name/tree/main",
        "https://zenodo.org/api/records/1234567",
    ] {
        let refused = network::allowed(location);
        assert!(
            refused.is_err(),
            "a run that forbade the network issued a request to {location}"
        );
        assert_eq!(
            refused.err().map(|error| error.kind()),
            Some(ErrorKind::PolicyOffline)
        );
    }
    assert!(network::forbidden());
}

/// `network::forbid()` is a latch with no unset, so this target holds one test
/// and cannot grow: a second test here would inherit a forbidden network and
/// the order they ran in would decide the answer.
#[test]
fn this_target_holds_one_test_because_the_latch_it_sets_cannot_be_unset() {
    let here = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("network.rs"),
    )
    .unwrap();
    let tests = here
        .lines()
        .filter(|line| line.trim_end() == "#[test]")
        .count();
    assert_eq!(
        tests, 2,
        "network.rs holds {tests} tests beside this one, and the latch it sets makes their order decide the outcome"
    );
}
