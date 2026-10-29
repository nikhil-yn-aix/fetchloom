//! what every format shares: the limits, stopping, unicode names, objects left behind, and
//! archives made by real tools.

use std::cell::Cell;
use std::path::PathBuf;

use super::samples::{Scratch, compress, tar_of, zip_of};
use super::{Detected, Format, Limits, Unpacked, detect, unpack};
use crate::error::{Error, Kind};

/// the tree hash of the folder every good fixture archives, as python computed it from the files
/// on disk: sha256 of sorted "path\tsize\tsha256\n" lines, paths in NFC.
const FIXTURE_TREE: &str = "8c79dd9ba52babe269e8d1d2183c1c2201f677df8175749fe496f4d37bd0f2c1";

fn fixture(name: &str) -> Result<Unpacked, Error> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archives")
        .join(name);
    let Detected::Archive(format) = detect(&path).unwrap() else {
        panic!("{name} is not detected as an archive");
    };
    let scratch = Scratch::new();
    let unpacked = unpack(&path, format, &Limits::default(), &scratch.cache, &|_| {
        false
    });
    if let Ok(unpacked) = &unpacked {
        for member in &unpacked.members {
            assert!(
                scratch
                    .cache
                    .find(&member.sha256, Some(member.size))
                    .is_some()
            );
        }
    } else {
        scratch.assert_empty();
    }
    unpacked
}

#[test]
fn archives_made_by_real_tools_unpack_to_the_tree_python_computed() {
    for name in [
        "bsdtar.zip",
        "bsdtar.tar.gz",
        "bsdtar.tar.xz",
        "bsdtar.tar.zst",
        "bsdtar.tar.bz2",
        "python.zip",
        "python-nfd.zip",
        "python-zip64-stream.zip",
        "python.tar.gz",
        "python-gnu.tar.bz2",
        "windows-explorer.zip",
        "windows-compress-archive.zip",
    ] {
        let unpacked = fixture(name).unwrap_or_else(|err| panic!("{name}: {}", err.message));
        assert_eq!(unpacked.tree.hex(), FIXTURE_TREE, "{name}");
        assert!(
            unpacked
                .members
                .iter()
                .all(|member| member.path.starts_with("dataset/")),
            "{name}"
        );
    }
}

#[test]
fn the_oldest_tar_format_unpacks() {
    let unpacked = fixture("python-v7.tar").unwrap();
    // python: the same lines over dataset/README.md and dataset/data/train.csv
    assert_eq!(
        unpacked.tree.hex(),
        "aac41d8ac40805957f5260c833a02f89a62665783d5a02ef907b532a205ff4fd"
    );
}

#[test]
fn hostile_archives_made_by_real_tools_are_refused() {
    for (name, reason) in [
        ("bsdtar-encrypted.zip", "because it is encrypted"),
        ("python-escape.zip", "because it climbs out of its folder"),
        ("python-symlink.tar", "because it is a symlink"),
        ("bsdtar-gnu.tar", "a name that is not valid UTF-8"),
    ] {
        let err = fixture(name).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid, "{name}");
        assert!(err.message.contains(reason), "{name}: {}", err.message);
    }
}

/// one format from each family: zip read through its directory, tar read as a stream.
const FAMILIES: [Format; 2] = [Format::Zip, Format::TarZst];

fn archive(format: Format, files: &[(&str, &[u8])]) -> Vec<u8> {
    match format {
        Format::Zip => zip_of(files),
        tar => compress(tar, &tar_of(files)),
    }
}

const FILES: &[(&str, &[u8])] = &[("a.csv", b"1"), ("b.csv", b"2"), ("c/d.csv", b"3")];

#[test]
fn a_ratio_bomb_is_refused() {
    let zeros = vec![0; 10 << 20];
    for format in FAMILIES {
        let bomb = archive(format, &[("zeros.bin", &zeros)]);
        let err = Scratch::new().refuse(format, &bomb);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(err.message.contains("200 times"), "{}", err.message);
    }
}

#[test]
fn the_member_limit_counts_every_entry() {
    let limits = Limits {
        members: 3,
        ..Limits::default()
    };
    for format in FAMILIES {
        let scratch = Scratch::new();
        let fits = archive(format, FILES);
        assert!(
            scratch
                .unpack_with(format, &fits, &limits, &|_| false)
                .is_ok()
        );
        let over = archive(format, &[FILES, &[("e.csv", b"4")]].concat());
        let err = scratch
            .unpack_with(format, &over, &limits, &|_| false)
            .unwrap_err();
        assert!(
            err.message.contains("more than 3 entries"),
            "{}",
            err.message
        );
    }
}

#[test]
fn the_byte_limit_counts_what_is_written() {
    let limits = Limits {
        bytes: 2,
        ..Limits::default()
    };
    for format in FAMILIES {
        let scratch = Scratch::new();
        let err = scratch
            .unpack_with(format, &archive(format, FILES), &limits, &|_| false)
            .unwrap_err();
        assert!(err.message.contains("past 2 B"), "{}", err.message);
        scratch.assert_empty();
    }
}

