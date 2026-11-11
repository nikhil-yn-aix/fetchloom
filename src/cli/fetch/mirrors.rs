//! pinned files and archive trees from the mirrors a run names, before their sources. mirrors are
//! asked in order; one that is not there is skipped for the rest of the run after one warning,
//! and every file a mirror gives is checked against its pin before the cache keeps it.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use super::Job;
use crate::cache::Cache;
use crate::cli::Context;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Gave, Missing, Tree};
use crate::mirror::{Location, Mirrors, Wanted};
use crate::project::lock::LockedFile;
use crate::settings::{Origin, Settings};
use crate::stop::Stop;
use crate::units;
use crate::work::Work;

/// damaged copies one mirror's warnings name before they are counted instead.
const NAMED: usize = 5;

/// the mirrors a run asks, and what they answered, so no mirror is asked twice for one file and
/// one that is not there is asked nothing more.
#[derive(Default)]
pub(in crate::cli) struct Mirrored {
    pub(in crate::cli) mirrors: Mirrors,
    answers: Mutex<Answers>,
}

#[derive(Default)]
struct Answers {
    /// mirrors found there this run, by their place in the list.
    up: HashSet<usize>,
    /// mirrors this run skips, with why.
    down: HashMap<usize, String>,
    /// what a mirror said of a file or tree it did not give.
    lacked: HashMap<(usize, Sha256), String>,
}

impl Mirrored {
    pub(in crate::cli) fn new(mirrors: Mirrors) -> Mirrored {
        Mirrored {
            mirrors,
            answers: Mutex::default(),
        }
    }

    fn answers(&self) -> std::sync::MutexGuard<'_, Answers> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// whether mirror `index` already said it lacks `digest`.
    fn lacks(&self, index: usize, digest: &Sha256) -> bool {
        self.answers().lacked.contains_key(&(index, *digest))
    }

    fn lacked(&self, index: usize, digest: Sha256, said: String) {
        self.answers().lacked.insert((index, digest), said);
    }

    /// whether mirror `index` may be asked: a folder whose marker is there, checked once a run.
    /// one that is not there is told once and skipped from then on.
    fn usable(&self, context: &Context, index: usize, location: &Location) -> bool {
        {
            let answers = self.answers();
            if answers.up.contains(&index) {
                return true;
            }
            if answers.down.contains_key(&index) {
                return false;
            }
        }
        let skipped = match location {
            Location::Folder(root) => match folder::probe(root) {
                Ok(()) => None,
                Err(Missing::Folder) => Some((
                    format!("the mirror {location} is not there, so this run skips it"),
                    "mount it, or check the mirrors setting and --mirror".to_owned(),
                    "not there".to_owned(),
                )),
                Err(Missing::Marker) => Some((
                    format!(
                        "the mirror {location} holds no fetchloom-mirror file, so this run skips it"
                    ),
                    format!(
                        "check that {location} is a mirror's top folder, and that its disk is mounted"
                    ),
                    "no fetchloom-mirror file".to_owned(),
                )),
                Err(Missing::Unreadable(err)) => Some((
                    format!(
                        "could not read the mirror {location}: {}, so this run skips it",
                        crate::error::said(&err)
                    ),
                    "check that this account may read it".to_owned(),
                    crate::error::said(&err),
                )),
            },
            Location::Web(_) => Some((String::new(), String::new(), "not asked".to_owned())),
        };
        let mut answers = self.answers();
        match skipped {
            None => {
                answers.up.insert(index);
                true
            }
            Some((warning, help, said)) => {
                if !warning.is_empty() {
                    context.output.warn_with(&warning, &help);
                }
                answers.down.insert(index, said);
                false
            }
        }
    }
}

