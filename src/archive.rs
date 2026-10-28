//! archives unpacked member by member into the cache, and refused whole when anything in them is
//! unsafe.

mod detect;
mod guard;
mod names;
mod publish;
mod tar;
mod tree;
mod zip;

use std::io::{self, Read};
use std::path::Path;

use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};

pub(crate) use detect::Format;
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "the sync loop calls these once it unpacks archives"
    )
)]
pub(crate) use detect::{Detected, detect};
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "the sync loop calls these once it unpacks archives"
    )
)]
pub(crate) use tree::common_top;
pub(crate) use tree::tree_hash;

use guard::{Guard, Meter};

/// bytes moved per read, from the archive file and out of the decompressor.
const BUFFER: usize = 256 * 1024;

/// what an archive may unpack to. every limit counts the bytes actually written, never the sizes
/// the archive declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// entries of any kind, folders included.
    pub(crate) members: u64,
    pub(crate) bytes: u64,
    /// bytes written for each compressed byte read.
    pub(crate) ratio: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            members: 1_000_000,
            bytes: 1 << 40,
            ratio: 200,
        }
    }
}

/// a file an archive unpacked to, stored in the cache under its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    /// in NFC, with `/` between folders.
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: Sha256,
}

/// every file an archive unpacked to, sorted by path, and the tree hash over them.
#[derive(Debug)]
pub(crate) struct Unpacked {
    pub(crate) members: Vec<Member>,
    pub(crate) tree: Sha256,
}

/// stores one file the guard let through, reading it from the archive.
type Keep<'k> = dyn FnMut(&mut Guard<'_>, String, &mut dyn Read) -> Result<(), Error> + 'k;

/// unpacks every file in `archive` into the cache. no object is published unless the whole
/// archive passes every check.
///
/// # Errors
///
/// `source.invalid` for an unsafe or damaged archive, `local.io` when the cache cannot take the
/// files, and `interrupted` once `stopped` returns true.
pub(crate) fn unpack(
    archive: &Path,
    format: Format,
    limits: &Limits,
    cache: &Cache,
    stopped: &dyn Fn() -> bool,
) -> Result<Unpacked, Error> {
    let meter = Meter::default();
    let mut guard = Guard::new(limits, stopped, &meter);
    let mut pending = Vec::new();
    let mut buffer = vec![0; BUFFER];
    let mut keep = |guard: &mut Guard<'_>, path: String, from: &mut dyn Read| {
        let mut writer = cache
            .object_writer(false)
            .map_err(|err| not_written(&path, &err))?;
        guard.copy(&path, from, &mut writer, &mut buffer)?;
        let closed = writer.close().map_err(|err| not_written(&path, &err))?;
        pending.push((path, closed));
        Ok(())
    };
    match format {
        Format::Zip => zip::unpack(archive, &mut guard, &mut keep)?,
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst | Format::TarBz2 => {
            tar::unpack(archive, format, &mut guard, &mut keep)?;
        }
    }
    guard.check_stopped()?;
    let published = publish::in_parallel(pending, |(path, closed)| {
        closed.sync()?;
        let stored = closed.publish()?;
        Ok(Member {
            path,
            size: stored.size,
            sha256: stored.digest,
        })
    });
    let mut members = published.map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("could not put unpacked files into the cache: {err}"),
        )
        .help("check that the cache's disk has room and that fetchloom may write to it")
    })?;
    members.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let tree = tree_hash(&members);
    Ok(Unpacked { members, tree })
}

const REFUSED_HELP: &str =
    "set extract = false on this dataset in data.toml to keep the archive packed";

/// an archive fetchloom will not unpack, for the reason given.
fn refused(message: impl Into<String>) -> Error {
    Error::new(Kind::SourceInvalid, message).help(REFUSED_HELP)
}

/// an archive holding a member that fetchloom refuses for its name or its kind.
fn refused_member(path: &str, reason: &str) -> Error {
    refused(format!(
        "the archive holds \"{path}\", which fetchloom refuses because {reason}"
    ))
}

/// an archive that could not be read to the end: cut short, corrupted or not what it seemed.
fn damaged(at: Option<&str>, err: &dyn std::fmt::Display) -> Error {
    let message = match at {
        Some(path) => format!("the archive is damaged at \"{path}\": {err}"),
        None => format!("the archive is damaged: {err}"),
    };
    Error::new(Kind::SourceInvalid, message)
        .help("ask the dataset's authors for a fixed copy, or set extract = false on this dataset in data.toml to keep it packed")
}

fn not_written(path: &str, err: &io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not write \"{path}\" into the cache: {err}"),
    )
    .help("check that the cache's disk has room and that fetchloom may write to it")
}

fn unreadable(archive: &Path, err: &io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not read {}: {err}", archive.display()),
    )
}

#[cfg(test)]
mod samples;
#[cfg(test)]
mod tests;
