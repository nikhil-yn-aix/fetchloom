//! what every format shares: the limits, stopping, unicode names, objects left behind, and
//! archives made by real tools.

use std::cell::Cell;

use super::guard::{Guard, Meter};
use super::samples::{Scratch, compress, seven_zip, tar_of, zip_of, zstd_asking_for};
use super::{Detected, Format, Input, Limits, System, Unpacked, detect, unpack};
use crate::error::{Error, Kind};

/// the tree hash of the folder every good fixture archives, as python computed it from the files
/// on disk: sha256 of sorted "path\tsize\tsha256\n" lines, paths in NFC.
const FIXTURE_TREE: &str = "8c79dd9ba52babe269e8d1d2183c1c2201f677df8175749fe496f4d37bd0f2c1";

fn fixture(name: &str) -> Result<Unpacked, Error> {
    let path = crate::test_paths::workspace()
        .join("tests/fixtures/archives")
        .join(name);
    let input = Input::File(path);
    let Detected::Archive(format) = detect(&input).unwrap() else {
        panic!("{name} is not detected as an archive");
    };
    let scratch = Scratch::new();
    let unpacked = unpack(
        &input,
        format,
        &Limits::default(),
        &scratch.cache,
        &|_| false,
        None,
    );
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
        ("sevenz-rust2-encrypted.7z", "the archive is encrypted"),
        (
            "sevenz-rust2-encrypted-headers.7z",
            "the archive is encrypted",
        ),
        ("python-escape.zip", "because it climbs out of its folder"),
        ("python-symlink.tar", "because it is a symlink"),
        ("bsdtar-gnu.tar", "a name that is not valid UTF-8"),
    ] {
        let err = fixture(name).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid, "{name}");
        assert!(err.message.contains(reason), "{name}: {}", err.message);
    }
}

/// one format from each family: zip read through its directory, tar read as a stream, and 7z
/// read block by block.
const FAMILIES: [Format; 3] = [Format::Zip, Format::TarZst, Format::SevenZ];

fn archive(format: Format, files: &[(&str, &[u8])]) -> Vec<u8> {
    match format {
        Format::Zip => zip_of(files),
        Format::SevenZ => seven_zip(files),
        tar => compress(tar, &tar_of(files)),
    }
}

const FILES: &[(&str, &[u8])] = &[("a.csv", b"1"), ("b.csv", b"2"), ("c/d.csv", b"3")];

#[test]
fn large_archives_with_modest_expansion_unpack() {
    let mut bytes = vec![0; 5 << 20];
    fastrand::Rng::with_seed(19).fill(&mut bytes);
    for format in FAMILIES {
        let packed = archive(format, &[("payload.bin", &bytes)]);
        assert!(packed.len() > bytes.len() / 200);
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(format, &packed).unwrap();
        assert_eq!(unpacked.members.len(), 1);
        scratch.assert_stored(&unpacked.members[0], &bytes);
    }
}

/// offsets where `pattern` starts in `bytes`.
fn found(bytes: &[u8], pattern: &[u8]) -> Vec<usize> {
    let windows = bytes.windows(pattern.len()).enumerate();
    windows
        .filter(|(_, window)| *window == pattern)
        .map(|(at, _)| at)
        .collect()
}

/// an archive cut anywhere into parts, a zip inside a local header or its central directory
/// too, unpacks from them to what it unpacks to whole.
#[test]
fn an_archive_cut_into_parts_unpacks_as_it_does_whole() {
    let files: &[(&str, &[u8])] = &[
        ("data/a.csv", b"x,y\n1,2\n"),
        ("data/b.csv", b"x,y\n3,4\n"),
        ("README.md", b"# readme\n"),
    ];
    for format in [Format::Zip, Format::TarGz, Format::TarZst, Format::SevenZ] {
        let bytes = archive(format, files);
        let whole = Scratch::new().unpack(format, &bytes).unwrap();
        let mut cuts = vec![vec![bytes.len() / 2], (1..bytes.len()).collect()];
        if format == Format::Zip {
            let local = found(&bytes, b"PK\x03\x04")[1] + 10;
            let central = found(&bytes, b"PK\x01\x02")[1] + 20;
            cuts.extend([vec![local], vec![central], vec![local, central]]);
        }
        for cut in cuts {
            let scratch = Scratch::new();
            let input = scratch.parts(&bytes, &cut);
            assert_eq!(detect(&input).unwrap(), Detected::Archive(format));
            let unpacked = unpack(
                &input,
                format,
                &Limits::default(),
                &scratch.cache,
                &|_| false,
                None,
            )
            .unwrap();
            assert_eq!(unpacked.members, whole.members, "{format:?} cut at {cut:?}");
            for member in &unpacked.members {
                let (_, bytes) = files.iter().find(|(path, _)| *path == member.path).unwrap();
                scratch.assert_stored(member, bytes);
            }
        }
    }
}

