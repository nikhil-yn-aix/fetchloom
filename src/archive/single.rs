//! single compressed files, as `reads.csv.gz`: unpacked when their dataset asks with `decompress`,
//! to one file named as the packed one without its ending, through the guard every archive unpacks
//! through, so the archive limits, the ratio against bombs and the room on the cache's disk hold.

use super::detect::{self, Single};
use super::guard::{Guard, Meter};
use super::input::Input;
use super::names::Named;
use super::{
    BUFFER, Limits, Skipped, Unpacked, Unpacking, by_name, record, refused, store, tar, unopened,
};
use crate::cache::{Cache, decompressed_key, tree_hash};
use crate::digest::Sha256;
use crate::error::Error;

/// the endings of the single compressed files `decompress` unpacks. an archive's ending, as
/// `.tar.gz`, is extract's, and wins.
const ENDINGS: [&str; 4] = [".bz2", ".zst", ".gz", ".xz"];

/// what the file at `path` decompresses to, by its name alone, so every machine decides alike: none
/// when the name ends in no compression `decompress` takes, or in an archive's; else the name
/// without the ending, or why no file may take that name.
pub(crate) fn decompressed_name(path: &str) -> Option<Result<String, String>> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ending = ENDINGS.iter().find(|ending| {
        name.len() >= ending.len()
            && name.as_bytes()[name.len() - ending.len()..].eq_ignore_ascii_case(ending.as_bytes())
    })?;
    if by_name(path).is_some() {
        return None;
    }
    let stripped = &name[..name.len() - ending.len()];
    let refused = |what: String| Some(Err(format!("\"{name}\" would decompress to {what}")));
    if stripped.is_empty() {
        return refused("a file with no name".to_owned());
    }
    if let Some(why) = crate::name::escapes(stripped).or_else(|| crate::name::metadata(stripped)) {
        let why = why.strip_prefix("it ").unwrap_or(why);
        return refused(format!("\"{stripped}\", which {why}"));
    }
    if let Some(why) = crate::name::unportable(stripped) {
        return refused(format!("a name not every system can store: {why}"));
    }
    Some(Ok(stripped.to_owned()))
}

/// why a file `decompress` was asked of stays as it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Left {
    /// a compressed tar, which only extract unpacks, and only by a name that says so.
    Tar,
    /// no gzip, bzip2, xz or zstd stream.
    Plain,
}

/// what decompressing a cached file came to.
#[derive(Debug)]
pub(crate) enum Decompressed {
    Unpacked(Unpacked),
    Left(Left),
}

/// decompresses the cached file `packed`, of `size` bytes, to one file named `name`, recording it
/// under [`decompressed_key`] when it is `tree` or none is known, so changed bytes never replace
/// data.lock's pin. `writers` and `watch` are as for [`super::unpack`].
///
/// # Errors
///
/// `source.invalid` past a limit or for a damaged stream, `local.io` past the room the cache's disk
/// had free or for a read or write that fails, and `interrupted` once `watch` returns true.
pub(crate) fn decompress_cached(
    cache: &Cache,
    (packed, size): (&Sha256, u64),
    name: &str,
    tree: Option<&Sha256>,
    limits: &Limits,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Decompressed, Error> {
    let input = Input::cached(cache, packed, Some(size))
        .map_err(|err| unopened(&cache.object_path(packed), &err))?;
    let format = match detect::single(&input, limits.memory)? {
        Single::Stream(format) => format,
        Single::Tar => return Ok(Decompressed::Left(Left::Tar)),
        Single::Other => return Ok(Decompressed::Left(Left::Plain)),
    };
    let unpacked = decompress(&input, format, name, limits, cache, watch, writers)?;
    let key = decompressed_key(packed, name);
    record(cache, &key, &unpacked, tree, Unpacking::File)?;
    Ok(Decompressed::Unpacked(unpacked))
}

/// decompresses `input`, of the compression `format` names, to one file named `name` in the cache,
/// leaving no object behind when refused part way, as [`super::unpack`] leaves none of an archive.
fn decompress(
    input: &Input,
    format: super::Format,
    name: &str,
    limits: &Limits,
    cache: &Cache,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Unpacked, Error> {
    let meter = Meter::default();
    let filesystem = crate::space::Filesystem::of(&cache.root().join("tmp"));
    let room = cache.share_room();
    let mut guard = Guard::new(limits, watch, &meter)
        .decompressing()
        .writing_to(cache.root(), filesystem)
        .with_room(cache.root(), &room);
    let (members, added) = store::store_all(cache, writers, input.keyed(), |writers| {
        guard.entry()?;
        // a person asked for this one file, so one a system writes by itself stays too
        let path = match guard.name(name, false)? {
            Named::File(path) | Named::System(path) => path,
            Named::Folder => return Err(refused(format!("\"{name}\" names a folder"))),
        };
        let counted = tar::counted(input, guard.meter())?;
        let mut decoded = tar::decompressed(format, counted, limits.memory)
            .map_err(|err| tar::unreadable_stream(Unpacking::File, None, &err, limits.memory))?;
        let mut buffer = vec![0; BUFFER];
        // the size is unknown until the stream ends, so the file streams to a writer
        writers.keep(&mut guard, path, u64::MAX, &mut decoded, &mut buffer)?;
        meter.read_to_end();
        guard.check_stopped()
    })?;
    room.keep();
    let tree = tree_hash(&members);
    Ok(Unpacked {
        members,
        tree,
        added,
        skipped: Skipped::default(),
    })
}

#[cfg(test)]
mod tests;
