//! Recognizing a packed format and extracting it.

use super::context::Materialization;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{EventPayload, Span};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::TreeEntry;
use std::io::Read;
use std::path::Path;

/// Returns the archive format an object holds, when it holds one this build
/// extracts.
///
/// # Errors
///
/// Fails with `archive.unsupported` when the name and the object's own leading
/// bytes disagree about what the object is.
pub(super) fn packed_format(
    with: &Materialization<'_>,
    digest: ContentDigest,
    name: &str,
) -> Result<Option<ArchiveFormat>, Error> {
    recognized_format(with, digest, name, None)
}

/// Returns the archive format an object holds, honoring a format a manifest
/// declared.
pub(super) fn recognized_format(
    with: &Materialization<'_>,
    digest: ContentDigest,
    name: &str,
    declared: Option<ArchiveFormat>,
) -> Result<Option<ArchiveFormat>, Error> {
    if !with.extract {
        return Ok(None);
    }
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if declared.is_none() && fetchloom_archive::format_from_extension(name).is_none() {
        return Ok(None);
    }
    let mut file = cache.read(digest)?;
    let mut header = [0_u8; fetchloom_archive::SNIFF_LENGTH];
    let filled = read_up_to(&mut file, &mut header)?;
    fetchloom_archive::recognize(declared, name, &header[..filled])
}

pub(super) fn read_up_to(file: &mut impl Read, into: &mut [u8]) -> Result<usize, Error> {
    let mut filled = 0;
    while filled < into.len() {
        let taken = file.read(&mut into[filled..]).map_err(|reason| {
            Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("the archive's leading bytes could not be read: {reason}"),
            )
        })?;
        if taken == 0 {
            break;
        }
        filled += taken;
    }
    Ok(filled)
}

/// Opens a reader over a cached archive object.
///
/// # Errors
///
/// Fails when the run has no cache and when the object cannot be opened.
pub(super) fn open_archive(
    with: &Materialization<'_>,
    digest: ContentDigest,
    format: ArchiveFormat,
    name: &str,
) -> Result<fetchloom_archive::ArchiveReader<fetchloom_cache::storage::Bytes>, Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run extracts an archive out of a store and neither the cache nor a scratch store beside the destination could be opened",
        ));
    };
    let file = cache.read(digest)?;
    fetchloom_archive::ArchiveReader::new(file, format, name.to_owned(), Limits::default())
}

/// Extracts a cached archive into a staging directory.
///
/// # Errors
///
/// Fails with the archive or destination failure the reader or the extraction
/// named, and publishes nothing.
pub(super) fn extract_into(
    with: &Materialization<'_>,
    digest: ContentDigest,
    format: ArchiveFormat,
    name: &str,
    staging: &Path,
    selection: &Selection,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let unpacking = Span::start();
    emit(EventPayload::ExtractStart);
    let mut reader = open_archive(with, digest, format, name)?;
    let result = fetchloom_archive::extract(
        &mut reader,
        selection,
        staging,
        Limits::default(),
        with.platform,
        with.work,
    );
    for entry in reader.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    if let Ok(entries) = &result {
        emit(EventPayload::ExtractEnd {
            entries: entries.len() as u64,
            bytes: entries
                .iter()
                .map(|entry| match entry {
                    TreeEntry::File { size, .. } | TreeEntry::Symlink { size, .. } => *size,
                    TreeEntry::Directory { .. } => 0,
                })
                .sum(),
            duration_ms: unpacking.elapsed_ms(),
        });
    }
    result
}
