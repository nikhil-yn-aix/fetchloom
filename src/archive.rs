//! archives unpacked into the cache member by member, and refused whole if anything is unsafe.

mod detect;
mod guard;
mod input;
mod names;
pub(crate) mod parts;
mod sevenz;
mod shards;
mod store;
mod tar;
mod zip;

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use crate::cache::{Cache, Member, parts_key, tree_hash};
use crate::digest::Sha256;
use crate::error::{Error, Kind};

pub(crate) use detect::Format;
pub(crate) use detect::{Detected, detect};
pub(crate) use input::Input;
pub(crate) use shards::{Shards, shards};

use guard::{Guard, Meter};

/// bytes moved per read, from the archive file and out of the decompressor.
const BUFFER: usize = 256 * 1024;

/// the endings of the archives fetchloom unpacks, longest first so `.tar.gz` wins over `.gz`.
const UNPACKED: [&str; 12] = [
    ".tar.bz2", ".tar.zst", ".tar.gz", ".tar.xz", ".tbz2", ".tzst", ".tar", ".tbz", ".tgz", ".txz",
    ".zip", ".7z",
];

/// what a file's name says about unpacking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packed {
    /// zip, 7z or a tar, which unpacks when its bytes agree.
    Archive,
    /// an archive fetchloom leaves packed: `rar`.
    Unsupported(&'static str),
}

/// what the file name in `path` says it is. only the name decides, so every machine decides alike
/// from the listing, and documents that are zips inside, as `.xlsx` and `.npz`, stay whole.
pub(crate) fn by_name(path: &str) -> Option<Packed> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if UNPACKED.iter().any(|ending| name.ends_with(ending)) {
        return Some(Packed::Archive);
    }
    let is_rar = name
        .rsplit_once('.')
        .is_some_and(|(_, ending)| ending == "rar");
    is_rar.then_some(Packed::Unsupported("rar"))
}

/// whether the bytes under `digest` are a zip of only the file `path` names, as `train.csv.zip`
/// holds `train.csv`. once they are dropped, the record of what they unpacked to answers.
pub(crate) fn wraps(cache: &Cache, digest: &Sha256, path: &str) -> bool {
    // unpacked members are recorded in NFC, so both answers compare in it on every machine
    let nfc = |text: &str| {
        icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
            .normalize(text)
            .into_owned()
    };
    let name = nfc(path.rsplit('/').next().unwrap_or(path));
    if let Some(members) = cache.unpacked(digest, None) {
        return matches!(members.as_slice(), [only] if only.path == name);
    }
    let Some(object) = cache.find(digest, None) else {
        return false;
    };
    matches!(
        detect::signature(&Input::File(object.clone())),
        Ok(Some(Detected::Archive(Format::Zip)))
    ) && zip::sole_member(&object).is_some_and(|member| nfc(&member) == name)
}

/// the format of an archive fetchloom does not unpack, such as rar, when that is what the cache
/// holds under `digest`, whatever the file's name says.
pub(crate) fn unsupported(cache: &Cache, digest: &Sha256, size: u64) -> Option<&'static str> {
    let object = cache.find(digest, Some(size))?;
    match detect::signature(&Input::File(object)) {
        Ok(Some(Detected::Unsupported(format))) => Some(format),
        _ => None,
    }
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

/// the operating systems a next step for an archive left packed is written for. any other unix
/// is told what linux is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum System {
    Linux,
    Macos,
    Windows,
}

impl System {
    pub(crate) const RUNNING: System = if cfg!(windows) {
        System::Windows
    } else if cfg!(target_os = "macos") {
        System::Macos
    } else {
        System::Linux
    };

    /// `text` as one word for this system's usual shell, a unix shell or powershell: `\` between
    /// folders on windows, and single-quoted when either shell would split or expand it.
    fn word(self, text: &str) -> String {
        let text = match self {
            System::Windows => text.replace('/', "\\"),
            System::Linux | System::Macos => text.to_owned(),
        };
        let plain = match self {
            System::Windows => "-_.:+=\\",
            System::Linux | System::Macos => "-_.:+=/@%,",
        };
        if !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || plain.contains(c))
        {
            return text;
        }
        let quote = match self {
            System::Windows => "''",
            System::Linux | System::Macos => r"'\''",
        };
        format!("'{}'", text.replace('\'', quote))
    }
}

