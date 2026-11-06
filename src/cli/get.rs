//! `fetchloom get <link>...`: datasets or files into new folders, once, through the same cache as
//! projects. archives unpack into the folder unless `--no-extract` keeps them packed. a second run
//! asks the server whether anything changed and moves no bytes when nothing did. several links,
//! given on the command line or listed in `--input-file`, download together, and one that fails
//! leaves the others to finish.

mod folder;

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::task::JoinSet;

use self::folder::{Outcome, check, put_all};
use super::args::Get;
use super::fetch::{self, Job};
use super::sync as engine;
use super::sync::place::doubtful;
use super::sync::unpack::{self as unpacking, Unpack};
use super::{Context, open_cache};
use crate::archive::{self, Packed};
use crate::cache::{Cache, UrlRecord};
use crate::digest::Sha256;
use crate::error::{Error, FileFailure, Kind};
use crate::name;
use crate::net::{self, Fetched, Net, Origin};
use crate::place::Method;
use crate::plan::{self, Patterns, Plan};
use crate::project::lock::LockedFile;
use crate::project::manifest::{self, Manifest};
use crate::source::search::is_search;
use crate::source::{Listing, Parsed, Reference, Remote};
use crate::units;

/// what `get` did, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    /// the folder of the one link asked for, or none when several were.
    folder: Option<PathBuf>,
    files: Vec<Got>,
    /// what happened to each link, in the order given.
    items: Vec<Item>,
}

#[derive(Clone, Serialize)]
struct Got {
    path: String,
    size: u64,
    sha256: Sha256,
    url: String,
    origin: Origin,
    /// how the file was put into the folder, or none when it was there already.
    method: Option<Method>,
    /// the archive the file was unpacked from, by its path in the download.
    unpacked_from: Option<String>,
}

/// one link's outcome.
#[derive(Serialize)]
struct Item {
    /// the link as given, a secret in it masked.
    reference: String,
    status: &'static str,
    folder: Option<PathBuf>,
    files: Vec<Got>,
    error: Option<Problem>,
}

/// why one link failed, in the form of the document's own errors.
#[derive(Serialize)]
struct Problem {
    kind: &'static str,
    message: String,
    details: Vec<String>,
    help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<FileFailure>,
}

impl Item {
    fn done(link: &str, done: &Done) -> Item {
        Item {
            reference: shown(link),
            status: "ok",
            folder: Some(done.folder.clone()),
            files: done.files.clone(),
            error: None,
        }
    }

    fn failed(link: &str, error: &Error) -> Item {
        Item {
            reference: shown(link),
            status: "error",
            folder: None,
            files: Vec::new(),
            error: Some(Problem {
                kind: error.kind.name(),
                message: error.message.clone(),
                details: error.details.clone(),
                help: error.help.clone(),
                failures: error.failures.clone(),
            }),
        }
    }
}

/// the folder one link filled, and what is in it.
struct Done {
    folder: PathBuf,
    files: Vec<Got>,
}

/// a link resolved and checked, with the downloads it needs.
struct Wanted {
    parsed: Parsed,
    listing: Listing,
    files: Vec<Remote>,
    jobs: Vec<Job>,
    /// the folder `-o` names for the one link.
    chosen: Option<PathBuf>,
    /// the folder `-o` names for several links, each in a folder of its own inside it.
    parent: Option<PathBuf>,
    extract: bool,
    /// the hash `--sha256` asks the one file to have.
    sha256: Option<Sha256>,
}

pub(super) async fn run(context: &Context, args: Get) -> Result<Report, Error> {
    // `-` reads a list for -i, but a download is a folder of files, never one stream
    if args.output.as_deref() == Some(std::path::Path::new("-")) {
        return Err(Error::new(
            Kind::Usage,
            "get writes into a folder, so -o cannot be - for stdout",
        )
        .help("give -o a folder name, then read the files there; `fetchloom get` prints the folder's path"));
    }
    let links = links(&args)?;
    if args.sha256.is_some() && links.len() != 1 {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "--sha256 checks one file, and this get has {}",
                units::count(links.len() as u64, "link", "links")
            ),
        )
        .help("run one `fetchloom get --sha256` for each file to check"));
    }
    let net = Arc::new(context.net());
    if let [link] = links.as_slice()
        && args.input_file.is_none()
    {
        return one(context, &net, &args, link.clone()).await;
    }
    many(context, &net, &args, &links).await
}

