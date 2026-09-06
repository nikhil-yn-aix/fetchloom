//! Contract tests over the three way compare a run makes when the record it
//! wrote is a merge base and upstream has moved since.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::merge::{Resolution, merge};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};

fn file(path: &str, bytes: &[u8]) -> TreeEntry {
    TreeEntry::File {
        path: EntryPath::new(path).unwrap(),
        mode: Mode::ReadWrite,
        size: bytes.len() as u64,
        content: hash_bytes(bytes),
    }
}

fn link(path: &str, target: &[u8]) -> TreeEntry {
    TreeEntry::Symlink {
        path: EntryPath::new(path).unwrap(),
        size: target.len() as u64,
        content: hash_bytes(target),
    }
}

fn directory(path: &str) -> TreeEntry {
    TreeEntry::Directory {
        path: EntryPath::new(path).unwrap(),
    }
}

fn resolution_of(merged: &[fetchloom_engine::merge::Merged], path: &str) -> Resolution {
    merged
        .iter()
        .find(|found| found.path.as_str() == path)
        .unwrap_or_else(|| panic!("nothing was decided for {path}"))
        .resolution
}

#[test]
fn an_entry_neither_side_touched_is_unchanged() {
    let base = vec![file("a.txt", b"one")];
    let merged = merge(&base, &base.clone(), &base.clone());
    assert_eq!(merged.len(), 1);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::Unchanged);
}

#[test]
fn an_entry_only_upstream_changed_takes_upstream() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"one")];
    let upstream = vec![file("a.txt", b"two")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::TakeUpstream);
}

#[test]
fn an_entry_only_you_changed_keeps_yours() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"mine")];
    let upstream = vec![file("a.txt", b"one")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::KeepYours);
}

#[test]
fn an_entry_both_sides_changed_differently_conflicts() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"mine")];
    let upstream = vec![file("a.txt", b"two")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::Conflict);
}

#[test]
fn an_entry_both_sides_changed_to_the_same_bytes_is_not_a_conflict() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"two")];
    let upstream = vec![file("a.txt", b"two")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::Unchanged);
}

#[test]
fn an_entry_you_deleted_and_upstream_changed_conflicts() {
    let base = vec![file("a.txt", b"one")];
    let yours = Vec::new();
    let upstream = vec![file("a.txt", b"two")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::Conflict);
}

#[test]
fn an_entry_you_deleted_and_upstream_left_alone_stays_deleted() {
    let base = vec![file("a.txt", b"one")];
    let yours = Vec::new();
    let upstream = vec![file("a.txt", b"one")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::StaysDeleted);
}

#[test]
fn an_entry_upstream_deleted_and_you_left_alone_takes_upstream() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"one")];
    let upstream = Vec::new();
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::TakeUpstream);
}

#[test]
fn an_entry_upstream_deleted_and_you_changed_conflicts() {
    let base = vec![file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"mine")];
    let upstream = Vec::new();
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::Conflict);
}

#[test]
fn an_entry_both_sides_deleted_stays_deleted() {
    let base = vec![file("a.txt", b"one")];
    let merged = merge(&base, &Vec::new(), &Vec::new());
    assert_eq!(resolution_of(&merged, "a.txt"), Resolution::StaysDeleted);
}

#[test]
fn an_entry_you_added_keeps_yours() {
    let yours = vec![file("mine.txt", b"mine")];
    let merged = merge(&Vec::new(), &yours, &Vec::new());
    assert_eq!(resolution_of(&merged, "mine.txt"), Resolution::KeepYours);
}

#[test]
fn an_entry_upstream_added_takes_upstream() {
    let upstream = vec![file("new.txt", b"new")];
    let merged = merge(&Vec::new(), &Vec::new(), &upstream);
    assert_eq!(resolution_of(&merged, "new.txt"), Resolution::TakeUpstream);
}

#[test]
fn one_path_added_by_both_sides_with_different_bytes_conflicts() {
    let yours = vec![file("new.txt", b"mine")];
    let upstream = vec![file("new.txt", b"theirs")];
    let merged = merge(&Vec::new(), &yours, &upstream);
    assert_eq!(resolution_of(&merged, "new.txt"), Resolution::Conflict);
}

#[test]
fn one_path_added_by_both_sides_with_the_same_bytes_is_unchanged() {
    let yours = vec![file("new.txt", b"same")];
    let upstream = vec![file("new.txt", b"same")];
    let merged = merge(&Vec::new(), &yours, &upstream);
    assert_eq!(resolution_of(&merged, "new.txt"), Resolution::Unchanged);
}

#[test]
fn a_symlink_whose_target_both_sides_moved_conflicts() {
    let base = vec![link("here", b"a.txt")];
    let yours = vec![link("here", b"mine.txt")];
    let upstream = vec![link("here", b"theirs.txt")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "here"), Resolution::Conflict);
}

#[test]
fn a_path_that_is_a_file_on_one_side_and_a_directory_on_the_other_conflicts() {
    let base = vec![file("thing", b"one")];
    let yours = vec![directory("thing")];
    let upstream = vec![file("thing", b"two")];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "thing"), Resolution::Conflict);
}

#[test]
fn a_mode_upstream_changed_takes_upstream() {
    let base = vec![file("run.sh", b"#!/bin/sh\n")];
    let yours = base.clone();
    let upstream = vec![TreeEntry::File {
        path: EntryPath::new("run.sh").unwrap(),
        mode: Mode::Executable,
        size: 10,
        content: hash_bytes(b"#!/bin/sh\n"),
    }];
    let merged = merge(&base, &yours, &upstream);
    assert_eq!(resolution_of(&merged, "run.sh"), Resolution::TakeUpstream);
}

#[test]
fn every_path_any_side_names_is_decided_exactly_once_and_in_canonical_order() {
    let base = vec![file("b.txt", b"one"), file("a.txt", b"one")];
    let yours = vec![file("a.txt", b"mine"), file("c.txt", b"mine")];
    let upstream = vec![file("b.txt", b"two"), file("d.txt", b"new")];
    let merged = merge(&base, &yours, &upstream);
    let named: Vec<&str> = merged.iter().map(|found| found.path.as_str()).collect();
    assert_eq!(named, vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
}