/// how a person on `system` unpacks the archive at `path`, left packed, into a folder named after
/// it in `parent`, both as seen from where fetchloom runs, then that `stop` stops this warning.
pub(crate) fn unpack_elsewhere(system: System, path: &str, parent: &str, stop: &str) -> String {
    format!("{}. {stop} stops this warning", unrar(system, path, parent))
}

/// the command [`unpack_elsewhere`] gives for a rar.
fn unrar(system: System, path: &str, parent: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let named = name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(name);
    let folder = format!("{parent}{named}-unpacked");
    let archive = system.word(path);
    match system {
        // rarlab's own reader: bsdtar and some distributions' unar fail solid rar4 or rar5
        System::Linux => format!(
            "unpack it with unrar from your package manager: `unrar x {archive} {}`",
            system.word(&format!("{folder}/"))
        ),
        System::Macos => format!(
            "unpack it with unar: `brew install unar`, then `unar -D -o {} {archive}`",
            system.word(&folder)
        ),
        System::Windows => format!(
            r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x {archive} {}`"#,
            system.word(&format!("-o{folder}"))
        ),
    }
}

/// how a person on `system` unpacks an archive that came inside another, as [`unpack_elsewhere`]
/// says: with a program the system has, or on linux bsdtar for a 7z, which gnu tar cannot read.
pub(crate) fn unpack_inner(system: System, path: &str, parent: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if by_name(name) == Some(Packed::Unsupported("rar")) {
        return unrar(system, path, parent);
    }
    let ending = name.rsplit_once('.').map(|(_, ending)| ending);
    let is = |format: &str| ending.is_some_and(|ending| ending.eq_ignore_ascii_case(format));
    let folder = system.word(&format!("{parent}{}-unpacked", stem(name)));
    let archive = system.word(path);
    let untar = |tar: &str| format!("`mkdir -p {folder} && {tar} -xf {archive} -C {folder}`");
    match system {
        System::Linux if is("zip") => {
            format!("unpack it with unzip from your package manager: `unzip {archive} -d {folder}`")
        }
        System::Linux if is("7z") => format!(
            "unpack it with bsdtar from your package manager: {}",
            untar("bsdtar")
        ),
        System::Linux => format!("unpack it with tar: {}", untar("tar")),
        System::Macos if is("zip") => {
            format!("unpack it with the unzip macos has: `unzip {archive} -d {folder}`")
        }
        System::Macos => format!("unpack it with the tar macos has: {}", untar("tar")),
        System::Windows if is("zip") => format!(
            "unpack it with powershell's Expand-Archive: `Expand-Archive {archive} -DestinationPath {folder}`"
        ),
        System::Windows => format!(
            "unpack it with the tar windows has: `mkdir {folder}; tar -xf {archive} -C {folder}`"
        ),
    }
}

/// [`unpack_elsewhere`] for a zip spanned over `.z01` files: zip joins the parts in `parent` for
/// unzip to unpack, and on windows 7-Zip reads them itself.
pub(crate) fn join_elsewhere(system: System, path: &str, parent: &str, stop: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let folder = format!("{parent}{}-unpacked", stem(name));
    let archive = system.word(path);
    let joined = system.word(&format!("{parent}{}-joined.zip", stem(name)));
    let unzip = format!(
        "`zip -s 0 {archive} --out {joined}`, then `unzip {joined} -d {}`",
        system.word(&folder)
    );
    match system {
        System::Linux => format!(
            "join its parts with zip from your package manager, then unpack them: {unzip}. {stop} stops this warning"
        ),
        System::Macos => format!(
            "join its parts with the zip macos has, then unpack them: {unzip}. {stop} stops this warning"
        ),
        System::Windows => format!(
            r#"unpack it with 7-Zip, which reads its parts: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x {archive} {}`. {stop} stops this warning"#,
            system.word(&format!("-o{folder}"))
        ),
    }
}

/// what an archive may unpack to, against archives built to fill a disk. only
/// `FETCHLOOM_ARCHIVE_*` or the settings file set it, never a data.toml someone else wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// entries of any kind, folders included.
    pub(crate) members: u64,
    pub(crate) bytes: u64,
    /// bytes written for each compressed byte read. deflate cannot pass about 1032, so the
    /// default lets nearly all zip data through and still stops bzip2, xz and zstd bombs.
    pub(crate) ratio: u64,
    /// the memory one archive's decompressor may take.
    pub(crate) memory: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            members: 10_000_000,
            bytes: 16 << 40,
            ratio: 1_000,
            memory: tar::XZ_MEMORY,
        }
    }
}

