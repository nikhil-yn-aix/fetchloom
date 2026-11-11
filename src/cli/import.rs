//! `add <path>`: files on this machine pinned by their bytes. every plain file below a folder is
//! found without following a link, checked by name as an archive's members are, and copied into
//! the cache, hashed as it is copied. the files themselves are only read: never changed, moved or
//! linked, since a link to the cache's copy would make a person's file read-only.

use std::collections::HashMap;
use std::collections::hash_map::Entry as Slot;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use icu_normalizer::ComposingNormalizerBorrowed;

use super::Context;
use crate::cache::{Cache, Imported, Sealed};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::project::lock::{self, Locked, LockedFile};
use crate::project::manifest::Source;
use crate::source::{self, Access, local};
use crate::stop::Stop;
use crate::volume::is_plain;
use crate::{name, place, units};

/// the longest path a file may have below the folder, in bytes, as an archive's members may.
const LONGEST_PATH: usize = 4096;

/// the file or folder `typed`, a path on this machine as [`local::is_path`] reads it, names: from
/// the root, from `here`, from the home folder `home` for a leading `~`, or as a `file:` link
/// gives it. links in the path are followed, since the person named it, and the result names no
/// link.
///
/// # Errors
///
/// nothing is there, `~` names another user's home folder, or a `file:` link names a file on
/// another machine.
pub(super) fn located(typed: &str, here: &Path, home: Option<&Path>) -> Result<PathBuf, Error> {
    let typed = typed.trim();
    let path = if typed
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        url::Url::parse(typed)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!("\"{typed}\" names no file on this machine"),
                )
                .help("give the path of the folder or file, as in `fetchloom add ./flowers`")
            })?
    } else if let Some(rest) = typed.strip_prefix('~') {
        let rest = match rest.chars().next() {
            None => "",
            Some('/' | '\\') => &rest[1..],
            Some(_) => {
                return Err(Error::new(
                    Kind::Usage,
                    format!("\"{typed}\" names another user's home folder"),
                )
                .help("fetchloom reads ~ only as your own home folder, as in ~/flowers"));
            }
        };
        let home = home.ok_or_else(|| {
            Error::new(
                Kind::Usage,
                "fetchloom cannot tell where your home folder is",
            )
            .help("give the whole path instead of ~")
        })?;
        home.join(rest)
    } else {
        here.join(typed)
    };
    std::fs::canonicalize(&path)
        .map_err(|err| Error::io(format_args!("could not find {typed}"), &err))
}

/// refuses to import `start`, a path with no link in it, where it overlaps what fetchloom keeps:
/// a folder holding the project at `root`, anything in its data folder `data`, which placing
/// writes, and the cache at `cache`. `shown` is how messages name `start`.
///
/// # Errors
///
/// `start` overlaps one of them.
pub(super) fn refuse_overlap(
    start: &Path,
    shown: &str,
    root: &Path,
    data: &Path,
    cache: &Path,
) -> Result<(), Error> {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let overlap = if real(root).is_some_and(|root| root.starts_with(start)) {
        Some("holds this project")
    } else if real(data).is_some_and(|data| start.starts_with(data)) {
        Some("is in the data folder, where fetchloom places what data.lock pins")
    } else if let Some(cache) = real(cache) {
        if start.starts_with(&cache) {
            Some("is in fetchloom's cache")
        } else if cache.starts_with(start) {
            Some("holds fetchloom's cache")
        } else {
            None
        }
    } else {
        None
    };
    match overlap {
        None => Ok(()),
        Some(overlap) => Err(Error::new(Kind::Usage, format!("{shown} {overlap}")).help(
            "name a folder of data outside the project's data folder, as in `fetchloom add ./raw`",
        )),
    }
}

/// the name a dataset imported from `start` takes without `--as`: the folder's, or the file's
/// without its endings, as the data folder would show it.
pub(super) fn dataset_name(start: &Path, is_file: bool) -> Option<String> {
    let own = start.file_name()?.to_str()?;
    if is_file {
        name::dataset_name(&name::folder_for(own))
    } else {
        name::dataset_name(own)
    }
}

/// a plain file found below the folder an import names.
#[derive(Debug)]
pub(super) struct Found {
    /// where the file goes in the dataset: NFC, with `/` between folders.
    pub(super) path: String,
    /// the file on this machine.
    source: PathBuf,
    pub(super) size: u64,
}

