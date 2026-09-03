//! A zip written to a stream, where the sizes follow the data rather than
//! preceding it.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the archive is the assertion"
)]

use std::io::Cursor;

use blake3 as _;
use bzip2 as _;
use fetchloom_platform as _;
use flate2 as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use tempfile as _;
use zip as _;

use fetchloom_archive::ArchiveReader;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter};

/// The general purpose bit that says the sizes follow the data.
const DESCRIPTOR: u16 = 1 << 3;

/// The value a size field carries when the real one is in a ZIP64 record.
const ZIP64_SENTINEL: u32 = u32::MAX;

fn streamed(writer: &mut ZipWriter, name: &[u8], data: &[u8]) {
    let offset = writer.offset();
    let mut local = ZipLocalHeader::store(name, data);
    let central = ZipCentralHeader::from_local(&local, offset);
    local.flags |= DESCRIPTOR;
    local.crc32 = 0;
    local.compressed_size = 0;
    local.uncompressed_size = 0;
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

#[test]
fn a_member_whose_sizes_follow_the_data_is_read_rather_than_called_tampered_with() {
    let mut writer = ZipWriter::new();
    streamed(&mut writer, b"notes.txt", b"hello there");
    let mut reader = reader_for(writer.finish(), "streamed.zip");

    let members = reader
        .members()
        .expect("a zip written to a stream was refused");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].path.as_str(), "notes.txt");
    assert_eq!(members[0].size, "hello there".len() as u64);
}

#[test]
fn a_member_whose_method_disagrees_is_still_refused_when_the_sizes_follow() {
    let mut writer = ZipWriter::new();
    let offset = writer.offset();
    let mut local = ZipLocalHeader::store(b"notes.txt", b"hello there");
    let central = ZipCentralHeader::from_local(&local, offset);
    local.flags |= DESCRIPTOR;
    local.crc32 = 0;
    local.compressed_size = 0;
    local.uncompressed_size = 0;
    local.method = 8;
    writer.push(ZipMember {
        local,
        central,
        data: b"hello there".to_vec(),
    });
    let mut reader = reader_for(writer.finish(), "wrong-method.zip");

    let refused = reader
        .members()
        .expect_err("a member whose method disagrees was accepted");
    assert_eq!(refused.kind(), ErrorKind::ArchiveUnsupported);
}

#[test]
fn an_archive_needing_zip64_is_refused_by_name_rather_than_misread() {
    let mut writer = ZipWriter::new();
    let offset = writer.offset();
    let mut local = ZipLocalHeader::store(b"huge.bin", b"x");
    let mut central = ZipCentralHeader::from_local(&local, offset);
    local.compressed_size = ZIP64_SENTINEL;
    local.uncompressed_size = ZIP64_SENTINEL;
    central.compressed_size = ZIP64_SENTINEL;
    central.uncompressed_size = ZIP64_SENTINEL;
    writer.push(ZipMember {
        local,
        central,
        data: b"x".to_vec(),
    });
    let mut reader = reader_for(writer.finish(), "zip64.zip");

    let refused = reader
        .members()
        .expect_err("an archive needing zip64 was read as though it did not");
    assert_eq!(refused.kind(), ErrorKind::ArchiveUnsupported);
    assert!(
        refused.next_action().to_lowercase().contains("zip64"),
        "the refusal does not name zip64: {}",
        refused.next_action()
    );
}
