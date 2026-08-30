//! Every format this build ships, read back from bytes it really holds.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the format that failed is the message"
)]

use blake3 as _;
use fetchloom_platform as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use tempfile as _;
use zip as _;

use std::io::{Cursor, Read, Write};

use bzip2::Compression as BzCompression;
use bzip2::write::BzEncoder;
use fetchloom_archive::ArchiveReader;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{Corpus, TYPEFLAG_REGULAR, TarHeader, TarWriter};
use flate2::Compression as GzCompression;
use flate2::write::GzEncoder;

/// The bytes of `xz -9` over `fetchloom xz round trip\n`, embedded because
/// the xz decoder this build carries does not encode.
const GREETING_XZ: &[u8] = &[
    253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 4, 192, 28, 24, 33, 1, 28, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 211, 125, 61, 97, 1, 0, 23, 102, 101, 116, 99, 104, 108, 111, 111, 109, 32, 120, 122,
    32, 114, 111, 117, 110, 100, 32, 116, 114, 105, 112, 10, 0, 122, 6, 34, 249, 146, 172, 124,
    103, 0, 1, 56, 24, 134, 145, 117, 36, 31, 182, 243, 125, 1, 0, 0, 0, 0, 4, 89, 90,
];

/// The bytes of `xz -9` over a ustar tar holding `hello.txt`, whose content
/// is `hello\n`, embedded for the same reason.
const TAR_XZ: &[u8] = &[
    253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 4, 192, 126, 128, 80, 33, 1, 28, 0, 0, 0, 0,
    0, 0, 0, 0, 134, 159, 55, 227, 224, 39, 255, 0, 118, 93, 0, 52, 25, 73, 238, 141, 240, 186,
    200, 255, 155, 255, 242, 12, 105, 175, 17, 235, 51, 124, 171, 120, 201, 212, 247, 199, 117, 52,
    105, 227, 231, 89, 171, 97, 101, 109, 219, 79, 8, 155, 57, 218, 201, 28, 163, 118, 133, 6, 120,
    155, 137, 253, 186, 231, 247, 131, 98, 29, 248, 37, 244, 214, 51, 254, 215, 55, 201, 178, 119,
    81, 6, 123, 225, 71, 232, 241, 238, 30, 98, 17, 160, 17, 85, 159, 188, 33, 194, 66, 75, 188,
    205, 38, 82, 146, 158, 231, 126, 191, 244, 194, 236, 109, 164, 210, 226, 18, 78, 78, 90, 124,
    23, 159, 242, 57, 80, 228, 46, 0, 0, 0, 0, 0, 239, 52, 138, 173, 48, 143, 143, 85, 0, 1, 154,
    1, 128, 80, 0, 0, 195, 80, 45, 195, 177, 196, 103, 251, 2, 0, 0, 0, 0, 4, 89, 90,
];

/// The greeting the embedded xz fixture decompresses to.
const GREETING: &[u8] = b"fetchloom xz round trip\n";

/// Wraps bytes in a zstd frame of one raw block, which is a frame the format
/// permits and the decoder must accept without any encoder existing here.
fn zstd_frame(content: &[u8]) -> Vec<u8> {
    assert!(
        content.len() < 256,
        "the single-byte size field bounds this"
    );
    let mut out = vec![0x28, 0xB5, 0x2F, 0xFD, 0x20];
    out.push(u8::try_from(content.len()).unwrap());
    let header = (u32::try_from(content.len()).unwrap() << 3) | 1;
    out.extend_from_slice(&header.to_le_bytes()[..3]);
    out.extend_from_slice(content);
    out
}

fn gzip(content: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), GzCompression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

