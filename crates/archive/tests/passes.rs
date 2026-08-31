//! What extracting an archive costs beyond the bytes it writes.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the archive that failed is the message"
)]

use blake3 as _;
use bzip2 as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use tempfile as _;
use zip as _;

use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use fetchloom_archive::{ArchiveReader, extract};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::TreeEntry;
use fetchloom_faults::{TYPEFLAG_REGULAR, TarHeader, TarWriter};
use fetchloom_platform::NativePlatform;
use flate2::Compression;
use flate2::write::GzEncoder;

/// A source that reports how many bytes were read out of it.
struct Counted {
    inner: Cursor<Vec<u8>>,
    read: Rc<std::cell::Cell<u64>>,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.read.set(self.read.get() + read as u64);
        Ok(read)
    }
}

impl Seek for Counted {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A gzip-wrapped tar of `count` files, each holding a kilobyte.
fn archive(count: usize) -> Vec<u8> {
    let mut writer = TarWriter::new();
    let body = vec![b'x'; 1024];
    for index in 0..count {
        let mut header =
            TarHeader::ustar(format!("body/{index:05}.bin").as_bytes(), TYPEFLAG_REGULAR);
        header.set_size(body.len() as u64).set_mode(0o644);
        writer.push(&header, &body);
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&writer.finish()).unwrap();
    encoder.finish().unwrap()
}

/// Extracts an archive of `count` members and returns the compressed bytes that
/// were read to do it.
fn compressed_bytes_read(count: usize) -> (u64, u64) {
    let bytes = archive(count);
    let length = bytes.len() as u64;
    let read = Rc::new(std::cell::Cell::new(0));
    let source = Counted {
        inner: Cursor::new(bytes),
        read: Rc::clone(&read),
    };
    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        source,
        ArchiveFormat::TarGzip,
        "passes.tar.gz",
        Limits::default(),
    )
    .unwrap();
    let entries = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        Limits::default(),
        &NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, TreeEntry::File { .. }))
            .count(),
        count,
        "the archive did not extract every member it holds"
    );
    (read.get(), length)
}

#[test]
fn a_compressed_tar_is_decompressed_once_to_list_and_once_to_read() {
    let (read, length) = compressed_bytes_read(64);
    assert!(
        read <= length * 2,
        "extracting read {read} compressed bytes out of an archive of {length},          which is more than one listing pass and one reading pass"
    );
    assert!(
        read > length,
        "extracting read only {read} bytes of {length}, so the archive was not          both listed and read"
    );
}

#[test]
fn the_passes_do_not_grow_with_the_entry_count() {
    for count in [16, 512] {
        let (read, length) = compressed_bytes_read(count);
        assert!(
            read <= length * 2,
            "an archive of {count} members read {read} bytes of its own {length},              so a pass is being run per member rather than one for the whole archive"
        );
    }
}
