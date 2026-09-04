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

const END_RECORD_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];

const CENTRAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];

const MAXIMUM_END_RECORD_SEARCH: u64 = 22 + 0xFFFF;

#[derive(Clone, Copy, Debug)]
pub struct ZipOffset {
    pub data_start: u64,
    pub compressed_size: u64,
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
    .with_member(member)
}

fn unsafe_path_member(member: &str, detail: &str) -> Error {
    Error::new(
        ErrorKind::ArchiveUnsafePath,
        format!("member \"{member}\" {detail}"),
    )
    .with_member(member)
}

fn is_backslash_separated(names: &[Vec<u8>]) -> bool {
    let holds_forward_slash = names.iter().any(|name| name.contains(&b'/'));
    let holds_backslash = names.iter().any(|name| name.contains(&b'\\'));
    !holds_forward_slash && holds_backslash
}

fn with_backslashes_as_separators(raw: &[u8]) -> Vec<u8> {
    raw.iter()
        .map(|&byte| if byte == b'\\' { b'/' } else { byte })
        .collect()
}

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
    flags: u16,
    method: u16,
    compressed_size: u32,
    uncompressed_size: u32,
    name: Vec<u8>,
    data_start: u64,
}

const SIZES_FOLLOW_THE_DATA: u16 = 1 << 3;

const ZIP64_SENTINEL: u32 = u32::MAX;

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
    let flags = u16::from_le_bytes([fixed[6], fixed[7]]);
    let method = u16::from_le_bytes([fixed[8], fixed[9]]);
    let compressed_size = u32::from_le_bytes([fixed[18], fixed[19], fixed[20], fixed[21]]);
    let uncompressed_size = u32::from_le_bytes([fixed[22], fixed[23], fixed[24], fixed[25]]);
    let name_len = usize::from(u16::from_le_bytes([fixed[26], fixed[27]]));
    let extra_len = usize::from(u16::from_le_bytes([fixed[28], fixed[29]]));
    let mut name = vec![0_u8; name_len];
    source.read_exact(&mut name).map_err(io_error)?;
    let data_start = header_start + 30 + name_len as u64 + extra_len as u64;
    Ok(LocalHeader {
        flags,
        method,
        compressed_size,
        uncompressed_size,
        name,
        data_start,
    })
}

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

struct Central<'a> {
    path: &'a str,
    name: &'a [u8],
    method: u16,
    compressed_size: u64,
    size: u64,
}

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
    if local.compressed_size == ZIP64_SENTINEL || local.uncompressed_size == ZIP64_SENTINEL {
        return Err(unsupported_member(
            central.path,
            "is recorded in the zip64 format, which this build does not read",
        ));
    }
    let states_its_sizes = local.flags & SIZES_FOLLOW_THE_DATA == 0;
    let size_disagrees = states_its_sizes
        && (u64::from(local.compressed_size) != central.compressed_size
            || u64::from(local.uncompressed_size) != central.size);
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

pub fn open_member<R: Read + Seek + 'static>(
    source: &SharedSource<R>,
    offset: ZipOffset,
) -> Result<Box<dyn Read>, Error> {
    open_body(source.clone(), offset)
}