/// the files a walk found to import, sorted by path, and the links and special files it left
/// out, by their path.
#[derive(Debug, Default)]
pub(super) struct Walked {
    /// the folder every file is opened below: the folder imported, or the one holding the file.
    root: PathBuf,
    pub(super) files: Vec<Found>,
    pub(super) skipped: Vec<String>,
    /// what the system writes into folders by itself, such as finder's `.DS_Store`, left out.
    pub(super) system: Vec<String>,
}

impl Walked {
    /// what the files take, all together.
    pub(super) fn bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0, |total, file| total.saturating_add(file.size))
    }
}

/// the plain files at `start`, a folder or one file without a link in its path, that `entry`'s
/// patterns keep, each checked by name. `shown` is how messages name `start`. repository metadata
/// is left out unseen; links and special files are left out, never followed, and named in
/// [`Walked::skipped`].
///
/// # Errors
///
/// a folder cannot be read, a kept name cannot be pinned on every machine, a select pattern keeps
/// nothing, more files are kept than one dataset may list, or a stop is asked.
pub(super) fn walk(
    start: &Path,
    shown: &str,
    source: &Source,
    stop: &Stop,
) -> Result<Walked, Error> {
    let found = std::fs::metadata(start)
        .map_err(|err| Error::io(format_args!("could not read {shown}"), &err))?;
    let mut walked = Walked {
        root: start.to_path_buf(),
        ..Walked::default()
    };
    if found.is_dir() {
        let mut folders = vec![(String::new(), start.to_path_buf())];
        while let Some((folder, path)) = folders.pop() {
            stop.check()?;
            let entries = std::fs::read_dir(&path).map_err(|err| {
                Error::io(format_args!("could not read {}", path.display()), &err)
            })?;
            for listed in entries {
                let listed = listed.map_err(|err| {
                    Error::io(format_args!("could not read {}", path.display()), &err)
                })?;
                let file_name = listed.file_name();
                let Some(file_name) = file_name.to_str() else {
                    return Err(refused(
                        shown,
                        &format!("{folder}/{}", file_name.to_string_lossy()),
                        "it is not text every machine can read",
                    ));
                };
                if name::metadata(file_name).is_some() {
                    continue;
                }
                let relative = if folder.is_empty() {
                    file_name.to_owned()
                } else {
                    format!("{folder}/{file_name}")
                };
                if name::made_by_system(file_name) {
                    walked.system.push(relative);
                    continue;
                }
                let kind = listed.file_type().map_err(|err| {
                    Error::io(
                        format_args!("could not read {}", listed.path().display()),
                        &err,
                    )
                })?;
                if kind.is_symlink() {
                    walked.skipped.push(relative);
                } else if kind.is_dir() {
                    folders.push((relative, listed.path()));
                } else {
                    let metadata = listed.metadata().map_err(|err| {
                        Error::io(
                            format_args!("could not read {}", listed.path().display()),
                            &err,
                        )
                    })?;
                    if is_plain(&metadata) {
                        walked.files.push(Found {
                            path: composed(&relative),
                            source: listed.path(),
                            size: metadata.len(),
                        });
                    } else {
                        walked.skipped.push(relative);
                    }
                }
            }
            // a folder of more files than a dataset may list is refused before it is all read
            if walked.files.len() > source::most_files() {
                return Err(source::too_many(
                    shown,
                    "import a smaller folder, or leave files out with --exclude",
                ));
            }
        }
    } else if is_plain(&found) {
        let file_name = start
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| refused(shown, shown, "its name is not text every machine can read"))?;
        walked.files.push(Found {
            path: composed(file_name),
            source: start.to_path_buf(),
            size: found.len(),
        });
        walked.root = start.parent().unwrap_or(start).to_path_buf();
    } else {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} is no folder or plain file, so there is nothing to import"),
        )
        .help("name a folder or a file, as in `fetchloom add ./flowers`"));
    }
    walked.skipped.sort_unstable();
    walked.system.sort_unstable();
    kept(&mut walked, source, shown)?;
    walked.files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    checked_names(&walked.files, shown)?;
    Ok(walked)
}

