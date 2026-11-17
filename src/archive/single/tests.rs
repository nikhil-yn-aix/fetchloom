use std::cell::Cell;
use std::fs;

use super::super::samples::{Scratch, compress, tar_of, xz_asking_for, zip_of, zstd_asking_for};
use super::super::{Format, Limits};
use super::*;
use crate::cache::Member;
use crate::cache::tests::store;
use crate::error::Kind;

const CSV: &[u8] = b"sepal,petal\n5.1,1.4\n";

/// the four compressions `decompress` takes, as the tar formats of the same compression name them.
const COMPRESSIONS: [Format; 4] = [Format::TarGz, Format::TarBz2, Format::TarXz, Format::TarZst];

/// decompresses `packed`, cached in `scratch`, to `name` within `limits`, watched by `watch`.
fn decompress_in(
    scratch: &Scratch,
    packed: &[u8],
    name: &str,
    limits: &Limits,
    watch: &dyn Fn(u64) -> bool,
) -> Result<Decompressed, Error> {
    let stored = store(&scratch.cache, packed);
    decompress_cached(
        &scratch.cache,
        (&stored.digest, stored.size),
        name,
        None,
        limits,
        watch,
        None,
    )
}

/// the one file `outcome` unpacked to, its bytes checked against the cache's.
fn only_file(scratch: &Scratch, outcome: Decompressed, bytes: &[u8]) -> Member {
    let Decompressed::Unpacked(unpacked) = outcome else {
        panic!("left as it came: {outcome:?}");
    };
    let [member] = unpacked.members.as_slice() else {
        panic!("{:?}", unpacked.members);
    };
    scratch.assert_stored(member, bytes);
    assert_eq!(unpacked.tree, tree_hash(&unpacked.members));
    member.clone()
}

/// how many objects and unfinished files the cache holds.
fn held(scratch: &Scratch) -> (usize, usize) {
    let objects = scratch.cache.objects().len();
    let temporary = fs::read_dir(scratch.cache.root().join("tmp"))
        .unwrap()
        .count();
    (objects, temporary)
}

#[test]
fn a_name_decompresses_to_itself_without_the_ending() {
    for (path, expected) in [
        ("iris.csv.gz", Some(Ok("iris.csv"))),
        ("raw/IRIS.CSV.GZ", Some(Ok("IRIS.CSV"))),
        ("reads.fastq.bz2", Some(Ok("reads.fastq"))),
        ("model.bin.Zst", Some(Ok("model.bin"))),
        ("notes.xz", Some(Ok("notes"))),
        ("twice.csv.gz.gz", Some(Ok("twice.csv.gz"))),
        ("flowers.tar.gz", None),
        ("flowers.tgz", None),
        ("flowers.tar.zst", None),
        ("flowers.zip", None),
        ("flowers.rar", None),
        ("iris.csv", None),
        ("gz", None),
        (
            ".gz",
            Some(Err("\".gz\" would decompress to a file with no name")),
        ),
        (
            "...gz",
            Some(Err(
                "\"...gz\" would decompress to \"..\", which climbs out of its folder",
            )),
        ),
        (
            ".git.xz",
            Some(Err(
                "\".git.xz\" would decompress to \".git\", which names repository metadata",
            )),
        ),
        (
            "aux.zst",
            Some(Err(
                "\"aux.zst\" would decompress to a name not every system can store: \"aux\" is a name windows reserves for devices",
            )),
        ),
        (
            "a..gz",
            Some(Err(
                "\"a..gz\" would decompress to a name not every system can store: \"a.\" ends with a dot or a space, which windows drops",
            )),
        ),
    ] {
        let expected = expected.map(|answer| answer.map(str::to_owned).map_err(str::to_owned));
        assert_eq!(decompressed_name(path), expected, "{path}");
    }
}

