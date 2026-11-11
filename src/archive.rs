//! archives unpacked member by member into the cache, and refused whole when anything in them is
//! unsafe.

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

/// what the name of the file at `path` says it is. only a name decides whether a file may
/// unpack, so every machine decides alike from the listing, and documents such as `.xlsx` or
/// `.npz`, which are zips inside, stay whole.
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

/// whether the bytes the cache holds under `digest` are a zip of the one file `path` names, as a
/// source sends a file it wrapped: `train.csv` as `train.csv.zip`. once their own bytes are
/// dropped, what this machine unpacked them to answers.
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
        detect(&Input::File(object.clone())),
        Ok(Detected::Archive(Format::Zip))
    ) && zip::sole_member(&object).is_some_and(|member| nfc(&member) == name)
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
    /// the system this build runs on.
    pub(crate) const RUNNING: System = if cfg!(windows) {
        System::Windows
    } else if cfg!(target_os = "macos") {
        System::Macos
    } else {
        System::Linux
    };

    /// `text` as one word of a command typed into this system's usual shell, a unix shell or
    /// powershell, with `\` between folders on windows. a word either shell would split or
    /// expand is single-quoted.
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

/// how a person on `system` unpacks the archive at `path`, which fetchloom leaves packed: a
/// command that unpacks it into a folder named after it in `parent`, both as they read from
/// where fetchloom runs, then `stop`, what keeps the archive packed without this warning. a
/// project's `parent` is the archive's own folder, inside the data folder git ignores; `get`'s
/// is outside the folder it fills, which a later `get` would otherwise refuse to fill again.
pub(crate) fn unpack_elsewhere(system: System, path: &str, parent: &str, stop: &str) -> String {
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
            "unpack it with unrar from your package manager: `unrar x {archive} {}`. {stop} stops this warning",
            system.word(&format!("{folder}/"))
        ),
        System::Macos => format!(
            "unpack it with unar: `brew install unar`, then `unar -D -o {} {archive}`. {stop} stops this warning",
            system.word(&folder)
        ),
        System::Windows => format!(
            r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x {archive} {}`. {stop} stops this warning"#,
            system.word(&format!("-o{folder}"))
        ),
    }
}

/// how a person on `system` unpacks the zip at `path` whose bytes go on in `.z01` files beside
/// it, which fetchloom leaves packed, as [`unpack_elsewhere`] tells it for a rar: zip joins the
/// parts into one archive in `parent` for unzip to unpack, and on windows 7-Zip reads the parts
/// itself.
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

/// what an archive may unpack to. every limit counts the bytes actually written, never the sizes
/// the archive declares. they guard against archives built to fill a disk, so only the person
/// running fetchloom moves them, through `FETCHLOOM_ARCHIVE_*` or the settings file, and never a
/// data.toml someone else wrote.
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

/// the limits of this run, once it read its settings.
static LIMITS: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();

/// sets the limits every archive in the run unpacks within.
pub(crate) fn set_limits(limits: Limits) {
    let _ = LIMITS.set(limits);
}

impl Limits {
    /// the limits of this run: what its settings say, else the defaults.
    pub(crate) fn current() -> Limits {
        LIMITS.get().copied().unwrap_or_default()
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

/// stores one file the guard let through, reading it from the archive.
type Keep<'k> = dyn FnMut(&mut Guard<'_>, String, &mut dyn Read) -> Result<(), Error> + 'k;

/// unpacks every file in `input` into the cache. an archive refused part way leaves no object
/// behind. `writers` caps its writer threads; zero keeps storage on the reading thread.
///
/// # Errors
///
/// `source.invalid` for an unsafe or damaged archive, `local.io` when the cached archive cannot
/// be read or the cache cannot take its files, and `interrupted` once `watch` returns true. `watch` hears how many bytes of the
/// archive were read so far.
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
        let mut keep = |guard: &mut Guard<'_>, path: String, from: &mut dyn Read| {
            writers.keep(guard, path, from, &mut buffer)
        };
        match format {
            Format::Zip => zip::unpack(input, &mut guard, &mut keep)?,
            Format::SevenZ => sevenz::unpack(input, &mut guard, &mut keep)?,
            Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst | Format::TarBz2 => {
                tar::unpack(input, format, &mut guard, &mut keep)?;
            }
        }
        guard.check_stopped()
    })?;
    members.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let tree = tree_hash(&members);
    Ok(Unpacked {
        members,
        tree,
        added,
        skipped: guard.left_out(),
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
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let object = cache.find(archive, Some(size)).ok_or_else(lost)?;
    unpack_recorded(cache, archive, &Input::File(object), tree, watch, writers)
}

/// [`unpack_cached`] for an archive split into `parts`, each the SHA-256 and size of an object
/// the cache holds, in order. what it unpacked to is recorded under the key [`parts_key`] gives.
///
/// # Errors
///
/// as [`unpack_cached`].
pub(crate) fn unpack_cached_parts(
    cache: &Cache,
    parts: &[(Sha256, u64)],
    tree: Option<&Sha256>,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let found = parts
        .iter()
        .map(|(part, size)| Some((cache.find(part, Some(*size))?, *size)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(lost)?;
    let key = parts_key(parts.iter().copied());
    unpack_recorded(cache, &key, &Input::Joined(found), tree, watch, writers)
}

/// unpacks `input` and records what it unpacked to under `key`, as [`unpack_cached`] describes.
fn unpack_recorded(
    cache: &Cache,
    key: &Sha256,
    input: &Input,
    tree: Option<&Sha256>,
    watch: &dyn Fn(u64) -> bool,
    writers: Option<usize>,
) -> Result<Option<Unpacked>, Error> {
    let format = match detect(input)? {
        Detected::Archive(format) => format,
        Detected::Unsupported(_) | Detected::Plain => return Ok(None),
    };
    let unpacked = unpack(input, format, &Limits::current(), cache, watch, writers)?;
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

/// an archive past a limit the person running fetchloom may move, as `variable` and the
/// settings file's `key` do.
fn past(message: impl Into<String>, variable: &str, key: &str) -> Error {
    Error::new(Kind::SourceInvalid, message).help(format!(
        "if you trust the archive, raise {variable}, or {key} in the settings file. otherwise {REFUSED_HELP}"
    ))
}

/// an archive holding a member that fetchloom refuses for its name or its kind.
fn refused_member(path: &str, reason: &str) -> Error {
    refused(format!(
        "the archive holds \"{path}\", which fetchloom refuses because {reason}"
    ))
}

/// an archive that could not be read to the end: cut short, corrupted or not what it seemed, or
/// refused when its xz stream asks for too much memory.
fn damaged(at: Option<&str>, err: &(dyn std::error::Error + 'static)) -> Error {
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
                crate::units::size(Limits::current().memory)
            ),
            "FETCHLOOM_MEMORY, which gives each archive a quarter of it",
            "memory",
        );
    }
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
    .or_help("free space on the cache's disk, or set FETCHLOOM_CACHE to a folder fetchloom may write to, then run the same command again")
}

fn unreadable(archive: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not read {}", archive.display()), err)
}

#[cfg(test)]
mod samples;
#[cfg(test)]
mod tests;
