//! Reading a zip container, store and deflate methods only.

use std::io::{Read, Seek, SeekFrom};

use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::archive::{ArchiveMember, MemberKind};
use fetchloom_engine::tree::Mode;
use flate2::read::DeflateDecoder;
use zip::CompressionMethod;

use crate::bomb::BombGuard;
use crate::path::{claim_member_path, validate_link_target, validate_member_path};
use crate::shared::SharedSource;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const LOCAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

/// The end of central directory record signature.
const END_RECORD_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];

/// The central directory file header signature.
const CENTRAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];

/// How far back from the end of the file the end record is looked for, which
/// is its fixed size plus the largest comment it may carry.
const MAXIMUM_END_RECORD_SEARCH: u64 = 22 + 0xFFFF;

/// Where one member's compressed bytes begin and how they are packed.
#[derive(Clone, Copy, Debug)]
pub struct ZipOffset {
    /// The byte offset, in the zip file, of the member's compressed data.
    pub data_start: u64,
    /// The number of compressed bytes the member holds.
    pub compressed_size: u64,
    /// Whether the member is stored raw rather than deflated.
    pub stored: bool,
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
}

fn unsafe_path_member(member: &str, detail: &str) -> Error {
    Error::new(
        ErrorKind::ArchiveUnsafePath,
        format!("member \"{member}\" {detail}"),
    )
}

/// Decides whether every backslash in this zip's member paths can safely be
/// read as a path separator.
///
/// Takes the raw name of every central directory record. Returns true only
/// when no name holds a forward slash anywhere and at least one name holds a
/// backslash, which is the one condition under which a backslash cannot be a
/// legal Unix filename character coexisting with the archive's own
/// separator: Windows PowerShell's `Compress-Archive`, before .NET 4.6.1
/// fixed it, wrote exactly such archives.
fn is_backslash_separated(names: &[Vec<u8>]) -> bool {
    let holds_forward_slash = names.iter().any(|name| name.contains(&b'/'));
    let holds_backslash = names.iter().any(|name| name.contains(&b'\\'));
    !holds_forward_slash && holds_backslash
}

/// Translates every backslash in a raw member name to a forward slash.
fn with_backslashes_as_separators(raw: &[u8]) -> Vec<u8> {
    raw.iter()
        .map(|&byte| if byte == b'\\' { b'/' } else { byte })
        .collect()
}

/// Decides whether a zip's paths need normalizing and, when they do, records
/// the one degradation this decision produces.
///
/// Takes every central directory member name and the archive's name for the
/// degradation message. Returns whether every backslash in this archive
/// should be read as a forward slash.
fn decide_and_record_separator(
    central_names: &[Vec<u8>],
    archive_name: &str,
    degradations: &DegradeQueue,
) -> bool {
    let backslash_separated = is_backslash_separated(central_names);
    if backslash_separated {
        degradations.record(
            format!("archive \"{archive_name}\" with member paths separated by \"/\""),
            format!(
                "archive \"{archive_name}\" with every \"\\\" in its member paths read as \"/\""
            ),
            "no member path in this archive holds a forward slash and at least one holds a backslash, which only a writer using the backslash as its path separator produces",
        );
    }
    backslash_separated
}

/// Validates every central directory member name and reports the first
/// collision, normalizing first when the archive was decided to be
/// backslash-separated.
fn validate_central_directory_paths(
    central_names: &[Vec<u8>],
    backslash_separated: bool,
    nesting_limit: u32,
) -> Result<(), Error> {
    let mut claimed = std::collections::BTreeSet::new();
    for name in central_names {
        let path = zip_member_path(name, backslash_separated, nesting_limit)?;
        claim_member_path(&mut claimed, &path)?;
    }
    Ok(())
}