/// the links to fetch: those on the command line, then those `--input-file` lists. several links
/// download together, so each names what it fetches, and only a link alone may be search words.
fn links(args: &Get) -> Result<Vec<String>, Error> {
    let mut links = args.links.clone();
    let several = links.len() > 1 || args.input_file.is_some();
    if several && let Some(word) = links.iter().find(|link| is_search(link)) {
        let error = Error::new(
            Kind::Usage,
            format!("\"{word}\" is not a link, a DOI or an id"),
        )
        .detail("several links download together, so each one names what it fetches");
        if links.len() == 1 {
            return Err(error.help(format!(
                "find its id with `fetchloom search {word}`, then pass that"
            )));
        }
        let example = if links.iter().all(|link| is_search(link)) {
            links.join(" ")
        } else {
            "fashion mnist".to_owned()
        };
        return Err(error.help(format!(
            "to search for several words, quote them, as in `fetchloom get \"{example}\"`"
        )));
    }
    let Some(path) = &args.input_file else {
        return Ok(links);
    };
    let (name, text) = read_list(path)?;
    let before = links.len();
    for (number, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if is_search(line) {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "line {} of {name} is not a link, a DOI or an id",
                    number + 1
                ),
            )
            .detail(line.to_owned())
            .help(format!(
                "find its id with `fetchloom search {line}`, then list that instead"
            )));
        }
        links.push(line.to_owned());
    }
    if links.len() == before {
        return Err(Error::new(Kind::Usage, format!("{name} lists no links"))
            .help("put one link a line in it. blank lines and lines starting with # are skipped"));
    }
    Ok(links)
}

/// the text of the list `--input-file` names, and the name messages give it.
fn read_list(path: &Path) -> Result<(String, String), Error> {
    let mut text = String::new();
    if path == Path::new("-") {
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| Error::io("could not read links from stdin", &err))?;
        return Ok(("stdin".to_owned(), text));
    }
    let name = path.display().to_string();
    std::fs::File::open(path)
        .and_then(|mut file| file.read_to_string(&mut text))
        .map_err(|err| Error::io(format_args!("could not read {name}"), &err))?;
    Ok((name, text))
}

/// a link or search words alone: into the folder `-o` names, or one named after the download.
async fn one(
    context: &Context,
    net: &Arc<Net>,
    args: &Get,
    mut link: String,
) -> Result<Report, Error> {
    if is_search(&link) {
        link = super::search::pick(context, net.as_ref(), &link, "get").await?;
    }
    let parsed = Reference::parse(&link)?;
    engine::tell_raw(&context.output, &link, &parsed.reference);
    let cache = open_cache()?;
    net.trust(parsed.reference.written_host());
    let listing = engine::resolve(&context.output, &parsed.reference, net).await?;
    engine::tell_resolved(&context.output, &listing);
    let mut wanted = prepare(&cache, args, args.output.as_deref(), None, parsed, listing)?;
    approve(context, &cache, &wanted.jobs)?;
    let started = Instant::now();
    let jobs = std::mem::take(&mut wanted.jobs);
    let fetched = fetch::all(context, net, &cache, jobs).await?;
    let done = finish(
        context,
        &cache,
        wanted,
        fetched,
        started.elapsed(),
        &mut Taken::default(),
    )
    .await?;
    context.output.answer(&done.folder.display().to_string());
    tell_next(context, args, &link);
    Ok(Report {
        folder: Some(done.folder.clone()),
        files: done.files.clone(),
        items: vec![Item::done(&link, &done)],
    })
}

/// several links in one run: resolved together, their files downloaded together within the
/// network's limits, then each placed in a folder of its own. a link that fails is told and
/// kept in the report while the others go on.
async fn many(
    context: &Context,
    net: &Arc<Net>,
    args: &Get,
    links: &[String],
) -> Result<Report, Error> {
    let cache = open_cache()?;
    let resolved = resolve_all(context, net, links).await?;
    let mut wanted = Vec::with_capacity(links.len());
    for (link, resolved) in links.iter().zip(resolved) {
        let prepared = resolved.and_then(|(parsed, listing)| {
            prepare(&cache, args, None, args.output.as_deref(), parsed, listing)
        });
        if let Err(error) = &prepared {
            tell_failed(context, link, error);
        }
        wanted.push(prepared);
    }
    let mut jobs = Vec::new();
    let mut owners = Vec::new();
    for (index, item) in wanted.iter_mut().enumerate() {
        if let Ok(item) = item {
            owners.extend(std::iter::repeat_n(index, item.jobs.len()));
            jobs.append(&mut item.jobs);
        }
    }
    approve(context, &cache, &jobs)?;
    let started = Instant::now();
    let mut fetched: Vec<Vec<Result<Fetched, Error>>> = links.iter().map(|_| Vec::new()).collect();
    if !jobs.is_empty() {
        for (owner, result) in owners
            .into_iter()
            .zip(fetch::apart(context, net, &cache, jobs).await?)
        {
            fetched[owner].push(result);
        }
    }
    let elapsed = started.elapsed();
    let mut items = Vec::with_capacity(links.len());
    let mut failed = Vec::new();
    let mut taken = Taken::default();
    for (index, ((link, wanted), fetched)) in links.iter().zip(wanted).zip(fetched).enumerate() {
        let wanted = match wanted {
            Ok(wanted) => wanted,
            Err(error) => {
                items.push(Item::failed(link, &error));
                failed.push((index, error));
                continue;
            }
        };
        match settle(context, &cache, wanted, fetched, elapsed, &mut taken).await {
            Ok(done) => {
                context.output.answer(&done.folder.display().to_string());
                items.push(Item::done(link, &done));
            }
            Err(error) => {
                tell_failed(context, link, &error);
                items.push(Item::failed(link, &error));
                failed.push((index, error));
            }
        }
    }
    let placed: usize = items.iter().map(|item| item.files.len()).sum();
    let report = Report {
        folder: None,
        files: Vec::new(),
        items,
    };
    if failed.is_empty() {
        context.output.done(
            "Finished",
            &format!(
                "{} · {}",
                units::count(links.len() as u64, "link", "links"),
                units::count(placed as u64, "file", "files")
            ),
        );
        return Ok(report);
    }
    let mut error = some_failed(links, &failed);
    if let Ok(result) = serde_json::to_value(&report) {
        error = error.with_result(result);
    }
    Err(error)
}