/// the ratio counts each byte of every part once: a split archive well inside the ratio unpacks,
/// though counting only one part would put it far past, and rereads of a zip's shared data still
/// cannot raise the count past the parts' total.
#[test]
fn the_ratio_counts_every_part_once() {
    let mut rng = fastrand::Rng::with_seed(7);
    let digits: Vec<u8> = (0..5 << 20).map(|_| b'0' + rng.u8(..10)).collect();
    for format in FAMILIES {
        let bytes = archive(format, &[("digits.txt", &digits)]);
        let limits = Limits {
            ratio: 3 * (digits.len() as u64).div_ceil(bytes.len() as u64),
            ..Limits::default()
        };
        let tenth = bytes.len() / 10;
        let cuts: Vec<usize> = (1..10).map(|part| part * tenth).collect();
        let scratch = Scratch::new();
        let input = scratch.parts(&bytes, &cuts);
        let unpacked = unpack(&input, format, &limits, &scratch.cache, &|_| false, None);
        assert!(unpacked.is_ok(), "{format:?}: {:?}", unpacked.err());
    }
    let shared = super::zip::tests::overlapping(512);
    let limits = Limits {
        ratio: 200,
        ..Limits::default()
    };
    let scratch = Scratch::new();
    let third = shared.len() / 3;
    let input = scratch.parts(&shared, &[third, 2 * third]);
    let err = unpack(
        &input,
        Format::Zip,
        &limits,
        &scratch.cache,
        &|_| false,
        Some(0),
    )
    .unwrap_err();
    assert!(err.message.contains("200 times"), "{}", err.message);
    scratch.assert_empty();
}

#[test]
fn a_ratio_bomb_is_refused() {
    // deflate cannot pass about 1032 times, so a ratio below that catches every family
    let limits = Limits {
        ratio: 200,
        ..Limits::default()
    };
    let zeros = vec![0; 10 << 20];
    for format in FAMILIES {
        let bomb = archive(format, &[("zeros.bin", &zeros)]);
        let scratch = Scratch::new();
        let err = scratch
            .unpack_with(format, &bomb, &limits, &|_| false)
            .unwrap_err();
        scratch.assert_empty();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(err.message.contains("200 times"), "{}", err.message);
        assert!(
            err.help
                .as_deref()
                .is_some_and(|help| help.contains("FETCHLOOM_ARCHIVE_RATIO")),
            "{:?}",
            err.help
        );
    }
}

#[test]
fn the_default_limits_let_big_archives_through_and_stop_bombs() {
    let limits = Limits::default();
    assert_eq!(limits.members, 10_000_000);
    assert_eq!(limits.bytes, 16 << 40);
    assert_eq!(limits.ratio, 1_000);
    let zeros = vec![0; 32 << 20];
    for format in [Format::TarBz2, Format::TarXz, Format::TarZst] {
        let bomb = archive(format, &[("zeros.bin", &zeros)]);
        let err = Scratch::new().refuse(format, &bomb);
        assert!(
            err.message.contains("1000 times"),
            "{format:?}: {}",
            err.message
        );
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
        None,
    )
    .unwrap();
    assert_eq!(scratch.cache.unpacked_tree(&archive.digest), Some(pinned));
}

