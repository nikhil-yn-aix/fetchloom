//! The laws the engine holds over every input, checked by generating the inputs
//! rather than by naming a few of them.

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

use std::cmp::Ordering;

use fetchloom_engine::canonical::{compare, tree_digest};
use fetchloom_engine::compression::{shuffle, unshuffle};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::merge::{Resolution, merge};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};

const STRIDES: [u8; 4] = [1, 2, 4, 8];

fn bytes_of(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u8::try_from(state >> 56).unwrap_or(0)
        })
        .collect()
}

#[test]
fn unshuffling_a_shuffled_object_returns_the_bytes_that_went_in() {
    for stride in STRIDES {
        for length in [0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 63, 64, 65, 4096, 4097] {
            let bytes = bytes_of(length, u64::from(stride) * 977 + length as u64);
            assert_eq!(
                unshuffle(&shuffle(&bytes, stride), stride),
                bytes,
                "stride {stride} over {length} bytes did not round trip"
            );
        }
    }
}

#[test]
fn shuffling_moves_bytes_and_never_adds_or_drops_one() {
    for stride in STRIDES {
        for length in [1, 5, 8, 13, 64, 4097] {
            let bytes = bytes_of(length, u64::from(stride) + 3);
            let moved = shuffle(&bytes, stride);
            assert_eq!(moved.len(), bytes.len(), "stride {stride} changed a length");
            let mut before = bytes.clone();
            let mut after = moved;
            before.sort_unstable();
            after.sort_unstable();
            assert_eq!(
                before, after,
                "stride {stride} over {length} bytes is not a permutation of them"
            );
        }
    }
}

fn file(path: &str, bytes: &[u8]) -> TreeEntry {
    TreeEntry::File {
        path: EntryPath::new(path).unwrap(),
        mode: Mode::ReadWrite,
        size: bytes.len() as u64,
        content: hash_bytes(bytes),
    }
}

#[derive(Clone, Copy)]
enum Side {
    Absent,
    Base,
    Changed,
    ChangedAgain,
}

impl Side {
    fn entry(self, path: &str) -> Option<TreeEntry> {
        match self {
            Self::Absent => None,
            Self::Base => Some(file(path, b"base")),
            Self::Changed => Some(file(path, b"yours")),
            Self::ChangedAgain => Some(file(path, b"upstreams")),
        }
    }
}

const SIDES: [Side; 4] = [Side::Absent, Side::Base, Side::Changed, Side::ChangedAgain];

#[test]
fn every_state_three_sides_can_be_in_is_decided_exactly_once_and_the_same_way_twice() {
    let mut decided = 0;
    for base in SIDES {
        for yours in SIDES {
            for upstream in SIDES {
                let path = "entry.bin";
                let base: Vec<TreeEntry> = base.entry(path).into_iter().collect();
                let yours: Vec<TreeEntry> = yours.entry(path).into_iter().collect();
                let upstream: Vec<TreeEntry> = upstream.entry(path).into_iter().collect();
                if base.is_empty() && yours.is_empty() && upstream.is_empty() {
                    assert!(merge(&base, &yours, &upstream).is_empty());
                    continue;
                }
                let merged = merge(&base, &yours, &upstream);
                assert_eq!(
                    merged.len(),
                    1,
                    "one path was decided {} times",
                    merged.len()
                );
                assert_eq!(merged[0].path.as_str(), path);
                let again = merge(&base, &yours, &upstream);
                assert_eq!(
                    merged[0].resolution, again[0].resolution,
                    "the same three sides were decided two ways"
                );
                decided += 1;
            }
        }
    }
    assert_eq!(decided, SIDES.len().pow(3) - 1, "a state was never decided");
}

#[test]
fn a_state_where_neither_side_moved_from_the_base_is_never_a_conflict() {
    for base in [Side::Base, Side::Absent] {
        for upstream in SIDES {
            let path = "entry.bin";
            let held: Vec<TreeEntry> = base.entry(path).into_iter().collect();
            let upstream: Vec<TreeEntry> = upstream.entry(path).into_iter().collect();
            let merged = merge(&held, &held.clone(), &upstream);
            for decided in merged {
                assert_ne!(
                    decided.resolution,
                    Resolution::Conflict,
                    "a side that did not move conflicted with upstream"
                );
            }
        }
    }
}