/// Validates one raw member name, normalizing a backslash-separated zip's
/// paths first.
///
/// Takes the raw name exactly as the archive wrote it, whether this archive
/// was decided to be backslash-separated, and the nesting depth limit.
/// Returns the validated path. This is the only place a raw zip member name
/// reaches `validate_member_path`, so a backslash-separated archive can never
/// have its `..` components checked against the original, unnormalized
/// bytes: normalization always runs first.
///
/// # Errors
///
/// Returns whatever `validate_member_path` fails with.
fn zip_member_path(
    raw: &[u8],
    backslash_separated: bool,
    nesting_limit: u32,
) -> Result<String, Error> {
    if backslash_separated {
        validate_member_path(&with_backslashes_as_separators(raw), nesting_limit)
    } else {
        validate_member_path(raw, nesting_limit)
    }
}

#[expect(
    deprecated,
    reason = "the crate's only accessor for an unmodeled method's wire number"
)]
fn method_number(method: CompressionMethod) -> u16 {
    if method == CompressionMethod::STORE {
        METHOD_STORE
    } else if method == CompressionMethod::DEFLATE {
        METHOD_DEFLATE
    } else if let CompressionMethod::Unsupported(code) = method {
        code
    } else {
        u16::MAX
    }
}

struct LocalHeader {
    method: u16,
    compressed_size: u32,
    uncompressed_size: u32,
    name: Vec<u8>,
    data_start: u64,
}

fn read_local_header<R: Read + Seek>(
    mut source: R,
    header_start: u64,
    archive_name: &str,
) -> Result<LocalHeader, Error> {
    let io_error = |error: std::io::Error| {
        unsupported_archive(archive_name, &format!("is truncated or malformed: {error}"))
    };
    source
        .seek(SeekFrom::Start(header_start))
        .map_err(io_error)?;
    let mut fixed = [0_u8; 30];
    source.read_exact(&mut fixed).map_err(io_error)?;
    if fixed[0..4] != LOCAL_HEADER_SIGNATURE {
        return Err(unsupported_archive(
            archive_name,
            "holds a local file header without the local file header signature",
        ));
    }
    let method = u16::from_le_bytes([fixed[8], fixed[9]]);
    let compressed_size = u32::from_le_bytes([fixed[18], fixed[19], fixed[20], fixed[21]]);
    let uncompressed_size = u32::from_le_bytes([fixed[22], fixed[23], fixed[24], fixed[25]]);
    let name_len = usize::from(u16::from_le_bytes([fixed[26], fixed[27]]));
    let extra_len = usize::from(u16::from_le_bytes([fixed[28], fixed[29]]));
    let mut name = vec![0_u8; name_len];
    source.read_exact(&mut name).map_err(io_error)?;
    let data_start = header_start + 30 + name_len as u64 + extra_len as u64;
    Ok(LocalHeader {
        method,
        compressed_size,
        uncompressed_size,
        name,
        data_start,
    })
}