/// every file an archive unpacked to, sorted by path, and the tree hash over them.
#[derive(Debug)]
pub(crate) struct Unpacked {
    pub(crate) members: Vec<Member>,
    pub(crate) tree: Sha256,
    /// the files new to the cache, by digest.
    pub(crate) added: Vec<Sha256>,
    /// the files a system writes by itself that it held, left out of `members` and the tree.
    pub(crate) skipped: Skipped,
}

/// the files a system writes by itself that an archive held, such as finder's `.DS_Store` and
/// the `._` twins under `__MACOSX`: how many, and the first, by its path in the archive.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub(crate) files: u64,
    pub(crate) first: Option<String>,
}

/// stores a guarded member, using its declared size only as a storage-scheduling hint.
type Keep<'k> = dyn FnMut(&mut Guard<'_>, String, u64, &mut dyn Read) -> Result<(), Error> + 'k;

/// unpacks `input` into the cache, leaving no object behind when refused part way. `writers`
/// caps the writer threads, zero storing on the reader. `watch` hears the bytes read, then the
/// whole archive once reading ends, though files may still be written.
///
/// # Errors
///
/// `source.invalid` for an unsafe or damaged archive, `local.io` for a read or a cache write that
/// fails, and `interrupted` once `watch` returns true.
pub(crate) fn unpack(
    input: &Input,
    format: Format,
    limits: &Limits,
    cache: &Cache,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Unpacked, Error> {
    let meter = Meter::default();
    let filesystem = crate::space::Filesystem::of(&cache.root().join("tmp"));
    let mut guard = Guard::new(limits, watch, &meter).writing_to(cache.root(), filesystem);
    let (mut members, added) = store::store_all(cache, writers, |writers| {
        let mut buffer = vec![0; BUFFER];
        let mut keep = |guard: &mut Guard<'_>, path: String, size: u64, from: &mut dyn Read| {
            writers.keep(guard, path, size, from, &mut buffer)
        };
        match format {
            Format::Zip => zip::unpack(input, &mut guard, &mut keep)?,
            Format::SevenZ => sevenz::unpack(input, &mut guard, &mut keep)?,
            Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst | Format::TarBz2 => {
                tar::unpack(input, format, &mut guard, &mut keep)?;
            }
        }
        meter.read_to_end();
        guard.check_stopped()
    })?;
    members.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let links = guard.links();
    if !links.is_empty() {
        let mut linked: HashMap<String, (Sha256, u64)> = HashMap::new();
        for (path, target) in links {
            // each target is a file stored before it, or a link to one
            let held = members
                .binary_search_by(|member| member.path.as_str().cmp(&target))
                .ok()
                .map(|at| (members[at].sha256, members[at].size))
                .or_else(|| linked.get(&target).copied());
            if let Some(held) = held {
                linked.insert(path, held);
            }
        }
        let linked = linked
            .into_iter()
            .map(|(path, (sha256, size))| Member { path, size, sha256 });
        members.extend(linked);
        members.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    }
    let tree = tree_hash(&members);
    Ok(Unpacked {
        members,
        tree,
        added,
        skipped: guard.left_out(),
    })
}

/// unpacks the cached `archive`, recording the result only when it is `tree` or none is known,
/// so changed bytes never replace data.lock's pin. none when it is no archive fetchloom unpacks.
/// fails as [`unpack`] does within `limits`, or `local.io` when the cache lost the archive or
/// cannot record it.
pub(crate) fn unpack_cached(
    cache: &Cache,
    archive: &Sha256,
    size: u64,
    tree: Option<&Sha256>,
    limits: &Limits,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let object = cache.find(archive, Some(size)).ok_or_else(lost)?;
    unpack_recorded(
        cache,
        archive,
        &Input::File(object),
        tree,
        limits,
        watch,
        writers,
    )
}

