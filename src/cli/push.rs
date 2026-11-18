//! `fetchloom push`: the files data.lock pins, copied from this machine's cache into a folder
//! mirror or an S3 bucket's, with the lists of what their archives unpack to. it writes only the
//! mirror, and the cache for a file it lacks, taken in as a sync would; it never changes the
//! project.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use serde::Serialize;

use super::Context;
use super::args;
use super::fetch::data_folder::DataFolder;
use super::project::{find, known, locked};
use super::sync::{Loop, Scope, place};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Failed, Fault, Item, Put, Refused, State, Target, There};
use crate::mirror::s3::{self, Pusher};
use crate::mirror::{Location, OBJECTS, TREES};
use crate::output::{Output, Warning};
use crate::project::Project;
use crate::project::lock::archives::Archive;
use crate::project::lock::{self, Lock, LockedFile, Stated};
use crate::project::manifest::Manifest;
use crate::source::Access;
use crate::stop::Stop;
use crate::volume::read_unfollowed;
use crate::{plan, units};

/// what a push did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    /// the mirror's folder, or its bucket and prefix as `s3://bucket/prefix`.
    target: String,
    /// whether the push started a mirror there.
    started: bool,
    /// the datasets named, or none when the push named none.
    selected: Option<Vec<String>>,
    /// every dataset pushed.
    datasets: Vec<String>,
    /// the files those datasets need on the mirror, each once, and their bytes.
    objects: u64,
    bytes: u64,
    /// the files the push set out to write, lacked or damaged on the mirror, and their bytes.
    planned: u64,
    planned_bytes: u64,
    /// the files written, and their bytes.
    copied: u64,
    copied_bytes: u64,
    /// the files found there after one look each, and left as they were.
    present: u64,
    present_bytes: u64,
    /// the files of `copied` written in place of a damaged copy.
    replaced: u64,
    /// the lists of what archives unpack to that were written.
    trees: u64,
    /// the datasets a push asks about before it copies their files.
    asks: Vec<Ask>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, what would be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Changes>,
}

/// a dataset a push asks about first, with what its source said of who may read it.
#[derive(Serialize)]
struct Ask {
    dataset: String,
    access: &'static str,
}

#[derive(Serialize)]
struct Changes {
    copy: Vec<Copied>,
    trees: Vec<Copied>,
    /// pinned files a push would download first, held by neither the cache nor the data folder.
    download: Vec<Fetch>,
    /// the archives among `download`, as `<dataset>/<path>`, which are unpacked once they are
    /// in. the files of one this machine never unpacked are not counted.
    unpack: Vec<String>,
}

/// a pinned file a push downloads first when the cache lacks what it brings: the needed file
/// itself, or the archive it is inside.
#[derive(Clone, Serialize)]
struct Fetch {
    /// `<dataset>/<path>`, as data.lock names it.
    path: String,
    size: u64,
    /// whether it is an archive, which is unpacked once it is in.
    #[serde(skip)]
    archive: bool,
}

/// a file a push would write: the first file that needs it, or the archive whose list of files
/// it is, as `<dataset>/<path>`.
#[derive(Serialize)]
struct Copied {
    path: String,
    sha256: Sha256,
    size: u64,
}

/// a file a push needs on the mirror, and where it comes from on this machine.
struct Needed {
    item: Item,
    /// `<dataset>/<path>` of the first file that needs it, or of the archive whose list it is.
    label: String,
    /// the dataset that first needs it.
    dataset: String,
    source: Source,
    /// for a file a source pins, the file whose download brings it.
    fetch: Option<Fetch>,
}

/// files a push writes, each with what the mirror held at its name.
type Writes<'a> = Vec<(&'a Needed, There)>;

/// where the bytes of a [`Needed`] are on this machine.
#[derive(Clone, Copy)]
enum Source {
    /// a file a source pins, which a push downloads when the cache and the data folder lack it.
    Pinned,
    /// the cache's record of what an archive unpacks to, by the record's key: the archive's
    /// SHA-256, or a split archive's parts key.
    List(Sha256),
}

/// a dataset a push asks about first, and why: what its source said of who may read it.
struct Asking {
    dataset: String,
    access: Access,
    /// data.lock calls it public, but this machine keeps a key for where its files come from, or
    /// one went with their download, as the cache marks it.
    keyed: bool,
    /// it is an output of the project, which no source says anything of.
    output: bool,
}

/// where this machine keeps a key: hosts with a saved login or token, a `.netrc` line or a site
/// sign-in, and the buckets its settings list for signing. a file from one of them may have needed
/// the key, so a push asks about it whatever data.lock, which a clone brings, says.
#[derive(Default)]
pub(in crate::cli) struct Keys {
    hosts: HashSet<String>,
    s3: Vec<crate::net::s3::Bucket>,
    gcs: bool,
    azure: bool,
    drive: bool,
}

impl Keys {
    pub(in crate::cli) fn read(settings: &crate::settings::Settings) -> Keys {
        let var = &crate::machine::env;
        let host = |named: &str| {
            let named = named.to_ascii_lowercase();
            named
                .rsplit_once(':')
                .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
                .map_or_else(|| named.clone(), |(host, _)| host.to_owned())
        };
        let mut hosts: HashSet<String> = crate::auth::credentials(var)
            .iter()
            .map(|key| host(&key.host))
            .collect();
        hosts.extend(
            crate::auth::site::sites(var)
                .iter()
                .map(|(site, _)| host(site)),
        );
        Keys {
            hosts,
            s3: settings.s3_buckets.value.clone(),
            gcs: !settings.gcs_buckets.value.is_empty(),
            azure: !settings.azure_containers.value.is_empty(),
            drive: !settings.drive_folders.value.is_empty(),
        }
    }

    /// whether this machine signs in where the file at `link` comes from, so its download may
    /// have needed a key.
    pub(in crate::cli) fn signs_in(&self, link: &str) -> bool {
        let Ok(url) = url::Url::parse(link) else {
            return false;
        };
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        self.hosts.contains(&host)
            || crate::net::s3::Bucket::holding(&url).is_some_and(|bucket| self.s3.contains(&bucket))
            || (self.gcs && host.ends_with("storage.googleapis.com"))
            || (self.azure && host.ends_with(".blob.core.windows.net"))
            || (self.drive && (host.ends_with("google.com") || host == "www.googleapis.com"))
    }

    /// what `locked` asks before a push, and whether a key of this machine made it ask: data.lock's
    /// access, or an account's for a dataset it calls public whose files may have needed a key,
    /// by their hosts, or came with one, as `marked` says the cache recorded for one of them.
    fn access(&self, locked: &crate::project::lock::Locked, marked: bool) -> (Access, bool) {
        let keyed = locked.access == Access::Public
            && (marked || locked.files.iter().any(|file| self.signs_in(&file.url)));
        if keyed {
            (Access::Login, true)
        } else {
            (locked.access, false)
        }
    }
}

/// where a push writes: a folder, or a prefix in an S3 bucket.
#[derive(Clone)]
enum To {
    /// a folder, and whether a copy of the right size there is read back and hashed before the
    /// push takes it as the pinned file, as `--verify` asks.
    Folder {
        root: PathBuf,
        verify: bool,
    },
    Bucket(Arc<Pusher>),
}

impl std::fmt::Display for To {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            To::Folder { root, .. } => write!(f, "{}", root.display()),
            To::Bucket(pusher) => write!(f, "{}", pusher.top()),
        }
    }
}

/// what a push found before writing anything.
struct Planned {
    target: To,
    state: State,
    selected: Option<Vec<String>>,
    datasets: Vec<String>,
    needed: Vec<Needed>,
    /// what the mirror holds at the name of each of `needed`.
    there: Vec<There>,
    asks: Vec<Asking>,
    /// where in `needed` the files are that the mirror and the cache lack.
    lacked: Vec<usize>,
    /// each archive whose files this machine does not know, with its dataset.
    unknown: Vec<(String, Fetch)>,
    /// where in `needed` the files are that the cache lacks and the data folder looks to hold,
    /// which the push takes into the cache before it writes.
    taking: Vec<usize>,
    /// the data folder `taking` comes from.
    folder: DataFolder,
    /// the archives sent whole, with their datasets, since their lists of files are longer than a
    /// mirror's reader takes.
    whole: Vec<(String, Vec<LockedFile>)>,
}

impl Planned {
    /// what a push writes of `needed`, all the mirror does not hold as it is, with what it holds
    /// at each one's name: the files, then the lists of what archives unpack to, in order.
    fn writes(&self) -> (Writes<'_>, Writes<'_>) {
        self.needed
            .iter()
            .zip(&self.there)
            .filter(|(_, there)| **there != There::Present)
            .map(|(needed, there)| (needed, *there))
            .partition(|(needed, _)| !matches!(needed.source, Source::List(_)))
    }
}

