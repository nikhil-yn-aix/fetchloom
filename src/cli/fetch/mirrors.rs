//! pinned files and archive trees from the mirrors a run names, asked in order before sources. one
//! not there is skipped after one warning, and every file one gives is checked against its pin.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use super::Job;
use super::data_folder::DataFolder;
use crate::cache::Cache;
use crate::cli::Context;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Gave, Missing};
use crate::mirror::{Location, Mirrors, Wanted, web};
use crate::net::{Fetched, Net};
use crate::output::Warning;
use crate::project::lock::archives::Archive;
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
    /// mirrors this run skips, with what they answered.
    down: HashMap<usize, String>,
    /// what a mirror answered for a file or tree it did not give.
    lacked: HashMap<(usize, Sha256), String>,
}

/// why a mirror is skipped for the rest of the run: what to warn, the help, and its answer in
/// short. no warning for one a person chose to skip, as a web mirror while offline.
struct Skip {
    warning: Option<(String, String)>,
    said: String,
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

    /// what each mirror said of the file pinned to `digest`, in order, or that none can be asked
    /// for a new file, whose SHA-256 is not known yet.
    fn said(&self, digest: Option<&Sha256>) -> String {
        let Some(digest) = digest else {
            return "new, so no mirror can be asked for it".to_owned();
        };
        let answers = self.answers();
        self.mirrors
            .list
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let said = answers
                    .lacked
                    .get(&(index, *digest))
                    .or_else(|| answers.down.get(&index))
                    .map_or("not asked", String::as_str);
                format!("{said} on {location}")
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// skips mirror `index` for the rest of the run, warning once.
    fn skip(&self, context: &Context, index: usize, skip: Skip) {
        let mut answers = self.answers();
        if answers.down.contains_key(&index) {
            return;
        }
        if let Some((warning, help)) = &skip.warning {
            context
                .output
                .warn_with(Warning::MirrorSkipped, warning, help);
        }
        answers.down.insert(index, skip.said);
    }

    /// whether mirror `index` may be asked: its marker was found, once a run. one without it is
    /// told once and skipped, a web mirror is never asked offline, and a stop while asking fails.
    async fn usable(
        &self,
        context: &Context,
        net: &Net,
        index: usize,
        location: &Location,
    ) -> Result<bool, Error> {
        {
            let answers = self.answers();
            if answers.up.contains(&index) {
                return Ok(true);
            }
            if answers.down.contains_key(&index) {
                return Ok(false);
            }
        }
        let skip = match location {
            Location::Folder(root) => folder_missing(location, folder::probe(root).err()),
            Location::Web(_) if net.offline().enabled() => Some(Skip {
                warning: None,
                said: "not asked offline".to_owned(),
            }),
            Location::Web(base) => web_missing(location, web::probe(net, base).await?),
        };
        match skip {
            None => {
                self.answers().up.insert(index);
                Ok(true)
            }
            Some(skip) => {
                self.skip(context, index, skip);
                Ok(false)
            }
        }
    }
}

/// why the folder mirror at `location` is skipped, when its look found it `missing`.
fn folder_missing(location: &Location, missing: Option<Missing>) -> Option<Skip> {
    let (warning, help, said) = match missing? {
        Missing::Folder => (
            format!("the mirror {location} is not there, so this run skips it"),
            "mount it, or check the mirrors setting and --mirror".to_owned(),
            "not there".to_owned(),
        ),
        Missing::Marker => (
            format!("the mirror {location} holds no fetchloom-mirror file, so this run skips it"),
            format!("check that {location} is a mirror's top folder, and that its disk is mounted"),
            "no fetchloom-mirror file".to_owned(),
        ),
        Missing::Unreadable(err) => (
            format!(
                "could not read the mirror {location}: {}, so this run skips it",
                crate::error::said(&err)
            ),
            "check that this account may read it".to_owned(),
            crate::error::said(&err),
        ),
    };
    Some(Skip {
        warning: Some((warning, help)),
        said,
    })
}

/// why the web mirror at `location` is skipped, after its marker was asked for.
fn web_missing(location: &Location, look: web::Look) -> Option<Skip> {
    let (warning, help, said) = match look {
        web::Look::There => return None,
        web::Look::NoMarker => (
            format!("the mirror {location} holds no fetchloom-mirror file, so this run skips it"),
            "check that the address names a mirror's top folder".to_owned(),
            "no fetchloom-mirror file".to_owned(),
        ),
        web::Look::Answered(status) => (
            format!("the mirror {location} answered {status}, so this run skips it"),
            refused_help(location, status),
            format!("answered {status}"),
        ),
        web::Look::Failed(error) => (
            format!(
                "could not reach the mirror {location}: {}, so this run skips it",
                error.message
            ),
            "check the address, and that this machine can reach it".to_owned(),
            error.message,
        ),
    };
    Some(Skip {
        warning: Some((warning, help)),
        said,
    })
}

/// what to do about a web mirror that answered `status` instead of a file.
fn refused_help(location: &Location, status: u16) -> String {
    match (status, location) {
        (401 | 403, Location::Web(base)) => format!(
            "if it needs a login, sign in with `fetchloom auth login {}`",
            crate::net::host(base)
        ),
        _ => "ask whoever keeps the mirror, or check its address".to_owned(),
    }
}

/// the mirrors this run asks: those `given` with `--mirror`, then the settings' list. `usage` for a
/// `--mirror` that is no folder or web address, or `mirrors_only` with no mirror to ask.
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

/// the error for what only a source gives, `what`, while `mirrors_only` lets fetchloom ask its
/// mirrors alone.
pub(in crate::cli) fn refused(context: &Context, what: &str) -> Error {
    refusal(
        context,
        what,
        "ask whoever keeps the mirrors to add what is missing",
    )
}

/// [`refused`], with `remedy` as what to do short of asking sources too.
fn refusal(context: &Context, what: &str, remedy: &str) -> Error {
    let off = if context.settings.mirrors_only.origin == Origin::File {
        "set mirrors_only = false in the settings file"
    } else {
        "unset FETCHLOOM_MIRRORS_ONLY"
    };
    Error::new(
        Kind::NetworkRefused,
        format!("{what}, and mirrors_only lets fetchloom ask its mirrors alone"),
    )
    .help(format!("{remedy}, or {off} to ask sources too"))
}

/// refuses, in mirror-only mode, to download `files` from their sources, with `network.refused`
/// naming each with what each mirror said of it. a file without a SHA-256 is new, so no mirror can
/// be asked for it.
pub(in crate::cli) fn sources_allowed(
    context: &Context,
    files: &[(String, Option<Sha256>)],
) -> Result<(), Error> {
    if !context.mirrors.mirrors.only || files.is_empty() {
        return Ok(());
    }
    let new = files.iter().filter(|(_, digest)| digest.is_none()).count() as u64;
    let pinned = files.len() as u64 - new;
    let named = format!(
        "{} data.lock names {} on no mirror",
        units::count(pinned, "file", "files"),
        if pinned == 1 { "is" } else { "are" }
    );
    let unknown = format!(
        "{} {} no SHA-256 yet",
        units::count(new, "new file", "new files"),
        if new == 1 { "has" } else { "have" }
    );
    let error = if new == 0 {
        refused(context, &named)
    } else if pinned == 0 {
        let (it, its) = if new == 1 {
            ("it", "its source")
        } else {
            ("them", "their sources")
        };
        refusal(
            context,
            &format!("{unknown}, so only {its} can give {it}"),
            &format!("pin {it} on a machine that may ask {its}, then push {it} to a mirror"),
        )
    } else {
        refusal(
            context,
            &format!("{named} and {unknown}"),
            "ask whoever keeps the mirrors to add what is missing, and pin new files on a machine that may ask their sources",
        )
    };
    Err(lacked(context, files)
        .into_iter()
        .fold(error, Error::detail))
}

/// a line for each of the first `files` a run lacks, by its name and with what each mirror
/// said of it, when it has mirrors, and how many more there are.
pub(in crate::cli) fn lacked(context: &Context, files: &[(String, Option<Sha256>)]) -> Vec<String> {
    let mut lines: Vec<String> = files
        .iter()
        .take(super::NAMED)
        .map(|(label, digest)| {
            if context.mirrors.mirrors.list.is_empty() {
                label.clone()
            } else {
                format!("{label}: {}", context.mirrors.said(digest.as_ref()))
            }
        })
        .collect();
    if files.len() > super::NAMED {
        lines.push(format!(
            "and {} more",
            units::grouped((files.len() - super::NAMED) as u64)
        ));
    }
    lines
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

/// puts into the cache what the data folder and then each mirror in order hold of the files
/// `wanted` lists; a damaged copy on a mirror is told and left for a later one or the source.
/// `wanted` runs only when one may hold something, since listing what the cache lacks reads every
/// file. fails at a stop, a cache write, or `unconfirmed` past what a person agreed to download.
pub(in crate::cli) async fn fill(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    folder: &DataFolder,
    wanted: impl FnOnce() -> Vec<Wanted>,
) -> Result<(), Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() && folder.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let wanted: Vec<Wanted> = wanted()
        .into_iter()
        .filter(|want| seen.insert(want.digest))
        .collect();
    let mut wanted = tokio::task::block_in_place(|| folder.take(context, cache, wanted))?;
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        let ask: Vec<Wanted> = wanted
            .iter()
            .filter(|want| !mirrored.lacks(index, &want.digest))
            .cloned()
            .collect();
        if ask.is_empty() || !mirrored.usable(context, net, index, location).await? {
            continue;
        }
        let stored = match location {
            Location::Folder(root) => {
                from_folder(context, cache, index, location, root, ask).await?
            }
            Location::Web(base) => {
                from_web(context, net, cache, index, location, base, ask).await?
            }
        };
        wanted.retain(|want| !stored.contains(&want.digest));
        if wanted.is_empty() {
            break;
        }
    }
    Ok(())
}