/// an archive whose unpacking cannot get a folder in the cache, or cannot save the list of files
/// it holds there, names the cache folder rather than fetchloom's own steps.
#[test]
fn unpacking_that_cannot_write_to_the_cache_names_the_cache_folder() {
    for (folder, expected) in [
        (
            "tmp",
            "could not make a folder to unpack the archive into, in ",
        ),
        ("unpacked", "could not save the archive's list of files in "),
    ] {
        let scratch = Scratch::new();
        let archive = crate::cache::tests::store(&scratch.cache, &tar_of(FILES));
        let blocked = scratch.cache.root().join(folder);
        let _ = std::fs::remove_dir_all(&blocked);
        std::fs::write(&blocked, b"a file where a folder belongs").unwrap();
        let err = super::unpack_cached(
            &scratch.cache,
            &archive.digest,
            archive.size,
            None,
            &|_| false,
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind, Kind::LocalIo, "{folder}");
        assert!(
            err.message
                .starts_with(&format!("{expected}{}", blocked.display())),
            "{folder}: {}",
            err.message
        );
    }
}

/// `zstd --long=28` and above write frames asking for more than libzstd's 128 MiB window. such a
/// tar must be refused by unpacking, never left packed as if it were no archive.
#[test]
fn a_tar_zst_asking_for_a_large_window_is_refused_not_left_packed() {
    let scratch = Scratch::new();
    let packed = crate::cache::tests::store(&scratch.cache, &zstd_asking_for(28, &tar_of(FILES)));
    let err = super::unpack_cached(
        &scratch.cache,
        &packed.digest,
        packed.size,
        None,
        &|_| false,
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, Kind::SourceInvalid);
    assert_eq!(
        err.message,
        "the archive's zstd stream needs a window larger than 128.0 MiB to unpack"
    );
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
        "x.7z",
    ] {
        assert_eq!(
            super::by_name(archive),
            Some(super::Packed::Archive),
            "{archive}"
        );
    }
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
fn a_rar_left_packed_has_a_next_step_for_each_system() {
    let step =
        |system| super::unpack_elsewhere(system, "data/sample/sample.rar", "", "extract = false");
    assert_eq!(
        step(System::Linux),
        "unpack it with unrar from your package manager: `unrar x data/sample/sample.rar sample-unpacked/`. extract = false stops this warning"
    );
    assert_eq!(
        step(System::Macos),
        "unpack it with unar: `brew install unar`, then `unar -D -o sample-unpacked data/sample/sample.rar`. extract = false stops this warning"
    );
    assert_eq!(
        step(System::Windows),
        r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x data\sample\sample.rar -osample-unpacked`. extract = false stops this warning"#
    );
}

/// a zip spanned over `.z01` files stays packed, and the next step joins its parts with zip, or
/// on windows unpacks them with 7-Zip, which reads them all from the `.zip`.
#[test]
fn a_spanned_zip_left_packed_has_a_next_step_for_each_system() {
    let step =
        |system| super::join_elsewhere(system, "data/scans/scans.zip", "", "extract = false");
    assert_eq!(
        step(System::Linux),
        "join its parts with zip from your package manager, then unpack them: `zip -s 0 data/scans/scans.zip --out scans-joined.zip`, then `unzip scans-joined.zip -d scans-unpacked`. extract = false stops this warning"
    );
    assert_eq!(
        step(System::Macos),
        "join its parts with the zip macos has, then unpack them: `zip -s 0 data/scans/scans.zip --out scans-joined.zip`, then `unzip scans-joined.zip -d scans-unpacked`. extract = false stops this warning"
    );
    assert_eq!(
        step(System::Windows),
        r#"unpack it with 7-Zip, which reads its parts: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x data\scans\scans.zip -oscans-unpacked`. extract = false stops this warning"#
    );
}