/// resolves every link at once within the network's limits, each to its listing or to why it
/// could not be, in the links' order. only an interruption stops them all.
async fn resolve_all(
    context: &Context,
    net: &Arc<Net>,
    links: &[String],
) -> Result<Vec<Result<(Parsed, Listing), Error>>, Error> {
    let mut results: Vec<Option<Result<(Parsed, Listing), Error>>> =
        links.iter().map(|_| None).collect();
    let mut tasks = JoinSet::new();
    let mut asked = Vec::new();
    for (index, link) in links.iter().enumerate() {
        let parsed = match Reference::parse(link) {
            Ok(parsed) => parsed,
            Err(error) => {
                results[index] = Some(Err(error));
                continue;
            }
        };
        engine::tell_raw(&context.output, link, &parsed.reference);
        net.trust(parsed.reference.written_host());
        if !engine::resolves_locally(&parsed.reference) {
            asked.push(parsed.reference.shown());
        }
        let net = net.clone();
        tasks.spawn(async move {
            let listing = parsed.reference.resolve(&*net).await;
            (index, listing.map(|listing| (parsed, listing)))
        });
    }
    let _waiting = match asked.as_slice() {
        [] => None,
        [only] => Some(context.output.wait("Resolving", only.clone())),
        several => Some(context.output.wait(
            "Resolving",
            units::count(several.len() as u64, "link", "links"),
        )),
    };
    while let Some(joined) = tasks.join_next().await {
        let (index, resolved) = joined.map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("resolving stopped unexpectedly: {err}"),
            )
        })?;
        match &resolved {
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error.clone());
            }
            Err(_) => {}
            Ok((_, listing)) => engine::tell_resolved(&context.output, listing),
        }
        results[index] = Some(resolved);
    }
    Ok(results
        .into_iter()
        .map(|result| {
            result
                .unwrap_or_else(|| Err(Error::new(Kind::LocalIo, "resolving stopped unexpectedly")))
        })
        .collect())
}

/// the files a resolved link keeps, the downloads they need, and the folder checked before any
/// byte arrives, so a refusal costs no download. an archive never unpacked here is checked once
/// its files are known, after the download.
fn prepare(
    cache: &Cache,
    args: &Get,
    chosen: Option<&Path>,
    parent: Option<&Path>,
    parsed: Parsed,
    listing: Listing,
) -> Result<Wanted, Error> {
    let files = selected(args, &parsed, &listing)?;
    if let Some(expected) = args.sha256 {
        one_file(&files, expected, &parsed)?;
    }
    let extract = !args.no_extract;
    let (names, foreseen) = foresee(cache, &files, extract);
    check(
        &folder_for(
            chosen,
            parent,
            &listing,
            &parsed,
            &unwrapped(&files, &names),
        ),
        foreseen.as_deref(),
    )?;
    let jobs: Vec<Job> = files
        .iter()
        .zip(&names)
        .map(|(remote, name)| fetch::listed_job(remote, name, extract))
        .collect();
    vet(cache, &files, &names, extract)?;
    Ok(Wanted {
        parsed,
        listing,
        files,
        jobs,
        chosen: chosen.map(Path::to_path_buf),
        parent: parent.map(Path::to_path_buf),
        extract,
        sha256: args.sha256,
    })
}

const SHA256_HELP: &str =
    "check the hash given to --sha256, or ask the file's owner which bytes are right";