#[test]
fn every_compression_decompresses_to_one_file_named_without_its_ending() {
    for format in COMPRESSIONS {
        let scratch = Scratch::new();
        let packed = compress(format, CSV);
        let outcome = decompress_in(&scratch, &packed, "iris.csv", &Limits::default(), &|_| {
            false
        });
        let member = only_file(&scratch, outcome.unwrap(), CSV);
        assert_eq!(member.path, "iris.csv", "{format:?}");
        let stored = store(&scratch.cache, &packed);
        let recorded = scratch
            .cache
            .unpacked(&decompressed_key(&stored.digest, "iris.csv"), None)
            .unwrap();
        assert_eq!(recorded, [member], "{format:?}");
    }
}

/// gzip, bzip2 and xz streams joined end to end, and zstd frames, as parallel compressors write
/// them, decompress whole.
#[test]
fn joined_streams_decompress_whole() {
    for format in COMPRESSIONS {
        let scratch = Scratch::new();
        let mut packed = compress(format, b"first,");
        packed.extend(compress(format, b"second\n"));
        let outcome = decompress_in(&scratch, &packed, "both.csv", &Limits::default(), &|_| {
            false
        });
        only_file(&scratch, outcome.unwrap(), b"first,second\n");
    }
}

/// the most a file may decompress to counts every stream joined in it, so streams that each fit
/// still stop together, and nothing of them is kept.
#[test]
fn the_size_limit_counts_every_joined_stream() {
    for format in COMPRESSIONS {
        let scratch = Scratch::new();
        let mut packed = compress(format, &[b'a'; 3000]);
        packed.extend(compress(format, &[b'b'; 3000]));
        let limits = Limits {
            bytes: 4096,
            ..Limits::default()
        };
        let err = decompress_in(&scratch, &packed, "joined.txt", &limits, &|_| false).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid, "{format:?}");
        assert_eq!(
            err.message,
            "\"joined.txt\" decompresses to more than 4.0 KiB, the most fetchloom unpacks from one file"
        );
        assert_eq!(held(&scratch), (1, 0), "only the packed file stays");
    }
}

/// a bzip2 of 785 bytes that holds 1 GiB stops once it grows past the ratio, a few MiB in.
#[test]
fn a_bomb_stops_at_the_ratio() {
    // nextest names the package's folder when it runs tests from an archive elsewhere
    let path = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(
            || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            std::path::PathBuf::from,
        )
        .join("tests/fixtures/archives/zeros-1gib.bz2");
    let packed = fs::read(path).unwrap();
    let scratch = Scratch::new();
    let err =
        decompress_in(&scratch, &packed, "zeros", &Limits::default(), &|_| false).unwrap_err();
    assert_eq!(err.kind, Kind::SourceInvalid);
    assert_eq!(
        err.message,
        "\"zeros\" grew past 1000 times its compressed size as it decompressed, which is how compression bombs fill disks"
    );
    assert_eq!(
        err.help.as_deref(),
        Some(
            "if you trust the file, raise FETCHLOOM_ARCHIVE_RATIO, or archive_ratio in the settings file. otherwise keep the file packed with decompress = false on its dataset in data.toml, or add it without --decompress"
        )
    );
    assert_eq!(held(&scratch), (1, 0));
}

/// a stop part way keeps nothing of the file, and the next try decompresses it from the start.
#[test]
fn a_stop_part_way_keeps_nothing_and_a_new_try_finishes() {
    let bytes: Vec<u8> = (0..(1 << 20)).map(|at: u32| (at % 251) as u8).collect();
    for format in COMPRESSIONS {
        let scratch = Scratch::new();
        let packed = compress(format, &bytes);
        let checks = Cell::new(0);
        let stopped = |_: u64| {
            checks.set(checks.get() + 1);
            checks.get() > 1
        };
        let err =
            decompress_in(&scratch, &packed, "long.bin", &Limits::default(), &stopped).unwrap_err();
        assert_eq!(err.kind, Kind::Interrupted, "{format:?}");
        assert_eq!(held(&scratch), (1, 0), "{format:?}");
        let stored = store(&scratch.cache, &packed);
        let key = decompressed_key(&stored.digest, "long.bin");
        assert!(scratch.cache.unpacked(&key, None).is_none(), "{format:?}");
        let outcome = decompress_in(&scratch, &packed, "long.bin", &Limits::default(), &|_| {
            false
        });
        only_file(&scratch, outcome.unwrap(), &bytes);
    }
}

