//! Reading a bare compressed object: one file, named by the location's stem
//! with the compression extension removed.

use std::io::{Read, Seek, SeekFrom};

use bzip2::read::BzDecoder;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::archive::{ArchiveMember, MemberKind};
use fetchloom_engine::tree::Mode;
use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use ruzstd::decoding::StreamingDecoder;

use crate::bomb::BombGuard;
use crate::shared::SharedSource;

/// Which compression a bare compressed object uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BareCompression {
    /// A gzip member.
    Gzip,
    /// A zstd frame.
    Zstd,
    /// An xz stream.
    Xz,
    /// A bzip2 stream.
    Bzip2,
}

fn unsupported_archive(name: &str, detail: &str) -> Error {
    Error::new(
        ErrorKind::ArchiveUnsupported,
        format!("archive \"{name}\" {detail}"),
    )
}

fn io_error(name: &str, error: &std::io::Error) -> Error {
    unsupported_archive(name, &format!("is truncated or malformed: {error}"))
}

fn build_decompressor<R: Read + 'static>(
    compression: BareCompression,
    source: SharedSource<R>,
    archive_name: &str,
) -> Result<Box<dyn Read>, Error> {
    match compression {
        BareCompression::Gzip => Ok(Box::new(GzDecoder::new(source))),
        BareCompression::Xz => Ok(Box::new(XzReader::new(source, false))),
        BareCompression::Bzip2 => Ok(Box::new(BzDecoder::new(source))),
        BareCompression::Zstd => StreamingDecoder::new(source)
            .map(|decoder| Box::new(decoder) as Box<dyn Read>)
            .map_err(|error| {
                unsupported_archive(archive_name, &format!("has an invalid zstd frame: {error}"))
            }),
    }
}

/// Derives the member name for a bare compressed object.
#[must_use]
pub fn member_name(location: &str, extension: &str) -> String {
    let file_name = location.rsplit(['/', '\\']).next().unwrap_or(location);
    file_name
        .strip_suffix(extension)
        .unwrap_or(file_name)
        .to_owned()
}

/// Decompresses a bare compressed object fully to learn its one member,
/// checking the entry, byte, and ratio limits as bytes are produced.
///
/// # Errors
///
/// Fails when the stream is truncated or malformed, and when the object exceeds
/// the byte or ratio limit.
pub fn list_member<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    compression: BareCompression,
    name: &str,
    on_disk_bytes: u64,
    limits: Limits,
) -> Result<ArchiveMember, Error> {
    let mut rewind = source.clone();
    rewind
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(name, &error))?;
    let mut decompressor = build_decompressor(compression, rewind, name)?;
    let mut guard = BombGuard::new(name, on_disk_bytes, limits);
    guard.observe_entry()?;
    let mut buffer = vec![0_u8; 65_536];
    let mut total = 0_u64;
    loop {
        let read = decompressor
            .read(&mut buffer)
            .map_err(|error| io_error(name, &error))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        guard.observe_bytes(read as u64)?;
    }
    Ok(ArchiveMember {
        path: name.to_owned(),
        kind: MemberKind::File,
        size: total,
        mode: Mode::ReadWrite,
        target: None,
    })
}

/// Opens the one member a bare compressed object holds, streaming from the
/// start.
///
/// # Errors
///
/// Fails when the stream cannot be rewound or is truncated or malformed.
pub fn open_member<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    compression: BareCompression,
    name: &str,
    size: u64,
) -> Result<Box<dyn Read>, Error> {
    let mut rewind = source.clone();
    rewind
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(name, &error))?;
    let decompressor = build_decompressor(compression, rewind, name)?;
    Ok(Box::new(decompressor.take(size)))
}