/// refuses `--sha256` for anything but one file, and a file whose source states another hash,
/// before it downloads.
fn one_file(files: &[Remote], expected: Sha256, parsed: &Parsed) -> Result<(), Error> {
    let [file] = files else {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "--sha256 checks one file, and {} has {}",
                parsed.reference.shown(),
                units::count(files.len() as u64, "file", "files")
            ),
        )
        .help("keep one file with --select, or leave out --sha256"));
    };
    match file.sha256 {
        Some(stated) if stated != expected => Err(Error::new(
            Kind::IntegrityMismatch,
            format!("{} is not the file --sha256 names", file.path),
        )
        .detail(format!(
            "expected sha256 {}, the source states {}",
            expected.short(),
            stated.short()
        ))
        .help(SHA256_HELP)),
        _ => Ok(()),
    }
}

/// the downloads of one of several links put in place, or every one of its files that failed.
async fn settle(
    context: &Context,
    cache: &Cache,
    wanted: Wanted,
    fetched: Vec<Result<Fetched, Error>>,
    elapsed: Duration,
    taken: &mut Taken,
) -> Result<Done, Error> {
    let labels: Vec<String> = wanted.files.iter().map(|file| file.path.clone()).collect();
    let mut found = Vec::with_capacity(fetched.len());
    let mut failed = Vec::new();
    for (index, result) in fetched.into_iter().enumerate() {
        match result {
            Ok(fetched) => found.push(fetched),
            Err(error) => failed.push((index, error)),
        }
    }
    if let Some(error) = fetch::together(&labels, failed) {
        return Err(error);
    }
    finish(context, cache, wanted, found, elapsed, taken).await
}

/// checks what arrived, unpacks archives and puts every file in the link's folder, unless an
/// earlier link of the run took that folder.
async fn finish(
    context: &Context,
    cache: &Cache,
    wanted: Wanted,
    fetched: Vec<Fetched>,
    elapsed: Duration,
    taken: &mut Taken,
) -> Result<Done, Error> {
    let Wanted {
        parsed,
        listing,
        files,
        chosen,
        parent,
        extract,
        sha256,
        ..
    } = wanted;
    let paths: Vec<String> = files
        .iter()
        .zip(&fetched)
        .map(|(remote, fetched)| {
            let path = match fetched.name.as_deref().and_then(name::sanitize) {
                Some(given) if remote.named_by_server => given,
                _ => remote.path.clone(),
            };
            fetch::wrapped(cache, remote, path, &fetched.digest)
        })
        .collect();
    let folder = folder_for(
        chosen.as_deref(),
        parent.as_deref(),
        &listing,
        &parsed,
        &unwrapped(&files, &paths),
    );
    // two links named alike, such as two iris.csv, each get a folder: iris, then iris-2
    let folder = if chosen.is_some() {
        folder
    } else {
        unclaimed(folder, taken)
    };
    tell_fetched(context, &paths, &fetched, &files, elapsed);
    vouch(&folder, &paths, &fetched, sha256, taken)?;
    let mut locked: Vec<LockedFile> = files
        .iter()
        .zip(&paths)
        .zip(&fetched)
        .map(|((remote, path), fetched)| LockedFile {
            path: path.clone(),
            size: fetched.size,
            sha256: fetched.digest,
            md5: remote.md5,
            // what get reports, so a secret in the link stays masked
            url: crate::net::Shown(&remote.url).to_string(),
            unpacks_to: None,
        })
        .collect();
    let unpacked = unpack(context, cache, &mut locked, extract).await?;
    let planned = planned(cache, &locked)?;
    let placed: Vec<String> = planned
        .targets
        .iter()
        .map(|target| target.path.clone())
        .collect();
    check(&folder, Some(&placed))?;
    let outcomes = put_all(context, cache, &folder, &planned.targets)?;
    unpacking::drop_archives(cache, &unpacked, None);
    let folder = std::path::absolute(&folder).unwrap_or(folder);
    let origins: HashMap<&str, Origin> = locked
        .iter()
        .zip(&fetched)
        .map(|(file, fetched)| (file.path.as_str(), fetched.origin))
        .collect();
    let files = planned
        .targets
        .iter()
        .zip(outcomes)
        .map(|(target, outcome)| Got {
            path: target.path.clone(),
            size: target.size,
            sha256: target.sha256,
            url: target.locked.url.clone(),
            origin: origins
                .get(target.locked.path.as_str())
                .copied()
                .unwrap_or(Origin::Cache),
            method: match outcome {
                Outcome::Placed(method) | Outcome::Replaced(method) => Some(method),
                Outcome::AlreadyThere => None,
            },
            unpacked_from: target.locked.unpacks_to.map(|_| target.locked.path.clone()),
        })
        .collect();
    Ok(Done { folder, files })
}