pub(super) fn run(context: &Context, args: &args::Push) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let target = target(context, args)?;
    let project = find(context)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        known(&manifest, name, "push")?;
    }
    let lock = project.lock()?;
    let datasets = pushed(context, &manifest, &args.names, lock.as_ref())?;
    if datasets.is_empty() {
        context.output.done("Ok", &nothing(&manifest));
        let target = match target {
            Location::Folder(root) => reached(root).display().to_string(),
            other => other.to_string(),
        };
        return Ok(Report::empty(target, dry_run));
    }
    let target = opened(context, target, args.verify)?;
    let scope = Scope::Only(datasets.iter().cloned().collect());
    locked(
        &project,
        &manifest,
        lock.as_ref(),
        "name only datasets data.lock pins",
        &scope,
    )?;
    let lock = lock.unwrap_or_default();
    let cache = if dry_run {
        super::peek_cache()?
    } else {
        super::open_cache()?
    };
    tell_changed_outputs(context, &project, (&manifest, &lock), &cache, &datasets);
    let selected = (!args.names.is_empty()).then(|| selected(&manifest, &args.names));
    let mut planned = plan(
        context, &manifest, &lock, &cache, target, selected, datasets,
    )?;
    settle(
        context,
        &project,
        &manifest,
        &lock,
        &cache,
        &mut planned,
        dry_run,
    )?;
    if dry_run {
        let report = report(&planned, Outcome::DryRun);
        tell_dry(context, &planned, &report);
        return Ok(report);
    }
    // the question comes before any download, so a no costs no request and adds nothing to the
    // cache
    ask(context, &planned)?;
    if !planned.lacked.is_empty() || !planned.unknown.is_empty() {
        let asked: Vec<String> = planned.asks.iter().map(|ask| ask.dataset.clone()).collect();
        fetch(
            context,
            &project,
            &manifest,
            &lock,
            &cache,
            fetching(&planned),
        )
        .map_err(|error| error.with_report(&report(&planned, Outcome::Stopped)))?;
        // what the archives hold is known now, so the push plans again
        planned = plan(
            context,
            &manifest,
            &lock,
            &cache,
            planned.target.clone(),
            planned.selected.clone(),
            planned.datasets.clone(),
        )?;
        settle(
            context,
            &project,
            &manifest,
            &lock,
            &cache,
            &mut planned,
            false,
        )?;
        ask_again(context, &mut planned, &asked)?;
        if !planned.lacked.is_empty() || !planned.unknown.is_empty() {
            let lacked: Vec<&Needed> = planned
                .lacked
                .iter()
                .map(|at| &planned.needed[*at])
                .collect();
            let error = missing(&lacked, &planned.unknown);
            return Err(error.with_report(&report(&planned, Outcome::Stopped)));
        }
    }
    take(context, &project, &manifest, &lock, &cache, &planned)?;
    let (started, puts) = write(context, &cache, &planned)?;
    let report = report(&planned, Outcome::Written(started, &puts));
    tell(context, &planned, &report);
    Ok(report)
}

/// warns of each output of `datasets` whose folder changed since it was pinned, as sync finds it:
/// push sends the files data.lock pins, never what the folder holds now. a folder that cannot be
/// read is told by the command that places it, so it warns of nothing.
fn tell_changed_outputs(
    context: &Context,
    project: &Project,
    (manifest, lock): (&Manifest, &Lock),
    cache: &Cache,
    datasets: &[String],
) {
    let outputs: Vec<&String> = datasets
        .iter()
        .filter(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(crate::project::manifest::Entry::is_output)
        })
        .collect();
    let folders: Vec<String> = outputs
        .iter()
        .filter_map(|name| manifest.datasets[*name].at.clone())
        .collect();
    if folders.is_empty() {
        return;
    }
    let within = super::sync::disk::Folders::of(manifest);
    let survey = super::sync::disk::Survey::read_within(
        project,
        &within,
        &folders,
        context.threads,
        &context.stop,
    );
    let scope = Scope::Only(outputs.into_iter().cloned().collect());
    let placing = survey.and_then(|survey| {
        place::Placing::new(project, manifest, lock, cache, false, &scope, survey)
    });
    let Ok(placing) = placing else {
        return;
    };
    for changed in placing.changed_outputs() {
        let name = &changed.name;
        context.output.warn_with(
            Warning::OutputChanged,
            &format!(
                "{} changed since {name} was pinned: {}, so push sends the files {name} pins, not these",
                changed.at,
                changed.summary()
            ),
            &format!(
                "run `{}` first to pin what it holds now",
                super::project::command(&crate::source::local::output_pin(name, &changed.shown))
            ),
        );
    }
}

/// the folder or S3 bucket to push to, as `--to` names it, else the `push_to` setting; `usage`
/// when neither names one, naming the mirrors of the `mirrors` setting push could copy into,
/// which it never picks itself, or when `--to` names a web address.
fn target(context: &Context, args: &args::Push) -> Result<Location, Error> {
    let Some(to) = &args.to else {
        let push_to = &context.settings.push_to;
        return push_to.value.clone().ok_or_else(|| {
            let error = Error::new(
                Kind::Usage,
                "push needs a folder or an S3 bucket to copy the files into",
            );
            let mirrors: Vec<String> = context
                .settings
                .mirrors
                .value
                .iter()
                .filter(|mirror| !matches!(mirror, Location::Web(_)))
                .map(ToString::to_string)
                .collect();
            let error = match mirrors.as_slice() {
                [] => error,
                [one] => error.detail(format!(
                    "the mirrors setting names the mirror {one}; push copies into it only once you name it"
                )),
                many => error.detail(format!(
                    "the mirrors setting names the mirrors {}; push copies into one only once you name it",
                    units::listed(many, "and")
                )),
            };
            // only an empty variable clears push_to with nothing in its place
            if push_to.origin == crate::settings::Origin::Environment {
                error.help("FETCHLOOM_PUSH_TO is set empty, which clears push_to: give --to <folder> or --to s3://bucket/prefix, or unset FETCHLOOM_PUSH_TO")
            } else {
                error.help(
                    "give --to <folder> or --to s3://bucket/prefix, or save it for every push with `fetchloom settings set push_to <folder>`",
                )
            }
        });
    };
    // a web address or a bucket is text, so a name that is not is a folder
    let Some(text) = to.to_str() else {
        return Ok(Location::Folder(to.clone()));
    };
    let shown = crate::shown::mask_typed(text);
    let scheme = text
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    match Location::parse(text, None) {
        Ok(Location::Web(_)) => Err(web_refused(text)),
        Ok(location) => Ok(location),
        // a web address with a login or a query is one all the same
        Err(_) if matches!(scheme.as_deref(), Some("http" | "https")) => Err(web_refused(text)),
        Err(refusal) => {
            // a bucket's or a file:// address's refusal says what to write instead, while one of
            // another scheme offers a web address too, which push writes to none of
            let bucket = text
                .get(..3)
                .is_some_and(|start| start.eq_ignore_ascii_case("s3:"));
            let instead = if bucket || scheme.as_deref() == Some("file") {
                refusal.instead
            } else {
                "give --to a folder, or a bucket such as s3://lab-data/fetchloom".to_owned()
            };
            Err(Error::new(
                Kind::Usage,
                format!("--to {shown} is no folder or S3 bucket: {}", refusal.reason),
            )
            .help(instead))
        }
    }
}

/// where `target` is written: a folder as the system reaches it, its copies read back first when
/// `verify` asks, or a bucket with the keys the AWS settings give for it.
fn opened(context: &Context, target: Location, verify: bool) -> Result<To, Error> {
    match target {
        Location::S3(top) if verify => Err(Error::new(
            Kind::Usage,
            format!("--verify reads back the copies on a folder mirror, and {top} is a bucket"),
        )
        .help("push to the bucket without --verify: S3 checked each file against its SHA-256 when push sent it")),
        Location::S3(top) => Ok(To::Bucket(Arc::new(Pusher::new(
            Arc::new(context.net()),
            top.at_endpoint(&crate::machine::env)?,
            Arc::clone(&context.stop),
        )?))),
        Location::Folder(root) => Ok(To::Folder {
            root: reached(root),
            verify,
        }),
        Location::Web(base) => Err(web_refused(base.as_str())),
    }
}