/// copies `ask` from the folder mirror `index` at `root`, and says which files it stored.
async fn from_folder(
    context: &Context,
    cache: &Cache,
    index: usize,
    location: &Location,
    root: &std::path::Path,
    ask: Vec<Wanted>,
) -> Result<HashSet<Sha256>, Error> {
    let mirrored = &context.mirrors;
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
    Ok(stored)
}

/// downloads `ask` from the web mirror `index` at `base` as any download, question included, and
/// says which files it stored. a mirror that refuses or fails is skipped for the rest of the run.
async fn from_web(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    index: usize,
    location: &Location,
    base: &url::Url,
    ask: Vec<Wanted>,
) -> Result<HashSet<Sha256>, Error> {
    let mirrored = &context.mirrors;
    let total = ask
        .iter()
        .map(|want| want.size)
        .fold(0, u64::saturating_add);
    crate::cli::confirm(
        &context.output,
        context.yes,
        context.settings.ask_above.value,
        &units::count(ask.len() as u64, "file", "files"),
        total,
        Some((cache.root(), total)),
    )?;
    let jobs = ask
        .iter()
        .map(|want| Job {
            fetch: web::object(base, want),
            label: want.label.clone(),
            dataset: None,
            again: None,
        })
        .collect();
    let started = Instant::now();
    let results = super::apart(context, net, cache, jobs).await?;
    let elapsed = Fetched::transfer(results.iter().flatten(), started);
    let mut stored = HashSet::new();
    let mut fetched = Vec::new();
    let mut damaged = Vec::new();
    let mut trouble = None;
    for (want, result) in ask.into_iter().zip(results) {
        let error = match result {
            Ok(found) => {
                Work::add(&context.work.mirror_objects, 1);
                Work::add(&context.work.mirror_bytes, found.size);
                fetched.push((found.size, found.moved));
                stored.insert(want.digest);
                continue;
            }
            Err(error) => error,
        };
        match error.kind {
            Kind::ReferenceUnresolved => {
                mirrored.lacked(index, want.digest, "not there".to_owned());
            }
            Kind::IntegrityMismatch => {
                mirrored.lacked(index, want.digest, "a damaged copy".to_owned());
                damaged.push(want.label);
            }
            Kind::LocalIo => return Err(error),
            _ => {
                mirrored.lacked(index, want.digest, error.message.clone());
                trouble.get_or_insert(error);
            }
        }
    }
    tell_damaged(context, location, &damaged);
    if !fetched.is_empty() {
        context.output.step(
            "Downloaded",
            &format!(
                "{} \u{b7} {} from {location}",
                units::count(fetched.len() as u64, "file", "files"),
                units::downloaded(fetched, elapsed)
            ),
        );
    }
    if let Some(error) = trouble {
        mirrored.skip(
            context,
            index,
            Skip {
                warning: Some((
                    format!(
                        "the mirror {location} failed: {}, so this run skips it",
                        error.message
                    ),
                    "check the address, and that this machine can reach it".to_owned(),
                )),
                said: error.message,
            },
        );
    }
    Ok(stored)
}