/// keeps the files `source`'s patterns select and do not exclude. a select pattern that keeps
/// nothing is a mistake worth stopping for.
fn kept(walked: &mut Walked, source: &Source, shown: &str) -> Result<(), Error> {
    let matches = |globs: &[Glob], path: &str| globs.iter().any(|glob| glob.matches(path));
    if let Some(glob) = source
        .select
        .iter()
        .find(|glob| !walked.files.iter().any(|file| glob.matches(&file.path)))
    {
        return Err(Error::new(
            Kind::Usage,
            format!("--select \"{}\" keeps no file of {shown}", glob.as_str()),
        )
        .help("check the pattern against the paths below the folder, such as \"**/*.csv\""));
    }
    walked.files.retain(|file| {
        (source.select.is_empty() || matches(&source.select, &file.path))
            && !matches(&source.exclude, &file.path)
    });
    if walked.files.is_empty() {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} holds no plain file to import"),
        )
        .help("name a folder that holds files, or loosen --select and --exclude"));
    }
    Ok(())
}

/// `path` in NFC, as an archive's member is kept, so a name written either way pins alike.
fn composed(path: &str) -> String {
    if path.is_ascii() {
        path.to_owned()
    } else {
        ComposingNormalizerBorrowed::new_nfc()
            .normalize(path)
            .into_owned()
    }
}

/// refuses a name data.lock could not pin for every machine, as an archive's member is refused:
/// one that holds a control character or a `\`, names a drive, is too long, cannot be stored here,
/// or is one name with another on disks that ignore case or unicode composition.
fn checked_names(files: &[Found], shown: &str) -> Result<(), Error> {
    let mut seen: HashMap<String, (&str, bool)> = HashMap::new();
    for file in files {
        let path = file.path.as_str();
        if path.len() > LONGEST_PATH {
            let start: String = path.chars().take(60).collect();
            return Err(refused(
                shown,
                &format!("{start}…"),
                &format!(
                    "its path is longer than {}",
                    units::count(LONGEST_PATH as u64, "byte", "bytes")
                ),
            ));
        }
        if let Some(reason) = name::escapes(path) {
            return Err(refused(shown, &path.escape_debug().to_string(), reason));
        }
        if let Some(reason) = name::unstorable(path) {
            return Err(refused(shown, path, &reason));
        }
        let ends = path.match_indices('/').map(|(end, _)| end);
        for (end, is_folder) in ends.map(|end| (end, true)).chain([(path.len(), false)]) {
            let named = &path[..end];
            match seen.entry(name::fold(named)) {
                Slot::Vacant(free) => {
                    free.insert((named, is_folder));
                }
                Slot::Occupied(taken) => {
                    let (other, was_folder) = *taken.get();
                    if other == named && was_folder && is_folder {
                        continue;
                    }
                    let problem = if other == named {
                        format!("{shown} holds \"{named}\" both as a file and as a folder")
                    } else {
                        format!(
                            "{shown} holds \"{other}\" and \"{named}\", which are one name on disks that ignore case"
                        )
                    };
                    return Err(Error::new(Kind::SourceInvalid, problem)
                        .help("rename one of them, or leave one out with --exclude"));
                }
            }
        }
    }
    Ok(())
}

/// the refusal of `path` below `shown`, for `reason`.
fn refused(shown: &str, path: &str, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("{shown} holds \"{path}\", which fetchloom refuses because {reason}"),
    )
    .help("rename it, or leave it out with --exclude")
}

/// refuses, before any byte is copied, files `walked` found in `shown` that the cache's disk
/// cannot take: one larger than FAT32 holds there, and, unless the person said yes, more than
/// the disk has free, which a disk that clones them would not need.
///
/// # Errors
///
/// a file is too large for the cache's disk, or the disk lacks the room and nobody said yes.
pub(super) fn room(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
    shown: &str,
) -> Result<(), Error> {
    crate::space::cache_holds(
        cache.root(),
        walked
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.size)),
    )?;
    if context.yes {
        return Ok(());
    }
    let needed = walked.bytes();
    let free = crate::space::free(cache.root());
    if crate::space::short(needed, free).is_none() {
        return Ok(());
    }
    super::ask_room(
        &context.output,
        &format!("importing {shown}"),
        needed,
        free.unwrap_or_default(),
        cache.root(),
    )
}

/// the files `found` copied into the cache and pinned by path, size and SHA-256, in `found`'s
/// order, with no link: no source has an import's bytes.
pub(super) struct Copied {
    pub(super) files: Vec<LockedFile>,
    /// whether every file is a clone sharing its blocks, which takes no room of its own.
    pub(super) cloned: bool,
}