/// the mirrors this run asks: those `given` with `--mirror`, then those the settings list.
///
/// # Errors
///
/// `usage` for a `--mirror` that is no folder or web address, and for `mirrors_only` with no
/// mirror to ask.
pub(in crate::cli) fn read(given: &[String], settings: &Settings) -> Result<Mirrors, Error> {
    let mut flagged = Vec::with_capacity(given.len());
    for text in given {
        let location = Location::parse(text, None).map_err(|refusal| {
            Error::new(
                Kind::Usage,
                format!(
                    "--mirror {} is no mirror: {}",
                    crate::net::mask_query(text),
                    refusal.reason
                ),
            )
            .help(refusal.instead)
        })?;
        flagged.push(location);
    }
    let only = settings.mirrors_only;
    let mirrors = Mirrors::new(flagged, &settings.mirrors.value, only.value);
    if mirrors.only && mirrors.list.is_empty() {
        let variable = crate::settings::variable("mirrors_only");
        let (named, undo) = if only.origin == Origin::File {
            (
                "mirrors_only in the settings file".to_owned(),
                "remove mirrors_only from the settings file".to_owned(),
            )
        } else {
            (variable.to_owned(), format!("unset {variable}"))
        };
        return Err(Error::new(
            Kind::Usage,
            format!("{named} asks for mirrors alone, but no mirror is set"),
        )
        .help(format!(
            "list mirrors in the settings file or FETCHLOOM_MIRRORS, give --mirror, or {undo}"
        )));
    }
    Ok(mirrors)
}

/// the pinned files among `jobs` that the cache lacks, as a mirror is asked for them.
pub(in crate::cli) fn pinned(jobs: &[Job], cache: &Cache) -> Vec<Wanted> {
    jobs.iter()
        .filter_map(|job| {
            let digest = job.fetch.expect.sha256?;
            let size = job.fetch.expect.size?;
            cache.find(&digest, Some(size)).is_none().then(|| Wanted {
                digest,
                size,
                label: job.label.clone(),
            })
        })
        .collect()
}

/// copies into the cache what the mirrors hold of `wanted`, asking each in order for the files
/// the ones before it lacked. a damaged copy is told and left, so a later mirror or the source
/// gives the file.
///
/// # Errors
///
/// a failure to write the cache, and a stop.
pub(in crate::cli) async fn fill(
    context: &Context,
    cache: &Cache,
    wanted: Vec<Wanted>,
) -> Result<(), Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() || wanted.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let mut wanted: Vec<Wanted> = wanted
        .into_iter()
        .filter(|want| seen.insert(want.digest))
        .collect();
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        let ask: Vec<Wanted> = wanted
            .iter()
            .filter(|want| !mirrored.lacks(index, &want.digest))
            .cloned()
            .collect();
        if ask.is_empty() || !mirrored.usable(context, index, location) {
            continue;
        }
        let Location::Folder(root) = location else {
            continue;
        };
        let (asked, gave) = copy_from_folder(context, cache, root, ask).await?;
        let mut copied = (0, 0);
        let mut stored = HashSet::new();
        let mut damaged = Vec::new();
        for (want, gave) in asked.into_iter().zip(gave) {
            match gave {
                Gave::Stored(object) => {
                    if object.new {
                        context.work.wrote(&object.digest);
                    }
                    Work::add(&context.work.mirror_objects, 1);
                    Work::add(&context.work.mirror_bytes, object.size);
                    copied = (copied.0 + 1, copied.1 + object.size);
                    stored.insert(want.digest);
                }
                Gave::Absent => mirrored.lacked(index, want.digest, "not there".to_owned()),
                Gave::Damaged => {
                    mirrored.lacked(index, want.digest, "a damaged copy".to_owned());
                    damaged.push(want.label);
                }
                Gave::Unreadable(err) => {
                    let said = crate::error::said(&err);
                    mirrored.lacked(index, want.digest, format!("unreadable: {said}"));
                    damaged.push(format!("{} ({said})", want.label));
                }
            }
        }
        wanted.retain(|want| !stored.contains(&want.digest));
        tell_damaged(context, location, &damaged);
        if copied.0 > 0 {
            context.output.step(
                "Copied",
                &format!(
                    "{} \u{b7} {} from {location}",
                    units::count(copied.0, "file", "files"),
                    units::size(copied.1)
                ),
            );
        }
        if wanted.is_empty() {
            break;
        }
    }
    Ok(())
}

