//! What a zip64 end of central directory record does to the name pre-scan, and
//! whether the pre-scan and the enumeration behind it can be made to disagree.

#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions, where a failed read is the failure being asserted"
)]

use blake3 as _;
use bzip2 as _;
use fetchloom_platform as _;
use flate2 as _;
use lzma_rust2 as _;
use tar as _;
use tempfile as _;
use zip as _;
use zstd as _;

use std::io::Cursor;

use fetchloom_archive::ArchiveReader;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{Zip64End, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter};

const CLASSIC_MAXIMUM: usize = u16::MAX as usize;

fn push(writer: &mut ZipWriter, name: &[u8], data: &[u8]) {
    let offset = writer.offset();
    let local = ZipLocalHeader::store(name, data);
    let central = ZipCentralHeader::from_local(&local, offset);
    writer.push(ZipMember {
        local,
        central,
        data: data.to_vec(),
    });
}

fn reader_for(bytes: Vec<u8>, name: &str) -> ArchiveReader<Cursor<Vec<u8>>> {
    ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Zip,
        name,
        Limits::default(),
    )
    .unwrap()
}

fn three_members(end: Zip64End) -> Vec<u8> {
    let mut writer = ZipWriter::new();
    push(&mut writer, b"a.txt", b"one");
    push(&mut writer, b"b.txt", b"two");
    push(&mut writer, b"c.txt", b"three");
    writer.finish_zip64(end)
}

#[test]
fn a_count_sentinel_over_a_small_directory_still_lists_every_member() {
    let mut reader = reader_for(
        three_members(Zip64End {
            count_sentinel: true,
            ..Zip64End::default()
        }),
        "count.zip",
    );

    let members = reader.members().unwrap();
    let paths: Vec<&str> = members.iter().map(|member| member.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt", "c.txt"]);
}

#[test]
fn an_offset_sentinel_still_lists_every_member() {
    let mut reader = reader_for(
        three_members(Zip64End {
            offset_sentinel: true,
            ..Zip64End::default()
        }),
        "offset.zip",
    );

    let members = reader.members().unwrap();
    let paths: Vec<&str> = members.iter().map(|member| member.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt", "c.txt"]);
}

#[test]
fn a_locator_over_a_malformed_zip64_record_fails_naming_zip64() {
    let mut reader = reader_for(
        three_members(Zip64End {
            count_sentinel: true,
            record_size: Some(8),
            ..Zip64End::default()
        }),
        "malformed.zip",
    );

    let error = reader.members().unwrap_err();
    assert_eq!(error.kind().label(), "archive.unsupported");
    assert!(
        error.next_action().contains("zip64"),
        "the refusal did not name zip64: {}",
        error.next_action()
    );
}

#[test]
fn an_archive_of_exactly_the_classic_maximum_is_not_read_as_zip64() {
    let mut writer = ZipWriter::new();
    for index in 0..CLASSIC_MAXIMUM {
        push(&mut writer, format!("m{index:05}.txt").as_bytes(), b"");
    }
    let mut reader = reader_for(writer.finish(), "maximum.zip");

    let members = reader.members().unwrap();
    assert_eq!(members.len(), CLASSIC_MAXIMUM);
    assert_eq!(members[CLASSIC_MAXIMUM - 1].path, "m65534.txt");
}

#[test]
fn a_member_past_the_classic_count_decides_the_separator() {
    let mut writer = ZipWriter::new();
    for index in 0..CLASSIC_MAXIMUM {
        push(
            &mut writer,
            format!("docs\\m{index:05}.txt").as_bytes(),
            b"",
        );
    }
    push(&mut writer, b"real/tail.txt", b"");
    let mut reader = reader_for(writer.finish_zip64(Zip64End::default()), "steered.zip");

    let Err(error) = reader.members() else {
        panic!("a backslash name was accepted beside a forward slash name");
    };
    assert_eq!(error.kind().label(), "archive.unsafe_path");
    assert!(
        reader.take_degradations().is_empty(),
        "a separator decided from every name should not degrade"
    );
}
