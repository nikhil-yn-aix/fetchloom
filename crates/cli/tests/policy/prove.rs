//! Contract tests over localized verification and repair.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::path::Path;
use std::process::Output;

use crate::support;
use fetchloom_engine::limits::{OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};
use fetchloom_faults::{Reply, Script, TestServer};

use tempfile::TempDir;

fn large_object() -> Vec<u8> {
    let len = usize::try_from(OUTBOARD_THRESHOLD + OUTBOARD_CHUNK_GROUP + 4097).unwrap();
    let mut bytes = vec![0u8; len];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from((index * 37 + index / 1021) % 251).unwrap_or(0);
    }
    bytes
}

struct Ground {
    scratch: TempDir,
    server: TestServer,
}

impl Ground {
    fn serving(object: Vec<u8>) -> Self {
        Self {
            scratch: TempDir::new().unwrap(),
            server: TestServer::start(
                Script::serving(object).tagged(vec!["\"phase-five\"".to_owned()]),
            )
            .unwrap(),
        }
    }

    fn cache(&self) -> std::path::PathBuf {
        self.scratch.path().join("cache")
    }

    fn url(&self) -> String {
        format!("{}/object", self.server.origin())
    }

    fn run(&self, arguments: &[&str]) -> Output {
        support::fetchloom()
            .current_dir(self.scratch.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", self.cache())
            .env("FETCHLOOM_COMPRESS", "none")
            .output()
            .unwrap()
    }

    fn fetch(&self) -> serde_json::Value {
        let url = self.url();
        let output = self.run(&[
            "get",
            &url,
            "--output",
            self.scratch.path().join("out").to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "the cold fetch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn object(&self) -> std::path::PathBuf {
        one_file_in(&self.cache().join("objects"))
    }

    fn outboard(&self) -> std::path::PathBuf {
        one_file_in(&self.cache().join("outboard"))
    }
}

fn one_file_in(directory: &Path) -> std::path::PathBuf {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|reason| panic!("{} could not be read: {reason}", directory.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    assert_eq!(
        found.len(),
        1,
        "{} holds {found:?} rather than one file",
        directory.display()
    );
    found.pop().unwrap()
}

fn receipt_for(ground: &Ground, destination: &Path) -> String {
    let wanted = destination
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    std::fs::read_dir(ground.cache().join("receipts"))
        .unwrap()
        .flatten()
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .find(|held| held.contains(&wanted))
        .unwrap_or_else(|| panic!("no receipt describes {}", destination.display()))
}

fn damage(path: &Path, at: u64, bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "damaging an object needs the permissions the platform gives a new file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();

    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(bytes).unwrap();
}

fn damage_region(path: &Path, at: u64, length: usize) {
    damage(path, at, &vec![0x5A; length]);
}

#[test]
fn repairing_a_one_megabyte_region_of_a_very_large_object_transfers_about_one_megabyte() {
    let object = large_object();
    let ground = Ground::serving(object.clone());
    ground.fetch();

    let group = OUTBOARD_CHUNK_GROUP;
    damage_region(
        &ground.object(),
        group * 40,
        usize::try_from(group).unwrap(),
    );

    let url = ground.url();
    let repaired = ground.run(&["repair", &url, "--json"]);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "the repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();

    assert_eq!(result["status"], "repaired");
    assert_eq!(
        result["ranges"].as_u64(),
        Some(1),
        "the damage was one region and the repair asked for {:?} ranges",
        result["ranges"]
    );
    assert_eq!(
        result["bytes"].as_u64(),
        Some(group),
        "a repair of one megabyte moved {:?} bytes",
        result["bytes"]
    );

    let work = &result["work"];
    let written = work["bytes_written"].as_u64().unwrap();
    let whole = object.len() as u64;
    assert!(
        written >= whole + group && written < whole + group * 2,
        "a repair of one megabyte wrote {written} bytes, where it rebuilds the object it \
         cannot patch in place: the whole {whole} bytes copied forward, the megabyte it \
         fetched, and the tree that covers it"
    );
    assert!(
        work["requests"].as_u64().is_some_and(|count| count <= 2),
        "a repair of one range issued {:?} requests",
        work["requests"]
    );

    let verified = ground.run(&["cache", "verify", "--json"]);
    assert_eq!(
        verified.status.code(),
        Some(0),
        "the repaired object did not verify: {}",
        String::from_utf8_lossy(&verified.stdout)
    );
    assert_eq!(
        std::fs::read(ground.object()).unwrap(),
        object,
        "the repaired object is not the object the source served"
    );
}

#[test]
fn a_repair_reports_unchanged_and_issues_no_body_request_when_nothing_is_damaged() {
    let ground = Ground::serving(large_object());
    ground.fetch();

    let url = ground.url();
    let repaired = ground.run(&["repair", &url, "--json"]);
    assert_eq!(repaired.status.code(), Some(0));
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();

    assert_eq!(result["status"], "unchanged");
    assert_eq!(result["ranges"].as_u64(), Some(0));
    assert_eq!(result["bytes"].as_u64(), Some(0));
}

#[test]
fn damage_at_the_first_group_the_last_group_and_across_a_boundary_is_each_repaired() {
    let object = large_object();
    let group = OUTBOARD_CHUNK_GROUP;
    let last_group_start = (object.len() as u64 / group) * group;
    let cases: [(&str, u64, usize, u64); 4] = [
        ("the first group", 0, 4096, 1),
        ("the last group", last_group_start + 16, 512, 1),
        ("a region spanning two groups", group * 12 - 8, 16, 2),
        ("one flipped bit", group * 31 + 7, 1, 1),
    ];

    for (name, at, length, groups) in cases {
        let ground = Ground::serving(object.clone());
        ground.fetch();
        if length == 1 {
            let held = std::fs::read(ground.object()).unwrap();
            damage(
                &ground.object(),
                at,
                &[held[usize::try_from(at).unwrap()] ^ 0b0000_0100],
            );
        } else {
            damage_region(&ground.object(), at, length);
        }

        let url = ground.url();
        let repaired = ground.run(&["repair", &url, "--json"]);
        assert_eq!(
            repaired.status.code(),
            Some(0),
            "repairing {name} failed: {}",
            String::from_utf8_lossy(&repaired.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
        assert_eq!(result["status"], "repaired", "repairing {name}");
        assert_eq!(
            result["ranges"].as_u64(),
            Some(1),
            "damage in {name} is one contiguous span and asked for {:?} ranges",
            result["ranges"]
        );
        let moved = result["bytes"].as_u64().unwrap();
        let expected = (groups * group).min(object.len() as u64 - (at / group) * group);
        assert!(
            moved <= expected,
            "repairing {name} moved {moved} bytes where at most {expected} cover it"
        );
        assert_eq!(
            std::fs::read(ground.object()).unwrap(),
            object,
            "repairing {name} did not restore the object"
        );
    }
}

#[test]
fn a_truncated_object_is_repaired_by_fetching_only_the_groups_past_the_truncation() {
    let object = large_object();
    let ground = Ground::serving(object.clone());
    ground.fetch();

    let group = OUTBOARD_CHUNK_GROUP;
    let kept = group * 60;
    let path = ground.object();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "truncating an object needs the permissions the platform gives a new file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(kept)
        .unwrap();

    let url = ground.url();
    let repaired = ground.run(&["repair", &url, "--json"]);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "repairing a truncation failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(result["ranges"].as_u64(), Some(1));
    assert_eq!(
        result["bytes"].as_u64(),
        Some(object.len() as u64 - kept),
        "the repair fetched something other than the bytes past the truncation"
    );
    assert_eq!(std::fs::read(&path).unwrap(), object);
}

#[test]
fn a_tree_an_attacker_controls_cannot_make_bad_bytes_verify() {
    let object = large_object();
    let ground = Ground::serving(object.clone());
    ground.fetch();

    let group = OUTBOARD_CHUNK_GROUP;
    let mut damaged = object.clone();
    for byte in &mut damaged[usize::try_from(group * 5).unwrap()..][..4096] {
        *byte ^= 0xFF;
    }
    damage_region(&ground.object(), group * 5, 4096);

    let forged = fetchloom_engine::hashing::Digester::new()
        .hash(
            &fetchloom_engine::pool::Processor::new(
                fetchloom_engine::threads::ThreadBudget::resolve(
                    std::num::NonZeroUsize::new(2).unwrap(),
                    None,
                ),
            )
            .unwrap(),
            std::io::Cursor::new(damaged),
        )
        .unwrap()
        .outboard
        .expect("an object this large has a tree");
    let tree = ground.outboard();
    damage(&tree, 0, forged.as_bytes());

    let verified = ground.run(&["cache", "verify", "--json"]);
    assert_eq!(
        verified.status.code(),
        Some(80),
        "a forged tree let damaged bytes verify: {}",
        String::from_utf8_lossy(&verified.stdout)
    );

    let quarantine = ground.cache().join("quarantine");
    let names: Vec<String> = std::fs::read_dir(&quarantine)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().any(|name| name.ends_with(".diagnosis")),
        "no diagnosis travelled beside the quarantined object: {names:?}"
    );
}

#[test]
fn a_tree_that_does_not_check_out_makes_a_repair_fetch_the_object_whole_and_say_so() {
    let object = large_object();
    let ground = Ground::serving(object.clone());
    ground.fetch();

    damage(&ground.outboard(), 8, &[0xFF; 64]);
    damage_region(&ground.object(), OUTBOARD_CHUNK_GROUP * 3, 4096);

    let url = ground.url();
    let events = ground.scratch.path().join("events.ndjson");
    let repaired = ground.run(&[
        "repair",
        &url,
        "--json",
        "--events",
        events.to_str().unwrap(),
    ]);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "the repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        result["bytes"].as_u64(),
        Some(object.len() as u64),
        "a tree that says nothing about the object still localized the damage"
    );

    let stream = std::fs::read_to_string(&events).unwrap();
    assert!(
        stream.lines().any(|line| line.contains("\"degrade\"")
            && line.contains("does not check out against the digest")),
        "the fall to a whole fetch was not reported: {stream}"
    );
    assert_eq!(std::fs::read(ground.object()).unwrap(), object);
}

#[test]
fn an_object_stored_before_trees_existed_is_repaired_by_fetching_it_whole_and_says_why() {
    let object = large_object();
    let ground = Ground::serving(object.clone());
    ground.fetch();

    std::fs::remove_file(ground.outboard()).unwrap();
    damage_region(&ground.object(), OUTBOARD_CHUNK_GROUP * 7, 4096);

    let url = ground.url();
    let events = ground.scratch.path().join("events.ndjson");
    let repaired = ground.run(&[
        "repair",
        &url,
        "--json",
        "--events",
        events.to_str().unwrap(),
    ]);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "the repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        result["bytes"].as_u64(),
        Some(object.len() as u64),
        "an object with no tree was repaired by range, which nothing could have localized"
    );

    let stream = std::fs::read_to_string(&events).unwrap();
    assert!(
        stream
            .lines()
            .any(|line| line.contains("\"degrade\"") && line.contains("no chunk tree was stored")),
        "the reason the damage could not be narrowed was not reported: {stream}"
    );
    assert_eq!(std::fs::read(ground.object()).unwrap(), object);
}

#[test]
fn cache_repair_builds_a_missing_tree_from_the_objects_own_bytes_and_reaches_no_network() {
    let ground = Ground::serving(large_object());
    ground.fetch();
    std::fs::remove_file(ground.outboard()).unwrap();

    let rebuilt = ground.run(&["cache", "repair", "--json"]);
    assert_eq!(
        rebuilt.status.code(),
        Some(0),
        "cache repair failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&rebuilt.stdout).unwrap();
    assert_eq!(result["trees_rebuilt"].as_u64(), Some(1));
    assert!(
        ground.outboard().is_file(),
        "the tree was reported rebuilt and is not there"
    );

    let repaired = ground.run(&["repair", &ground.url(), "--json"]);
    let after: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        after["status"], "unchanged",
        "the rebuilt tree does not agree with the object it was built from"
    );
}

#[test]
fn cache_repair_leaves_an_object_whose_own_bytes_fail_in_quarantine_and_names_the_command() {
    let ground = Ground::serving(large_object());
    ground.fetch();
    std::fs::remove_file(ground.outboard()).unwrap();
    damage_region(&ground.object(), OUTBOARD_CHUNK_GROUP * 2, 4096);

    let rebuilt = ground.run(&["cache", "repair"]);
    assert_eq!(
        rebuilt.status.code(),
        Some(80),
        "cache repair reported success on an object it cannot repair"
    );
    let printed = String::from_utf8_lossy(&rebuilt.stdout);
    assert!(
        printed.contains("needs a source") && printed.contains("repair"),
        "cache repair did not name the command that can fetch the bytes again: {printed}"
    );
    assert_eq!(
        std::fs::read_dir(ground.cache().join("objects"))
            .unwrap()
            .flatten()
            .count(),
        0,
        "an object that failed its own digest was left where everything has been verified"
    );
}

/// The two policies catch damaged cached bytes at different sites: `always`
/// rereads the object against its outboard tree and names the damaged range,
/// and `fingerprint` finds the object changed since it was published.
#[test]
fn verification_never_passes_on_damaged_content_under_any_policy() {
    for policy in ["always", "fingerprint", "never"] {
        let object = large_object();
        let ground = Ground::serving(object.clone());
        ground.fetch();
        damage_region(&ground.object(), OUTBOARD_CHUNK_GROUP * 9, 4096);

        let second = ground.scratch.path().join(format!("out-{policy}"));
        let url = ground.url();
        let output = ground.run(&[
            "get",
            &url,
            "--output",
            second.to_str().unwrap(),
            "--verify",
            policy,
            "--json",
        ]);

        if policy == "never" {
            assert_eq!(
                output.status.code(),
                Some(0),
                "under never the run stopped, which is a check it promises not to make: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let receipt = receipt_for(&ground, &second);
            assert!(
                receipt.contains("unverified"),
                "a run under never recorded a trust class other than unverified: {receipt}"
            );
            continue;
        }

        assert_ne!(
            output.status.code(),
            Some(0),
            "under {policy} the run served damaged cached bytes as if they were sound: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if policy == "always" {
            "integrity.range_mismatch"
        } else {
            "cache.corrupt"
        };
        assert!(
            said.contains(expected),
            "under {policy} the failure was not {expected}, which is where that policy checks: {said}"
        );
    }
}

#[test]
fn verify_always_names_the_damaged_range_rather_than_only_the_object() {
    let ground = Ground::serving(large_object());
    ground.fetch();
    let at = OUTBOARD_CHUNK_GROUP * 22;
    damage_region(&ground.object(), at, 4096);

    let url = ground.url();
    let output = ground.run(&[
        "get",
        &url,
        "--output",
        ground.scratch.path().join("again").to_str().unwrap(),
        "--verify",
        "always",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&at.to_string()),
        "a full check with a tree failed without naming the damaged range: {stderr}"
    );
}

#[test]
fn a_first_fetch_with_no_prior_digest_is_first_use_and_records_its_own_observation() {
    let ground = Ground::serving(large_object());
    ground.fetch();

    let receipt = std::fs::read_to_string(one_file_in(&ground.cache().join("receipts"))).unwrap();
    assert!(
        receipt.contains("tofu"),
        "a first fetch with no prior digest and no witnesses recorded {receipt}"
    );

    let witnesses = std::fs::read_dir(ground.cache().join("meta").join("witness"))
        .unwrap()
        .flatten()
        .count();
    assert_eq!(
        witnesses, 1,
        "a run that transferred and verified the bytes recorded no observation"
    );
}

#[test]
fn running_twice_against_one_source_never_reaches_corroborated() {
    let ground = Ground::serving(large_object());
    ground.fetch();
    let url = ground.url();
    let again = ground.run(&[
        "get",
        &url,
        "--output",
        ground.scratch.path().join("twice").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(again.status.code(), Some(0));

    for entry in std::fs::read_dir(ground.cache().join("receipts"))
        .unwrap()
        .flatten()
    {
        let receipt = std::fs::read_to_string(entry.path()).unwrap();
        assert!(
            !receipt.contains("corroborated"),
            "one machine fetching one origin twice talked itself into corroborated: {receipt}"
        );
    }
}

#[test]
fn a_warm_run_of_an_unpinned_reference_issues_one_request_and_reads_no_body() {
    let ground = Ground::serving(large_object());
    let first = ground.fetch();
    let cold = first["work"]["requests"].as_u64().unwrap();
    assert!(cold >= 2, "the cold run issued {cold} requests");

    let url = ground.url();
    let warm = ground.run(&[
        "get",
        &url,
        "--output",
        ground.scratch.path().join("out").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        warm.status.code(),
        Some(0),
        "the warm run failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();

    assert_eq!(
        result["work"]["requests"].as_u64(),
        Some(1),
        "a warm run of an unpinned reference issued {:?} requests where one conditional request answers it",
        result["work"]["requests"]
    );
    assert_eq!(
        result["status"], "unchanged",
        "the warm run rewrote a destination that already held the tree"
    );
}

#[test]
fn a_witness_names_the_origin_that_served_the_bytes_and_not_the_one_that_was_asked() {
    let scratch = TempDir::new().unwrap();
    let serving = TestServer::start(
        Script::serving(large_object()).tagged(vec!["\"phase-five\"".to_owned()]),
    )
    .unwrap();
    let mut script = Script::serving(Vec::new());
    script.then = Reply::Redirect {
        code: 307,
        location: format!("{}/object", serving.origin()),
    };
    let redirecting = TestServer::start(script).unwrap();

    let asked = format!("{}/object", redirecting.origin());
    let output = support::fetchloom()
        .current_dir(scratch.path())
        .args([
            "get",
            &asked,
            "--output",
            scratch.path().join("out").to_str().unwrap(),
        ])
        .env("FETCHLOOM_CACHE_DIR", scratch.path().join("cache"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = every_witness(&scratch.path().join("cache").join("meta").join("witness"));
    assert!(
        recorded.contains(serving.origin().trim_start_matches("http://")),
        "the witness named the source that redirected rather than the one that served: {recorded}"
    );
}

fn every_witness(directory: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return String::new();
    };
    let mut found = String::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.push_str(&every_witness(&path));
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            found.push_str(&text);
        }
    }
    found
}

#[test]
fn a_local_reference_can_be_repaired_from_the_file_it_named() {
    let scratch = TempDir::new().unwrap();
    let object = large_object();
    let source = scratch.path().join("object.bin");
    std::fs::write(&source, &object).unwrap();
    let cache = scratch.path().join("cache");

    let run = |arguments: &[&str]| {
        support::fetchloom()
            .current_dir(scratch.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", &cache)
            .env("FETCHLOOM_COMPRESS", "none")
            .output()
            .unwrap()
    };
    let fetched = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        scratch.path().join("out").to_str().unwrap(),
    ]);
    assert_eq!(
        fetched.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );

    let group = OUTBOARD_CHUNK_GROUP;
    damage_region(
        &one_file_in(&cache.join("objects")),
        group * 40,
        usize::try_from(group).unwrap(),
    );

    let repaired = run(&["repair", source.to_str().unwrap(), "--json"]);
    assert_eq!(
        repaired.status.code(),
        Some(0),
        "a local reference could not be repaired: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(result["status"], "repaired");
    assert_eq!(result["ranges"].as_u64(), Some(1));
    assert_eq!(result["bytes"].as_u64(), Some(group));
}