/// copies every file `walked` found into the cache on the placement workers, as many at once as
/// the threads setting allows, and pins each. nothing is in the cache under its hash until every
/// file is on disk, and a failure or a stop leaves none of them there.
///
/// # Errors
///
/// a file cannot be read, the cache cannot take it, or a stop is asked.
pub(super) fn into_cache(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
) -> Result<Copied, Error> {
    let found = &walked.files;
    // many objects flush together, since one flush each costs far more than writing a small file
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = if found.len() > crate::cache::MOST_FLUSHED_ALONE {
        cache
            .batch()
            .map_err(|err| Error::io("could not open the cache to flush what is imported", &err))?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush_each = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush_each = true;
    let Sealing { sealed, cloned } = sealed(context, cache, walked, flush_each)?;
    context.stop.check()?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        batch
            .flush()
            .map_err(|err| Error::io("could not flush the imported files to disk", &err))?;
    }
    let published = place::run_all(&sealed, context.threads, |slot| {
        let sealed: Option<Sealed> = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        sealed.map(|sealed| sealed.publish(cache))
    });
    let mut files = Vec::with_capacity(found.len());
    for (file, stored) in found.iter().zip(published) {
        let stored = stored
            .ok_or_else(|| Error::new(Kind::LocalIo, "importing stopped unexpectedly"))?
            .map_err(|err| {
                Error::io(
                    format_args!("could not put {} in the cache", file.path),
                    &cache.explained(&cache.root().join("objects"), err),
                )
            })?;
        if stored.new {
            context.work.wrote(&stored.digest);
        }
        files.push(LockedFile {
            path: file.path.clone(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: String::new(),
            unpacks_to: None,
        });
    }
    Ok(Copied { files, cloned })
}

/// a copy of each file an import found, sealed in a staging folder of the cache and waiting to be
/// put in place under its hash, in the files' order.
struct Sealing {
    /// each taken once to be put in place.
    sealed: Vec<Mutex<Option<Sealed>>>,
    /// whether every copy is a clone.
    cloned: bool,
}

/// [`Sealing`] for the files `walked` found. a failure stops the files not begun yet.
fn sealed(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
    flush_each: bool,
) -> Result<Sealing, Error> {
    let found = &walked.files;
    #[cfg(unix)]
    let beneath = Beneath::of(&walked.root).map_err(|err| {
        Error::io(
            format_args!("could not read {}", walked.root.display()),
            &err,
        )
    })?;
    // a folder swapped for a link while the files are read is refused, not read through
    #[cfg(unix)]
    let open = |file: &Found| beneath.open(below(&walked.root, file)?);
    #[cfg(not(unix))]
    let open = |file: &Found| {
        below(&walked.root, file)?;
        crate::cache::read_unfollowed(&file.source)
    };
    let staging = cache.staging().map_err(|err| {
        Error::io(
            format_args!(
                "could not make a folder to import into, in {}",
                cache.root().join("tmp").display()
            ),
            &err,
        )
    })?;
    let what = units::count(found.len() as u64, "file", "files");
    let bar = context.output.bar("Copying", what.clone());
    bar.start(Some(walked.bytes()), 0);
    let ticker = context
        .output
        .ticker("Copying", what, found.iter().map(|file| Some(file.size)));
    let failed = AtomicBool::new(false);
    let stop = &context.stop;
    let imported = place::run_all(found, context.threads, |file| {
        if failed.load(Ordering::Relaxed) {
            return None;
        }
        let imported =
            open(file).and_then(|source| cache.import_file(source, &staging, flush_each, stop));
        bar.advance(file.size);
        ticker.advance(file.size);
        if imported.is_err() {
            failed.store(true, Ordering::Relaxed);
        }
        Some(imported)
    });
    drop((bar, ticker));
    let mut sealed = Vec::with_capacity(found.len());
    let mut cloned = true;
    for (file, imported) in found.iter().zip(imported) {
        match imported {
            Some(Ok(Imported {
                sealed: one,
                cloned: shared,
            })) => {
                cloned &= shared;
                sealed.push(Mutex::new(Some(one)));
            }
            Some(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => {
                return Err(Stop::stopped());
            }
            Some(Err(err)) => {
                return Err(Error::io(
                    format_args!("could not copy {} into the cache", file.source.display()),
                    &err,
                ));
            }
            None => {}
        }
    }
    Ok(Sealing { sealed, cloned })
}

/// where `file` is below `root`, the folder a walk found it in.
fn below<'f>(root: &Path, file: &'f Found) -> std::io::Result<&'f Path> {
    file.source
        .strip_prefix(root)
        .map_err(|_| std::io::Error::other("a file outside the folder imported"))
}