/// [`unpack_cached`], failing as it does, for an archive split into `parts`, each the SHA-256 and
/// size of a cached object, in order. its tree is recorded under the key [`parts_key`] gives.
pub(crate) fn unpack_cached_parts(
    cache: &Cache,
    parts: &[(Sha256, u64)],
    tree: Option<&Sha256>,
    limits: &Limits,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let found = parts
        .iter()
        .map(|(part, size)| Some((cache.find(part, Some(*size))?, *size)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(lost)?;
    let key = parts_key(parts.iter().copied());
    unpack_recorded(
        cache,
        &key,
        &Input::Joined(found),
        tree,
        limits,
        watch,
        writers,
    )
}

/// unpacks `input` and records what it unpacked to under `key`, as [`unpack_cached`] describes.
fn unpack_recorded(
    cache: &Cache,
    key: &Sha256,
    input: &Input,
    tree: Option<&Sha256>,
    limits: &Limits,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let format = match detect(input, limits.memory)? {
        Detected::Archive(format) => format,
        Detected::Unsupported(_) | Detected::Plain => return Ok(None),
    };
    let unpacked = unpack(input, format, limits, cache, watch, writers)?;
    if tree.is_none_or(|tree| *tree == unpacked.tree) {
        cache
            .record_unpacked(key, &unpacked.members)
            .map_err(|err| {
                Error::io(
                    format_args!(
                        "could not save the archive's list of files in {}",
                        cache.root().join("unpacked").display()
                    ),
                    &err,
                )
            })?;
    }
    Ok(Some(unpacked))
}

fn lost() -> Error {
    Error::new(
        Kind::LocalIo,
        "the cache lost an archive while unpacking it",
    )
    .help("run the same command again to download it")
}

const REFUSED_HELP: &str = "keep the archive packed with extract = false on its dataset in data.toml, or with --no-extract";

fn refused(message: impl Into<String>) -> Error {
    Error::new(Kind::SourceInvalid, message).help(REFUSED_HELP)
}

/// an archive past a limit that `variable` or the settings file's `key` raises.
fn past(message: impl Into<String>, variable: &str, key: &str) -> Error {
    Error::new(Kind::SourceInvalid, message).help(format!(
        "if you trust the archive, raise {variable}, or {key} in the settings file. otherwise {REFUSED_HELP}"
    ))
}

fn refused_member(path: &str, reason: &str) -> Error {
    refused(format!(
        "the archive holds \"{}\", which fetchloom refuses because {reason}",
        crate::name::abridged(path)
    ))
}

/// the error for an archive not read to its end, unpacked within `memory`; a disk's own failure
/// stays `local.io`.
fn damaged(at: Option<&str>, err: &(dyn std::error::Error + 'static), memory: u64) -> Error {
    if let Some(error) = crate::error::io_cause(err) {
        let doing = match at {
            Some(path) => format!("could not read the cached archive at \"{path}\""),
            None => "could not read the cached archive".to_owned(),
        };
        return Error::io(doing, error);
    }
    if tar::needs_too_much_memory(err) {
        return past(
            format!(
                "the archive's xz stream needs more than {} of memory to unpack",
                crate::units::size(memory)
            ),
            "FETCHLOOM_MEMORY, which gives each archive a quarter of it",
            "memory",
        );
    }
    let at = at.map(crate::name::abridged);
    let message = match (at, ends_early(err)) {
        (Some(path), true) => {
            format!("the archive is damaged: it ends in the middle of \"{path}\"")
        }
        (None, true) => "the archive is damaged: it ends in the middle of a file".to_owned(),
        (Some(path), false) => format!("the archive is damaged at \"{path}\": {err}"),
        (None, false) => format!("the archive is damaged: {err}"),
    };
    Error::new(Kind::SourceInvalid, message).help(DAMAGED_HELP)
}

/// whether `err` says the archive's bytes ran out before what they hold did: a decoder's
/// end-of-file error, or the tar crate's own words for one.
fn ends_early(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(err);
    while let Some(error) = cause {
        let Some(io) = error.downcast_ref::<io::Error>() else {
            cause = error.source();
            continue;
        };
        if io.kind() == io::ErrorKind::UnexpectedEof
            || matches!(
                io.to_string().as_str(),
                "unexpected EOF during skip" | "failed to read entire block"
            )
        {
            return true;
        }
        cause = io
            .get_ref()
            .map(|inner| inner as &(dyn std::error::Error + 'static));
    }
    false
}

const DAMAGED_HELP: &str = "ask the dataset's authors for a fixed copy, or keep it packed with extract = false in data.toml or --no-extract";

fn cut_short() -> Error {
    Error::new(
        Kind::SourceInvalid,
        "the archive is cut short: a zip lists its files at its end, and that list is missing",
    )
    .help(DAMAGED_HELP)
}

fn not_written(path: &str, err: &io::Error) -> Error {
    Error::io(
        format_args!("could not write \"{path}\" into the cache"),
        err,
    )
    .or_help("free space on the cache's disk, or set FETCHLOOM_CACHE to a folder fetchloom may write to, then run the same command again")
}

fn unreadable(archive: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not read {}", archive.display()), err)
}

#[cfg(test)]
mod samples;
#[cfg(test)]
mod tests;