/// a compressed tar is extract's, and a file with no compressed stream has nothing to unpack:
/// both stay as they came.
#[test]
fn a_compressed_tar_or_a_plain_file_is_left_as_it_came() {
    let tar = tar_of(&[("iris.csv", CSV)]);
    for (bytes, left) in [
        (compress(Format::TarGz, &tar), Left::Tar),
        (compress(Format::TarZst, &tar), Left::Tar),
        (CSV.to_vec(), Left::Plain),
        (zip_of(&[("iris.csv", CSV)]), Left::Plain),
    ] {
        let scratch = Scratch::new();
        let outcome = decompress_in(&scratch, &bytes, "iris.csv", &Limits::default(), &|_| false);
        assert!(
            matches!(outcome, Ok(Decompressed::Left(found)) if found == left),
            "{left:?}: {outcome:?}"
        );
        assert_eq!(held(&scratch), (1, 0));
    }
}

/// an xz stream asking for more memory, or a zstd frame for a larger window, than the memory
/// setting allows is refused, naming the file and the setting, before it decompresses.
#[test]
fn a_stream_past_the_memory_allowed_is_refused() {
    let limits = Limits {
        memory: 256 << 20,
        ..Limits::default()
    };
    // a 512 MiB dictionary, and a window of 2 GiB
    let xz = xz_asking_for(&compress(Format::TarXz, CSV), 34);
    for (packed, message) in [
        (
            xz,
            "the file's xz stream needs more than 256.0 MiB of memory to decompress",
        ),
        (
            zstd_asking_for(31, CSV),
            "the file's zstd stream needs a window larger than 128.0 MiB to decompress",
        ),
    ] {
        let scratch = Scratch::new();
        let err = decompress_in(&scratch, &packed, "iris.csv", &limits, &|_| false).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(err.message, message);
        assert_eq!(
            err.help.as_deref(),
            Some(
                "if you trust the file, raise FETCHLOOM_MEMORY, which gives each file a quarter of it, or memory in the settings file. otherwise keep the file packed with decompress = false on its dataset in data.toml, or add it without --decompress"
            )
        );
        assert_eq!(held(&scratch), (1, 0));
    }
}

/// what a file decompresses to counts against the room the cache's disk had free.
#[test]
fn decompressing_past_the_room_its_disk_had_free_stops() {
    let scratch = Scratch::new();
    let packed = compress(Format::TarGz, &vec![b'x'; 64 << 10]);
    let stored = store(&scratch.cache, &packed);
    let cache = scratch.cache.clone().with_free_room(16 << 10);
    let err = decompress_cached(
        &cache,
        (&stored.digest, stored.size),
        "wide.csv",
        None,
        &Limits::default(),
        &|_| false,
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, Kind::LocalIo);
    assert!(
        err.message.starts_with(
            "the file decompresses to more than the 16.0 KiB the disk holding the cache at "
        ),
        "{}",
        err.message
    );
    assert_eq!(held(&scratch), (1, 0));
}

/// the same packed bytes under two names decompress to two files, each with its own record, so
/// neither dataset ever places the other's name.
#[test]
fn the_same_bytes_under_two_names_keep_a_record_each() {
    let scratch = Scratch::new();
    let packed = compress(Format::TarGz, CSV);
    for name in ["a.csv", "b.csv"] {
        let outcome = decompress_in(&scratch, &packed, name, &Limits::default(), &|_| false);
        only_file(&scratch, outcome.unwrap(), CSV);
    }
    let stored = store(&scratch.cache, &packed);
    for name in ["a.csv", "b.csv"] {
        let recorded = scratch
            .cache
            .unpacked(&decompressed_key(&stored.digest, name), None)
            .unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].path, name);
    }
}
