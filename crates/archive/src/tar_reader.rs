//! Reading a POSIX ustar stream, bare or wrapped in one compression.

use std::cell::{Cell, RefCell};
use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;

use bzip2::read::BzDecoder;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::archive::{ArchiveMember, MemberKind};
use fetchloom_engine::tree::Mode;
use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use ruzstd::decoding::StreamingDecoder;

use crate::bomb::BombGuard;
use crate::path::{claim_member_path, validate_link_target, validate_member_path};
use crate::shared::SharedSource;

/// Which compression, if any, wraps the tar stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TarCompression {
    /// No compression: a bare ustar stream.
    None,
    /// A tar wrapped in a gzip member.
    Gzip,
    /// A tar wrapped in a zstd frame.
    Zstd,
    /// A tar wrapped in an xz stream.
    Xz,
    /// A tar wrapped in a bzip2 stream.
    Bzip2,
}

/// Where one member's bytes begin and how many there are, within the
/// decompressed tar byte stream.
#[derive(Clone, Copy, Debug)]
pub struct TarOffset {
    /// The byte offset, in the decompressed stream, of the member's data.
    pub data_start: u64,
    /// The number of bytes the member holds.
    pub size: u64,
}

fn unsupported_archive(name: &str, detail: &str) -> Error {
    Error::new(
        ErrorKind::ArchiveUnsupported,
        format!("archive \"{name}\" {detail}"),
    )
}

fn unsupported_member(member: &str, detail: &str) -> Error {
    Error::new(
        ErrorKind::ArchiveUnsupported,
        format!("member \"{member}\" {detail}"),
    )
    .with_member(member)
}

fn io_error(name: &str, error: &std::io::Error) -> Error {
    unsupported_archive(name, &format!("is truncated or malformed: {error}"))
}

fn build_decompressor<R: Read + 'static>(
    compression: TarCompression,
    source: SharedSource<R>,
    archive_name: &str,
) -> Result<Box<dyn Read>, Error> {
    match compression {
        TarCompression::None => Ok(Box::new(source)),
        TarCompression::Gzip => Ok(Box::new(GzDecoder::new(source))),
        TarCompression::Xz => Ok(Box::new(XzReader::new(source, false))),
        TarCompression::Bzip2 => Ok(Box::new(BzDecoder::new(source))),
        TarCompression::Zstd => StreamingDecoder::new(source)
            .map(|decoder| Box::new(decoder) as Box<dyn Read>)
            .map_err(|error| {
                unsupported_archive(archive_name, &format!("has an invalid zstd frame: {error}"))
            }),
    }
}

/// The pax records extraction reads or discards without complaint.
const IGNORED_PAX_KEYS: [&str; 8] = [
    "path", "linkpath", "size", "mtime", "atime", "ctime", "charset", "comment",
];

/// The pax records that name ownership, which a tree never carries.
const OWNERSHIP_PAX_KEYS: [&str; 4] = ["uid", "gid", "uname", "gname"];

/// The prefix a pax record uses for a real extended attribute.
const EXTENDED_ATTRIBUTE_PREFIX: &str = "SCHILY.xattr.";

/// The prefix a pax record uses for a sparse member, which this build cannot
/// reconstruct.
const SPARSE_PREFIX: &str = "GNU.sparse.";

fn reject_disallowed_pax_extensions(
    entry: &mut tar::Entry<'_, Box<dyn Read>>,
    archive_name: &str,
    member: &str,
) -> Result<(), Error> {
    let Some(extensions) = entry
        .pax_extensions()
        .map_err(|error| io_error(archive_name, &error))?
    else {
        return Ok(());
    };
    for extension in extensions {
        let extension = extension.map_err(|error| io_error(archive_name, &error))?;
        let key = extension
            .key()
            .map_err(|_| unsupported_member(member, "carries a pax key that is not valid UTF-8"))?;
        if IGNORED_PAX_KEYS.contains(&key) {
            continue;
        }
        if OWNERSHIP_PAX_KEYS.contains(&key) {
            return Err(unsupported_member(
                member,
                &format!("records ownership in a pax header, as \"{key}\""),
            ));
        }
        if key.starts_with(EXTENDED_ATTRIBUTE_PREFIX) {
            return Err(unsupported_member(
                member,
                &format!("carries the extended attribute \"{key}\""),
            ));
        }
        if key.starts_with(SPARSE_PREFIX) {
            return Err(unsupported_member(
                member,
                "is a sparse member, which this build cannot reconstruct",
            ));
        }
        return Err(unsupported_member(
            member,
            &format!("carries the pax record \"{key}\", which this build does not read"),
        ));
    }
    Ok(())
}

