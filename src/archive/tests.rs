//! what every format shares: the limits, stopping, unicode names and objects left behind.

use std::cell::Cell;

use super::samples::{Scratch, compress, tar_of, zip_of};
use super::{Format, Limits};
use crate::error::Kind;

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
                .unpack_with(format, &fits, &limits, &|| false)
                .is_ok()
        );
        let over = archive(format, &[FILES, &[("e.csv", b"4")]].concat());
        let err = scratch
            .unpack_with(format, &over, &limits, &|| false)
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
            .unpack_with(format, &archive(format, FILES), &limits, &|| false)
            .unwrap_err();
        assert!(err.message.contains("past 2 B"), "{}", err.message);
        scratch.assert_empty();
    }
}

#[test]
fn a_stop_part_way_publishes_nothing() {
    for format in FAMILIES {
        let checks = Cell::new(0);
        let stopped = || {
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