fn bzip2(content: &[u8]) -> Vec<u8> {
    let mut encoder = BzEncoder::new(Vec::new(), BzCompression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

fn one_file_tar(name: &[u8], content: &[u8]) -> Vec<u8> {
    let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
    header.set_size(content.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, content);
    writer.finish()
}

fn read_back(bytes: Vec<u8>, format: ArchiveFormat, name: &str) -> Vec<(String, Vec<u8>)> {
    let mut reader =
        ArchiveReader::new(Cursor::new(bytes), format, name, Limits::default()).unwrap();
    let members = reader.members().unwrap();
    let mut read = Vec::with_capacity(members.len());
    for member in &members {
        let mut content = Vec::new();
        reader
            .open(member)
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        read.push((member.path.clone(), content));
    }
    read
}

#[test]
fn a_bare_tar_round_trips() {
    let read = read_back(
        one_file_tar(b"hello.txt", b"hello\n"),
        ArchiveFormat::Tar,
        "plain.tar",
    );
    assert_eq!(read, vec![("hello.txt".to_owned(), b"hello\n".to_vec())]);
}

#[test]
fn a_gzip_wrapped_tar_round_trips() {
    let read = read_back(
        gzip(&one_file_tar(b"hello.txt", b"hello\n")),
        ArchiveFormat::TarGzip,
        "plain.tar.gz",
    );
    assert_eq!(read, vec![("hello.txt".to_owned(), b"hello\n".to_vec())]);
}

#[test]
fn a_zstd_wrapped_tar_round_trips() {
    let tar = one_file_tar(b"hello.txt", b"hello\n");
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x40];
    let mut remaining = tar.as_slice();
    while !remaining.is_empty() {
        let take = remaining.len().min(255);
        let last = u32::from(take == remaining.len());
        let header = (u32::try_from(take).unwrap() << 3) | last;
        frame.extend_from_slice(&header.to_le_bytes()[..3]);
        frame.extend_from_slice(&remaining[..take]);
        remaining = &remaining[take..];
    }
    let read = read_back(frame, ArchiveFormat::TarZstd, "plain.tar.zst");
    assert_eq!(read, vec![("hello.txt".to_owned(), b"hello\n".to_vec())]);
}

#[test]
fn an_xz_wrapped_tar_round_trips() {
    let read = read_back(TAR_XZ.to_vec(), ArchiveFormat::TarXz, "plain.tar.xz");
    assert_eq!(read, vec![("hello.txt".to_owned(), b"hello\n".to_vec())]);
}

#[test]
fn a_bzip2_wrapped_tar_round_trips() {
    let read = read_back(
        bzip2(&one_file_tar(b"hello.txt", b"hello\n")),
        ArchiveFormat::TarBzip2,
        "plain.tar.bz2",
    );
    assert_eq!(read, vec![("hello.txt".to_owned(), b"hello\n".to_vec())]);
}

#[test]
fn a_zip_of_stored_and_deflated_members_round_trips() {
    let benign = Corpus::build();
    let entry = benign
        .entries()
        .iter()
        .find(|candidate| candidate.name() == "zip_benign_two_files_and_dir")
        .unwrap();
    let read = read_back(entry.bytes().to_vec(), ArchiveFormat::Zip, "plain.zip");
    assert!(!read.is_empty());
}

#[test]
fn a_bare_gzip_object_materializes_as_one_member() {
    let read = read_back(gzip(GREETING), ArchiveFormat::Gzip, "greeting.txt.gz");
    assert_eq!(read, vec![("greeting.txt".to_owned(), GREETING.to_vec())]);
}

#[test]
fn a_bare_zstd_object_materializes_as_one_member() {
    let read = read_back(
        zstd_frame(GREETING),
        ArchiveFormat::Zstd,
        "greeting.txt.zst",
    );
    assert_eq!(read, vec![("greeting.txt".to_owned(), GREETING.to_vec())]);
}

#[test]
fn a_bare_xz_object_materializes_as_one_member() {
    let read = read_back(GREETING_XZ.to_vec(), ArchiveFormat::Xz, "greeting.txt.xz");
    assert_eq!(read, vec![("greeting.txt".to_owned(), GREETING.to_vec())]);
}

#[test]
fn a_bare_bzip2_object_materializes_as_one_member() {
    let read = read_back(bzip2(GREETING), ArchiveFormat::Bzip2, "greeting.txt.bz2");
    assert_eq!(read, vec![("greeting.txt".to_owned(), GREETING.to_vec())]);
}