/// the archives among `unknown` whose trees no mirror gave. a tree a mirror gives that hashes to
/// data.lock's pin becomes the record of what its archive unpacks to, so its files can come one by
/// one without the archive. fails at a stop or a cache write.
pub(in crate::cli) async fn trees<'f>(
    context: &Context,
    net: &Net,
    cache: &Cache,
    unknown: Vec<(String, Archive<'f>)>,
) -> Result<Vec<(String, Archive<'f>)>, Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() {
        return Ok(unknown);
    }
    let mut rest = Vec::with_capacity(unknown.len());
    for (dataset, archive) in unknown {
        let found = match archive.file.unpacks_to {
            Some(tree) => tree_from_mirrors(context, net, cache, &dataset, &archive, tree).await?,
            None => false,
        };
        if !found {
            rest.push((dataset, archive));
        }
    }
    Ok(rest)
}

/// what a mirror gave when asked for a tree.
enum Got {
    Bytes(Vec<u8>),
    Absent,
    Damaged,
    /// the mirror failed, with its answer in short.
    Trouble(String),
}

/// asks each mirror in order for the tree `tree` of `archive` in `dataset`, recording the first
/// that matches under the archive's key, a split archive's from its parts. says whether one did.
async fn tree_from_mirrors(
    context: &Context,
    net: &Net,
    cache: &Cache,
    dataset: &str,
    archive: &Archive<'_>,
    tree: Sha256,
) -> Result<bool, Error> {
    let file = archive.file;
    let mirrored = &context.mirrors;
    let label = format!("the list of files {dataset}/{} holds", file.path);
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        if mirrored.lacks(index, &tree) || !mirrored.usable(context, net, index, location).await? {
            continue;
        }
        let got = match location {
            Location::Folder(root) => {
                let (root, stop) = (root.clone(), context.stop.clone());
                match blocking(move || folder::read_tree(&root, &tree, &stop)).await {
                    Ok(folder::Tree::Bytes(bytes)) => Got::Bytes(bytes),
                    Ok(folder::Tree::Absent) => Got::Absent,
                    Ok(folder::Tree::Damaged) => Got::Damaged,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(Stop::stopped());
                    }
                    Err(err) => Got::Trouble(crate::error::said(&err)),
                }
            }
            Location::Web(base) => match web::read_tree(net, base, &tree).await? {
                web::Tree::Bytes(bytes) => Got::Bytes(bytes),
                web::Tree::Absent => Got::Absent,
                web::Tree::Answered(status) => Got::Trouble(format!("answered {status}")),
                web::Tree::Failed(error) => Got::Trouble(error.message),
            },
        };
        let bytes = match got {
            Got::Bytes(bytes) => bytes,
            Got::Absent => {
                mirrored.lacked(index, tree, "not there".to_owned());
                continue;
            }
            Got::Damaged => Vec::new(),
            Got::Trouble(said) => {
                mirrored.lacked(index, tree, said.clone());
                mirrored.skip(
                    context,
                    index,
                    Skip {
                        warning: Some((
                            format!("the mirror {location} failed: {said}, so this run skips it"),
                            "check the mirror, and that this machine can reach it".to_owned(),
                        )),
                        said,
                    },
                );
                continue;
            }
        };
        let recorded = !bytes.is_empty()
            && cache
                .record_tree_bytes(&archive.key(), &tree, &bytes)
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
        tell_damaged(context, location, std::slice::from_ref(&label));
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
            Warning::MirrorDamaged,
            &format!("the mirror {location} holds a damaged copy of {label}, so it was not used"),
            "ask whoever keeps the mirror to replace it",
        );
    }
    if labels.len() > NAMED {
        context.output.warn(
            Warning::MirrorDamaged,
            &format!(
                "the mirror {location} holds {} more damaged copies, so they were not used",
                units::grouped((labels.len() - NAMED) as u64)
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    /// listing what the cache lacks looks at every file, so a run with nowhere to ask never does.
    #[test]
    fn a_run_without_a_mirror_lists_nothing_to_ask_for() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let net = Arc::new(context.net());
        let listed = AtomicBool::new(false);
        block_on(fill(&context, &net, &cache, &DataFolder::default(), || {
            listed.store(true, Ordering::Relaxed);
            Vec::new()
        }))
        .unwrap();
        assert!(!listed.load(Ordering::Relaxed));
    }

    /// refused together, each counted for what it is.
    #[test]
    fn mirror_only_mode_tells_pinned_files_from_new_ones() {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        let lab = Location::Folder(std::path::PathBuf::from("/mnt/lab"));
        context.mirrors = Mirrored::new(Mirrors::new(Vec::new(), &[lab], true));
        let pinned = (
            "iris/iris.csv".to_owned(),
            Some(crate::digest::sha256(b"iris")),
        );
        let new = ("iris2/iris.csv".to_owned(), None);
        let error = sources_allowed(&context, &[pinned, new]).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused);
        assert_eq!(
            error.message,
            "1 file data.lock names is on no mirror and 1 new file has no SHA-256 yet, and mirrors_only lets fetchloom ask its mirrors alone"
        );
        assert_eq!(
            error.details,
            [
                "iris/iris.csv: not asked on /mnt/lab",
                "iris2/iris.csv: new, so no mirror can be asked for it"
            ]
        );
    }
}