/// the folder an import reads, open, so each file is opened below it one name at a time and never
/// through a link, wherever a link appears meanwhile.
#[cfg(unix)]
struct Beneath(std::fs::File);

#[cfg(unix)]
impl Beneath {
    fn of(folder: &Path) -> std::io::Result<Beneath> {
        std::fs::File::open(folder).map(Beneath)
    }

    /// the plain file at `relative` below the folder.
    fn open(&self, relative: &Path) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsFd as _;

        use rustix::fs::{Mode, OFlags};

        let refused = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "it is reached through a link, or is a link or a special file, which fetchloom never imports",
            )
        };
        let mut names = relative.components().peekable();
        let mut folder: Option<std::os::fd::OwnedFd> = None;
        while let Some(component) = names.next() {
            let std::path::Component::Normal(name) = component else {
                return Err(refused());
            };
            let at = folder
                .as_ref()
                .map_or_else(|| self.0.as_fd(), |folder| folder.as_fd());
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            if names.peek().is_some() {
                let next = rustix::fs::openat(at, name, flags | OFlags::DIRECTORY, Mode::empty())
                    .map_err(|err| match err {
                    rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => refused(),
                    err => err.into(),
                })?;
                folder = Some(next);
            } else {
                // a fifo must not block opening before it can be refused
                let file = rustix::fs::openat(at, name, flags | OFlags::NONBLOCK, Mode::empty())
                    .map_err(|err| match err {
                        rustix::io::Errno::LOOP => refused(),
                        err => err.into(),
                    })?;
                let file = std::fs::File::from(file);
                return if is_plain(&file.metadata()?) {
                    Ok(file)
                } else {
                    Err(refused())
                };
            }
        }
        Err(refused())
    }
}

/// the pin of `files`, imported for the dataset `name` as data.toml's `source` gives it. it names
/// no source to fetch from: relocking learns what its archives unpack to, and stamps when it was
/// pinned.
pub(super) fn pinned(name: &str, source: &Source, files: Vec<LockedFile>) -> Locked {
    Locked {
        name: name.to_owned(),
        reference: local::IMPORTED.to_owned(),
        spec: lock::spec(source),
        extract: source.extract,
        resolved: local::IMPORTED.to_owned(),
        title: None,
        license: None,
        doi: None,
        access: Access::Unknown,
        timestamp: String::new(),
        files,
    }
}

/// the first way the files a folder holds, `found`, differ from `pinned`, the files data.lock pins
/// for a dataset, both sorted by path: a file only one of them has, or another size. none when
/// they may hold the same bytes, which only hashing tells.
pub(super) fn first_difference(found: &[Found], pinned: &[LockedFile]) -> Option<String> {
    let mut found = found.iter().peekable();
    let mut pinned = pinned.iter().peekable();
    loop {
        return match (found.peek(), pinned.peek()) {
            (None, None) => None,
            (Some(file), None) => Some(format!("{} is not in data.lock", file.path)),
            (None, Some(pin)) => Some(format!("{} is missing", pin.path)),
            (Some(file), Some(pin)) if file.path < pin.path => {
                Some(format!("{} is not in data.lock", file.path))
            }
            (Some(file), Some(pin)) if file.path > pin.path => {
                Some(format!("{} is missing", pin.path))
            }
            (Some(file), Some(pin)) if file.size != pin.size => Some(format!(
                "{} has {}, and data.lock pins {}",
                file.path,
                units::count(file.size, "byte", "bytes"),
                units::count(pin.size, "byte", "bytes")
            )),
            _ => {
                found.next();
                pinned.next();
                continue;
            }
        };
    }
}

