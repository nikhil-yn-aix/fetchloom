//! Contract tests over reconciling the tree a run resolved against what a
//! destination currently holds.

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

use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::reconcile::{ReconcileOutcome, reconcile};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};

fn file(path: &str, bytes: &[u8]) -> TreeEntry {
    TreeEntry::File {
        path: EntryPath::new(path).unwrap(),
        mode: Mode::ReadWrite,
        size: bytes.len() as u64,
        content: hash_bytes(bytes),
    }
}

fn directory(path: &str) -> TreeEntry {
    TreeEntry::Directory {
        path: EntryPath::new(path).unwrap(),
    }
}

#[test]
fn an_identical_entry_is_unchanged() {
    let resolved = vec![file("a.txt", b"hello")];
    let destination = vec![file("a.txt", b"hello")];
    let outcome = reconcile(&resolved, &destination);
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Unchanged);
}

#[test]
fn a_missing_entry_is_restored() {
    let resolved = vec![file("a.txt", b"hello")];
    let destination: Vec<TreeEntry> = Vec::new();
    let outcome = reconcile(&resolved, &destination);
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Restored);
}

#[test]
fn an_entry_with_different_bytes_is_modified() {
    let resolved = vec![file("a.txt", b"hello")];
    let destination = vec![file("a.txt", b"goodbye")];
    let outcome = reconcile(&resolved, &destination);
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Modified);
}

#[test]
fn a_destination_only_entry_is_foreign() {
    let resolved: Vec<TreeEntry> = Vec::new();
    let destination = vec![file("stray.txt", b"nobody asked for this")];
    let outcome = reconcile(&resolved, &destination);
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Foreign);
}

#[test]
fn a_directory_that_became_a_file_is_modified_not_unchanged() {
    let resolved = vec![directory("data")];
    let destination = vec![file("data", b"not a directory anymore")];
    let outcome = reconcile(&resolved, &destination);
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Modified);
}

#[test]
fn outcomes_are_ordered_by_path_and_every_kind_can_appear_together() {
    let resolved = vec![
        file("a.txt", b"unchanged"),
        file("b.txt", b"resolved bytes"),
        file("m.txt", b"resolved"),
    ];
    let destination = vec![
        file("a.txt", b"unchanged"),
        file("m.txt", b"local bytes"),
        file("z.txt", b"nobody resolved this"),
    ];
    let outcome = reconcile(&resolved, &destination);
    let names: Vec<&str> = outcome.iter().map(|found| found.path.as_str()).collect();
    assert_eq!(names, vec!["a.txt", "b.txt", "m.txt", "z.txt"]);
    assert_eq!(outcome[0].outcome, ReconcileOutcome::Unchanged);
    assert_eq!(outcome[1].outcome, ReconcileOutcome::Restored);
    assert_eq!(outcome[2].outcome, ReconcileOutcome::Modified);
    assert_eq!(outcome[3].outcome, ReconcileOutcome::Foreign);
}

#[test]
fn reconciling_a_large_tree_costs_time_proportional_to_its_size() {
    // Ten times the entries costs about ten times the work when reconciling is
    // linear and about a hundred when it is not. Comparing the two against each
    // other rather than against a fixed number of seconds is what keeps a
    // loaded machine from deciding the answer.
    let timed = |count: usize| {
        let resolved: Vec<TreeEntry> = (0..count)
            .map(|index| file(&format!("data/{index}.bin"), b"same"))
            .collect();
        let destination = resolved.clone();
        let started = std::time::Instant::now();
        let decided = reconcile(&resolved, &destination).len();
        assert_eq!(
            decided, count,
            "reconciling {count} entries decided {decided}"
        );
        started.elapsed()
    };

    let small = timed(10_000).max(std::time::Duration::from_millis(1));
    let large = timed(100_000);
    let grew = large.as_nanos() / small.as_nanos().max(1);
    assert!(
        grew < 40,
        "ten times the entries cost {grew} times the work, which is the growth of comparing every entry against every other rather than looking each one up"
    );
}
