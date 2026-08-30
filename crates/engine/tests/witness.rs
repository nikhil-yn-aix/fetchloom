//! Contract tests over witnesses and the trust class they can raise.
//!
//! The definition of independence is the one most easily weakened by accident,
//! so most of these tests are adversarial: two witnesses that differ in only
//! one of the three ways must not corroborate, and neither must a pair that
//! looks independent by a name rather than by an origin.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::digest::{ContentDigest, ManifestDigest};
use fetchloom_engine::identity::MachineId;
use fetchloom_engine::timestamp::Timestamp;
use fetchloom_engine::trust::{ArtifactKey, RunId, TrustClass, Witness, classify};

fn digest(seed: u8) -> ContentDigest {
    ContentDigest::from_bytes(*blake3::hash(&[seed]).as_bytes())
}

fn witness(machine: &str, origin: &str, run: &str, seed: u8) -> Witness {
    Witness {
        digest: digest(seed),
        machine: MachineId::new(machine),
        origin: origin.to_owned(),
        run: RunId::new(run),
        observed_at: Timestamp::from_epoch_seconds(1_700_000_000),
    }
}

#[test]
fn a_digest_supplied_before_the_run_is_verified_whatever_the_witnesses_say() {
    let observed = digest(1);
    assert_eq!(
        classify(Some(observed), observed, &[]),
        TrustClass::Verified
    );
    assert_eq!(
        classify(
            Some(observed),
            observed,
            &[
                witness("one", "https://a/x", "r1", 1),
                witness("two", "https://b/x", "r2", 1),
            ]
        ),
        TrustClass::Verified
    );
}

#[test]
fn no_prior_digest_and_no_witnesses_is_first_use() {
    assert_eq!(classify(None, digest(1), &[]), TrustClass::Tofu);
}

#[test]
fn two_witnesses_differing_in_machine_origin_and_run_corroborate() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("two", "https://b/x", "r2", 1),
    ];
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Corroborated
    );
}

#[test]
fn two_observations_from_one_machine_are_one_observation() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("one", "https://b/x", "r2", 1),
    ];
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Tofu,
        "one machine observing twice raised its own trust"
    );
}

#[test]
fn two_observations_from_one_origin_are_one_observation() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("two", "https://a/x", "r2", 1),
    ];
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Tofu,
        "one origin serving twice raised trust"
    );
}

#[test]
fn two_observations_from_one_run_are_one_observation() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("two", "https://b/x", "r1", 1),
    ];
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Tofu,
        "one run reaching two mirrors raised trust"
    );
}

#[test]
fn a_witness_recorded_twice_under_two_names_is_still_one_witness() {
    let one = witness("one", "https://a/x", "r1", 1);
    let mut again = one.clone();
    again.observed_at = Timestamp::from_epoch_seconds(1_800_000_000);
    assert_eq!(
        classify(None, digest(1), &[one, again]),
        TrustClass::Tofu,
        "the same observation at two times counted as two"
    );
}

#[test]
fn witnesses_carrying_another_digest_do_not_corroborate_this_one() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 9),
        witness("two", "https://b/x", "r2", 9),
    ];
    assert_eq!(classify(None, digest(1), &witnesses), TrustClass::Tofu);
}

#[test]
fn a_third_witness_that_agrees_with_neither_pair_member_changes_nothing() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("two", "https://b/x", "r2", 9),
        witness("three", "https://c/x", "r3", 7),
    ];
    assert_eq!(classify(None, digest(1), &witnesses), TrustClass::Tofu);
}

#[test]
fn three_witnesses_of_which_two_are_independent_corroborate() {
    let witnesses = [
        witness("one", "https://a/x", "r1", 1),
        witness("one", "https://a/x", "r9", 1),
        witness("two", "https://b/x", "r2", 1),
    ];
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Corroborated
    );
}

#[test]
fn one_witness_however_strong_is_first_use() {
    assert_eq!(
        classify(None, digest(1), &[witness("one", "https://a/x", "r1", 1)]),
        TrustClass::Tofu
    );
}

#[test]
fn an_artifact_key_is_the_same_for_one_manifest_and_artifact_and_differs_otherwise() {
    let manifest = ManifestDigest::from_bytes(*blake3::hash(b"manifest").as_bytes());
    let other = ManifestDigest::from_bytes(*blake3::hash(b"another").as_bytes());
    assert_eq!(
        ArtifactKey::of(manifest, "corpus"),
        ArtifactKey::of(manifest, "corpus")
    );
    assert_ne!(
        ArtifactKey::of(manifest, "corpus"),
        ArtifactKey::of(manifest, "corpu")
    );
    assert_ne!(
        ArtifactKey::of(manifest, "corpus"),
        ArtifactKey::of(other, "corpus")
    );
}

#[test]
fn an_artifact_key_cannot_be_confused_by_where_one_name_ends() {
    let manifest = ManifestDigest::from_bytes(*blake3::hash(b"manifest").as_bytes());
    assert_ne!(
        ArtifactKey::of(manifest, "ab"),
        ArtifactKey::of(manifest, "a\u{0}b")
    );
}