/// the error for `--to`, `text`, naming a web address.
fn web_refused(text: &str) -> Error {
    let shown = crate::shown::mask_typed(text);
    Error::new(
        Kind::Usage,
        format!("--to {shown} is a web address, and push writes to folders and S3 buckets"),
    )
    .help("give --to a folder, or a bucket such as s3://lab-data/fetchloom, and serve what it holds from the web server")
}

/// runs `work`, a push's requests to a bucket or downloads, to its end on the command's runtime.
fn waiting<T>(work: impl std::future::Future<Output = Result<T, Error>>) -> Result<T, Error> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("cannot send the push's requests: {err}"),
        )
    })?;
    runtime.block_on(work)
}

/// `target` as an absolute path without `.` or `..`, the folder the system reaches: on unix a link
/// before a `..` reaches another folder than the path without both, so the part up to the last `..`
/// is resolved as the system resolves it.
fn reached(target: PathBuf) -> PathBuf {
    let absolute = std::path::absolute(&target).unwrap_or(target);
    let parts: Vec<Component<'_>> = absolute.components().collect();
    let Some(last) = parts.iter().rposition(|part| *part == Component::ParentDir) else {
        return absolute;
    };
    match std::fs::canonicalize(parts[..=last].iter().collect::<PathBuf>()) {
        Ok(real) => parts[last + 1..]
            .iter()
            .fold(real, |path, part| path.join(part)),
        Err(_) => absolute,
    }
}

/// the datasets a push of `names` pushes, every one when it names none: of every one, those
/// waiting for their lists in `lock` and those not downloaded yet are left out with a warning, as
/// a sync places the rest, while one named stops the push, as for every command that needs its
/// files.
fn pushed(
    context: &Context,
    manifest: &Manifest,
    names: &[String],
    lock: Option<&Lock>,
) -> Result<Vec<String>, Error> {
    let mut datasets = selected(manifest, names);
    if let Some(lock) = lock {
        if names.is_empty() {
            let waiting: Vec<_> = lock.still_waiting().cloned().collect();
            super::project::tell_waiting(&context.output, &waiting, &Scope::All);
            let unfetched: Vec<&Stated> = lock
                .stated
                .iter()
                .filter(|stated| datasets.iter().any(|name| name == stated.name()))
                .collect();
            tell_unfetched(&context.output, &unfetched);
            datasets.retain(|name| lock.waits(name).is_none() && lock.stated(name).is_none());
        }
        lock.refuse_waiting(datasets.iter().map(String::as_str))?;
        lock.refuse_stated(datasets.iter().map(String::as_str))?;
    }
    Ok(datasets)
}

/// warns that a push of every dataset leaves out `unfetched`, which only what their sources state
/// pins until a sync downloads them, naming that sync.
fn tell_unfetched(output: &Output, unfetched: &[&Stated]) {
    let Some(error) = lock::stated_error(unfetched) else {
        return;
    };
    let names: Vec<&str> = unfetched.iter().map(|stated| stated.name()).collect();
    let them = if names.len() == 1 { "it" } else { "them" };
    output.warn_detailed(
        Warning::PushLeftOut,
        &format!("{}, so the push leaves {them} out", error.message),
        Some(&format!(
            "run `{}` where the data can download, then push again",
            super::project::command(&format!("sync {}", names.join(" ")))
        )),
        &names,
    );
}

/// the datasets a push sends: `names`, else every dataset in data.toml.
fn selected(manifest: &Manifest, names: &[String]) -> Vec<String> {
    let chosen: BTreeSet<&str> = if names.is_empty() {
        manifest.datasets.keys().map(String::as_str).collect()
    } else {
        names.iter().map(String::as_str).collect()
    };
    chosen.into_iter().map(str::to_owned).collect()
}

/// what pushing `datasets` into `target` needs, what the mirror holds of it already, one look a
/// file, and what neither the mirror nor the cache holds; fails for a folder that cannot take a
/// push, or a mirror that cannot be looked in or refuses this machine's keys.
fn plan(
    context: &Context,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    target: To,
    selected: Option<Vec<String>>,
    datasets: Vec<String>,
) -> Result<Planned, Error> {
    let Wanted {
        needed,
        unknown,
        whole,
    } = needed(manifest, lock, cache, &datasets)?;

    let items: Vec<Item> = needed.iter().map(|needed| needed.item).collect();
    let (state, there) = match &target {
        To::Folder { root, verify } => {
            let state = folder::look(root).map_err(|refused| refusal(root, refused))?;
            let mut there =
                folder::survey(root, state, &items, context.threads).map_err(|err| {
                    Error::io(
                        format_args!("could not look in the mirror {}", root.display()),
                        &err,
                    )
                })?;
            if *verify {
                read_back(context, root, &needed, &mut there)?;
            }
            (state, there)
        }
        To::Bucket(pusher) => waiting(async {
            let state = pusher.look().await?;
            let there = pusher.survey(state, &items).await?;
            Ok((state, there))
        })?,
    };
    // an archive whose files are not known yet has them to copy, as far as anyone can tell
    let copying: BTreeSet<&str> = needed
        .iter()
        .zip(&there)
        .filter(|(_, there)| **there != There::Present)
        .map(|(needed, _)| needed.dataset.as_str())
        .chain(unknown.iter().map(|(dataset, _)| dataset.as_str()))
        .collect();
    let marked = marked(context, cache, lock, &needed, &there);
    let asks = asking(lock, &datasets, &Keys::read(&context.settings), &marked)
        .into_iter()
        .filter(|ask| copying.contains(ask.dataset.as_str()))
        .collect();
    let lacked = lacking(context, cache, &needed, &there);
    Ok(Planned {
        target,
        state,
        selected,
        datasets,
        needed,
        there,
        asks,
        lacked,
        unknown,
        taking: Vec::new(),
        folder: DataFolder::default(),
        whole,
    })
}

/// reads back and hashes each copy the folder mirror at `root` holds at the right size, as `there`
/// says, and marks one whose bytes are not its pin's damaged, so the push replaces it. fails for a
/// copy that cannot be read, or at a stop.
fn read_back(
    context: &Context,
    root: &Path,
    needed: &[Needed],
    there: &mut [There],
) -> Result<(), Error> {
    let present: Vec<usize> = (0..there.len())
        .filter(|at| there[*at] == There::Present)
        .collect();
    let items: Vec<Item> = present.iter().map(|at| needed[*at].item).collect();
    let held =
        folder::check_items(root, &items, context.threads, &context.stop).map_err(|err| {
            if err.kind() == std::io::ErrorKind::Interrupted {
                Stop::stopped()
            } else {
                Error::io(
                    format_args!("could not read the mirror {}", root.display()),
                    &err,
                )
            }
        })?;
    for (at, held) in present.into_iter().zip(held) {
        there[at] = match held {
            folder::Held::Verified(_) => There::Present,
            folder::Held::Absent => There::Absent,
            folder::Held::Damaged => There::Damaged,
            folder::Held::Unreadable(err) => {
                return Err(Error::io(
                    format_args!(
                        "could not read the copy of {} on the mirror {}",
                        needed[at].label,
                        root.display()
                    ),
                    &err,
                ));
            }
        };
    }
    Ok(())
}

/// moves to `planned.taking` the files of `planned.lacked` the data folder holds: a `dry_run` only
/// those vouched for or hashing to their pin, a push any of the pinned size the state does not
/// record as other bytes, since taking checks it. fails with `project.invalid` for a pin no bytes
/// can have.
fn settle(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &mut Planned,
    dry_run: bool,
) -> Result<(), Error> {
    if !planned.lacked.is_empty() {
        planned.folder = DataFolder::planned(project, manifest, lock, cache);
        let wanted: Vec<crate::mirror::Wanted> = planned
            .lacked
            .iter()
            .map(|at| wanted(&planned.needed[*at]))
            .collect();
        let held = if dry_run {
            planned.folder.holds(context, &wanted)
        } else {
            planned.folder.may_hold(context, &wanted)
        };
        context.stop.check()?;
        let (taking, lacked): (Vec<_>, Vec<_>) =
            planned.lacked.iter().zip(held).partition(|(_, held)| *held);
        planned.taking = taking.into_iter().map(|(at, _)| *at).collect();
        planned.lacked = lacked.into_iter().map(|(at, _)| *at).collect();
    }
    let lacked: Vec<&Needed> = planned
        .lacked
        .iter()
        .map(|at| &planned.needed[*at])
        .collect();
    match mispinned(cache, lock, &lacked) {
        Some(error) => Err(error.with_report(&report(planned, Outcome::Stopped))),
        None => Ok(()),
    }
}

/// what a push downloads first: the datasets with files it lacks or archives of unknown files,
/// and the archives it sends whole.
type Fetching<'a> = (BTreeSet<&'a str>, &'a [(String, Vec<LockedFile>)]);