/// the checks between a download and its placing: the hash `--sha256` asks for, and a folder no
/// earlier link of the run took, which then counts as taken.
fn vouch(
    folder: &Path,
    paths: &[String],
    fetched: &[Fetched],
    sha256: Option<Sha256>,
    taken: &mut Taken,
) -> Result<(), Error> {
    if let (Some(expected), [path], [found]) = (sha256, paths, fetched)
        && found.digest != expected
    {
        return Err(Error::new(
            Kind::IntegrityMismatch,
            format!("{path} does not match --sha256"),
        )
        .detail(format!(
            "expected sha256 {}, got {}",
            expected.short(),
            found.digest.short()
        ))
        .help(SHA256_HELP));
    }
    // a folder another link filled could hold files of the same name, which its bytes in the
    // cache would let this link replace
    if !taken.claim(folder) {
        return Err(Error::new(
            Kind::LocalNotEmpty,
            format!(
                "{} already holds the files of another link in this run",
                folder.display()
            ),
        )
        .help("fetch these links in separate runs, each with its own -o folder"));
    }
    Ok(())
}

/// the folders the links of one run filled. windows and macos see `Data` and `data` as one
/// folder, so they are compared as [`name::fold`] folds them, on every system alike. a folder
/// reached through a link or junction is the folder it leads to.
#[derive(Default)]
struct Taken(std::collections::HashSet<String>);

impl Taken {
    fn key(folder: &Path) -> String {
        let whole = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        name::fold(&resolved(&whole).to_string_lossy())
    }

    fn holds(&self, folder: &Path) -> bool {
        self.0.contains(&Self::key(folder))
    }

    /// takes `folder` for one link, or says another link took it already.
    fn claim(&mut self, folder: &Path) -> bool {
        self.0.insert(Self::key(folder))
    }
}