/// Reads the member name of every central directory record, in order.
///
/// Takes the source and the archive's name for error messages. Returns one
/// name per record the container declares, including two records that name
/// one path, which an index keyed by name cannot represent.
///
/// # Errors
///
/// Fails when the end record cannot be found and when the central directory
/// is truncated or malformed.
fn central_directory_names<R: Read + Seek>(
    mut source: R,
    archive_name: &str,
) -> Result<Vec<Vec<u8>>, Error> {
    let io_error = |error: std::io::Error| {
        unsupported_archive(archive_name, &format!("is truncated or malformed: {error}"))
    };
    let length = source.seek(SeekFrom::End(0)).map_err(io_error)?;
    let window = length.min(MAXIMUM_END_RECORD_SEARCH);
    source
        .seek(SeekFrom::Start(length - window))
        .map_err(io_error)?;
    let mut tail = vec![0_u8; usize::try_from(window).unwrap_or(usize::MAX)];
    source.read_exact(&mut tail).map_err(io_error)?;
    let at = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|start| tail[*start..*start + 4] == END_RECORD_SIGNATURE)
        .ok_or_else(|| {
            unsupported_archive(archive_name, "holds no end of central directory record")
        })?;
    let declared = usize::from(u16::from_le_bytes([tail[at + 10], tail[at + 11]]));
    let directory_at = u64::from(u32::from_le_bytes([
        tail[at + 16],
        tail[at + 17],
        tail[at + 18],
        tail[at + 19],
    ]));

    source
        .seek(SeekFrom::Start(directory_at))
        .map_err(io_error)?;
    let mut names = Vec::with_capacity(declared);
    for _ in 0..declared {
        let mut fixed = [0_u8; 46];
        source.read_exact(&mut fixed).map_err(io_error)?;
        if fixed[0..4] != CENTRAL_HEADER_SIGNATURE {
            return Err(unsupported_archive(
                archive_name,
                "holds a central directory record without the record signature",
            ));
        }
        let name_len = usize::from(u16::from_le_bytes([fixed[28], fixed[29]]));
        let extra_len = usize::from(u16::from_le_bytes([fixed[30], fixed[31]]));
        let comment_len = usize::from(u16::from_le_bytes([fixed[32], fixed[33]]));
        let mut name = vec![0_u8; name_len];
        source.read_exact(&mut name).map_err(io_error)?;
        source
            .seek(SeekFrom::Current(
                i64::try_from(extra_len + comment_len).unwrap_or(i64::MAX),
            ))
            .map_err(io_error)?;
        names.push(name);
    }
    Ok(names)
}

/// What the central directory says about one member.
struct Central<'a> {
    path: &'a str,
    name: &'a [u8],
    method: u16,
    compressed_size: u64,
    size: u64,
}

/// Checks one member's local file header against its central directory entry.
///
/// Takes the local header and what the central directory recorded. Returns
/// nothing when the two agree. Fails with `archive.unsafe_path` naming both
/// paths when they name different members, and with `archive.unsupported`
/// naming both values when the method or either size disagrees.
///
/// # Errors
///
/// Returns the disagreement, which is a member two headers describe
/// differently and which must therefore never be extracted.
fn agrees_with_central_directory(local: &LocalHeader, central: &Central<'_>) -> Result<(), Error> {
    if local.name != central.name {
        let local_lossy = String::from_utf8_lossy(&local.name);
        return Err(unsafe_path_member(
            central.path,
            &format!(
                "has a local header naming \"{local_lossy}\" but the central directory names \"{}\"",
                central.path
            ),
        ));
    }
    let size_disagrees = (local.compressed_size != u32::MAX
        && u64::from(local.compressed_size) != central.compressed_size)
        || (local.uncompressed_size != u32::MAX
            && u64::from(local.uncompressed_size) != central.size);
    if local.method != central.method || size_disagrees {
        return Err(unsupported_member(
            central.path,
            &format!(
                "has a local header whose size or method disagrees with the central directory: local method {} size {}/{}, central method {} size {}/{}",
                local.method,
                local.compressed_size,
                local.uncompressed_size,
                central.method,
                central.compressed_size,
                central.size
            ),
        ));
    }
    Ok(())
}

fn open_body(
    mut source: impl Read + Seek + 'static,
    offset: ZipOffset,
) -> Result<Box<dyn Read>, Error> {
    let io_error = |error: std::io::Error| {
        Error::new(
            ErrorKind::ArchiveUnsupported,
            format!("could not seek to member data: {error}"),
        )
    };
    source
        .seek(SeekFrom::Start(offset.data_start))
        .map_err(io_error)?;
    let bounded = source.take(offset.compressed_size);
    if offset.stored {
        Ok(Box::new(bounded))
    } else {
        Ok(Box::new(DeflateDecoder::new(bounded)))
    }
}