/// [`Fetching`] for `planned`: the datasets with files it lacks or unknown archives.
fn fetching(planned: &Planned) -> Fetching<'_> {
    let names = planned
        .lacked
        .iter()
        .map(|at| &planned.needed[*at])
        .filter(|needed| matches!(needed.source, Source::Pinned))
        .map(|needed| needed.dataset.as_str())
        .chain(planned.unknown.iter().map(|(dataset, _)| dataset.as_str()))
        .collect();
    (names, &planned.whole)
}

/// downloads into the cache what a push lacks of the fetched datasets `names`, as a sync does,
/// asking the data folder and mirrors first, then each archive of `whole` it sends whole; fails
/// offline or mirror-only for what no mirror holds, for a failed download, or at a stop.
fn fetch(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    (names, whole): Fetching<'_>,
) -> Result<(), Error> {
    if names.is_empty() {
        return Ok(());
    }
    let work = Loop::new(context, project, Arc::new(context.net()), cache.clone());
    waiting(work.ready(manifest, lock, &names))?;
    let files: Vec<(String, &LockedFile)> = whole
        .iter()
        .flat_map(|(dataset, parts)| parts.iter().map(move |file| (dataset.clone(), file)))
        .collect();
    waiting(work.ready_files(manifest, lock, &files))
}

/// the error for a file of `lacked` that `lock` pins at another size than the bytes its SHA-256
/// names, which the cache holds: no download could bring other bytes, so none is tried.
fn mispinned(cache: &Cache, lock: &Lock, lacked: &[&Needed]) -> Option<Error> {
    lacked
        .iter()
        .filter(|needed| matches!(needed.source, Source::Pinned))
        .find_map(|needed| {
            let (digest, size) = (needed.item.digest, needed.item.size);
            let actual = place::cached_other_size(cache, &digest, size)?;
            let file = lock.get(&needed.dataset)?.files.iter().find(|file| {
                file.unpacks_to.is_none() && file.sha256 == digest && file.size == size
            })?;
            let label = format!("{}/{}", needed.dataset, file.path);
            Some(place::mispinned(&label, size, actual))
        })
}

/// takes the files `planned.taking` names into the cache from the data folder, each only when its
/// bytes hash to its pin, else downloads it.
fn take(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &Planned,
) -> Result<(), Error> {
    if planned.taking.is_empty() {
        return Ok(());
    }
    let wanted = planned
        .taking
        .iter()
        .map(|at| wanted(&planned.needed[*at]))
        .collect();
    let rest = planned.folder.take(context, cache, wanted)?;
    if rest.is_empty() {
        return Ok(());
    }
    let refused: HashSet<Sha256> = rest.iter().map(|want| want.digest).collect();
    let lacked: Vec<&Needed> = planned
        .taking
        .iter()
        .map(|at| &planned.needed[*at])
        .filter(|needed| refused.contains(&needed.item.digest))
        .collect();
    let names = lacked
        .iter()
        .map(|needed| needed.dataset.as_str())
        .collect();
    fetch(
        context,
        project,
        manifest,
        lock,
        cache,
        (names, &planned.whole),
    )
    .map_err(|error| error.with_report(&report(planned, Outcome::Stopped)))
}

/// `needed` as the data folder is asked for it.
fn wanted(needed: &Needed) -> crate::mirror::Wanted {
    crate::mirror::Wanted {
        digest: needed.item.digest,
        size: needed.item.size,
        label: needed.label.clone(),
        link: None,
    }
}

/// what a push sends to a mirror, and the archives whose files this machine does not know.
struct Wanted {
    /// each file once, then the lists of what their archives unpack to.
    needed: Vec<Needed>,
    /// each archive whose files this machine does not know, with its dataset.
    unknown: Vec<(String, Fetch)>,
    /// the archives sent whole, with their datasets, since their lists of files are longer than a
    /// mirror's reader takes: each one's file, or a split one's parts.
    whole: Vec<(String, Vec<LockedFile>)>,
}

/// what pushing `datasets` needs on the mirror. an archive's members are the ones its
/// dataset's members tables keep, as a sync places them, so a member left out is not sent.
#[expect(
    clippy::too_many_lines,
    reason = "an archive's members, its list and the archive sent whole are chosen in one walk"
)]
fn needed(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    datasets: &[String],
) -> Result<Wanted, Error> {
    let mut seen = HashMap::new();
    let (mut files, mut lists, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    let mut whole = Vec::new();
    let mut add =
        |into: &mut Vec<Needed>, item: Item, label: String, dataset: &str, source, fetch| {
            if seen.contains_key(&(item.folder, item.digest)) {
                return;
            }
            seen.insert((item.folder, item.digest), into.len());
            into.push(Needed {
                item,
                label,
                dataset: dataset.to_owned(),
                source,
                fetch,
            });
        };
    for name in datasets {
        let (Some(entry), Some(locked)) = (manifest.datasets.get(name), lock.get(name)) else {
            continue;
        };
        let filters = &entry.source.members;
        // each archive's list of files, as the record that gave its members holds it
        let records = RefCell::new(Vec::new());
        // archives no mirror's reader takes the list of, which go whole in place of their files
        let wholes = RefCell::new(Vec::new());
        let members = |archive: &Archive<'_>| {
            let found = archive.recorded(cache)?;
            if let Some(tree) = archive.file.unpacks_to {
                let size = crate::cache::tree(&found).len() as u64;
                if size > crate::mirror::largest_tree() {
                    let parts: Vec<LockedFile> = archive.files().into_iter().cloned().collect();
                    wholes.borrow_mut().push(parts);
                    return Some(found);
                }
                let item = Item {
                    folder: TREES,
                    digest: tree,
                    size,
                };
                records
                    .borrow_mut()
                    .push((item, archive.key(), archive.file.path.clone()));
            }
            Some(found)
        };
        let mut planned = plan::Plan::default();
        plan::expand(
            &mut planned,
            name,
            name,
            false,
            &locked.files,
            filters,
            &members,
        )?;
        let fetch = |file: &LockedFile| Fetch {
            path: format!("{name}/{}", file.path),
            size: file.size,
            archive: file.unpacks_to.is_some(),
        };
        let wholes = wholes.into_inner();
        let sent_whole: HashSet<Sha256> = wholes.iter().flatten().map(|file| file.sha256).collect();
        for parts in &wholes {
            whole.push((name.clone(), parts.clone()));
            for file in parts {
                let item = object(file.sha256, file.size);
                let label = format!("{name}/{}", file.path);
                add(
                    &mut files,
                    item,
                    label,
                    name,
                    Source::Pinned,
                    Some(fetch(file)),
                );
            }
        }
        for target in &planned.targets {
            // a member of an archive sent whole comes with it
            if sent_whole.contains(&target.file.sha256) && target.file.unpacks_to.is_some() {
                continue;
            }
            let item = object(target.sha256, target.size);
            let brings = Some(fetch(target.file));
            add(
                &mut files,
                item,
                target.path.clone(),
                name,
                Source::Pinned,
                brings,
            );
        }
        unknown.extend(
            planned
                .unknown
                .iter()
                .map(|(_, file)| (name.clone(), fetch(file))),
        );
        for (item, archive, path) in records.into_inner() {
            let label = format!("{name}/{path}");
            add(&mut lists, item, label, name, Source::List(archive), None);
        }
    }
    files.extend(lists);
    Ok(Wanted {
        needed: files,
        unknown,
        whole,
    })
}

fn object(digest: Sha256, size: u64) -> Item {
    Item {
        folder: OBJECTS,
        digest,
        size,
    }
}

/// where in `needed` the files are that the mirror, as `there` says, and the cache both lack.
fn lacking(context: &Context, cache: &Cache, needed: &[Needed], there: &[There]) -> Vec<usize> {
    let looked: Vec<(usize, &Needed)> = needed
        .iter()
        .enumerate()
        .zip(there)
        .filter(|((_, needed), there)| {
            **there != There::Present && !matches!(needed.source, Source::List(_))
        })
        .map(|(at, _)| at)
        .collect();
    let found = crate::place::run_all(&looked, context.threads, |(_, needed)| {
        cache
            .find(&needed.item.digest, Some(needed.item.size))
            .is_some()
    });
    looked
        .iter()
        .zip(found)
        .filter(|(_, found)| !found)
        .map(|((at, _), _)| *at)
        .collect()
}

