//! Whether a zip's backslash-separated member paths are normalized, and that
//! normalization can never run after the path-safety checks it must precede.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where a failed read is the failure being asserted"
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
use fetchloom_faults::{
    TYPEFLAG_REGULAR, TarHeader, TarWriter, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter,
};

fn push_zip_member(writer: &mut ZipWriter, name: &[u8], data: &[u8]) {
    let offset = writer.offset();
    let local = ZipLocalHeader::store(name, data);
    let central = ZipCentralHeader::from_local(&local, offset);
    writer.push(ZipMember {
        local,
        central,
        data: data.to_vec(),
    });
}

fn reader_for(bytes: Vec<u8>, format: ArchiveFormat, name: &str) -> ArchiveReader<Cursor<Vec<u8>>> {
    ArchiveReader::new(Cursor::new(bytes), format, name, Limits::default()).unwrap()
}

#[test]
fn a_backslash_only_zip_normalizes_every_member_and_degrades() {
    let mut writer = ZipWriter::new();
    push_zip_member(&mut writer, b"docs\\readme.txt", b"hello");
    push_zip_member(&mut writer, b"notes.txt", b"world");
    let mut reader = reader_for(writer.finish(), ArchiveFormat::Zip, "windows.zip");

    let members = reader.members().unwrap();
    let mut paths: Vec<&str> = members.iter().map(|member| member.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["docs/readme.txt", "notes.txt"]);

    let degradations = reader.take_degradations();
    assert_eq!(degradations.len(), 1);
    assert!(
        degradations[0].requested.contains("windows.zip")
            || degradations[0].used.contains("windows.zip"),
        "the degradation did not name the archive: {degradations:?}"
    );
}

#[test]
fn normalization_runs_before_the_dotdot_rejection() {
    let mut writer = ZipWriter::new();
    push_zip_member(&mut writer, b"..\\..\\escape.txt", b"x");
    let mut reader = reader_for(writer.finish(), ArchiveFormat::Zip, "escape.zip");

    let error = reader.members().unwrap_err();
    assert_eq!(error.kind().label(), "archive.unsafe_path");
}

#[test]
fn a_zip_mixing_forward_and_backslash_separators_stays_rejected() {
    let mut writer = ZipWriter::new();
    push_zip_member(&mut writer, b"a/b.txt", b"x");
    push_zip_member(&mut writer, b"c\\d.txt", b"y");
    let mut reader = reader_for(writer.finish(), ArchiveFormat::Zip, "mixed.zip");

    let error = reader.members().unwrap_err();
    assert_eq!(error.kind().label(), "archive.unsafe_path");
}

#[test]
fn a_sole_backslash_member_normalizes_even_when_it_names_an_ordinary_unix_file() {
    let mut writer = ZipWriter::new();
    push_zip_member(&mut writer, b"weird\\name.txt", b"x");
    let mut reader = reader_for(writer.finish(), ArchiveFormat::Zip, "sole.zip");

    let members = reader.members().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].path, "weird/name.txt");
    assert_eq!(reader.take_degradations().len(), 1);
}

#[test]
fn a_tar_member_with_a_backslash_is_still_rejected() {
    let mut writer = TarWriter::new();
    let mut header = TarHeader::ustar(b"dir\\evil.txt", TYPEFLAG_REGULAR);
    header.set_size(1);
    writer.push(&header, b"x");
    let mut reader = reader_for(writer.finish(), ArchiveFormat::Tar, "windows.tar");

    let error = reader.members().unwrap_err();
    assert_eq!(error.kind().label(), "archive.unsafe_path");
    assert!(error.next_action().contains("dir\\evil.txt"));
}