/// Lists every member of a zip container.
///
/// Takes the shared source, the archive's name for error messages, its
/// on-disk size, the configured limits, and where to record a degradation
/// when this archive's member paths had to be normalized. Returns the
/// members alongside where each one's compressed data begins, in the same
/// order.
///
/// When every member path in the whole archive holds no forward slash and at
/// least one holds a backslash, every backslash is translated to a forward
/// slash before any path is validated, and one degradation is recorded
/// naming this archive. An archive mixing both separators anywhere is left
/// exactly as written, so its backslash is refused as unsafe.
///
/// # Errors
///
/// Fails when the container is truncated or malformed, when a member's path
/// is rejected, when a local header disagrees with its central directory
/// entry, when a member uses a compression method that is not store or
/// deflate, and when the archive exceeds the entry, byte, or ratio limit.
pub fn list_members<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    archive_name: &str,
    on_disk_bytes: u64,
    limits: Limits,
    degradations: &DegradeQueue,
) -> Result<(Vec<ArchiveMember>, Vec<ZipOffset>), Error> {
    let mut archive = zip::ZipArchive::new(source.clone()).map_err(|error| {
        unsupported_archive(archive_name, &format!("is truncated or malformed: {error}"))
    })?;
    let mut guard = BombGuard::new(archive_name, on_disk_bytes, limits);
    let central_names = central_directory_names(source.clone(), archive_name)?;
    let backslash_separated =
        decide_and_record_separator(&central_names, archive_name, degradations);
    validate_central_directory_paths(&central_names, backslash_separated, limits.nesting_depth)?;
    let mut claimed = std::collections::BTreeSet::new();
    let mut members = Vec::new();
    let mut offsets = Vec::new();

    for index in 0..archive.len() {
        guard.observe_entry()?;
        let (
            name_bytes,
            size,
            compressed_size,
            method,
            is_dir,
            is_symlink,
            unix_mode,
            header_start,
        ) = {
            let file = archive.by_index_raw(index).map_err(|error| {
                unsupported_archive(archive_name, &format!("is truncated or malformed: {error}"))
            })?;
            (
                file.name_raw().to_vec(),
                file.size(),
                file.compressed_size(),
                file.compression(),
                file.is_dir(),
                file.is_symlink(),
                file.unix_mode(),
                file.header_start(),
            )
        };
        let path = zip_member_path(&name_bytes, backslash_separated, limits.nesting_depth)?;
        claim_member_path(&mut claimed, &path)?;
        let local = read_local_header(source.clone(), header_start, archive_name)?;
        let central_method = method_number(method);
        agrees_with_central_directory(
            &local,
            &Central {
                path: &path,
                name: &name_bytes,
                method: central_method,
                compressed_size,
                size,
            },
        )?;
        if central_method != METHOD_STORE && central_method != METHOD_DEFLATE {
            return Err(unsupported_member(
                &path,
                &format!(
                    "uses compression method {central_method}, which is neither store nor deflate"
                ),
            ));
        }
        let stored = central_method == METHOD_STORE;
        let offset = ZipOffset {
            data_start: local.data_start,
            compressed_size,
            stored,
        };
        let kind = if is_dir {
            MemberKind::Directory
        } else if is_symlink {
            MemberKind::Symlink
        } else {
            MemberKind::File
        };
        let target = if matches!(kind, MemberKind::Symlink) {
            let mut body = open_body(source.clone(), offset)?;
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).map_err(|error| {
                unsupported_archive(archive_name, &format!("is truncated or malformed: {error}"))
            })?;
            validate_link_target(&path, &bytes)?;
            Some(bytes)
        } else {
            None
        };
        guard.observe_bytes(size)?;
        members.push(ArchiveMember {
            path,
            kind,
            size,
            mode: Mode::reduce(unix_mode.unwrap_or(0o644)),
            target,
        });
        offsets.push(offset);
    }

    Ok((members, offsets))
}

/// Opens one member's bytes, at random access.
///
/// Takes the shared source and the offset `list_members` recorded for the
/// member. Returns a reader over exactly the member's decompressed bytes.
///
/// # Errors
///
/// Fails when the member's data cannot be seeked to.
pub fn open_member<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    offset: ZipOffset,
) -> Result<Box<dyn Read>, Error> {
    open_body(source.clone(), offset)
}