/// the error for files, `lacked`, and archives of unknown files, `unknown`, a push still lacks
/// after fetching them, as when something took them meanwhile; the next push fetches them again.
fn missing(lacked: &[&Needed], unknown: &[(String, Fetch)]) -> Error {
    let lines: Vec<String> = unknown
        .iter()
        .map(|(_, archive)| format!("the files inside {}", archive.path))
        .chain(lacked.iter().map(|needed| needed.label.clone()))
        .collect();
    let error = Error::new(
        Kind::OfflineMissing,
        "this machine's cache lacks files the push needs",
    );
    error
        .details_up_to(&lines, super::fetch::NAMED)
        .help("run the push again, which downloads them")
}

/// the datasets of `needed` that data.lock calls public with a file to copy, as `there` says,
/// that a key of this machine came with, as the cache marks it. every other dataset asks anyway.
fn marked<'n>(
    context: &Context,
    cache: &Cache,
    lock: &Lock,
    needed: &'n [Needed],
    there: &[There],
) -> BTreeSet<&'n str> {
    let copied: Vec<&Needed> = needed
        .iter()
        .zip(there)
        .filter(|(needed, there)| {
            **there != There::Present
                && matches!(needed.source, Source::Pinned)
                && lock
                    .get(&needed.dataset)
                    .is_some_and(|locked| locked.access == Access::Public)
        })
        .map(|(needed, _)| needed)
        .collect();
    let keyed = crate::place::run_all(&copied, context.threads, |needed| {
        cache.is_keyed(&needed.item.digest)
    });
    copied
        .into_iter()
        .zip(keyed)
        .filter(|(_, keyed)| *keyed)
        .map(|(needed, _)| needed.dataset.as_str())
        .collect()
}

/// the datasets that ask before they are pushed: one whose source wanted an account or accepted
/// terms or did not say, or one with a file `marked` names. `access` is a hint, never a licence
/// check.
fn asking(lock: &Lock, datasets: &[String], keys: &Keys, marked: &BTreeSet<&str>) -> Vec<Asking> {
    datasets
        .iter()
        .filter_map(|name| {
            let locked = lock.get(name)?;
            let (access, keyed) = keys.access(locked, marked.contains(name.as_str()));
            (access != Access::Public).then(|| Asking {
                dataset: name.clone(),
                access,
                keyed,
                output: locked.at.is_some(),
            })
        })
        .collect()
}

/// asks before copying the files of datasets whose sources wanted an account or accepted terms, or
/// did not say, since a mirror's readers get them without either. `--yes` answers; with no one to
/// ask, the push stops with `unconfirmed` and no command to run, as only a person may agree.
fn ask(context: &Context, planned: &Planned) -> Result<(), Error> {
    if planned.asks.is_empty() || context.yes {
        return Ok(());
    }
    let (it, its) = if planned.asks.len() == 1 {
        ("it", "its")
    } else {
        ("them", "their")
    };
    let why = reasons(&planned.asks);
    let target = &planned.target;
    let names: Vec<&str> = planned
        .asks
        .iter()
        .map(|ask| ask.dataset.as_str())
        .collect();
    let cancel = format!("the push to {target}");
    let question = format!(
        "{why}. anyone who can read {target} can take {its} files from there. push {it} anyway?"
    );
    match context
        .output
        .confirm(&question)
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not push {}", named(&names)),
        )
        .cancelled(cancel)
        .unchanged()),
        None => {
            let error = Error::new(
                Kind::Unconfirmed,
                format!("{why}, so pushing {it} needs a yes"),
            );
            let help = format!(
                "answer yes at a terminal, or add --yes once everyone who can read {target} may have {it}"
            );
            Err(error
                .help(help)
                .with_report(&report(planned, Outcome::Stopped)))
        }
    }
}

/// asks about the datasets the second plan adds beside `asked`, those the first plan asked about,
/// so no dataset is pushed without its question; `unconfirmed` when nobody said yes.
fn ask_again(context: &Context, planned: &mut Planned, asked: &[String]) -> Result<(), Error> {
    let (earlier, fresh): (Vec<Asking>, Vec<Asking>) = std::mem::take(&mut planned.asks)
        .into_iter()
        .partition(|ask| asked.contains(&ask.dataset));
    planned.asks = fresh;
    let answered = ask(context, planned);
    planned.asks.extend(earlier);
    planned.asks.sort_by(|a, b| a.dataset.cmp(&b.dataset));
    answered
}

/// why `asks` ask first, as a person reads it: `iris needs accepted terms`.
fn reasons(asks: &[Asking]) -> String {
    let alike = |access: Access| -> Vec<&str> {
        asks.iter()
            .filter(|ask| ask.access == access && !ask.keyed)
            .map(|ask| ask.dataset.as_str())
            .collect()
    };
    let needs = |names: &[&str], what: &str| {
        let verb = if names.len() == 1 { "needs" } else { "need" };
        format!("{} {verb} {what}", named(names))
    };
    let mut reasons = Vec::new();
    let login = alike(Access::Login);
    if !login.is_empty() {
        reasons.push(needs(&login, "an account to read"));
    }
    let terms = alike(Access::Terms);
    if !terms.is_empty() {
        reasons.push(needs(&terms, "accepted terms"));
    }
    let keyed: Vec<&str> = asks
        .iter()
        .filter(|ask| ask.keyed)
        .map(|ask| ask.dataset.as_str())
        .collect();
    match keyed.as_slice() {
        [] => {}
        [one] => reasons.push(format!(
            "this machine signs in where {one}'s files come from, though data.lock calls it public"
        )),
        many => reasons.push(format!(
            "this machine signs in where the files of {} come from, though data.lock calls them public",
            named(many)
        )),
    }
    let (outputs, sourced): (Vec<&str>, Vec<&str>) = alike(Access::Unknown)
        .into_iter()
        .partition(|name| asks.iter().any(|ask| ask.dataset == *name && ask.output));
    match sourced.as_slice() {
        [] => {}
        [one] => reasons.push(format!("the source of {one} did not say who may read it")),
        many => reasons.push(format!(
            "the sources of {} did not say who may read them",
            named(many)
        )),
    }
    match outputs.as_slice() {
        [] => {}
        [one] => reasons.push(format!(
            "fetchloom cannot tell who may read {one}, an output of this project"
        )),
        many => reasons.push(format!(
            "fetchloom cannot tell who may read {}, outputs of this project",
            named(many)
        )),
    }
    reasons.join("; ")
}

/// writes what `planned` found the mirror lacks, files before lists so no list comes before its
/// files, and says whether it started the mirror and what each write gave, in `planned.needed`
/// order. the first write that fails ends it; files written before keep their names.
fn write(context: &Context, cache: &Cache, planned: &Planned) -> Result<(bool, Vec<Put>), Error> {
    let root = match &planned.target {
        To::Folder { root, .. } => root,
        To::Bucket(pusher) => return write_bucket(context, cache, planned, pusher),
    };
    let (mut target, started) =
        Target::open(root, Arc::clone(&context.stop)).map_err(|refused| refusal(root, refused))?;
    if started {
        context
            .output
            .step("Started", &format!("a mirror in {}", root.display()));
    }
    let (files, lists) = planned.writes();
    let items: Vec<Item> = files
        .iter()
        .chain(&lists)
        .map(|(needed, _)| needed.item)
        .collect();
    target.prepare(&items).map_err(|err| {
        Error::io(
            format_args!("could not make folders in the mirror {}", root.display()),
            &err,
        )
    })?;
    let (removed, bytes) = target.sweep(SystemTime::now());
    if removed > 0 {
        context.output.step(
            "Removed",
            &format!(
                "{} \u{b7} {} that a stopped push left in {}",
                units::count(removed, "unfinished file", "unfinished files"),
                units::size(bytes),
                root.join("tmp").display()
            ),
        );
    }
    let mut puts = put_files(context, cache, &target, &files)?;
    puts.extend(put_lists(context, cache, &target, &lists)?);
    let written: Vec<Item> = files
        .iter()
        .chain(&lists)
        .zip(&puts)
        .filter(|(_, put)| **put != Put::Present)
        .map(|((needed, _), _)| needed.item)
        .collect();
    // every file has its name, so a stop during the flush lets the push finish it
    context.stop.finish();
    if started || !written.is_empty() {
        target.flush(&written).map_err(|err| {
            Error::io(
                format_args!(
                    "pushed to {}, but could not make sure the files stay after a power cut",
                    root.display()
                ),
                &err,
            )
        })?;
    }
    Ok((started, puts))
}