/// `path` with the longest part of it that exists resolved, links and all, and the rest kept.
fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            return missing
                .iter()
                .rev()
                .fold(real, |whole, part| whole.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(part)) => {
                missing.push(part);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `folder`, or the first of `folder-2`, `folder-3` and so on that no earlier link of the run
/// took.
fn unclaimed(folder: PathBuf, taken: &Taken) -> PathBuf {
    if !taken.holds(&folder) {
        return folder;
    }
    let name = folder.file_name().map_or_else(
        || "download".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    (2..=u32::MAX)
        .map(|number| folder.with_file_name(format!("{name}-{number}")))
        .find(|candidate| !taken.holds(candidate))
        .unwrap_or(folder)
}

/// a link as reports show it, a secret in it masked.
fn shown(link: &str) -> String {
    net::mask_query(link.trim()).into_owned()
}

/// says at once that one of several links failed, so the others' story goes on around it.
fn tell_failed(context: &Context, link: &str, error: &Error) {
    context
        .output
        .step("Failed", &format!("{} · {}", shown(link), error.message));
}

/// one error for the links that failed: the first one's kind, and a line naming each link and
/// what happened to it, in the order given. every link keeps its own error in the report.
fn some_failed(links: &[String], failed: &[(usize, Error)]) -> Error {
    let Some((_, first)) = failed.first() else {
        return Error::new(Kind::LocalIo, "a link failed without a reason");
    };
    let shared_help = failed
        .iter()
        .all(|(_, error)| error.help == first.help)
        .then(|| first.help.clone())
        .flatten();
    let mut error = Error::new(
        first.kind,
        format!(
            "{} of {} could not be fetched",
            failed.len(),
            units::count(links.len() as u64, "link", "links")
        ),
    );
    error.failures = failed
        .iter()
        .map(|(index, failure)| FileFailure {
            file: shown(&links[*index]),
            kind: failure.kind.name(),
            message: failure.message.clone(),
            details: failure.details.clone(),
            help: failure.help.clone(),
        })
        .collect();
    for (index, failure) in failed.iter().take(fetch::NAMED) {
        error = error.detail(format!("{}: {}", shown(&links[*index]), failure.message));
        if shared_help.is_none()
            && let Some(help) = &failure.help
        {
            error = error.detail(format!("help: {help}"));
        }
    }
    if failed.len() > fetch::NAMED {
        error = error.detail(format!("and {} more", failed.len() - fetch::NAMED));
    }
    error.help(shared_help.unwrap_or_else(|| {
        "fetched links stay in place and downloaded files stay in the cache. fix the errors above, then run the same command again"
            .to_owned()
    }))
}

fn approve(context: &Context, cache: &Cache, jobs: &[Job]) -> Result<(), Error> {
    if context.offline.enabled() {
        return Ok(());
    }
    let missing: Vec<&Job> = jobs
        .iter()
        .filter(|job| job.fetch.cached(cache).is_none())
        .collect();
    let size: u64 = missing.iter().filter_map(|job| job.fetch.expect.size).sum();
    super::confirm(
        &context.output,
        context.yes,
        &units::count(missing.len() as u64, "file", "files"),
        size,
    )
}

/// the names the files will have, and the paths they will take in the folder when the cache
/// already knows them: a file the server names keeps the name it gave last time, and an archive
/// this machine unpacked before stands for its files. the paths are none while an archive among
/// them was never unpacked here, since only its bytes tell what it holds.
fn foresee(cache: &Cache, files: &[Remote], extract: bool) -> (Vec<String>, Option<Vec<String>>) {
    let alone = files.len() == 1;
    let mut names = Vec::with_capacity(files.len());
    let mut paths = Some(Vec::with_capacity(files.len()));
    for remote in files {
        let record = cache.url_record(remote.url.as_str());
        let given = record
            .as_ref()
            .and_then(|record| record.name.as_deref())
            .and_then(name::sanitize);
        let name = match given {
            Some(given) if remote.named_by_server => given,
            _ => remote.path.clone(),
        };
        // a file its source sent wrapped in a zip is that zip again, so its unpacked files can
        // stand for it
        let name = match known(cache, remote, record.as_ref()) {
            Some((digest, _)) => fetch::wrapped(cache, remote, name, &digest),
            None => name,
        };
        if extract && archive::by_name(&name) == Some(Packed::Archive) {
            let members = known(cache, remote, record.as_ref())
                .and_then(|(digest, _)| cache.unpacked(&digest, None));
            match (members, paths.as_mut()) {
                (Some(members), Some(paths)) => {
                    paths.extend(plan::unpacked_paths(&name, alone, &members));
                }
                _ => paths = None,
            }
        } else if let Some(paths) = paths.as_mut() {
            paths.push(name.clone());
        }
        names.push(name);
    }
    (names, paths)
}

/// the bytes a listed file is known to have before it downloads: by a checksum its source states,
/// or by what its link served last time.
fn known(
    cache: &Cache,
    remote: &Remote,
    record: Option<&UrlRecord>,
) -> Option<(Sha256, Option<u64>)> {
    if let Some(digest) = remote.sha256 {
        return Some((digest, remote.size));
    }
    if let Some((digest, size)) = remote
        .md5
        .and_then(|md5| cache.stated_md5(&md5, remote.url.as_str()))
    {
        return Some((digest, Some(size)));
    }
    record.map(|record| (record.sha256, Some(record.size)))
}

/// hashes the cached objects in doubt that this run would take for `files`, and those an archive
/// among them unpacked to, before anything downloads, so changed bytes are marked for replacement.
fn vet(cache: &Cache, files: &[Remote], names: &[String], extract: bool) -> Result<(), Error> {
    let marks = cache.changed();
    for (remote, name) in files.iter().zip(names) {
        let record = cache.url_record(remote.url.as_str());
        let Some((digest, size)) = known(cache, remote, record.as_ref()) else {
            continue;
        };
        cache
            .trusted(&digest, size, &marks)
            .map_err(|doubt| doubtful(&doubt))?;
        if extract
            && archive::by_name(name) == Some(Packed::Archive)
            && let Some(members) = cache.unpacked(&digest, None)
        {
            for member in &members {
                cache
                    .trusted(&member.sha256, Some(member.size), &marks)
                    .map_err(|doubt| doubtful(&doubt))?;
            }
        }
    }
    Ok(())
}

/// unpacks each archive among the fetched files that this machine has not unpacked before, and
/// returns every archive whose files are now in the cache, so its own bytes can go.
async fn unpack(
    context: &Context,
    cache: &Cache,
    files: &mut [LockedFile],
    extract: bool,
) -> Result<Vec<Sha256>, Error> {
    let mut done = Vec::new();
    let mut slots = Vec::new();
    let mut todo = Vec::new();
    for (index, file) in files.iter_mut().enumerate() {
        match archive::by_name(&file.path) {
            Some(Packed::Archive) if extract => {}
            Some(Packed::Unsupported(format)) if extract => {
                context.output.warn(&format!(
                    "{} stays packed, since fetchloom does not unpack {format}",
                    file.path
                ));
                continue;
            }
            _ => continue,
        }
        if cache.holds_unpacked(&file.sha256, None) {
            file.unpacks_to = cache.unpacked_tree(&file.sha256);
            done.push(file.sha256);
            continue;
        }
        slots.push(index);
        todo.push(Unpack {
            archive: file.sha256,
            size: file.size,
            label: file.path.clone(),
            tree: None,
        });
    }
    let results = unpacking::unpack_all(context, cache, todo).await?;
    for (index, result) in slots.into_iter().zip(results) {
        if let Some(result) = result {
            files[index].unpacks_to = Some(result.tree);
            done.push(files[index].sha256);
        }
    }
    Ok(done)
}

/// where every file lands in the folder, each archive standing for the files it unpacked to.
fn planned<'a>(cache: &Cache, files: &'a [LockedFile]) -> Result<Plan<'a>, Error> {
    let mut planned = Plan::default();
    let members = |file: &LockedFile| cache.unpacked(&file.sha256, file.unpacks_to.as_ref());
    plan::expand(&mut planned, "", "", false, files, &members);
    if let Some((_, file)) = planned.unknown.first() {
        return Err(Error::new(
            Kind::LocalIo,
            format!("the cache lost what {} unpacked to", file.path),
        )
        .help("run the same command again to unpack it"));
    }
    plan::check(&planned.targets, Patterns::OnCommandLine)?;
    Ok(planned)
}