/// the archives among `unknown` whose trees no mirror gave. a tree a mirror gives that hashes to
/// the one data.lock pins becomes the record of what its archive unpacks to, so the files it
/// holds can come one by one without the archive.
///
/// # Errors
///
/// a failure to write the cache, and a stop.
pub(in crate::cli) async fn trees<'f>(
    context: &Context,
    cache: &Cache,
    unknown: Vec<(String, &'f LockedFile)>,
) -> Result<Vec<(String, &'f LockedFile)>, Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() {
        return Ok(unknown);
    }
    let mut rest = Vec::with_capacity(unknown.len());
    for (dataset, file) in unknown {
        let found = match file.unpacks_to {
            Some(tree) => tree_from_mirrors(context, cache, &dataset, file, tree).await?,
            None => false,
        };
        if !found {
            rest.push((dataset, file));
        }
    }
    Ok(rest)
}

/// asks each mirror in order for the tree `tree` that `file` of `dataset` unpacks to, and
/// records the first that matches. says whether one did.
async fn tree_from_mirrors(
    context: &Context,
    cache: &Cache,
    dataset: &str,
    file: &LockedFile,
    tree: Sha256,
) -> Result<bool, Error> {
    let mirrored = &context.mirrors;
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        if mirrored.lacks(index, &tree) || !mirrored.usable(context, index, location) {
            continue;
        }
        let Location::Folder(root) = location else {
            continue;
        };
        let read = {
            let (root, stop) = (root.clone(), context.stop.clone());
            blocking(move || folder::read_tree(&root, &tree, &stop)).await
        };
        let bytes = match read {
            Ok(Tree::Bytes(bytes)) => bytes,
            Ok(Tree::Absent) => {
                mirrored.lacked(index, tree, "not there".to_owned());
                continue;
            }
            Ok(Tree::Damaged) => {
                mirrored.lacked(index, tree, "a damaged copy".to_owned());
                tell_damaged(
                    context,
                    location,
                    &[format!("the list of files {dataset}/{} holds", file.path)],
                );
                continue;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                return Err(Stop::stopped());
            }
            Err(err) => {
                let said = crate::error::said(&err);
                mirrored.lacked(index, tree, format!("unreadable: {said}"));
                tell_damaged(
                    context,
                    location,
                    &[format!("the files {dataset}/{} holds ({said})", file.path)],
                );
                continue;
            }
        };
        let recorded = cache
            .record_tree_bytes(&file.sha256, &tree, &bytes)
            .map_err(|err| {
                Error::io(
                    format_args!("could not record what {dataset}/{} holds", file.path),
                    &err,
                )
            })?;
        if recorded {
            return Ok(true);
        }
        mirrored.lacked(index, tree, "a damaged copy".to_owned());
        tell_damaged(
            context,
            location,
            &[format!("the list of files {dataset}/{} holds", file.path)],
        );
    }
    Ok(false)
}

/// copies `ask` from the folder mirror at `root` on blocking threads, and returns it with what
/// the mirror gave for each.
async fn copy_from_folder(
    context: &Context,
    cache: &Cache,
    root: &std::path::Path,
    ask: Vec<Wanted>,
) -> Result<(Vec<Wanted>, Vec<Gave>), Error> {
    let (cache, root, threads, stop) = (
        cache.clone(),
        root.to_path_buf(),
        context.threads,
        context.stop.clone(),
    );
    let shown = root.display().to_string();
    blocking(move || {
        let gave = folder::copy_all(&cache, &root, &ask, threads, &stop)?;
        Ok((ask, gave))
    })
    .await
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::Interrupted {
            Stop::stopped()
        } else {
            Error::io(
                format_args!("could not copy from the mirror {shown} into the cache"),
                &err,
            )
        }
    })
}

/// runs `work` on a thread that may wait on the disk, so the network's workers never do.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|err| Err(std::io::Error::other(err)))
}

/// warns about the damaged copies `location` holds: the first few by name, the rest counted.
fn tell_damaged(context: &Context, location: &Location, labels: &[String]) {
    for label in labels.iter().take(NAMED) {
        context.output.warn_with(
            &format!("the mirror {location} holds a damaged copy of {label}, so it was not used"),
            "ask whoever keeps the mirror to replace it",
        );
    }
    if labels.len() > NAMED {
        context.output.warn(&format!(
            "the mirror {location} holds {} more damaged copies, so they were not used",
            labels.len() - NAMED
        ));
    }
}