/// [`write()`] for a mirror in a bucket: the marker when the bucket had none, then the files and the
/// lists, each a PUT the bucket checks against its SHA-256, or an upload in parts.
fn write_bucket(
    context: &Context,
    cache: &Cache,
    planned: &Planned,
    pusher: &Arc<Pusher>,
) -> Result<(bool, Vec<Put>), Error> {
    let (files, lists) = planned.writes();
    let mut uploads = Vec::with_capacity(lists.len());
    for (needed, there) in &lists {
        let Source::List(archive) = needed.source else {
            continue;
        };
        let Some(members) = cache.pinned_tree(&archive, &needed.item.digest) else {
            return Err(failed_list(
                Failed::File(
                    uploads.len(),
                    Fault::Source(std::io::ErrorKind::NotFound.into()),
                ),
                &lists,
                Path::new(""),
            ));
        };
        uploads.push(s3::Upload {
            item: needed.item,
            there: *there,
            source: s3::Source::Made(crate::cache::tree(&members).into_bytes()),
        });
    }
    let started = planned.state == State::New;
    let top = pusher.top().to_string();
    let output = &context.output;
    waiting(async {
        if started {
            pusher.start().await?;
            output.step("Started", &format!("a mirror in {top}"));
        }
        let shown = Arc::new(Mutex::new(Some(progress(output, "Pushing", &files))));
        let advance = Arc::clone(&shown);
        let sent: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |size| {
            if let Some((bar, ticker)) = &*advance.lock().unwrap_or_else(PoisonError::into_inner) {
                bar.advance(size);
                ticker.advance(size);
            }
        });
        let pushing = files
            .iter()
            .map(|(needed, there)| s3::Upload {
                item: needed.item,
                there: *there,
                source: s3::Source::Cached(cache.object_path(&needed.item.digest)),
            })
            .collect();
        let put = pusher.put_all(pushing, sent).await;
        drop(shown.lock().unwrap_or_else(PoisonError::into_inner).take());
        let mut puts = put.map_err(|failed| {
            let (at, fault) = *failed;
            bucket_failure(fault, files[at].0)
        })?;
        let listed = pusher
            .put_all(uploads, Arc::new(|_| {}))
            .await
            .map_err(|failed| {
                let (at, fault) = *failed;
                bucket_failure(fault, lists[at].0)
            })?;
        puts.extend(listed);
        Ok((started, puts))
    })
}

/// a bar and a ticker of `verb` over the bytes of `files`, none of them done yet.
fn progress(
    output: &crate::output::Output,
    verb: &'static str,
    files: &[(&Needed, There)],
) -> (crate::output::Bar, crate::output::Ticker) {
    let total = files.iter().map(|(needed, _)| needed.item.size).sum();
    let what = units::count(files.len() as u64, "file", "files");
    let bar = output.bar(verb, what.clone());
    bar.start(Some(total), 0);
    let sizes = files.iter().map(|(needed, _)| Some(needed.item.size));
    (bar, output.ticker(verb, what, sizes))
}

/// the error for `fault`, from pushing `needed` into a bucket.
fn bucket_failure(fault: s3::Fault, needed: &Needed) -> Error {
    match fault {
        s3::Fault::Bucket(error) if error.kind == Kind::Interrupted => Stop::stopped(),
        s3::Fault::Bucket(error) => error,
        s3::Fault::Source(err) => unreadable(&needed.label, &err),
        s3::Fault::OtherBytes => other_bytes(&needed.label),
    }
}

/// writes `files` from the cache as [`Target::put_all`] does, with a bar of the bytes written,
/// then, once they are on the disk, one of the bytes read back and named in its place.
fn put_files(
    context: &Context,
    cache: &Cache,
    target: &Target,
    files: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let output = &context.output;
    let writing = Mutex::new(Some(progress(output, "Pushing", files)));
    let checking = OnceLock::new();
    let wrote = |size| {
        if let Some((bar, ticker)) = &*writing.lock().unwrap_or_else(PoisonError::into_inner) {
            bar.advance(size);
            ticker.advance(size);
        }
    };
    let checked = |size| {
        let (bar, ticker) = checking.get_or_init(|| {
            drop(
                writing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
            progress(output, "Checking", files)
        });
        bar.advance(size);
        ticker.advance(size);
    };
    let items: Vec<(Item, There)> = files
        .iter()
        .map(|(needed, there)| (needed.item, *there))
        .collect();
    let source = |item: &Item| read_unfollowed(&cache.object_path(&item.digest));
    let put = target.put_all(&items, context.threads, &source, &wrote, &checked);
    drop((writing, checking));
    put.map_err(|failed| failed_put(failed, files, target.root()))
}

/// writes `lists`, rendered from the cache's records, as [`Target::put_all`] does.
fn put_lists(
    context: &Context,
    cache: &Cache,
    target: &Target,
    lists: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let archives: HashMap<Sha256, Sha256> = lists
        .iter()
        .filter_map(|(needed, _)| match needed.source {
            Source::List(archive) => Some((needed.item.digest, archive)),
            Source::Pinned => None,
        })
        .collect();
    let items: Vec<(Item, There)> = lists
        .iter()
        .map(|(needed, there)| (needed.item, *there))
        .collect();
    let source = |item: &Item| {
        let members = archives
            .get(&item.digest)
            .and_then(|archive| cache.pinned_tree(archive, &item.digest))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the cache no longer records what the archive unpacks to",
                )
            })?;
        Ok(std::io::Cursor::new(
            crate::cache::tree(&members).into_bytes(),
        ))
    };
    target
        .put_all(&items, context.threads, &source, &|_| {}, &|_| {})
        .map_err(|failed| failed_list(failed, lists, target.root()))
}

/// the error for `failed`, from writing the lists `lists` into the mirror at `root`. a list the
/// cache stopped recording after the push planned is missing, as before a push fetched it.
fn failed_list(failed: Failed, lists: &[(&Needed, There)], root: &Path) -> Error {
    match failed {
        Failed::File(at, Fault::Source(_)) => Error::new(
            Kind::OfflineMissing,
            format!(
                "the cache no longer knows the files inside {}",
                lists[at].0.label
            ),
        )
        .help("run the push again, which fetches what it holds"),
        failed => failed_put(failed, lists, root),
    }
}

/// the error for `failed`, from writing `files` into the mirror at `root`.
fn failed_put(failed: Failed, files: &[(&Needed, There)], root: &Path) -> Error {
    match failed {
        Failed::File(at, fault) => failure(fault, files[at].0, root),
        Failed::Flush(err) if err.kind() == std::io::ErrorKind::Interrupted => Stop::stopped(),
        Failed::Flush(err) => Error::io(
            format_args!(
                "could not put the files pushed to {} on its disk",
                root.display()
            ),
            &err,
        ),
    }
}

/// the error for `fault`, from writing `needed` into the mirror at `root`.
fn failure(fault: Fault, needed: &Needed, root: &Path) -> Error {
    let label = &needed.label;
    match fault {
        Fault::Mirror(err) if err.kind() == std::io::ErrorKind::Interrupted => Stop::stopped(),
        Fault::Mirror(err) => Error::io(
            format_args!("could not push {label} to {}", root.display()),
            &err,
        ),
        Fault::Source(err) => unreadable(label, &err),
        Fault::OtherBytes => other_bytes(label),
        Fault::ReadBack => Error::new(
            Kind::IntegrityMismatch,
            format!(
                "{label}, read back from {}, holds other bytes than were written, so it was not given its name",
                root.display()
            ),
        )
        .help("check the disk that holds the mirror, then push again"),
        Fault::Replaced => Error::new(
            Kind::LocalIo,
            format!(
                "something replaced {label} in the mirror {} while push wrote it, so it was not given its name",
                root.display()
            ),
        )
        .help("stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again"),
    }
}

/// the error for the cache's copy of the file a person knows as `label` that could not be read.
fn unreadable(label: &str, err: &std::io::Error) -> Error {
    Error::io(
        format_args!("could not read the cache's copy of {label}"),
        err,
    )
}

/// the error for the cache's copy of `label` holding other bytes than its pin.
fn other_bytes(label: &str) -> Error {
    Error::new(
        Kind::IntegrityMismatch,
        format!("the cache's copy of {label} does not hold its pinned bytes, so it was not pushed"),
    )
    .help(format!(
        "run `{}`, which notes it, then `{}` to fetch it again",
        crate::suggested::anywhere("cache verify"),
        super::project::command("sync")
    ))
}

/// the error for a folder that cannot take a push.
fn refusal(root: &Path, refused: Refused) -> Error {
    let shown = root.display();
    match refused {
        Refused::NotEmpty => Error::new(
            Kind::LocalNotEmpty,
            format!("{shown} holds files, but no fetchloom-mirror file says it is a mirror"),
        )
        .help("name an empty folder, or the top folder of a mirror, which holds its fetchloom-mirror file, and check that its disk is mounted"),
        Refused::NoParent => Error::new(
            Kind::LocalIo,
            format!("neither {shown} nor the folder above it is there"),
        )
        .help("check the folder's name, and that its disk is mounted"),
        Refused::NotAFolder => Error::new(Kind::LocalIo, format!("{shown} is not a folder"))
            .help("name a folder for the mirror"),
        Refused::Failed(err) => Error::io(format_args!("could not use the mirror {shown}"), &err),
    }
}

