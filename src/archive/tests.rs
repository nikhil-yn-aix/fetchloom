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
    let input = Input::file(&path).unwrap();
    let Detected::Archive(format) = detect(&input, Limits::default().memory).unwrap() else {
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

/// one format of each family: zip by its directory, tar as a stream and 7z block by block.
const FAMILIES: [Format; 3] = [Format::Zip, Format::TarZst, Format::SevenZ];

fn archive(format: Format, files: &[(&str, &[u8])]) -> Vec<u8> {
    match format {
        Format::Zip => zip_of(files),
        Format::SevenZ => seven_zip(files),
        tar => compress(tar, &tar_of(files)),
    }
}

const FILES: &[(&str, &[u8])] = &[("a.csv", b"1"), ("b.csv", b"2"), ("c/d.csv", b"3")];

/// every format leaves out what a system writes by itself, as finder's `__MACOSX` twins and the
/// `._` files a mac's tar adds, storing and recording none of it.
#[test]
fn what_a_system_makes_is_left_out_and_counted() {
    let files: &[(&str, &[u8])] = &[
        ("recordings/a.wav", b"RIFF"),
        ("recordings/.DS_Store", b"finder"),
        ("__MACOSX/recordings/._a.wav", b"appledouble"),
        ("recordings/._b.png", b"appledouble"),
        ("recordings/b.png", b"PNG"),
    ];
    for format in FAMILIES {
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(format, &archive(format, files)).unwrap();
        let paths: Vec<&str> = unpacked
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect();
        assert_eq!(
            paths,
            ["recordings/a.wav", "recordings/b.png"],
            "{format:?}"
        );
        assert_eq!(unpacked.skipped.files, 3, "{format:?}");
        assert_eq!(
            unpacked.skipped.first.as_deref(),
            Some("recordings/.DS_Store"),
            "{format:?}"
        );
        assert_eq!(scratch.cache.objects().len(), 2, "{format:?}");
    }
}

/// 5 MiB of random bytes in each family, past the 4 MiB the ratio waits for, so the check runs
/// and lets a modest expansion through. building the 7z at test time takes most of its time.
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

/// the parts may be cut anywhere, in a zip inside a local header or its central directory too.
/// one part a byte puts a cut inside every header and field: hundreds of part files, which take
/// a few seconds where making a file costs as much as on windows.
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
            assert_eq!(
                detect(&input, Limits::default().memory).unwrap(),
                Detected::Archive(format)
            );
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

/// a part another file took the place of after the archive was judged, as someone sharing the
/// cache may put one at its name, or a part cut short then, is the cache's fault on this machine,
/// not the archive's: the error names the part and says to run the command again, never to ask
/// the dataset's authors for a fixed copy, and nothing it unpacked is left behind.
#[test]
fn a_part_changed_while_it_is_read_is_no_damage_to_the_archive() {
    let a = vec![b'x'; 64 << 10];
    let bytes = tar_of(&[("data/a.csv", &a), ("data/b.csv", b"x,y\n3,4\n")]);
    let change = |path: &std::path::Path, cut_short: bool| {
        if cut_short {
            let part = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            part.set_len(part.metadata().unwrap().len() / 2).unwrap();
        } else {
            let aside = path.with_extension("aside");
            std::fs::copy(path, &aside).unwrap();
            std::fs::rename(&aside, path).unwrap();
        }
    };
    for (cut_short, why) in [
        (false, "it is not the file fetchloom checked"),
        (true, "it is shorter than when fetchloom checked it"),
    ] {
        let scratch = Scratch::new();
        let half = bytes.len() / 2;
        let first = scratch.file("archive.001", &bytes[..half]);
        let second = scratch.file("archive.002", &bytes[half..]);
        let sizes = (half as u64, (bytes.len() - half) as u64);
        let input = Input::joined(vec![(first, sizes.0), (second.clone(), sizes.1)]).unwrap();
        // the first header is read from the first part alone, which holds the second unopened
        let done = Cell::new(false);
        let watch = |_| {
            if !done.replace(true) {
                change(&second, cut_short);
            }
            false
        };
        let err = unpack(
            &input,
            Format::Tar,
            &Limits::default(),
            &scratch.cache,
            &watch,
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind, Kind::LocalIo, "{why}: {}", err.message);
        assert!(
            err.message
                .starts_with(&format!("the cached file {} changed", second.display()))
                && err.message.contains(why),
            "{}",
            err.message
        );
        assert_eq!(err.help.as_deref(), Some("run the same command again"));
        scratch.assert_empty();
    }
}

/// the ratio counts each byte of every part once: a split archive within it unpacks, and rereads
/// of a zip's shared data cannot raise the count past the parts' total. each family holds 5 MiB,
/// past the 4 MiB the ratio waits for; building the 7z at test time takes most of its time.
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
    let shared = super::zip::tests::overlapping(8, 1 << 20);
    let limits = Limits {
        ratio: 4,
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
    assert!(err.message.contains("past 4 times"), "{}", err.message);
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

/// in a cache a group shares, someone may put a link where an archive's fan folder or the archive
/// itself sits, to a private archive of whoever unpacks it next, as one in their own cache: the
/// unpack reads only the shared cache's own file, so none of the private files lands there.
#[test]
fn a_cached_archive_is_never_read_through_a_link() {
    let private = Scratch::new();
    if !crate::cache::tests::hardlinks(private.cache.root()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no links");
    }
    let secret = crate::cache::tests::store(&private.cache, &tar_of(&[("secret.csv", b"ill\n")]));
    let hex = secret.digest.hex();
    let theirs = private.cache.root().join("objects").join(&hex[..2]);
    // windows makes a link to a file only with a privilege, so its case is unix's
    let planted: &[(&str, bool)] = if cfg!(unix) {
        &[("fan", false), ("fan", true), ("object", false)]
    } else {
        &[("fan", false), ("fan", true)]
    };
    for &(planted, split) in planted {
        let shared = Scratch::new();
        let fan = shared.cache.root().join("objects").join(&hex[..2]);
        if planted == "fan" {
            crate::test_links::link_folder(&theirs, &fan).unwrap();
        } else {
            std::fs::create_dir_all(&fan).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(theirs.join(&hex[2..]), fan.join(&hex[2..])).unwrap();
        }
        let never = |_| false;
        let limits = Limits::default();
        let unpacked = if split {
            let parts = [(secret.digest, secret.size)];
            super::unpack_cached_parts(&shared.cache, &parts, None, &limits, &never, None)
        } else {
            let (digest, size) = (&secret.digest, secret.size);
            super::unpack_cached(&shared.cache, digest, size, None, &limits, &never, None)
        };
        assert!(unpacked.is_err(), "{planted}, split: {split}");
        let leaked = crate::digest::sha256(b"ill\n");
        assert!(
            !shared.cache.object_path(&leaked).exists(),
            "{planted}, split: {split}"
        );
    }
}

/// a read-only file with another name put at a cached archive's name, as a hardlink someone
/// sharing the cache makes to a private archive where links to others' files are allowed, passes
/// for the archive only if it hashes to the archive's name.
#[test]
fn a_cached_archive_with_another_name_must_hash_to_its_name() {
    let scratch = Scratch::new();
    if !crate::cache::tests::hardlinks(scratch.cache.root()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let archive = crate::cache::tests::store(&scratch.cache, &tar_of(&[("open.csv", b"a\n")]));
    let secret = scratch.file("secret.tar", &tar_of(&[("secret.csv", b"ill\n")]));
    crate::cache::tests::seal(&secret);
    let object = scratch.cache.object_path(&archive.digest);
    let mut permissions = std::fs::metadata(&object).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "windows removes no read-only file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&object, permissions).unwrap();
    std::fs::remove_file(&object).unwrap();
    std::fs::hard_link(&secret, &object).unwrap();
    assert_eq!(std::fs::metadata(&object).unwrap().len(), archive.size);
    let unpacked = super::unpack_cached(
        &scratch.cache,
        &archive.digest,
        archive.size,
        None,
        &Limits::default(),
        &|_| false,
        None,
    );
    let leaked = crate::digest::sha256(b"ill\n");
    assert!(
        !scratch.cache.object_path(&leaked).exists(),
        "the private archive's file landed in the cache"
    );
    assert_eq!(
        unpacked.err().map(|err| (err.kind, err.message)),
        Some((
            Kind::SourceInvalid,
            format!(
                "the cached archive at {} changed: its bytes no longer hash to its name",
                scratch.cache.object_path(&archive.digest).display()
            )
        ))
    );
}

/// the files a keyed archive unpacks to, by its whole object or by any one of its parts, are
/// marked as files a key came with before they take their names, but for a file the cache held
/// already, which keeps what brought it; an archive no key came with marks nothing.
#[test]
fn what_a_keyed_archive_unpacks_to_is_marked() {
    let scratch = Scratch::new();
    let cache = &scratch.cache;
    let held = crate::cache::tests::store(cache, b"held\n");
    let keyed = crate::cache::tests::store(
        cache,
        &tar_of(&[("own.csv", b"own\n"), ("held.csv", b"held\n")]),
    );
    let plain = crate::cache::tests::store(cache, &tar_of(&[("plain.csv", b"plain\n")]));
    cache.mark_keyed(&keyed.digest).unwrap();
    for archive in [&keyed, &plain] {
        let limits = Limits::default();
        super::unpack_cached(
            cache,
            &archive.digest,
            archive.size,
            None,
            &limits,
            &|_| false,
            None,
        )
        .unwrap();
    }
    let split = tar_of(&[("split.csv", b"split\n")]);
    let (first, second) = split.split_at(split.len() / 2);
    let parts = [
        crate::cache::tests::store(cache, first),
        crate::cache::tests::store(cache, second),
    ];
    cache.mark_keyed(&parts[1].digest).unwrap();
    let parts = parts.map(|part| (part.digest, part.size));
    super::unpack_cached_parts(cache, &parts, None, &Limits::default(), &|_| false, None).unwrap();
    let keyed = |bytes: &[u8]| cache.is_keyed(&crate::digest::sha256(bytes));
    assert!(keyed(b"own\n") && keyed(b"split\n"));
    assert!(!keyed(b"held\n") && !keyed(b"plain\n"));
    assert!(!cache.is_keyed(&held.digest));
}

/// an archive is judged as it is opened, then read again by its name: a file someone sharing the
/// cache puts at that name meanwhile, as a hardlink to a private archive, is never read.
#[cfg(unix)]
#[test]
fn a_cached_archive_swapped_once_judged_is_never_read() {
    let scratch = Scratch::new();
    let archive = crate::cache::tests::store(&scratch.cache, &tar_of(&[("open.csv", b"a\n")]));
    let secret = scratch.file("secret.tar", &tar_of(&[("secret.csv", b"ill\n")]));
    let object = scratch.cache.object_path(&archive.digest);
    crate::cache::swap_meanwhile(move || {
        std::fs::remove_file(&object).unwrap();
        std::fs::hard_link(&secret, &object).unwrap();
    });
    let unpacked = super::unpack_cached(
        &scratch.cache,
        &archive.digest,
        archive.size,
        None,
        &Limits::default(),
        &|_| false,
        None,
    );
    assert!(crate::cache::swapped(), "the archive was swapped");
    let leaked = crate::digest::sha256(b"ill\n");
    assert!(
        !scratch.cache.object_path(&leaked).exists(),
        "the private archive's file landed in the cache"
    );
    let err = unpacked.err().map(|err| err.message).unwrap_or_default();
    assert!(err.contains("is not the file fetchloom checked"), "{err}");
}

/// archives unpacking at once share the room the cache's disk had free, so together they never
/// fill it: the one whose files would pass it is refused, whichever takes its room last.
#[test]
fn archives_unpacking_at_once_share_the_room_the_disk_had_free() {
    let scratch = Scratch::new();
    let block = 4 << 10;
    let first = crate::cache::tests::store(&scratch.cache, &tar_of(&[("a.bin", &[1; 3 << 12])]));
    let second = crate::cache::tests::store(&scratch.cache, &tar_of(&[("b.bin", &[2; 3 << 12])]));
    let cache = scratch.cache.clone().with_free_room(5 * block);
    let limits = Limits::default();
    let inner = std::cell::RefCell::new(None);
    // the second archive unpacks whole while the first, begun, holds its part of the room
    let watch = |_| {
        if inner.borrow().is_none() {
            let (digest, size) = (&second.digest, second.size);
            let unpacked =
                super::unpack_cached(&cache, digest, size, None, &limits, &|_| false, Some(0));
            *inner.borrow_mut() = Some(unpacked);
        }
        false
    };
    let (digest, size) = (&first.digest, first.size);
    let outer = super::unpack_cached(&cache, digest, size, None, &limits, &watch, Some(0));
    let inner = inner.into_inner().unwrap();
    let refused: Vec<String> = [outer, inner]
        .into_iter()
        .filter_map(Result::err)
        .map(|err| err.message)
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].starts_with("the archive unpacks to more than the 20.0 KiB"),
        "{refused:?}"
    );
}

/// an archive refused part way gives back what it took of the room, as its files are removed, so
/// the archives unpacking beside it still have it.
#[test]
fn an_archive_refused_part_way_gives_its_room_back() {
    let scratch = Scratch::new();
    let block = 4 << 10;
    let first = crate::cache::tests::store(&scratch.cache, &tar_of(&[("a.bin", &[1; 3 << 12])]));
    let whole = tar_of(&[("b.bin", &[2; 3 << 12]), ("c.bin", &[3; 1 << 12])]);
    // cut in the middle of c.bin, once b.bin is written whole
    let cut = &whole[..512 + (3 << 12) + 512 + (1 << 11)];
    let second = crate::cache::tests::store(&scratch.cache, cut);
    let cache = scratch.cache.clone().with_free_room(5 * block);
    let limits = Limits::default();
    let inner = std::cell::RefCell::new(None);
    let watch = |_| {
        if inner.borrow().is_none() {
            let (digest, size) = (&second.digest, second.size);
            let unpacked =
                super::unpack_cached(&cache, digest, size, None, &limits, &|_| false, Some(0));
            *inner.borrow_mut() = Some(unpacked);
        }
        false
    };
    let (digest, size) = (&first.digest, first.size);
    let outer = super::unpack_cached(&cache, digest, size, None, &limits, &watch, Some(0));
    let inner = inner.into_inner().unwrap();
    assert!(
        inner.is_err_and(|err| err.message.contains("ends in the middle of")),
        "the second archive is damaged"
    );
    assert!(outer.is_ok(), "{:?}", outer.err().map(|err| err.message));
}

/// a tar has no checksum of its files, so changed cached bytes unpack to another tree silently.
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
        &Limits::default(),
        &|_| false,
        None,
    )
    .unwrap();
    assert_eq!(scratch.cache.unpacked_tree(&archive.digest), Some(pinned));
}