/// the step after a download for a person at a terminal: pinning the same link in a project.
fn tell_next(context: &Context, args: &Get, link: &str) {
    if let Some(command) = pin_command(args, link) {
        context.output.next(&format!(
            "to pin this dataset in a project, run `{command}` there"
        ));
    }
}

fn pin_command(args: &Get, link: &str) -> Option<String> {
    // add refuses links holding secrets, and no hint may repeat them.
    let Cow::Borrowed(link) = net::mask_query(link.trim()) else {
        return None;
    };
    let mut parts = vec![
        "fetchloom add".to_owned(),
        super::shell_word(link).into_owned(),
    ];
    for (flag, patterns) in [
        ("--select", &args.filter.select),
        ("--exclude", &args.filter.exclude),
    ] {
        for pattern in patterns {
            parts.push(flag.to_owned());
            parts.push(super::shell_word(pattern).into_owned());
        }
    }
    if args.no_extract {
        parts.push("--no-extract".to_owned());
    }
    Some(parts.join(" "))
}

/// the files `--select` and `--exclude` keep, or the one file a pasted link names.
fn selected(args: &Get, parsed: &Parsed, listing: &Listing) -> Result<Vec<Remote>, Error> {
    let select = if args.filter.select.is_empty() {
        parsed
            .file
            .iter()
            .map(|file| crate::glob::escape(file))
            .collect()
    } else {
        args.filter.select.clone()
    };
    if select.is_empty() && args.filter.exclude.is_empty() && listing.unavailable.is_empty() {
        return Ok(listing.files.clone());
    }
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "get",
        "get",
        &select,
        &args.filter.exclude,
        true,
    )?)?;
    let mut entry = alone.datasets["get"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.shown(), &entry, listing)
}

/// the names to call a folder after: a file its source wrapped in a zip by its listed name, so
/// the folder is the same whether or not kaggle wrapped it.
fn unwrapped(files: &[Remote], names: &[String]) -> Vec<String> {
    files
        .iter()
        .zip(names)
        .map(|(remote, name)| {
            if crate::source::may_wrap(&remote.url) {
                remote.path.clone()
            } else {
                name.clone()
            }
        })
        .collect()
}

/// where the files go: the folder asked for, else one named after the file or the dataset,
/// inside the folder asked for when there are several links.
fn folder_for(
    chosen: Option<&Path>,
    parent: Option<&Path>,
    listing: &Listing,
    parsed: &Parsed,
    paths: &[String],
) -> PathBuf {
    if let Some(chosen) = chosen {
        return chosen.to_path_buf();
    }
    let name = match paths {
        [only] => name::folder_for(only.rsplit('/').next().unwrap_or(only)),
        _ => listing
            .title
            .as_deref()
            .and_then(name::title_name)
            .or_else(|| name::reference_name(&parsed.reference.to_string()))
            .unwrap_or_else(|| "download".to_owned()),
    };
    // a folder named con or aux cannot exist on windows
    let name = if name::is_reserved_on_windows(&name) {
        format!("{name}-data")
    } else {
        name
    };
    parent.map_or_else(|| PathBuf::from(&name), |parent| parent.join(&name))
}

fn tell_fetched(
    context: &Context,
    paths: &[String],
    fetched: &[Fetched],
    files: &[Remote],
    elapsed: Duration,
) {
    let output = &context.output;
    if let ([path], [fetched], [remote]) = (paths, fetched, files) {
        match fetched.origin {
            Origin::Network => {
                let mut text = format!(
                    "{path} · {} in {}",
                    units::size(fetched.size),
                    units::duration(elapsed)
                );
                if elapsed >= Duration::from_secs(1) {
                    text.push_str(" · ");
                    text.push_str(&units::rate(fetched.size, elapsed));
                }
                output.step("Downloaded", &text);
            }
            Origin::NotModified => output.step(
                "Checked",
                &format!("{path} · unchanged on {}", net::host(&remote.url)),
            ),
            Origin::Cache => output.step("Found", &format!("{path} in the cache")),
        }
        output.detail(&format!("sha256 {}", fetched.digest));
        return;
    }
    let downloaded: Vec<&Fetched> = fetched
        .iter()
        .filter(|fetched| fetched.origin == Origin::Network)
        .collect();
    if downloaded.is_empty() {
        output.step(
            "Found",
            &format!(
                "{} · in the cache or unchanged",
                units::count(fetched.len() as u64, "file", "files")
            ),
        );
        return;
    }
    let bytes: u64 = downloaded.iter().map(|fetched| fetched.size).sum();
    let mut text = format!(
        "{} · {} in {}",
        units::count(downloaded.len() as u64, "file", "files"),
        units::size(bytes),
        units::duration(elapsed)
    );
    if elapsed >= Duration::from_secs(1) {
        text.push_str(" · ");
        text.push_str(&units::rate(bytes, elapsed));
    }
    output.step("Downloaded", &text);
}