/// how a push that `planned` ended.
#[derive(Clone, Copy)]
enum Outcome<'a> {
    /// it wrote: whether it started the mirror, and what writing each file not there gave, in
    /// order, every file before every list.
    Written(bool, &'a [Put]),
    /// it stopped before writing, as when the cache lacks a file or nobody said yes.
    Stopped,
    DryRun,
}

/// the report of `planned` as it ended: what it set out to write and wrote, or would write.
fn report(planned: &Planned, outcome: Outcome<'_>) -> Report {
    let dry_run = matches!(outcome, Outcome::DryRun);
    let mut report = Report::empty(planned.target.to_string(), dry_run);
    let (started, puts) = match outcome {
        Outcome::Written(started, puts) => (started, Some(puts)),
        Outcome::Stopped => (false, None),
        Outcome::DryRun => (planned.state == State::New, None),
    };
    report.started = started;
    report.selected.clone_from(&planned.selected);
    report.datasets.clone_from(&planned.datasets);
    report.asks = planned
        .asks
        .iter()
        .map(|ask| Ask {
            dataset: ask.dataset.clone(),
            access: ask.access.word(),
        })
        .collect();
    let mut puts = puts.map(<[Put]>::iter);
    for (needed, there) in planned.needed.iter().zip(&planned.there) {
        let put = match (there, &mut puts) {
            (There::Present, _) => Some(Put::Present),
            (_, Some(puts)) => puts.next().copied(),
            // a dry run counts what it would write; a push that stopped wrote nothing
            (There::Damaged, None) if dry_run => Some(Put::Replaced),
            (There::Absent, None) if dry_run => Some(Put::Copied),
            (There::Damaged | There::Absent, None) => None,
        };
        report.tally(needed, *there, put);
    }
    if let Some(changes) = &mut report.changes {
        (changes.download, changes.unpack) = downloads(planned);
    }
    report
}

/// what a push would download first: the file that brings each it lacks, each once, and the
/// archives among them, which are unpacked once they are in.
fn downloads(planned: &Planned) -> (Vec<Fetch>, Vec<String>) {
    let mut seen = HashSet::new();
    let lacking = planned
        .lacked
        .iter()
        .filter_map(|at| planned.needed[*at].fetch.as_ref());
    let download: Vec<Fetch> = lacking
        .chain(planned.unknown.iter().map(|(_, archive)| archive))
        .filter(|fetch| seen.insert(fetch.path.as_str()))
        .cloned()
        .collect();
    let unpack = download
        .iter()
        .filter(|fetch| fetch.archive)
        .map(|fetch| fetch.path.clone())
        .collect();
    (download, unpack)
}

impl Report {
    /// a report of nothing pushed yet into `target`.
    fn empty(target: String, dry_run: bool) -> Report {
        Report {
            target,
            started: false,
            selected: None,
            datasets: Vec::new(),
            objects: 0,
            bytes: 0,
            planned: 0,
            planned_bytes: 0,
            copied: 0,
            copied_bytes: 0,
            present: 0,
            present_bytes: 0,
            replaced: 0,
            trees: 0,
            asks: Vec::new(),
            dry_run,
            changes: dry_run.then(|| Changes {
                copy: Vec::new(),
                trees: Vec::new(),
                download: Vec::new(),
                unpack: Vec::new(),
            }),
        }
    }

    /// counts `needed`, which the mirror held as `there` when the push planned, and which writing
    /// it gave `put`, or which was not written.
    fn tally(&mut self, needed: &Needed, there: There, put: Option<Put>) {
        let size = needed.item.size;
        if let Source::List(_) = needed.source {
            if matches!(put, Some(Put::Copied | Put::Replaced)) {
                self.trees += 1;
                if let Some(changes) = &mut self.changes {
                    changes.trees.push(copied(needed));
                }
            }
            return;
        }
        self.objects += 1;
        self.bytes += size;
        if there != There::Present {
            self.planned += 1;
            self.planned_bytes += size;
        }
        match put {
            Some(Put::Present) => {
                self.present += 1;
                self.present_bytes += size;
            }
            Some(put @ (Put::Copied | Put::Replaced)) => {
                self.copied += 1;
                self.copied_bytes += size;
                self.replaced += u64::from(put == Put::Replaced);
                if let Some(changes) = &mut self.changes {
                    changes.copy.push(copied(needed));
                }
            }
            None => {}
        }
    }
}

fn copied(needed: &Needed) -> Copied {
    Copied {
        path: needed.label.clone(),
        sha256: needed.item.digest,
        size: needed.item.size,
    }
}

/// the lines of a push that wrote: what it copied, what it replaced, what it found there.
fn tell(context: &Context, planned: &Planned, report: &Report) {
    let output = &context.output;
    let target = &planned.target;
    for (dataset, parts) in &planned.whole {
        if let Some(file) = parts.first() {
            output.hint(&format!(
                "{dataset}/{} went whole, since the list of files it holds is longer than a mirror's reader takes",
                file.path
            ));
        }
    }
    if report.copied > 0 {
        output.step(
            "Pushed",
            &format!(
                "{} \u{b7} {} to {target}",
                units::count(report.copied, "file", "files"),
                units::size(report.copied_bytes)
            ),
        );
    }
    if report.trees > 0 {
        output.detail(&format!(
            "and what {}",
            units::count(report.trees, "archive holds", "archives hold")
        ));
    }
    if report.replaced > 0 {
        output.mark(
            "Replaced",
            &format!(
                "{} on the mirror",
                units::count(report.replaced, "damaged copy", "damaged copies")
            ),
        );
    }
    if report.present > 0 {
        output.step(
            "Skipped",
            &format!(
                "{} already there \u{b7} {}",
                units::count(report.present, "file", "files"),
                units::size(report.present_bytes)
            ),
        );
    }
    output.done("Ok", &everything(&planned.datasets));
    output.next(&format!(
        "teammates run `fetchloom settings set mirrors {}`, naming any mirrors they use already too",
        crate::suggested::shell_word(&target.to_string())
    ));
}