/// the first file of `copied` that holds other bytes than `pinned` pins at its path, both sorted
/// by path and alike in paths and sizes.
pub(super) fn first_other_bytes(copied: &[LockedFile], pinned: &[LockedFile]) -> Option<String> {
    copied
        .iter()
        .zip(pinned)
        .find(|(file, pin)| file.path != pin.path || file.sha256 != pin.sha256)
        .map(|(file, _)| format!("{} holds other bytes than data.lock pins", file.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::manifest::Manifest;

    fn entry(select: &[&str], exclude: &[&str]) -> Source {
        let quoted = |patterns: &[&str]| {
            patterns
                .iter()
                .map(|pattern| format!("\"{pattern}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = format!(
            "[datasets.flowers]\nref = \"local\"\nselect = [{}]\nexclude = [{}]\n",
            quoted(select),
            quoted(exclude)
        );
        Manifest::parse(&text).unwrap().datasets["flowers"]
            .source()
            .unwrap()
            .clone()
    }

    fn paths(walked: &Walked) -> Vec<&str> {
        walked.files.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn a_walk_pins_plain_files_by_path_and_leaves_repository_metadata_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("petals/red")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join("sepals.csv"), b"a,b\n").unwrap();
        std::fs::write(root.join("petals/red/rose.csv"), b"rose\n").unwrap();
        std::fs::write(root.join("petals/notes.txt"), b"notes\n").unwrap();
        std::fs::write(root.join(".git/HEAD"), b"ref\n").unwrap();
        std::fs::write(root.join(".gitignore"), b"*.tmp\n").unwrap();
        let walked = walk(&root, "./flowers", &entry(&[], &[]), &Stop::default()).unwrap();
        assert_eq!(
            paths(&walked),
            [
                ".gitignore",
                "petals/notes.txt",
                "petals/red/rose.csv",
                "sepals.csv"
            ]
        );
        assert_eq!(walked.bytes(), 4 + 5 + 6 + 6);
        let csv = walk(
            &root,
            "./flowers",
            &entry(&["**/*.csv"], &["sepals.csv"]),
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&csv), ["petals/red/rose.csv"]);
        let nothing = walk(
            &root,
            "./flowers",
            &entry(&["*.parquet"], &[]),
            &Stop::default(),
        )
        .unwrap_err();
        assert_eq!(nothing.kind, Kind::Usage);
        assert_eq!(
            nothing.message,
            "--select \"*.parquet\" keeps no file of ./flowers"
        );
        let one = walk(
            &root.join("sepals.csv"),
            "./sepals.csv",
            &entry(&[], &[]),
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&one), ["sepals.csv"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_inside_an_imported_folder_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"keep out\n").unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("iris.csv"), b"iris\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("secret.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("more")).unwrap();
        // rustix makes no fifo on macos, and every unix has mkfifo
        let made = std::process::Command::new("mkfifo")
            .arg(root.join("queue"))
            .status()
            .unwrap();
        assert!(made.success());
        let walked = walk(&root, "./flowers", &entry(&[], &[]), &Stop::default()).unwrap();
        assert_eq!(paths(&walked), ["iris.csv"]);
        assert_eq!(walked.skipped, ["more", "queue", "secret.txt"]);
    }

    /// what the system writes into folders by itself is no data: finder's `.DS_Store`, the `._`
    /// files a mac leaves on other disks and windows' thumbnails are left out and named, and a
    /// folder of them is not read.
    #[test]
    fn files_the_system_makes_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("__MACOSX")).unwrap();
        for (path, bytes) in [
            ("iris.csv", &b"iris\n"[..]),
            ("sub/petals.csv", b"petals\n"),
            (".DS_Store", b"finder"),
            ("sub/.DS_Store", b"finder"),
            ("._iris.csv", b"appledouble"),
            ("Thumbs.db", b"thumbs"),
            ("__MACOSX/._iris.csv", b"appledouble"),
        ] {
            std::fs::write(root.join(path), bytes).unwrap();
        }
        let walked = walk(&root, "./flowers", &entry(&[], &[]), &Stop::default()).unwrap();
        assert_eq!(paths(&walked), ["iris.csv", "sub/petals.csv"]);
        assert_eq!(
            walked.system,
            [
                ".DS_Store",
                "._iris.csv",
                "Thumbs.db",
                "__MACOSX",
                "sub/.DS_Store"
            ]
        );
        assert_eq!(walked.skipped, Vec::<String>::new());
    }

    /// what an import must leave as it was: a file's bytes, length and modified time, and on
    /// unix its inode and mode, which a hardlink or a sealed copy would change.
    fn look(path: &Path) -> (Vec<u8>, u64, std::time::SystemTime, u64, u32) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        #[cfg(unix)]
        let (inode, mode) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino(), metadata.mode())
        };
        #[cfg(not(unix))]
        let (inode, mode) = (0, 0);
        (
            std::fs::read(path).unwrap(),
            metadata.len(),
            metadata.modified().unwrap(),
            inode,
            mode,
        )
    }

    #[test]
    fn an_import_never_changes_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("petals")).unwrap();
        let files = [root.join("iris.csv"), root.join("petals/red.csv")];
        std::fs::write(&files[0], b"iris\n").unwrap();
        std::fs::write(&files[1], b"red\n").unwrap();
        let before = files.each_ref().map(|file| look(file));
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = crate::cli::project::tests::offline_context();
        let walked = walk(&root, "./flowers", &entry(&[], &[]), &context.stop).unwrap();
        let copied = into_cache(&context, &cache, &walked).unwrap();
        let pins: Vec<(&str, u64, crate::digest::Sha256, &str)> = copied
            .files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.size,
                    file.sha256,
                    file.url.as_str(),
                )
            })
            .collect();
        assert_eq!(
            pins,
            [
                ("iris.csv", 5, crate::digest::sha256(b"iris\n"), ""),
                ("petals/red.csv", 4, crate::digest::sha256(b"red\n"), ""),
            ]
        );
        assert_eq!(files.each_ref().map(|file| look(file)), before);
        for (file, pin) in files.iter().zip(&copied.files) {
            let object = cache.object_path(&pin.sha256);
            assert!(
                !same_file::is_same_file(&object, file).unwrap(),
                "{}",
                pin.path
            );
        }
    }

    #[test]
    fn names_are_refused_as_an_archive_refuses_its_members() {
        let found = |paths: &[&str]| -> Vec<Found> {
            paths
                .iter()
                .map(|path| Found {
                    path: (*path).to_owned(),
                    source: PathBuf::new(),
                    size: 0,
                })
                .collect()
        };
        for (paths, message) in [
            (
                &["Iris.csv", "iris.csv"][..],
                "./flowers holds \"Iris.csv\" and \"iris.csv\", which are one name on disks that ignore case",
            ),
            (
                &["Data/a.csv", "data/b.csv"],
                "./flowers holds \"Data\" and \"data\", which are one name on disks that ignore case",
            ),
            (
                &["a\\b.csv"],
                "./flowers holds \"a\\\\b.csv\", which fetchloom refuses because it has a \\, which windows reads as a folder separator",
            ),
            (
                &["bell\u{7}.csv"],
                "./flowers holds \"bell\\u{7}.csv\", which fetchloom refuses because it holds a control character",
            ),
        ] {
            let error = checked_names(&found(paths), "./flowers").unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid, "{paths:?}");
            assert_eq!(error.message, message, "{paths:?}");
        }
        assert!(checked_names(&found(&["a/b.csv", "a/c.csv", "b.csv"]), "./flowers").is_ok());
        assert_eq!(composed("cafe\u{301}.csv"), "caf\u{e9}.csv");
    }

    #[test]
    fn a_supplied_folder_is_compared_by_path_and_size_before_its_bytes() {
        let pin = |path: &str, size: u64, byte: u8| LockedFile {
            path: path.to_owned(),
            size,
            sha256: crate::digest::sha256(&[byte]),
            md5: None,
            url: String::new(),
            unpacks_to: None,
        };
        let file = |path: &str, size: u64| Found {
            path: path.to_owned(),
            source: PathBuf::new(),
            size,
        };
        let pinned = [pin("a.csv", 1, 1), pin("b.csv", 2, 2)];
        assert_eq!(
            first_difference(&[file("a.csv", 1), file("b.csv", 2)], &pinned),
            None
        );
        for (found, difference) in [
            (vec![file("a.csv", 1)], "b.csv is missing"),
            (
                vec![file("a.csv", 1), file("b.csv", 2), file("c.csv", 3)],
                "c.csv is not in data.lock",
            ),
            (
                vec![file("a.csv", 1), file("b.csv", 3)],
                "b.csv has 3 bytes, and data.lock pins 2 bytes",
            ),
            (
                vec![file("a.csv", 1), file("ab.csv", 1), file("b.csv", 2)],
                "ab.csv is not in data.lock",
            ),
        ] {
            assert_eq!(
                first_difference(&found, &pinned).as_deref(),
                Some(difference)
            );
        }
        let copied = [pin("a.csv", 1, 1), pin("b.csv", 2, 9)];
        assert_eq!(
            first_other_bytes(&copied, &pinned).as_deref(),
            Some("b.csv holds other bytes than data.lock pins")
        );
        assert_eq!(first_other_bytes(&pinned, &pinned), None);
    }
}