#[test]
fn a_stop_part_way_publishes_nothing() {
    for format in FAMILIES {
        let checks = Cell::new(0);
        let stopped = |_: u64| {
            checks.set(checks.get() + 1);
            checks.get() > 3
        };
        let scratch = Scratch::new();
        let err = scratch
            .unpack_with(
                format,
                &archive(format, FILES),
                &Limits::default(),
                &stopped,
            )
            .unwrap_err();
        assert_eq!(err.kind, Kind::Interrupted);
        scratch.assert_empty();
    }
}

#[test]
fn names_unpack_in_nfc() {
    for format in FAMILIES {
        let unpacked = Scratch::new()
            .unpack(
                format,
                &archive(format, &[("cafe\u{301}/n\u{303}.csv", b"1")]),
            )
            .unwrap();
        assert_eq!(unpacked.members[0].path, "caf\u{e9}/\u{f1}.csv");
    }
}

/// a tar holds no checksum over its files, so bytes that changed in the cache unpack to another
/// tree without any error. what the archive unpacked to when data.lock pinned it must survive.
#[test]
fn unpacking_again_never_replaces_the_tree_data_lock_pins() {
    let scratch = Scratch::new();
    let archive = crate::cache::tests::store(&scratch.cache, &tar_of(FILES));
    let pinned = scratch
        .cache
        .record_unpacked(
            &archive.digest,
            &[crate::cache::Member {
                path: "a.csv".to_owned(),
                size: 1,
                sha256: crate::digest::sha256(b"0"),
            }],
        )
        .unwrap();
    super::unpack_cached(
        &scratch.cache,
        &archive.digest,
        archive.size,
        Some(&pinned),
        &|_| false,
    )
    .unwrap();
    assert_eq!(scratch.cache.unpacked_tree(&archive.digest), Some(pinned));
}

#[test]
fn names_that_collide_refuse_the_whole_archive() {
    let pairs: [&[(&str, &[u8])]; 2] = [
        &[("Data.csv", b"1"), ("data.csv", b"2")],
        &[("caf\u{e9}.csv", b"1"), ("cafe\u{301}.csv", b"2")],
    ];
    for format in FAMILIES {
        for files in pairs {
            let err = Scratch::new().refuse(format, &archive(format, files));
            assert_eq!(err.kind, Kind::SourceInvalid, "{files:?}");
        }
    }
}

/// times unpacking 20,000 files of 4 KiB, where creating files costs more than inflating them:
/// `cargo test --release --lib archive::tests::many_small_files -- --ignored --nocapture`
#[test]
#[ignore = "writes 20,000 files"]
fn many_small_files() {
    let row = b"0.125,3.5,17,2026-10-28,ok\n".repeat(152);
    let names: Vec<String> = (0..20_000)
        .map(|index| format!("corpus/{:03}/{index:06}.csv", index % 100))
        .collect();
    let files: Vec<(&str, &[u8])> = names.iter().map(|name| (name.as_str(), &row[..])).collect();
    let zip = zip_of(&files);
    for round in 0..3 {
        let scratch = Scratch::new();
        let path = scratch.file("small.zip", &zip);
        let started = std::time::Instant::now();
        let unpacked = unpack(
            &path,
            Format::Zip,
            &Limits::default(),
            &scratch.cache,
            &|_| false,
        )
        .unwrap();
        println!(
            "round {round}: {} files in {} ms",
            unpacked.members.len(),
            started.elapsed().as_millis()
        );
    }
}

#[test]
fn only_names_decide_what_may_unpack() {
    for archive in [
        "a.zip",
        "dir/A.ZIP",
        "x.tar",
        "x.tar.gz",
        "x.tgz",
        "x.tar.xz",
        "x.txz",
        "x.tar.zst",
        "x.tzst",
        "x.tar.bz2",
        "x.tbz2",
        "x.tbz",
    ] {
        assert_eq!(
            super::by_name(archive),
            Some(super::Packed::Archive),
            "{archive}"
        );
    }
    assert_eq!(
        super::by_name("a.7z"),
        Some(super::Packed::Unsupported("7z"))
    );
    assert_eq!(
        super::by_name("a.RAR"),
        Some(super::Packed::Unsupported("rar"))
    );
    for whole in [
        "reads.fastq.gz",
        "data.csv",
        "results.xlsx",
        "weights.npz",
        "model.pt",
        "zip",
        "a.zip.part",
    ] {
        assert_eq!(super::by_name(whole), None, "{whole}");
    }
}

#[test]
fn a_stem_drops_the_archive_ending() {
    assert_eq!(super::stem("train.tar.gz"), "train");
    assert_eq!(super::stem("Images.ZIP"), "Images");
    assert_eq!(super::stem("x.tgz"), "x");
    assert_eq!(super::stem(".zip"), ".zip");
    assert_eq!(super::stem("notes.txt"), "notes.txt");
}
