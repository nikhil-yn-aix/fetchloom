//! One lookup answers where an object's bytes are, and every reader uses it.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use fetchloom_faults as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;

mod support;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use fetchloom_cache::storage::Placement;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::PACK_THRESHOLD;

use support::{bytes_of, cache, publish};

#[test]
fn an_object_the_cache_holds_reads_back_as_the_bytes_that_were_published() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(4096, 11);
    let digest = publish(&held, &bytes);

    let mut reader = held.read(digest).unwrap();
    assert_eq!(reader.length(), bytes.len() as u64);
    let mut read = Vec::new();
    reader.read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes);
}

#[test]
fn a_reader_seeks_within_the_object_and_never_outside_it() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(4096, 12);
    let digest = publish(&held, &bytes);

    let mut reader = held.read(digest).unwrap();
    assert_eq!(reader.seek(SeekFrom::Start(4000)).unwrap(), 4000);
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, bytes[4000..]);

    assert_eq!(reader.seek(SeekFrom::End(-16)).unwrap(), 4080);
    let mut last = Vec::new();
    reader.read_to_end(&mut last).unwrap();
    assert_eq!(last.len(), 16);
}

#[test]
fn an_object_the_cache_does_not_hold_is_absent_from_every_answer() {
    let (_scratch, held) = cache();
    let missing = hash_bytes(b"nothing was ever published for this");

    assert!(held.placement(missing).is_none());
    assert!(!held.holds(missing));
    assert_eq!(held.size_of(missing), None);
    assert!(held.read(missing).is_err());
    assert!(held.fingerprint_of(missing).is_err());
}

#[test]
fn the_size_the_lookup_reports_is_the_length_of_the_object() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(1 << 16, 13);
    let digest = publish(&held, &bytes);

    assert_eq!(held.size_of(digest), Some(bytes.len() as u64));
}

#[test]
fn nothing_outside_the_lookup_turns_a_digest_into_a_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut offenders = Vec::new();
    for crate_name in ["cache", "cli"] {
        let directory = root.join("crates").join(crate_name).join("src");
        let mut files = Vec::new();
        collect(&directory, &mut files);
        for file in files {
            if file.file_name().is_some_and(|name| name == "storage.rs")
                || file.file_name().is_some_and(|name| name == "layout.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for (number, line) in text.lines().enumerate() {
                if line.contains(".object(") && !line.contains("fn object(") {
                    offenders.push(format!("{}:{}", file.display(), number + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these turn a digest into a path themselves instead of asking the one lookup: {offenders:#?}"
    );
}

fn collect(directory: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
            into.push(path);
        }
    }
}

#[test]
fn ingesting_a_small_file_costs_fewer_file_operations_than_giving_it_one() {
    let (scratch, held) = cache();
    let mut published = Vec::new();
    let before = held.work().taken().file_operations;
    for seed in 0..64u8 {
        let path = scratch.path().join(format!("source-{seed}.bin"));
        std::fs::write(&path, bytes_of(4096, seed)).unwrap();
        published.push(held.ingest(&path).unwrap().digest);
    }
    let each = (held.work().taken().file_operations - before) / 64;

    assert!(
        each < 3,
        "ingesting a small file still costs {each} file operations, which is what packing is for"
    );
    for digest in published {
        assert!(held.holds(digest));
    }
}

#[test]
fn an_object_above_the_threshold_keeps_a_file_of_its_own() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(usize::try_from(PACK_THRESHOLD).unwrap() + 1, 21);
    let digest = publish(&held, &bytes);

    assert!(
        matches!(held.placement(digest), Some(Placement::Loose(_))),
        "a large object was packed"
    );
}

#[test]
fn a_packed_object_and_a_loose_one_read_back_identically() {
    let (_scratch, held) = cache();
    let small = bytes_of(4096, 22);
    let large = bytes_of(usize::try_from(PACK_THRESHOLD).unwrap() + 1, 23);
    let packed = publish(&held, &small);
    let loose = publish(&held, &large);

    assert!(matches!(
        held.placement(packed),
        Some(Placement::Packed { .. })
    ));
    assert!(matches!(held.placement(loose), Some(Placement::Loose(_))));

    let mut read = Vec::new();
    held.read(packed).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, small);
    assert_eq!(held.size_of(packed), Some(small.len() as u64));

    read.clear();
    held.read(loose).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, large);
}

#[test]
fn several_packed_objects_in_one_container_each_read_back_as_their_own_bytes() {
    let (_scratch, held) = cache();
    let written: Vec<Vec<u8>> = (0..16u8).map(|seed| bytes_of(1024, seed + 40)).collect();
    let digests: Vec<_> = written.iter().map(|bytes| publish(&held, bytes)).collect();

    let containers: std::collections::BTreeSet<PathBuf> = digests
        .iter()
        .map(|digest| held.placement(*digest).unwrap().container().to_path_buf())
        .collect();
    assert_eq!(
        containers.len(),
        1,
        "sixteen small objects went into {} containers",
        containers.len()
    );

    for (digest, expected) in digests.iter().zip(&written) {
        let mut read = Vec::new();
        held.read(*digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(&read, expected);
    }
}

#[test]
fn removing_a_packed_object_leaves_the_others_readable() {
    let (_scratch, held) = cache();
    let kept: Vec<Vec<u8>> = (0..8u8).map(|seed| bytes_of(2048, seed + 60)).collect();
    let digests: Vec<_> = kept.iter().map(|bytes| publish(&held, bytes)).collect();

    held.remove_object(digests[3]).unwrap();
    assert!(!held.holds(digests[3]));

    for (index, digest) in digests.iter().enumerate() {
        if index == 3 {
            continue;
        }
        let mut read = Vec::new();
        held.read(*digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(&read, &kept[index]);
    }
}