/// the lines of a dry run: what a push would take in, write, find there and ask about.
fn tell_dry(context: &Context, planned: &Planned, report: &Report) {
    let output = &context.output;
    let target = &planned.target;
    if !planned.taking.is_empty() {
        let bytes = planned
            .taking
            .iter()
            .map(|at| planned.needed[*at].item.size)
            .sum();
        let files = planned.taking.len() as u64;
        planned.folder.tell(output, "Would copy", files, bytes);
    }
    let (download, unpack): (&[Fetch], &[String]) = match &report.changes {
        Some(changes) => (&changes.download, &changes.unpack),
        None => (&[], &[]),
    };
    for fetch in download {
        output.step(
            "Would fetch",
            &format!("{} \u{b7} {}", fetch.path, units::size(fetch.size)),
        );
    }
    for archive in unpack {
        output.step("Would open", archive);
    }
    if !planned.unknown.is_empty() {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    if report.started {
        output.step("Would start", &format!("a mirror in {target}"));
    }
    if report.copied > 0 {
        output.step(
            "Would push",
            &format!(
                "{} \u{b7} {} to {target}",
                units::count(report.copied, "file", "files"),
                units::size(report.copied_bytes)
            ),
        );
    }
    if report.replaced > 0 {
        output.mark(
            "Damaged",
            &format!(
                "{} on the mirror, which a push replaces",
                units::count(report.replaced, "copy", "copies")
            ),
        );
    }
    if report.present > 0 {
        output.step(
            "Would skip",
            &format!(
                "{} already there \u{b7} {}",
                units::count(report.present, "file", "files"),
                units::size(report.present_bytes)
            ),
        );
    }
    if !planned.asks.is_empty() && !context.yes {
        output.step("Would ask", &format!("first: {}", reasons(&planned.asks)));
    }
    let nothing = report.copied == 0 && report.trees == 0 && !report.started && download.is_empty();
    let everything = everything(&planned.datasets);
    super::project::dry_run_done(output, nothing.then_some(everything.as_str()));
}

/// the closing line of a push with nothing to push: data.toml names no dataset, or the push leaves
/// out every one it names.
fn nothing(manifest: &Manifest) -> String {
    if manifest.datasets.is_empty() {
        everything(&[])
    } else {
        "no dataset in data.toml can be pushed yet, so there is nothing to push".to_owned()
    }
}

/// the closing line of a push of `datasets`: every file they need is on the mirror.
fn everything(datasets: &[String]) -> String {
    let names: Vec<&str> = datasets.iter().map(String::as_str).collect();
    match names.as_slice() {
        [] => "data.toml has no datasets yet, so there is nothing to push".to_owned(),
        [one] => format!("every file {one} needs is on the mirror"),
        few if few.len() <= 3 => format!(
            "every file {} need is on the mirror",
            units::listed(few, "and")
        ),
        many => format!(
            "every file the {} datasets need is on the mirror",
            units::grouped(many.len() as u64)
        ),
    }
}

/// dataset names as a person reads them in a sentence: the first few, then how many more.
fn named(names: &[&str]) -> String {
    const FEW: usize = 5;
    if names.len() <= FEW + 1 {
        return units::listed(names, "and");
    }
    units::some_of(names, FEW)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::lock::{Locked, LockedFile};

    fn locked(name: &str, access: Access) -> Locked {
        Locked {
            name: name.to_owned(),
            reference: "zenodo:1".to_owned(),
            spec: "sha256:00".to_owned(),
            at: None,
            extract: true,
            decompress: false,
            web_page: false,
            resolved: "zenodo:1@1".to_owned(),
            title: None,
            license: None,
            doi: None,
            access,
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            files: Vec::<LockedFile>::new(),
        }
    }

    /// with no `--to` and no `push_to`, the help names `settings set push_to` and the details the
    /// mirrors a person may choose; an empty `FETCHLOOM_PUSH_TO` is named as what cleared
    /// `push_to`, rather than sending a person to set it in a settings file that may already.
    #[test]
    fn an_empty_push_to_variable_is_named_as_what_cleared_the_folder() {
        let mut context = Context::new(crate::output::Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            crate::output::Level::Quiet,
            false,
            false,
        ));
        let args = args::Push {
            names: Vec::new(),
            to: None,
            verify: false,
            dry: args::DryRun { dry_run: false },
        };
        let error = target(&context, &args).unwrap_err();
        assert_eq!(
            error.help.as_deref(),
            Some(
                "give --to <folder> or --to s3://bucket/prefix, or save it for every push with `fetchloom settings set push_to <folder>`"
            )
        );
        assert_eq!(error.details, Vec::<String>::new());
        // a folder or a bucket is offered, a web mirror is none push can write, and none is taken
        let folder = |path: &str| Location::Folder(PathBuf::from(path));
        let bucket = Location::parse("s3://lab-data/fetchloom", None).unwrap();
        let named = bucket.to_string();
        context.settings.mirrors = crate::settings::Set {
            value: vec![
                folder("/mnt/lab/fetchloom"),
                Location::Web(url::Url::parse("https://data.lab.example/fetchloom/").unwrap()),
                bucket,
            ],
            origin: crate::settings::Origin::File,
        };
        let error = target(&context, &args).unwrap_err();
        assert_eq!(
            error.details,
            [format!(
                "the mirrors setting names the mirrors /mnt/lab/fetchloom and {named}; push copies into one only once you name it"
            )]
        );
        context.settings.mirrors.value.truncate(1);
        assert_eq!(
            target(&context, &args).unwrap_err().details,
            [
                "the mirrors setting names the mirror /mnt/lab/fetchloom; push copies into it only once you name it"
            ]
        );
        context.settings.push_to = crate::settings::Set {
            value: None,
            origin: crate::settings::Origin::Environment,
        };
        let error = target(&context, &args).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "push needs a folder or an S3 bucket to copy the files into"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "FETCHLOOM_PUSH_TO is set empty, which clears push_to: give --to <folder> or --to s3://bucket/prefix, or unset FETCHLOOM_PUSH_TO"
            )
        );
    }

    /// a dataset data.lock calls public is asked about when this machine keeps a key for where its
    /// files come from, or the cache marks one of its files as one a key came with, since a clone's
    /// data.lock cannot vouch for either.
    #[test]
    fn a_key_for_a_files_host_asks_whatever_data_lock_says() {
        let mut raw = locked("raw", Access::Public);
        raw.files = vec![LockedFile {
            path: "a.csv".to_owned(),
            size: 1,
            sha256: crate::digest::sha256(b"a"),
            md5: None,
            url: "https://data.lab.example/raw/a.csv".to_owned(),
            unpacks_to: None,
        }];
        let lock = Lock {
            datasets: vec![raw, locked("vault", Access::Public)],
            stated: Vec::new(),
            waiting: Vec::new(),
        };
        let names = ["raw".to_owned(), "vault".to_owned()];
        let unmarked = BTreeSet::new();
        assert!(asking(&lock, &names, &Keys::default(), &unmarked).is_empty());
        let keys = Keys {
            hosts: HashSet::from(["data.lab.example".to_owned()]),
            ..Keys::default()
        };
        let asked = |asks: &[Asking]| -> Vec<(String, bool)> {
            asks.iter()
                .map(|ask| (ask.dataset.clone(), ask.keyed))
                .collect()
        };
        let marked = asking(&lock, &names, &Keys::default(), &BTreeSet::from(["vault"]));
        assert_eq!(asked(&marked), [("vault".to_owned(), true)]);
        let asks = asking(&lock, &names, &keys, &unmarked);
        assert_eq!(asked(&asks), [("raw".to_owned(), true)]);
        assert_eq!(
            reasons(&asks),
            "this machine signs in where raw's files come from, though data.lock calls it public"
        );
    }

    /// losing a list after the push planned is no disk failure, since the next push fetches it.
    #[test]
    fn a_list_the_cache_lost_is_missing_with_what_to_run() {
        let digest = crate::digest::sha256(b"images.zip");
        let needed = Needed {
            item: Item {
                folder: TREES,
                digest: crate::digest::sha256(b"its list"),
                size: 8,
            },
            label: "pets/images.zip".into(),
            dataset: "pets".into(),
            source: Source::List(digest),
            fetch: None,
        };
        let lost = std::io::Error::new(std::io::ErrorKind::NotFound, "no record");
        let error = failed_list(
            Failed::File(0, Fault::Source(lost)),
            &[(&needed, There::Absent)],
            Path::new("mirror"),
        );
        assert_eq!(error.kind, Kind::OfflineMissing);
        assert_eq!(
            error.message,
            "the cache no longer knows the files inside pets/images.zip"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("run the push again, which fetches what it holds")
        );
    }

    /// bytes read back wrong point at the disk, `integrity.mismatch`; a file something else put in
    /// its place is `local.io`, with what to stop, since no disk fault caused it.
    #[test]
    fn a_replaced_file_is_told_apart_from_bytes_the_disk_changed() {
        let needed = Needed {
            item: object(crate::digest::sha256(b"small file\n"), 11),
            label: "many/d00/f00".into(),
            dataset: "many".into(),
            source: Source::Pinned,
            fetch: None,
        };
        let root = Path::new("/mnt/lab/fetchloom");
        let changed = failure(Fault::ReadBack, &needed, root);
        assert_eq!(
            (changed.kind, changed.kind.exit_code()),
            (Kind::IntegrityMismatch, 30)
        );
        let replaced = failure(Fault::Replaced, &needed, root);
        assert_eq!(
            (replaced.kind, replaced.kind.exit_code()),
            (Kind::LocalIo, 50)
        );
        assert_eq!(
            replaced.message,
            "something replaced many/d00/f00 in the mirror /mnt/lab/fetchloom while push wrote it, so it was not given its name"
        );
        assert_eq!(
            replaced.help.as_deref(),
            Some(
                "stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again"
            )
        );
    }

    fn asks(dataset: &str, access: Access) -> Asking {
        Asking {
            dataset: dataset.to_owned(),
            access,
            keyed: false,
            output: false,
        }
    }

    /// reasons are grouped as a person reads them.
    #[test]
    fn the_question_says_why_each_dataset_asks() {
        let told = [
            asks("pets", Access::Terms),
            asks("iris", Access::Unknown),
            asks("dogs", Access::Unknown),
            asks("vault", Access::Login),
        ];
        assert_eq!(
            reasons(&told),
            "vault needs an account to read; pets needs accepted terms; the sources of iris and dogs did not say who may read them"
        );
        assert_eq!(
            reasons(&[asks("iris", Access::Unknown)]),
            "the source of iris did not say who may read it"
        );
        let six = ["a", "b", "c", "d", "e", "f"];
        assert_eq!(named(&six), "a, b, c, d, e and f");
        let eight = ["a", "b", "c", "d", "e", "f", "g", "h"];
        assert_eq!(named(&eight), "a, b, c, d, e and 3 more");
    }
}