/// run below the project's folder, the command reaches the archive and the project's folder from
/// there, and quotes what its shell would split or expand as that shell reads it.
#[test]
fn a_next_step_runs_as_typed_where_fetchloom_ran() {
    let step =
        |system| super::unpack_elsewhere(system, "../data/my data/it's.RAR", "../", "--no-extract");
    assert_eq!(
        step(System::Linux),
        r"unpack it with unrar from your package manager: `unrar x '../data/my data/it'\''s.RAR' '../it'\''s-unpacked/'`. --no-extract stops this warning"
    );
    assert_eq!(
        step(System::Windows),
        r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x '..\data\my data\it''s.RAR' '-o..\it''s-unpacked'`. --no-extract stops this warning"#
    );
}

#[test]
fn a_stem_drops_the_archive_ending() {
    assert_eq!(super::stem("train.tar.gz"), "train");
    assert_eq!(super::stem("Images.ZIP"), "Images");
    assert_eq!(super::stem("x.tgz"), "x");
    assert_eq!(super::stem(".zip"), ".zip");
    assert_eq!(super::stem("notes.txt"), "notes.txt");
}

#[test]
fn a_zip_of_one_file_wraps_only_that_file() {
    let scratch = Scratch::new();
    let cache = &scratch.cache;
    let stored = |bytes: &[u8]| crate::cache::tests::store(cache, bytes).digest;
    let wrapped = stored(&zip_of(&[("train.csv", b"a,b\n1,2\n")]));
    assert!(super::wraps(cache, &wrapped, "train.csv"));
    assert!(super::wraps(cache, &wrapped, "folder/train.csv"));
    assert!(!super::wraps(cache, &wrapped, "test.csv"));
    let two = stored(&zip_of(&[("train.csv", b"a\n"), ("more.csv", b"b\n")]));
    assert!(!super::wraps(cache, &two, "train.csv"));
    let folder = stored(&zip_of(&[("train.csv/", b"")]));
    assert!(!super::wraps(cache, &folder, "train.csv"));
    let plain = stored(b"a,b\n1,2\n");
    assert!(!super::wraps(cache, &plain, "train.csv"));

    // once unpacked and dropped, the record of what the zip held answers
    let size = std::fs::metadata(cache.find(&wrapped, None).unwrap())
        .unwrap()
        .len();
    super::unpack_cached(cache, &wrapped, size, None, &|_| false, None)
        .unwrap()
        .unwrap();
    cache.drop_object(&wrapped).unwrap();
    assert!(super::wraps(cache, &wrapped, "train.csv"));
    assert!(!super::wraps(cache, &wrapped, "test.csv"));
}

#[test]
fn refused_archive_workers_keep_every_member_without_a_queue_wait() {
    let _refusing = crate::threads::refuse_after(0);
    for format in FAMILIES {
        let bytes = archive(format, FILES);
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(format, &bytes).unwrap();
        assert_eq!(unpacked.members.len(), FILES.len());
        for member in &unpacked.members {
            let (_, expected) = FILES.iter().find(|(path, _)| *path == member.path).unwrap();
            scratch.assert_stored(member, expected);
        }
    }
}

/// a member larger than the cache's disk holds, as FAT32 holds no file of 4 GiB or more, is
/// refused by the size the archive declares for it, before any of its bytes are written.
#[test]
fn a_member_the_cache_disk_cannot_hold_is_refused_before_it_is_written() {
    let limits = Limits::default();
    for format in FAMILIES {
        let scratch = Scratch::new();
        let files: &[(&str, &[u8])] = &[("small.csv", b"abc"), ("large.csv", b"abcd")];
        let path = Input::File(scratch.file("archive", &archive(format, files)));
        let meter = Meter::default();
        let mut guard =
            Guard::new(&limits, &|_| false, &meter).holding_at_most(scratch.cache.root(), 3);
        let mut kept = Vec::new();
        let mut keep = |_: &mut Guard<'_>, path: String, from: &mut dyn std::io::Read| {
            std::io::copy(from, &mut std::io::sink()).unwrap();
            kept.push(path);
            Ok(())
        };
        let refused = match format {
            Format::Zip => super::zip::unpack(&path, &mut guard, &mut keep),
            Format::SevenZ => super::sevenz::unpack(&path, &mut guard, &mut keep),
            tar => super::tar::unpack(&path, tar, &mut guard, &mut keep),
        }
        .unwrap_err();
        assert_eq!(
            refused.kind,
            Kind::LocalIo,
            "{format:?}: {}",
            refused.message
        );
        assert!(
            refused
                .message
                .contains("and \"large.csv\" in the archive is 4 B"),
            "{}",
            refused.message
        );
        assert_eq!(kept, ["small.csv"], "{format:?}");
    }
}