#[test]
fn canonical_order_is_the_order_of_the_bytes_and_not_of_the_letters() {
    for (left, right) in [
        (&b"B"[..], &b"a"[..]),
        (&b"Z"[..], &b"a"[..]),
        (&b"a"[..], &b"ab"[..]),
        (&b"a/b"[..], &b"a0"[..]),
    ] {
        assert_eq!(
            compare(left, right),
            Ordering::Less,
            "{} did not sort before {} by its bytes, so the entry stream is ordered by something else",
            String::from_utf8_lossy(left),
            String::from_utf8_lossy(right)
        );
    }
}

#[test]
fn a_tree_digest_is_the_same_whatever_order_its_entries_arrive_in() {
    let entries = vec![
        file("b.txt", b"two"),
        file("a/b.txt", b"one"),
        file("a.txt", b"three"),
        file("z/y/x.txt", b"four"),
    ];
    let expected = tree_digest(&entries);
    let mut order: Vec<usize> = (0..entries.len()).collect();
    let mut permutations = 0;
    permute(&mut order, 0, &mut |arrangement| {
        let shuffled: Vec<TreeEntry> = arrangement
            .iter()
            .map(|index| entries[*index].clone())
            .collect();
        assert_eq!(
            tree_digest(&shuffled),
            expected,
            "one tree digested two ways depending on the order it was given in"
        );
        permutations += 1;
    });
    assert_eq!(permutations, 24, "not every arrangement was tried");
}

fn permute(order: &mut Vec<usize>, at: usize, each: &mut impl FnMut(&[usize])) {
    if at == order.len() {
        each(order);
        return;
    }
    for index in at..order.len() {
        order.swap(at, index);
        permute(order, at + 1, each);
        order.swap(at, index);
    }
}

#[test]
fn two_trees_that_differ_anywhere_digest_differently() {
    let base = vec![
        file("a.txt", b"one"),
        TreeEntry::Directory {
            path: EntryPath::new("d").unwrap(),
        },
    ];
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(format!("{}", tree_digest(&base)));

    let variants: Vec<(&str, Vec<TreeEntry>)> = vec![
        ("nothing at all", Vec::new()),
        ("one entry fewer", vec![file("a.txt", b"one")]),
        (
            "one entry more",
            vec![
                file("a.txt", b"one"),
                file("b.txt", b"two"),
                TreeEntry::Directory {
                    path: EntryPath::new("d").unwrap(),
                },
            ],
        ),
        (
            "a path renamed",
            vec![
                file("A.txt", b"one"),
                TreeEntry::Directory {
                    path: EntryPath::new("d").unwrap(),
                },
            ],
        ),
        (
            "content changed",
            vec![
                file("a.txt", b"two"),
                TreeEntry::Directory {
                    path: EntryPath::new("d").unwrap(),
                },
            ],
        ),
        (
            "a mode changed",
            vec![
                TreeEntry::File {
                    path: EntryPath::new("a.txt").unwrap(),
                    mode: Mode::Executable,
                    size: 3,
                    content: hash_bytes(b"one"),
                },
                TreeEntry::Directory {
                    path: EntryPath::new("d").unwrap(),
                },
            ],
        ),
        (
            "a size that lies",
            vec![
                TreeEntry::File {
                    path: EntryPath::new("a.txt").unwrap(),
                    mode: Mode::ReadWrite,
                    size: 4,
                    content: hash_bytes(b"one"),
                },
                TreeEntry::Directory {
                    path: EntryPath::new("d").unwrap(),
                },
            ],
        ),
        (
            "a directory that became a symlink",
            vec![
                file("a.txt", b"one"),
                TreeEntry::Symlink {
                    path: EntryPath::new("d").unwrap(),
                    size: 3,
                    content: hash_bytes(b"one"),
                },
            ],
        ),
    ];

    for (what, entries) in variants {
        assert!(
            seen.insert(format!("{}", tree_digest(&entries))),
            "a tree with {what} digested as one already seen"
        );
    }
    assert_eq!(seen.len(), 9, "two variants collided without being noticed");
}

#[test]
fn every_resolution_the_merge_table_names_is_written_as_its_own_row() {
    for (resolution, written) in [
        (Resolution::Unchanged, "\"unchanged\""),
        (Resolution::TakeUpstream, "\"take_upstream\""),
        (Resolution::KeepYours, "\"keep_yours\""),
        (Resolution::StaysDeleted, "\"stays_deleted\""),
        (Resolution::Conflict, "\"conflict\""),
    ] {
        assert_eq!(
            serde_json::to_string(&resolution).unwrap(),
            written,
            "{resolution:?} is written into an artifact as something the merge table does not name"
        );
    }
}
