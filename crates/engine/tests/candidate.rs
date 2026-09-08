//! The order the seven scoring inputs put candidates in, and what breaks a tie.

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

use std::time::Duration;

use fetchloom_engine::candidate::{Probed, Separator, score};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{Cost, SourceIdentity, SourceMetadata};

fn metadata(location: &str) -> SourceMetadata {
    SourceMetadata {
        location: SafeUrl::new(location),
        host: Host::new("one.example".to_owned()),
        size: Some(1024),
        content: None,
        interop: None,
        identity: SourceIdentity::None,
        last_modified: None,
        supports_ranges: false,
        time_to_first_byte: Duration::from_millis(10),
        retry_after: None,
        cost: Cost::default(),
    }
}

fn probed(index: usize, location: &str) -> Probed {
    Probed {
        index,
        location: location.to_owned(),
        metadata: Some(metadata(location)),
        throughput: None,
        time_to_first_byte_ms: None,
        refused: false,
        headroom: 1,
    }
}

fn chosen(mut candidates: Vec<Probed>) -> (String, Separator) {
    let separator = score(&mut candidates);
    (candidates[0].location.clone(), separator)
}

#[test]
fn a_reachable_candidate_is_taken_over_one_that_answered_nothing() {
    let mut unreachable = probed(0, "https://first.example/object");
    unreachable.metadata = None;
    let (taken, separator) = chosen(vec![
        unreachable,
        probed(1, "https://second.example/object"),
    ]);
    assert_eq!(taken, "https://second.example/object");
    assert_eq!(separator, Separator::Reachable);
}

#[test]
fn range_support_is_taken_over_manifest_order() {
    let mut second = probed(1, "https://second.example/object");
    second.metadata.as_mut().unwrap().supports_ranges = true;
    let (taken, separator) = chosen(vec![probed(0, "https://first.example/object"), second]);
    assert_eq!(taken, "https://second.example/object");
    assert_eq!(separator, Separator::Ranges);
}

#[test]
fn an_immutable_identity_is_taken_over_range_support_alone() {
    let mut ranged = probed(0, "https://ranged.example/object");
    ranged.metadata.as_mut().unwrap().supports_ranges = true;
    let mut immutable = probed(1, "https://immutable.example/object");
    {
        let found = immutable.metadata.as_mut().unwrap();
        found.supports_ranges = true;
        found.identity = SourceIdentity::ImmutableVersion("v1".to_owned());
    }
    let (taken, separator) = chosen(vec![ranged, immutable]);
    assert_eq!(taken, "https://immutable.example/object");
    assert_eq!(separator, Separator::ImmutableIdentity);
}

#[test]
fn recorded_throughput_separates_candidates_nothing_else_separates() {
    let mut slow = probed(0, "https://slow.example/object");
    slow.throughput = Some(1000);
    let mut fast = probed(1, "https://fast.example/object");
    fast.throughput = Some(9000);
    let (taken, separator) = chosen(vec![slow, fast]);
    assert_eq!(taken, "https://fast.example/object");
    assert_eq!(separator, Separator::Throughput);
}

#[test]
fn time_to_first_byte_separates_candidates_of_equal_throughput() {
    let mut slow = probed(0, "https://slow.example/object");
    slow.throughput = Some(9000);
    slow.time_to_first_byte_ms = Some(400);
    let mut quick = probed(1, "https://quick.example/object");
    quick.throughput = Some(9000);
    quick.time_to_first_byte_ms = Some(20);
    let (taken, separator) = chosen(vec![slow, quick]);
    assert_eq!(taken, "https://quick.example/object");
    assert_eq!(separator, Separator::TimeToFirstByte);
}

#[test]
fn egress_cost_separates_two_candidates_that_both_stated_it() {
    let mut charged = probed(0, "https://charged.example/object");
    charged.metadata.as_mut().unwrap().cost = Cost {
        egress_charged: Some(true),
        requester_pays: Some(false),
    };
    let mut free = probed(1, "https://free.example/object");
    free.metadata.as_mut().unwrap().cost = Cost {
        egress_charged: Some(false),
        requester_pays: Some(false),
    };
    let (taken, separator) = chosen(vec![charged, free]);
    assert_eq!(taken, "https://free.example/object");
    assert_eq!(separator, Separator::EgressCost);
}

#[test]
fn a_source_that_stated_no_cost_is_neither_preferred_nor_refused_over_one_that_did() {
    let mut charged = probed(0, "https://charged.example/object");
    charged.metadata.as_mut().unwrap().cost = Cost {
        egress_charged: Some(true),
        requester_pays: Some(false),
    };
    let silent = probed(1, "https://silent.example/object");
    let (taken, separator) = chosen(vec![charged, silent]);
    assert_eq!(
        taken, "https://charged.example/object",
        "silence about cost was read as evidence, where contracts states none"
    );
    assert_eq!(separator, Separator::ManifestOrder);
}

#[test]
fn remaining_politeness_headroom_separates_candidates_nothing_else_separates() {
    let mut busy = probed(0, "https://busy.example/object");
    busy.headroom = 0;
    let mut idle = probed(1, "https://idle.example/object");
    idle.headroom = 3;
    let (taken, separator) = chosen(vec![busy, idle]);
    assert_eq!(taken, "https://idle.example/object");
    assert_eq!(separator, Separator::PolitenessHeadroom);
}

#[test]
fn manifest_order_breaks_a_tie_so_selection_is_the_same_every_run() {
    let (taken, separator) = chosen(vec![
        probed(0, "https://first.example/object"),
        probed(1, "https://second.example/object"),
    ]);
    assert_eq!(taken, "https://first.example/object");
    assert_eq!(separator, Separator::ManifestOrder);

    let (reversed, _) = chosen(vec![
        probed(1, "https://second.example/object"),
        probed(0, "https://first.example/object"),
    ]);
    assert_eq!(
        reversed, "https://first.example/object",
        "the tie was broken by the order the candidates arrived in rather than by manifest order"
    );
}

#[test]
fn the_host_of_a_location_is_the_address_a_bracketed_literal_holds() {
    use fetchloom_engine::reference::Host;
    assert_eq!(
        Host::of_location("http://[::1]:8080/object").as_str(),
        "::1"
    );
    assert_eq!(Host::of_location("http://[::1]/object").as_str(), "::1");
    assert_eq!(
        Host::of_location("https://user:secret@example.org:443/object").as_str(),
        "example.org"
    );
    assert_eq!(
        Host::of_location("https://example.org/object").as_str(),
        "example.org"
    );
    assert_eq!(Host::of_location("./relative/path").as_str(), "");
}

#[test]
fn a_candidate_this_run_never_measured_is_neither_preferred_nor_refused() {
    let measure = |mut candidate: Probed| {
        candidate.throughput = Some(1);
        candidate.time_to_first_byte_ms = Some(9999);
        candidate
    };

    let (taken, separator) = chosen(vec![
        probed(0, "https://silent.example/object"),
        measure(probed(1, "https://measured.example/object")),
    ]);
    assert_eq!(
        taken, "https://silent.example/object",
        "a candidate this run never measured was refused for its silence"
    );
    assert_eq!(separator, Separator::ManifestOrder);

    let (taken, separator) = chosen(vec![
        measure(probed(0, "https://measured.example/object")),
        probed(1, "https://silent.example/object"),
    ]);
    assert_eq!(
        taken, "https://measured.example/object",
        "a candidate this run never measured was preferred over one it measured"
    );
    assert_eq!(separator, Separator::ManifestOrder);
}
