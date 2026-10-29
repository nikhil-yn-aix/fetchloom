//! archives unpacked member by member into the cache, and refused whole when anything in them is
//! unsafe.

mod detect;
mod guard;
mod names;
mod store;
mod tar;
mod zip;

use std::io::{self, Read};
use std::path::Path;

use crate::cache::{Cache, Member, tree_hash};
use crate::digest::Sha256;
use crate::error::{Error, Kind};

pub(crate) use detect::Format;
pub(crate) use detect::{Detected, detect};

use guard::{Guard, Meter};

/// bytes moved per read, from the archive file and out of the decompressor.
const BUFFER: usize = 256 * 1024;

/// the endings of the archives fetchloom unpacks, longest first so `.tar.gz` wins over `.gz`.
const UNPACKED: [&str; 11] = [
    ".tar.bz2", ".tar.zst", ".tar.gz", ".tar.xz", ".tbz2", ".tzst", ".tar", ".tbz", ".tgz", ".txz",
    ".zip",
];

/// what a file's name says about unpacking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packed {
    /// zip or a tar, which unpacks when its bytes agree.
    Archive,
    /// an archive fetchloom leaves packed: `7z` or `rar`.
    Unsupported(&'static str),
}

/// what the name of the file at `path` says it is. only a name decides whether a file may
/// unpack, so every machine decides alike from the listing, and documents such as `.xlsx` or
/// `.npz`, which are zips inside, stay whole.
pub(crate) fn by_name(path: &str) -> Option<Packed> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if UNPACKED.iter().any(|ending| name.ends_with(ending)) {
        return Some(Packed::Archive);
    }
    [("7z", ".7z"), ("rar", ".rar")]
        .into_iter()
        .find(|(_, ending)| name.ends_with(ending))
        .map(|(format, _)| Packed::Unsupported(format))
}

/// an archive's file name without its archive ending: `train` for `train.tar.gz`.
pub(crate) fn stem(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    UNPACKED
        .iter()
        .find(|ending| lower.ends_with(*ending))
        .map(|ending| &name[..name.len() - ending.len()])
        .filter(|stem| !stem.is_empty())
        .unwrap_or(name)
}

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

/// every file an archive unpacked to, sorted by path, and the tree hash over them.
#[derive(Debug)]
pub(crate) struct Unpacked {
    pub(crate) members: Vec<Member>,
    pub(crate) tree: Sha256,
    /// how many of the files were new to the cache.
    pub(crate) written: u64,
}

/// stores one file the guard let through, reading it from the archive.
type Keep<'k> = dyn FnMut(&mut Guard<'_>, String, &mut dyn Read) -> Result<(), Error> + 'k;

/// unpacks every file in `archive` into the cache. an archive refused part way leaves no object
/// behind.
///
/// # Errors
///
/// `source.invalid` for an unsafe or damaged archive, `local.io` when the cache cannot take the
/// files, and `interrupted` once `watch` returns true. `watch` hears how many bytes of the
/// archive were read so far.
pub(crate) fn unpack(
    archive: &Path,
    format: Format,
    limits: &Limits,
    cache: &Cache,
    watch: &dyn Fn(u64) -> bool,
) -> Result<Unpacked, Error> {
    let meter = Meter::default();
    let mut guard = Guard::new(limits, watch, &meter);
    let (mut members, written) = store::store_all(cache, |writers| {
        let mut buffer = vec![0; BUFFER];
        let mut keep = |guard: &mut Guard<'_>, path: String, from: &mut dyn Read| {
            writers.keep(guard, path, from, &mut buffer)
        };
        match format {
            Format::Zip => zip::unpack(archive, &mut guard, &mut keep)?,
            Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst | Format::TarBz2 => {
                tar::unpack(archive, format, &mut guard, &mut keep)?;
            }
        }
        guard.check_stopped()
    })?;
    members.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let tree = tree_hash(&members);
    Ok(Unpacked {
        members,
        tree,
        written,
    })
}

/// unpacks the archive the cache holds under `archive`, and records what it unpacked to when
/// that is `tree`, or when no tree is known yet. bytes that changed in the cache unpack to other
/// files, and recording those would replace the tree data.lock pins. none when its bytes turn
/// out to be no archive fetchloom unpacks, so the file stays as it is.
///
/// # Errors
///
/// as [`unpack`], and `local.io` when the cache lost the archive or cannot record it.
pub(crate) fn unpack_cached(
    cache: &Cache,
    archive: &Sha256,
    size: u64,
    tree: Option<&Sha256>,
    watch: &dyn Fn(u64) -> bool,
) -> Result<Option<Unpacked>, Error> {
    let object = cache.find(archive, Some(size)).ok_or_else(|| {
        Error::new(
            Kind::LocalIo,
            "the cache lost an archive while unpacking it",
        )
        .help("run the same command again to download it")
    })?;
    let format = match detect(&object).map_err(|err| unreadable(&object, &err))? {
        Detected::Archive(format) => format,
        Detected::Unsupported(_) | Detected::Plain => return Ok(None),
    };
    let unpacked = unpack(&object, format, &Limits::default(), cache, watch)?;
    if tree.is_none_or(|tree| *tree == unpacked.tree) {
        cache
            .record_unpacked(archive, &unpacked.members)
            .map_err(|err| Error::io("could not record what an archive unpacked to", &err))?;
    }
    Ok(Some(unpacked))
}

const REFUSED_HELP: &str = "keep the archive packed with extract = false on its dataset in data.toml, or with --no-extract";

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
        .help("ask the dataset's authors for a fixed copy, or keep it packed with extract = false in data.toml or --no-extract")
}

fn not_written(path: &str, err: &io::Error) -> Error {
    Error::io(
        format_args!("could not write \"{path}\" into the cache"),
        err,
    )
    .or_help("check that the cache's disk has room and that fetchloom may write to it")
}

fn unreadable(archive: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not read {}", archive.display()), err)
}

#[cfg(test)]
mod samples;
#[cfg(test)]
mod tests;