/// Lists every member of a tar stream, bare or wrapped in one compression.
///
/// # Errors
///
/// Fails when the stream is truncated or malformed, when a member's path or
/// mode is rejected, when a member is a device, FIFO, or carries a pax extended
/// attribute or ownership record, and when the archive exceeds the entry, byte,
/// or ratio limit.
pub fn list_members<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    compression: TarCompression,
    archive_name: &str,
    on_disk_bytes: u64,
    limits: Limits,
) -> Result<(Vec<ArchiveMember>, Vec<TarOffset>), Error> {
    let mut rewind = source.clone();
    rewind
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(archive_name, &error))?;
    let decompressor = build_decompressor(compression, rewind, archive_name)?;
    let mut archive = tar::Archive::new(decompressor);
    let mut guard = BombGuard::new(archive_name, on_disk_bytes, limits);
    let mut claimed = std::collections::BTreeSet::new();
    let mut members = Vec::new();
    let mut offsets = Vec::new();

    let entries = archive
        .entries()
        .map_err(|error| io_error(archive_name, &error))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| io_error(archive_name, &error))?;
        guard.observe_entry()?;
        let raw_path = entry.path_bytes().into_owned();
        let path = validate_member_path(&raw_path, limits.nesting_depth)?;
        claim_member_path(&mut claimed, &path)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_character_special() {
            return Err(unsupported_member(&path, "is a character device"));
        }
        if entry_type.is_block_special() {
            return Err(unsupported_member(&path, "is a block device"));
        }
        if entry_type.is_fifo() {
            return Err(unsupported_member(&path, "is a FIFO"));
        }
        let mode_bits = entry
            .header()
            .mode()
            .map_err(|error| io_error(archive_name, &error))?;
        if mode_bits & 0o4000 != 0 {
            return Err(unsupported_member(&path, "carries the setuid bit"));
        }
        if mode_bits & 0o2000 != 0 {
            return Err(unsupported_member(&path, "carries the setgid bit"));
        }
        reject_disallowed_pax_extensions(&mut entry, archive_name, &path)?;
        let kind = if entry_type.is_dir() {
            MemberKind::Directory
        } else if entry_type.is_symlink() {
            MemberKind::Symlink
        } else if entry_type.is_hard_link() {
            MemberKind::HardLink
        } else {
            MemberKind::File
        };
        let target = if matches!(kind, MemberKind::Symlink | MemberKind::HardLink) {
            let bytes = entry.link_name_bytes().map(std::borrow::Cow::into_owned);
            if let Some(raw) = bytes.as_deref() {
                validate_link_target(&path, raw)?;
            }
            bytes
        } else {
            None
        };
        let size = entry.size();
        guard.observe_bytes(size)?;
        let data_start = entry.raw_file_position();
        offsets.push(TarOffset { data_start, size });
        members.push(ArchiveMember {
            path,
            kind,
            size,
            mode: Mode::reduce(mode_bits),
            target,
        });
    }

    Ok((members, offsets))
}

/// A decompressed tar stream held open between members.
pub struct TarStream {
    decoder: Rc<RefCell<Box<dyn Read>>>,
    position: Rc<Cell<u64>>,
}

impl Clone for TarStream {
    fn clone(&self) -> Self {
        Self {
            decoder: Rc::clone(&self.decoder),
            position: Rc::clone(&self.position),
        }
    }
}

/// One member's bytes, read out of the stream the archive holds open.
pub struct MemberBody {
    stream: TarStream,
    remaining: u64,
}

impl Read for MemberBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let limit = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        let take = buffer.len().min(limit);
        let read = self.stream.decoder.borrow_mut().read(&mut buffer[..take])?;
        self.remaining -= read as u64;
        self.stream
            .position
            .set(self.stream.position.get() + read as u64);
        Ok(read)
    }
}

/// Opens one member's bytes out of a stream held open across members.
///
/// # Errors
///
/// Fails when the stream cannot be rewound or is truncated or malformed before
/// reaching the member's offset.
pub fn open_member<R: Read + Seek + 'static>(
    held: &mut Option<TarStream>,
    source: &SharedSource<R>,
    compression: TarCompression,
    archive_name: &str,
    offset: TarOffset,
) -> Result<Box<dyn Read>, Error> {
    let reusable = held
        .as_ref()
        .is_some_and(|stream| stream.position.get() <= offset.data_start);
    if !reusable {
        let mut rewind = source.clone();
        rewind
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error(archive_name, &error))?;
        *held = Some(TarStream {
            decoder: Rc::new(RefCell::new(build_decompressor(
                compression,
                rewind,
                archive_name,
            )?)),
            position: Rc::new(Cell::new(0)),
        });
    }
    let Some(stream) = held.as_ref() else {
        return Err(io_error(
            archive_name,
            &std::io::Error::other("the stream was not opened"),
        ));
    };
    skip_to(stream, offset.data_start, archive_name)?;
    Ok(Box::new(MemberBody {
        stream: stream.clone(),
        remaining: offset.size,
    }))
}

fn skip_to(stream: &TarStream, target: u64, archive_name: &str) -> Result<(), Error> {
    let mut buffer = vec![0_u8; 65_536];
    while stream.position.get() < target {
        let remaining = target - stream.position.get();
        let chunk = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = stream
            .decoder
            .borrow_mut()
            .read(&mut buffer[..chunk])
            .map_err(|error| io_error(archive_name, &error))?;
        if read == 0 {
            return Err(io_error(
                archive_name,
                &std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "stream ended early"),
            ));
        }
        stream.position.set(stream.position.get() + read as u64);
    }
    Ok(())
}
