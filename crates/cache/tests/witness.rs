//! Contract tests over where witnesses are kept and what the store does with
//! them.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use serde as _;
use serde_json as _;
use tempfile as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use fetchloom_engine::digest::{ContentDigest, ManifestDigest};
use fetchloom_engine::identity::MachineId;
use fetchloom_engine::timestamp::Timestamp;
use fetchloom_engine::trust::{ArtifactKey, RunId, TrustClass, Witness, classify};

use support::cache;

fn digest(seed: u8) -> ContentDigest {
    ContentDigest::from_bytes(*blake3::hash(&[seed]).as_bytes())
}

fn key() -> ArtifactKey {
    ArtifactKey::of(
        ManifestDigest::from_bytes(*blake3::hash(b"a manifest").as_bytes()),
        "corpus",
    )
}

fn witness(machine: &str, origin: &str, run: &str) -> Witness {
    Witness {
        digest: digest(1),
        machine: MachineId::new(machine),
        origin: origin.to_owned(),
        run: RunId::new(run),
        observed_at: Timestamp::now(),
    }
}

#[test]
fn a_cache_that_has_never_fetched_an_artifact_holds_no_witness_for_it() {
    let (_scratch, held) = cache();
    assert!(held.witnesses(&key()).unwrap().is_empty());
}

#[test]
fn a_recorded_witness_is_read_back_whole() {
    let (_scratch, held) = cache();
    let seen = witness("one", "https://a/x", "r1");
    held.record_witness(&key(), seen.clone()).unwrap();
    assert_eq!(held.witnesses(&key()).unwrap(), vec![seen]);
}

#[test]
fn the_same_observation_recorded_twice_is_stored_once() {
    let (_scratch, held) = cache();
    let seen = witness("one", "https://a/x", "r1");
    held.record_witness(&key(), seen.clone()).unwrap();
    let mut later = seen.clone();
    later.observed_at = "2099-01-01T00:00:00Z".parse().unwrap();
    held.record_witness(&key(), later).unwrap();

    let found = held.witnesses(&key()).unwrap();
    assert_eq!(
        found.len(),
        1,
        "one machine, one origin and one run recorded twice became two observations: {found:?}"
    );
    assert_eq!(
        classify(None, digest(1), &found),
        TrustClass::Tofu,
        "running twice raised trust"
    );
}

#[test]
fn witnesses_for_two_artifacts_of_one_manifest_never_share_a_record() {
    let (_scratch, held) = cache();
    let manifest = ManifestDigest::from_bytes(*blake3::hash(b"a manifest").as_bytes());
    let one = ArtifactKey::of(manifest, "corpus");
    let other = ArtifactKey::of(manifest, "labels");
    held.record_witness(&one, witness("one", "https://a/x", "r1"))
        .unwrap();

    assert_eq!(held.witnesses(&one).unwrap().len(), 1);
    assert!(
        held.witnesses(&other).unwrap().is_empty(),
        "a witness for one artifact was read as a witness for another"
    );
}

#[test]
fn one_run_reaching_two_machines_and_two_origins_is_still_one_observation() {
    let (_scratch, held) = cache();
    let key = key();
    for seen in [
        witness("one", "https://a/x", "r1"),
        witness("one", "https://b/x", "r1"),
        witness("two", "https://a/x", "r1"),
        witness("two", "https://b/x", "r1"),
    ] {
        held.record_witness(&key, seen).unwrap();
    }
    assert_eq!(
        classify(None, digest(1), &held.witnesses(&key).unwrap()),
        TrustClass::Tofu,
        "four observations made by one run corroborated, where one run has one view of the          network however many machines and mirrors it reached"
    );

    held.record_witness(&key, witness("three", "https://c/x", "r9"))
        .unwrap();
    assert_eq!(
        classify(None, digest(1), &held.witnesses(&key).unwrap()),
        TrustClass::Corroborated,
        "an observation independent of every other did not corroborate"
    );
}

#[test]
fn a_witness_carrying_another_digest_never_corroborates_this_one() {
    let (_scratch, held) = cache();
    let key = key();
    for machine in ["one", "two", "three"] {
        let mut seen = witness(machine, &format!("https://{machine}/x"), machine);
        seen.digest = digest(9);
        held.record_witness(&key, seen).unwrap();
    }
    assert_eq!(
        classify(None, digest(1), &held.witnesses(&key).unwrap()),
        TrustClass::Tofu
    );
}

#[test]
fn every_witness_a_run_can_write_carries_its_own_machine_so_none_of_them_corroborate() {
    let (_scratch, held) = cache();
    let key = key();
    let mine = held.token().machine.clone();
    for (origin, run) in [
        ("https://a/x", "r1"),
        ("https://b/x", "r2"),
        ("https://c/x", "r3"),
    ] {
        held.record_witness(
            &key,
            Witness {
                digest: digest(1),
                machine: mine.clone(),
                origin: origin.to_owned(),
                run: RunId::new(run),
                observed_at: Timestamp::now(),
            },
        )
        .unwrap();
    }
    let witnesses = held.witnesses(&key).unwrap();
    assert!(
        witnesses.iter().all(|seen| seen.machine == mine),
        "a witness in this cache was observed by a machine no run here could have been"
    );
    assert_eq!(
        classify(None, digest(1), &witnesses),
        TrustClass::Tofu,
        "witnesses this machine wrote by itself corroborated, so corroborated is reachable and contracts.md must say so"
    );
}

#[test]
fn corroborating_needs_a_witness_from_a_machine_that_no_channel_in_this_build_delivers() {
    let (_scratch, held) = cache();
    let key = key();
    let mine = held.token().machine.clone();
    held.record_witness(
        &key,
        Witness {
            digest: digest(1),
            machine: mine.clone(),
            origin: "https://a/x".to_owned(),
            run: RunId::new("r1"),
            observed_at: Timestamp::now(),
        },
    )
    .unwrap();
    let mut with_a_stranger = held.witnesses(&key).unwrap();
    with_a_stranger.push(witness("another-machine", "https://b/x", "r2"));
    assert_ne!(
        with_a_stranger[1].machine, mine,
        "the stranger was this machine, so the case being described is not the one being tested"
    );
    assert_eq!(
        classify(None, digest(1), &with_a_stranger),
        TrustClass::Corroborated,
        "a second machine's witness did not corroborate, so the rule corroborated rests on has changed"
    );
}
