//! The latch that refuses a request at the point it would be issued, whatever
//! resolved to the location and whichever adapter would send it.

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