/// a folder that cannot be made, or a file list that cannot be saved, both name the cache folder.
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
            &Limits::default(),
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

/// `zstd --long=28` and above write frames asking for more than libzstd's 128 MiB window.
#[test]
fn a_tar_zst_asking_for_a_large_window_is_refused_not_left_packed() {
    let scratch = Scratch::new();
    let packed = crate::cache::tests::store(&scratch.cache, &zstd_asking_for(28, &tar_of(FILES)));
    let err = super::unpack_cached(
        &scratch.cache,
        &packed.digest,
        packed.size,
        None,
        &Limits::default(),
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

/// a nested archive stays packed, with a next step for each system: unzip or powershell for a
/// zip, tar for the rest, and on linux bsdtar for a 7z, which gnu tar does not read.
#[test]
fn an_archive_inside_another_has_a_next_step_for_each_system() {
    let step = |system, name| super::unpack_inner(system, &format!("data/x/{name}"), "data/x/");
    assert_eq!(
        step(System::Linux, "inner.zip"),
        "unpack it with unzip from your package manager: `unzip data/x/inner.zip -d data/x/inner-unpacked`"
    );
    assert_eq!(
        step(System::Linux, "train.tar.gz"),
        "unpack it with tar: `mkdir -p data/x/train-unpacked && tar -xf data/x/train.tar.gz -C data/x/train-unpacked`"
    );
    assert_eq!(
        step(System::Linux, "scans.7z"),
        "unpack it with bsdtar from your package manager: `mkdir -p data/x/scans-unpacked && bsdtar -xf data/x/scans.7z -C data/x/scans-unpacked`"
    );
    assert_eq!(
        step(System::Macos, "inner.zip"),
        "unpack it with the unzip macos has: `unzip data/x/inner.zip -d data/x/inner-unpacked`"
    );
    assert_eq!(
        step(System::Macos, "scans.7z"),
        "unpack it with the tar macos has: `mkdir -p data/x/scans-unpacked && tar -xf data/x/scans.7z -C data/x/scans-unpacked`"
    );
    assert_eq!(
        step(System::Windows, "inner.zip"),
        r"unpack it with powershell's Expand-Archive: `Expand-Archive data\x\inner.zip -DestinationPath data\x\inner-unpacked`"
    );
    assert_eq!(
        step(System::Windows, "train.tar.gz"),
        r"unpack it with the tar windows has: `mkdir data\x\train-unpacked; tar -xf data\x\train.tar.gz -C data\x\train-unpacked`"
    );
    assert_eq!(
        step(System::Linux, "clips.rar"),
        "unpack it with unrar from your package manager: `unrar x data/x/clips.rar data/x/clips-unpacked/`"
    );
}

/// zip joins a spanned zip's parts, and on windows 7-Zip reads them all from the `.zip`.
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

/// powershell splits a bare word starting with one `-` at its first dot, so 7-Zip would get
/// `-ox` and a stray `.part1-unpacked`: the `-o` word is quoted whenever it holds a dot.
#[test]
fn a_seven_zip_folder_with_a_dot_stays_one_word() {
    assert_eq!(
        super::unpack_elsewhere(System::Windows, "data/x/x.part1.rar", "", "extract = false"),
        r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x data\x\x.part1.rar "-ox.part1-unpacked"`. extract = false stops this warning"#
    );
    let line = r#"7z x data\x\x.part1.rar "-ox.part1-unpacked""#;
    assert_eq!(
        crate::suggested::command_words(line, true),
        Some(vec![
            "7z".to_owned(),
            "x".to_owned(),
            r"data\x\x.part1.rar".to_owned(),
            "-ox.part1-unpacked".to_owned()
        ])
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
        r#"unpack it with unrar from your package manager: `unrar x "../data/my data/it's.RAR" "../it's-unpacked/"`. --no-extract stops this warning"#
    );
    assert_eq!(
        step(System::Windows),
        r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x "..\data\my data\it's.RAR" "-o..\it's-unpacked"`. --no-extract stops this warning"#
    );
}

/// each word a shell would split or expand reads back whole from a next step, in powershell too,
/// which ends a quoted string at a curly quote as at a straight one.
#[test]
fn a_next_steps_words_keep_shell_characters_literal() {
    for (word, _, on_windows) in crate::suggested::tests::QUOTED {
        for system in [System::Linux, System::Macos, System::Windows] {
            let windows = system == System::Windows;
            // a word withheld on windows is never in a command there
            if windows && on_windows.is_none() {
                continue;
            }
            let line = format!("unrar x {}", system.word(word));
            assert_eq!(
                crate::suggested::command_words(&line, windows),
                Some(vec!["unrar".to_owned(), "x".to_owned(), word.to_owned()]),
                "{line}"
            );
        }
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
    super::unpack_cached(
        cache,
        &wrapped,
        size,
        None,
        &Limits::default(),
        &|_| false,
        None,
    )
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

/// FAT32, for one, holds no file of 4 GiB; the refusal goes by the size the archive declares.
#[test]
fn a_member_the_cache_disk_cannot_hold_is_refused_before_it_is_written() {
    let limits = Limits::default();
    for format in FAMILIES {
        let scratch = Scratch::new();
        let files: &[(&str, &[u8])] = &[("small.csv", b"abc"), ("large.csv", b"abcd")];
        let path = Input::file(&scratch.file("archive", &archive(format, files))).unwrap();
        let meter = Meter::default();
        let mut guard =
            Guard::new(&limits, &|_| false, &meter).holding_at_most(scratch.cache.root(), 3);
        let mut kept = Vec::new();
        let mut keep = |_: &mut Guard<'_>, path: String, _: u64, from: &mut dyn std::io::Read| {
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

/// bytes no compressor shrinks, so cutting a compressed tar in half cuts it inside a member.
fn noise(length: usize) -> Vec<u8> {
    let mut state: u32 = 0x9e37_79b9;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// a tar cut short, however compressed, is told in plain words rather than its decoder's, while a
/// failure of the disk itself stays one, since no fixed copy of the archive helps.
#[test]
fn a_tar_cut_short_is_told_in_plain_words() {
    let bytes = noise(64 << 10);
    let files: &[(&str, &[u8])] = &[("a.wav", &bytes), ("b.wav", &bytes)];
    for format in [
        Format::Tar,
        Format::TarGz,
        Format::TarXz,
        Format::TarZst,
        Format::TarBz2,
    ] {
        let packed = archive(format, files);
        let err = Scratch::new().refuse(format, &packed[..packed.len() / 2]);
        assert_eq!(err.kind, Kind::SourceInvalid, "{format:?}");
        assert!(
            err.message
                .starts_with("the archive is damaged: it ends in the middle of "),
            "{format:?}: {}",
            err.message
        );
        assert!(err.help.unwrap().contains("fixed copy"), "{format:?}");
    }
    let disk = std::io::Error::from_raw_os_error(5);
    assert_eq!(
        super::damaged(
            super::Unpacking::Archive,
            None,
            &disk,
            Limits::default().memory
        )
        .kind,
        Kind::LocalIo
    );
}

/// any long name an archive is refused for is shown short too.
#[test]
fn a_huge_member_name_is_shown_once_and_short() {
    let long = format!("{}.wav", "x".repeat(300));
    let err = Scratch::new().refuse(Format::Tar, &tar_of(&[(&long, b"too long a name")]));
    assert_eq!(
        err.message,
        format!(
            "the archive holds \"{}…{}.wav\", which this machine cannot store: its name is longer than disks allow",
            "x".repeat(60),
            "x".repeat(26)
        )
    );
    let refused = super::refused_member(&format!("data/{}", "y".repeat(3000)), "it is a symlink");
    assert_eq!(
        refused.message,
        format!(
            "the archive holds \"data/{}…{}\", which fetchloom refuses because it is a symlink",
            "y".repeat(55),
            "y".repeat(30)
        )
    );
}

/// files this machine cannot store are refused together once the archive ends, each named, so
/// one run shows every one; nothing the archive unpacked meanwhile stays in the cache.
#[test]
fn every_file_this_machine_cannot_store_is_named_in_one_refusal() {
    let (long, longer) = ("x".repeat(256), "y".repeat(300));
    let files: &[(&str, &[u8])] = &[(&long, b"1"), ("fine.csv", b"2"), (&longer, b"3")];
    let shown = |letter: &str| {
        format!(
            "\"{}…{}\": its name is longer than disks allow",
            letter.repeat(60),
            letter.repeat(30)
        )
    };
    for format in FAMILIES {
        let err = Scratch::new().refuse(format, &archive(format, files));
        assert_eq!(err.kind, Kind::SourceInvalid, "{format:?}");
        assert_eq!(
            err.message, "the archive holds 2 files this machine cannot store",
            "{format:?}"
        );
        let mut named = err.details.clone();
        named.sort();
        assert_eq!(named, [shown("x"), shown("y")], "{format:?}");
    }
}

/// windows' own rules name every file together too, as the harness's archive of reserved names
/// showed when only the first was named.
#[cfg(windows)]
#[test]
fn names_windows_refuses_are_named_together() {
    let files: &[(&str, &[u8])] = &[("CON", b"1"), ("ok.csv", b"2"), ("pipe|.csv", b"3")];
    let err = Scratch::new().refuse(Format::Zip, &archive(Format::Zip, files));
    assert_eq!(
        err.message,
        "the archive holds 2 files this machine cannot store"
    );
    assert_eq!(
        err.details,
        [
            "\"CON\": \"CON\" is a name windows reserves for devices",
            "\"pipe|.csv\": \"pipe|.csv\" has '|', which windows does not allow in names",
        ]
    );
}

/// once every file is read, the caller hears the whole archive as read, though a tar `tar -b 2048`
/// padded to 1 MiB has bytes left, so a bar tells writing the files apart from reading.
#[test]
fn the_end_of_reading_is_heard_as_the_whole_archive() {
    let mut padded = tar_of(FILES);
    padded.resize(1 << 20, 0);
    let mut cases: Vec<(Format, Vec<u8>)> = FAMILIES
        .iter()
        .map(|&format| (format, archive(format, FILES)))
        .collect();
    cases.push((Format::Tar, padded));
    for (format, bytes) in cases {
        let heard = Cell::new(0);
        let watch = |now: u64| {
            heard.set(heard.get().max(now));
            false
        };
        Scratch::new()
            .unpack_with(format, &bytes, &Limits::default(), &watch)
            .unwrap();
        assert_eq!(heard.get(), bytes.len() as u64, "{format:?}");
    }
}