#[cfg(test)]
mod drive_docs_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Filter;

    #[test]
    fn folders_differing_only_in_case_are_one_folder_for_the_run() {
        let mut taken = Taken::default();
        assert!(taken.claim(Path::new("data")));
        assert!(
            !taken.claim(Path::new("DATA")),
            "windows and macos see one folder"
        );
        assert_eq!(
            unclaimed(PathBuf::from("Data"), &taken),
            PathBuf::from("Data-2")
        );
        assert_eq!(
            unclaimed(PathBuf::from("other"), &taken),
            PathBuf::from("other")
        );
    }

    #[test]
    fn a_dataset_named_like_a_windows_device_gets_a_usable_folder() {
        let parsed = Reference::parse("s3://bucket/aux/").unwrap();
        let listing = Listing {
            resolved: parsed.reference.clone(),
            title: Some("Aux".to_owned()),
            license: None,
            doi: None,
            files: Vec::new(),
            unavailable: Vec::new(),
        };
        let paths = ["aux/a.csv".to_owned(), "aux/b.csv".to_owned()];
        assert_eq!(
            folder_for(None, None, &listing, &parsed, &paths),
            PathBuf::from("aux-data")
        );
    }

    #[test]
    fn a_link_is_shown_without_its_login_or_secret_query() {
        assert_eq!(
            shown(" https://ada:pw@example.org/a.csv?token=t&v=1 "),
            "https://***@example.org/a.csv?token=***&v=1"
        );
        assert_eq!(shown("zenodo:1"), "zenodo:1");
    }

    fn approval_context() -> Context {
        Context::new(crate::output::Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            crate::output::Level::Quiet,
            true,
            false,
        ))
    }

    #[test]
    fn get_approval_excludes_cached_archives_and_offline_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let digest = crate::digest::sha256(b"a previously unpacked archive");
        cache.record_unpacked(&digest, &[]).unwrap();
        let remote = Remote {
            path: "large.zip".to_owned(),
            size: Some(super::super::ASK_ABOVE + 1),
            sha256: Some(digest),
            md5: None,
            url: url::Url::parse("https://example.org/large.zip").unwrap(),
            named_by_server: false,
        };
        let mut context = approval_context();
        let mut jobs = vec![fetch::listed_job(&remote, &remote.path, true)];
        approve(&context, &cache, &jobs).unwrap();
        jobs[0].fetch.unpack = false;
        assert_eq!(
            approve(&context, &cache, &jobs).unwrap_err().kind,
            Kind::Unconfirmed
        );
        context.offline = net::Offline::Flag;
        approve(&context, &cache, &jobs).unwrap();
    }

    #[test]
    fn get_approval_counts_only_missing_files_toward_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let digest = crate::digest::sha256(b"cached archive");
        cache.record_unpacked(&digest, &[]).unwrap();
        let mut remote = Remote {
            path: "cached.zip".to_owned(),
            size: Some(super::super::ASK_ABOVE),
            sha256: Some(digest),
            md5: None,
            url: url::Url::parse("https://example.org/cached.zip").unwrap(),
            named_by_server: false,
        };
        let mut jobs = vec![fetch::listed_job(&remote, &remote.path, true)];
        remote.path = "missing.bin".to_owned();
        remote.sha256 = Some(crate::digest::sha256(b"missing"));
        jobs.push(fetch::listed_job(&remote, &remote.path, false));
        let context = approval_context();
        approve(&context, &cache, &jobs).unwrap();
        jobs[1].fetch.expect.size = Some(super::super::ASK_ABOVE + 1);
        assert_eq!(
            approve(&context, &cache, &jobs).unwrap_err().kind,
            Kind::Unconfirmed
        );
    }

    #[test]
    fn the_pin_hint_keeps_filters_and_extraction_settings() {
        let args = Get {
            links: vec!["https://example.org/data.zip".to_owned()],
            output: Some(PathBuf::from("chosen")),
            input_file: None,
            sha256: None,
            filter: Filter {
                select: vec!["*.zip".to_owned(), "*.csv".to_owned()],
                exclude: vec!["old/*".to_owned()],
            },
            no_extract: true,
        };
        assert_eq!(
            pin_command(&args, "https://example.org/data.zip").as_deref(),
            Some(
                "fetchloom add https://example.org/data.zip --select \"*.zip\" --select \"*.csv\" --exclude \"old/*\" --no-extract"
            )
        );
        assert_eq!(
            pin_command(&args, "https://example.org/data.zip?token=secret"),
            None
        );
    }
}
