//! What the bomb guard is asked before a member's body is touched.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where a listing that did not fail is the assertion"
)]

use blake3 as _;
use bzip2 as _;
use fetchloom_platform as _;
use flate2 as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use tempfile as _;
use zip as _;

use std::io::Cursor;

use fetchloom_archive::ArchiveReader;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{METHOD_DEFLATE, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter};

fn symlink_declaring(size: u32) -> Vec<u8> {
    let compressed = b"\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let mut writer = ZipWriter::new();
    let offset = writer.offset();
    let local = ZipLocalHeader {
        version_needed: 20,
        flags: 0,
        method: METHOD_DEFLATE,
        mod_time: 0,
        mod_date: 0,
        crc32: 0,
        compressed_size: u32::try_from(compressed.len()).unwrap(),
        uncompressed_size: size,
        name: b"link".to_vec(),
        extra: Vec::new(),
    };
    let mut central = ZipCentralHeader::from_local(&local, offset);
    central.external_attrs = 0o120_777u32 << 16;
    writer.push(ZipMember {
        local,
        central,
        data: compressed,
    });
    writer.finish()
}

fn listing_error(bytes: Vec<u8>, name: &str, limits: Limits) -> fetchloom_engine::error::Error {
    let mut reader =
        ArchiveReader::new(Cursor::new(bytes), ArchiveFormat::Zip, name, limits).unwrap();
    let Err(error) = reader.members() else {
        panic!("{name} listed its members instead of being refused as a bomb")
    };
    error
}

#[test]
fn a_symlink_declaring_more_bytes_than_the_limit_is_refused_before_its_body_is_read() {
    let limits = Limits {
        expanded_bytes: 4096,
        expansion_ratio: u64::MAX,
        ..Limits::default()
    };
    let error = listing_error(symlink_declaring(1_000_000), "expanded.zip", limits);
    assert_eq!(error.kind().label(), "archive.bomb", "{}", error.next_action());
    assert!(
        error.next_action().contains("expanded.zip"),
        "the refusal does not name the archive: {}",
        error.next_action()
    );
}

#[test]
fn a_symlink_declaring_a_ratio_past_the_limit_is_refused_before_its_body_is_read() {
    let limits = Limits {
        expanded_bytes: u64::MAX,
        expansion_ratio: 200,
        ..Limits::default()
    };
    let error = listing_error(symlink_declaring(100_000_000), "ratio.zip", limits);
    assert_eq!(error.kind().label(), "archive.bomb", "{}", error.next_action());
    assert!(
        error.next_action().contains("ratio.zip"),
        "the refusal does not name the archive: {}",
        error.next_action()
    );
}
