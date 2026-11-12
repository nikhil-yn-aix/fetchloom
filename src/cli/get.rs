//! `fetchloom get <link>...`: datasets or files into new folders, once, through the same cache as
//! projects. archives unpack into the folder unless `--no-extract` keeps them packed. a second run
//! asks the server whether anything changed and moves no bytes when nothing did. several links,
//! given on the command line or listed in `--input-file`, download together, and one that fails
//! leaves the others to finish. `-o` writes the one file a link resolves to instead.

mod file;
mod folder;
mod remembered;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::task::JoinSet;

use self::folder::{Outcome, check, put_all};
use super::args::Get;
use super::fetch::{self, Job};
use super::sync as engine;
use super::sync::place::doubtful;
use super::sync::unpack::{self as unpacking, Unpack};
use super::{Context, links, open_cache};
use crate::archive::{self, Packed, System};
use crate::cache::{Cache, UrlRecord};
use crate::digest::{Block, Sha256};
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
    /// the file `-o` names, or none when get filled folders.
    file: Option<PathBuf>,
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
    method: Option<Written>,
    /// the archive the file was unpacked from, by its path in the download.
    unpacked_from: Option<String>,
}

/// how get wrote a file: as a folder is filled, or, for the file `-o` writes from a download
/// nothing else needs, by moving the cache's copy there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Written {
    Reflink,
    Hardlink,
    Copy,
    Move,
}

impl From<Method> for Written {
    fn from(method: Method) -> Written {
        match method {
            Method::Reflink => Written::Reflink,
            Method::Hardlink => Written::Hardlink,
            Method::Copy => Written::Copy,
        }
    }
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
                help: error.help.as_deref().map(str::to_owned),
                failures: error.failures.to_vec(),
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
    /// the folder `--dir` names for the one link.
    chosen: Option<PathBuf>,
    /// the folder `--dir` names for several links, each in a folder of its own inside it.
    parent: Option<PathBuf>,
    extract: bool,
    /// the hash `--sha256` or the link's line asks the one file to have.
    expected: Option<Expected>,
    /// what the link is remembered by, when only its source can list it.
    key: Option<String>,
}

pub(super) async fn run(context: &Context, args: Get) -> Result<Report, Error> {
    run_links(context, args)
        .await
        .map_err(|error| unprojected(context.offline, error))
}

async fn run_links(context: &Context, args: Get) -> Result<Report, Error> {
    if let Some(path) = &args.output {
        file::usage(path, &args.links, args.input_file.is_some())?;
    }
    for path in args.output.iter().chain(&args.dir) {
        warn_tilde(context, path);
    }
    if context.mirrors.mirrors.only {
        return Err(super::fetch::mirrors::refused(
            context,
            "get fetches each link from its source",
        ));
    }
    let mut asked = links(context, &args).await?;
    if let Some(sha256) = args.sha256 {
        let [one] = asked.as_mut_slice() else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "--sha256 checks one file, and this get has {}",
                    units::count(asked.len() as u64, "link", "links")
                ),
            )
            .help("run one `fetchloom get --sha256` for each file to check"));
        };
        if let Some(listed) = &one.expected
            && listed.sha256 != sha256
        {
            return Err(Error::new(
                Kind::Usage,
                format!("--sha256 and {} give two SHA-256s", listed.named()),
            )
            .help("leave out --sha256, or give the hash the file has"));
        }
        one.expected.get_or_insert(Expected::flag(sha256));
    }
    let net = Arc::new(context.net());
    if let [one] = asked.as_slice()
        && args.input_file.is_none()
    {
        return one_link(context, &net, &args, one.clone()).await;
    }
    many(context, &net, &args, &asked).await
}

/// `error` in the words of a run that keeps no project: get was given its link on the command
/// line or in a list and pins nothing, so no help names data.toml or a pin. `offline` says what
/// turned offline mode on.
fn unprojected(offline: net::Offline, mut error: Error) -> Error {
    let reworded = |help: &str| -> Option<String> {
        if help == net::MOVED_HELP {
            Some("check the link in a browser. if it moved, run get with the new link".to_owned())
        } else if help == crate::source::UNKNOWN_HELP {
            Some("check the id in a browser, then run get with the right one. a dataset that was removed cannot be fetched".to_owned())
        } else if help == offline.unlearned_help() {
            Some(format!(
                "{} to download it once. after that, `fetchloom get <link> --offline` reuses it",
                offline.retry()
            ))
        } else {
            None
        }
    };
    if let Some(help) = error.help.as_deref().and_then(reworded) {
        error = error.help(help);
    }
    for failure in &mut error.failures {
        if let Some(help) = failure.help.as_deref().and_then(reworded) {
            failure.help = Some(help);
        }
    }
    error
}

/// a link to fetch, and the SHA-256 its one file must have when one was given.
#[derive(Clone)]
struct Asked {
    link: String,
    expected: Option<Expected>,
}

/// the SHA-256 a file must have, and where it was given.
#[derive(Clone)]
struct Expected {
    sha256: Sha256,
    /// the line of a list that gave it, as `line 2 of links.txt`, or none for `--sha256`.
    line: Option<String>,
}

impl Expected {
    /// the hash `--sha256` gives.
    fn flag(sha256: Sha256) -> Expected {
        Expected { sha256, line: None }
    }

    /// how messages name the hash: `--sha256`, or the SHA-256 on a line of a list.
    fn named(&self) -> String {
        match &self.line {
            None => "--sha256".to_owned(),
            Some(line) => format!("the SHA-256 on {line}"),
        }
    }

    /// what to do about bytes other than the hash names.
    fn help(&self) -> String {
        let given = match &self.line {
            None => "the hash given to --sha256".to_owned(),
            Some(line) => format!("the hash on {line}"),
        };
        format!("check {given}, or ask the file's owner which bytes are right")
    }
}

/// the links to fetch: those on the command line, then those `--input-file` lists, each with the
/// SHA-256 its line gives. several links download together, so each names what it fetches, and
/// only a link alone may be search words.
async fn links(context: &Context, args: &Get) -> Result<Vec<Asked>, Error> {
    context.stop.check()?;
    let links = &args.links;
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
            "quote the words of one search, as in `fetchloom get \"{example}\"`, or get one dataset at a time"
        )));
    }
    let mut asked: Vec<Asked> = links
        .iter()
        .map(|link| Asked {
            link: link.clone(),
            expected: None,
        })
        .collect();
    let Some(path) = &args.input_file else {
        return Ok(asked);
    };
    let (name, text) = links::read(path, &context.stop).await?;
    let listed = links::checked(&name, links::parse(&name, &text)?, false)?;
    asked.extend(listed.into_iter().map(|(line, _)| Asked {
        expected: line.sha256.map(|sha256| Expected {
            sha256,
            line: Some(line.named(&name)),
        }),
        link: line.link,
    }));
    Ok(asked)
}

/// warns of a path that starts with a folder named `~`, as a quoted `~` reaches fetchloom: it
/// reads as the home folder, and get makes a folder of that name here.
fn warn_tilde(context: &Context, path: &Path) {
    let mut parts = path.components();
    if parts.next() != Some(std::path::Component::Normal(std::ffi::OsStr::new("~"))) {
        return;
    }
    let shown = path.display();
    let home = std::env::home_dir().map_or_else(
        || "the whole path of your home folder".to_owned(),
        |home| home.join(parts.as_path()).display().to_string(),
    );
    context.output.warn_with(
        &format!(
            "{shown} starts with a folder named ~ here, not your home folder, since a shell turns ~ into it only outside quotes"
        ),
        &format!("write {home}, or ./{shown} to keep a folder named ~"),
    );
}

/// a link or search words alone: into the file `-o` names, the folder `--dir` names, or one named
/// after the download.
async fn one_link(
    context: &Context,
    net: &Arc<Net>,
    args: &Get,
    asked: Asked,
) -> Result<Report, Error> {
    let mut link = asked.link;
    super::search::refuse_local(&link, false)?;
    if is_search(&link) {
        link = super::search::pick(context, net.as_ref(), &link, "get").await?;
    }
    let parsed = Reference::parse(&link)?;
    engine::tell_raw(&context.output, &link, &parsed.reference);
    let cache = open_cache()?;
    let key = remembered::key(args, &parsed);
    // offline, what the link kept last time stands in for a listing only its source can give
    let offline = key.as_deref().filter(|_| context.offline.enabled());
    if let (Some(path), Some(key)) = (&args.output, offline)
        && let Some((file, done)) =
            file::again_offline(context, net, &cache, args, path, &parsed, key).await?
    {
        return Ok(wrote(context, args, &link, file, &done));
    }
    let remembered = offline
        .and_then(|key| cache.url_record(key))
        .and_then(|kept| {
            remembered::listing(&parsed, &kept, asked.expected.as_ref().map(|e| e.sha256))
        });
    let (listing, files) = if let Some(listing) = remembered {
        let files = selected(args, &parsed, &listing)?;
        (listing, files)
    } else {
        net.trust(parsed.reference.written_origin());
        let folders = crate::glob::folders_of(&wanted_patterns(args, &parsed));
        let listing = engine::resolve(&context.output, &parsed.reference, net, folders.as_deref())
            .await
            .map_err(|error| unlisted_offline(context.offline, error))?;
        let files = told(context, args, &parsed, &listing)?;
        (listing, files)
    };
    if let Some(path) = &args.output {
        let (file, done) = file::one(
            context,
            net,
            &cache,
            args,
            path,
            (&parsed, &listing, files),
            key.as_deref(),
        )
        .await?;
        return Ok(wrote(context, args, &link, file, &done));
    }
    let mut wanted = prepare(
        &cache,
        args,
        (args.dir.as_deref(), None),
        (parsed, listing, files),
        asked.expected,
    )?;
    hint_shards(context, &wanted);
    approve(context, &cache, &wanted.jobs)?;
    let started = Instant::now();
    let jobs = std::mem::take(&mut wanted.jobs);
    // nothing is placed before every file is in, so a download cancelled at its question
    // changed nothing
    let fetched = fetch::all(context, net, &cache, jobs)
        .await
        .map_err(Error::unchanged)?;
    let done = finish(
        context,
        &cache,
        wanted,
        fetched,
        started,
        (&mut Taken::default(), true),
    )
    .await?;
    context.output.answer(&done.folder.display().to_string());
    tell_next(context, args, &link);
    Ok(Report {
        file: None,
        folder: Some(done.folder.clone()),
        files: done.files.clone(),
        items: vec![Item::done(&link, &done)],
    })
}

/// tells the file `-o` wrote and what to do next, and reports it.
fn wrote(context: &Context, args: &Get, link: &str, file: PathBuf, done: &Done) -> Report {
    context.output.answer(&file.display().to_string());
    tell_next(context, args, link);
    Report {
        file: Some(file),
        folder: Some(done.folder.clone()),
        files: done.files.clone(),
        items: vec![Item::done(link, done)],
    }
}

/// a source's refusal to list a link offline, with what works offline for get, which keeps no
/// project. `offline` says what turned offline mode on, so the help says how to turn it off.
fn unlisted_offline(offline: net::Offline, error: Error) -> Error {
    if error.kind != Kind::OfflineMissing {
        return error;
    }
    error.help(format!(
        "{}. after that, a link that keeps one file works offline too",
        offline.retry()
    ))
}

/// several links in one run: resolved together, their files downloaded together within the
/// network's limits, then each placed in a folder of its own. a link that fails is kept in the
/// report while the others go on, told at once when it fails before the downloads, and named in
/// the run's one error.
async fn many(
    context: &Context,
    net: &Arc<Net>,
    args: &Get,
    asked: &[Asked],
) -> Result<Report, Error> {
    let cache = open_cache()?;
    let links: Vec<String> = asked.iter().map(|asked| asked.link.clone()).collect();
    let links = links.as_slice();
    let resolved = resolve_all(context, net, &cache, args, links).await?;
    let mut wanted = Vec::with_capacity(links.len());
    for ((link, asked), resolved) in links.iter().zip(asked).zip(resolved) {
        let prepared = resolved.and_then(|found| {
            prepare(
                &cache,
                args,
                (None, args.dir.as_deref()),
                found,
                asked.expected.clone(),
            )
        });
        match &prepared {
            Ok(wanted) => hint_shards(context, wanted),
            Err(error) => tell_failed(context, link, error),
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
        let fetched_all = fetch::apart(context, net, &cache, jobs)
            .await
            .map_err(Error::unchanged)?;
        for (owner, result) in owners.into_iter().zip(fetched_all) {
            fetched[owner].push(result);
        }
    }
    let mut items = Vec::with_capacity(links.len());
    let mut failed = Vec::new();
    let mut taken = Taken::default();
    for (index, ((link, wanted), fetched)) in links.iter().zip(wanted).zip(fetched).enumerate() {
        let wanted = match wanted {
            Ok(wanted) => wanted,
            Err(error) => {
                let error = unprojected(context.offline, error);
                items.push(Item::failed(link, &error));
                failed.push((index, error));
                continue;
            }
        };
        let last = index + 1 == links.len();
        match settle(
            context,
            &cache,
            wanted,
            fetched,
            started,
            (&mut taken, last),
        )
        .await
        {
            Ok(done) => {
                context.output.answer(&done.folder.display().to_string());
                items.push(Item::done(link, &done));
            }
            // the run's error, told next, names it
            Err(error) => {
                let error = unprojected(context.offline, error);
                items.push(Item::failed(link, &error));
                failed.push((index, error));
            }
        }
    }
    let placed: usize = items.iter().map(|item| item.files.len()).sum();
    let report = Report {
        file: None,
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
    cache: &Cache,
    args: &Get,
    links: &[String],
) -> Result<Vec<Result<Resolved, Error>>, Error> {
    let mut results: Vec<Option<Result<Resolved, Error>>> = links.iter().map(|_| None).collect();
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
        // offline, what the link kept last time stands in for a listing only its source can give
        if let Some(listing) = remembered::key(args, &parsed)
            .filter(|_| context.offline.enabled())
            .and_then(|key| cache.url_record(&key))
            .and_then(|kept| remembered::listing(&parsed, &kept, None))
        {
            let files = selected(args, &parsed, &listing);
            results[index] = Some(files.map(|files| (parsed, listing, files)));
            continue;
        }
        // a list comes from a file anyone could have written, as data.toml does
        if index < args.links.len() {
            net.trust(parsed.reference.written_origin());
        }
        if !engine::resolves_locally(&parsed.reference) {
            asked.push(parsed.reference.shown());
        }
        let net = net.clone();
        let folders = crate::glob::folders_of(&wanted_patterns(args, &parsed));
        let offline = context.offline;
        tasks.spawn(async move {
            let listing = parsed
                .reference
                .resolve_in(&*net, folders.as_deref())
                .await
                .map_err(|error| unlisted_offline(offline, error));
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
        let resolved = match resolved {
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => Err(error),
            Ok((parsed, listing)) => {
                told(context, args, &parsed, &listing).map(|files| (parsed, listing, files))
            }
        };
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

/// the files a resolved link keeps and the downloads they need. a lone link checks its known
/// destination before downloading; batches check once final names and suffixes are known.
fn prepare(
    cache: &Cache,
    args: &Get,
    (chosen, parent): (Option<&Path>, Option<&Path>),
    (parsed, listing, files): Resolved,
    expected: Option<Expected>,
) -> Result<Wanted, Error> {
    if let Some(expected) = &expected {
        one_file(&files, expected, &parsed)?;
    }
    let extract = !args.no_extract;
    let (names, foreseen) = foresee(cache, &files, extract);
    // a batch's final folder can gain a suffix or a server-provided name after downloading.
    let lone = (args.input_file.is_none() && args.links.len() == 1).then(|| {
        folder_for(
            chosen,
            parent,
            &listing,
            &parsed,
            &unwrapped(&files, &names),
        )
    });
    if let Some(lone) = &lone {
        check(lone, foreseen.as_deref())?;
    }
    let jobs: Vec<Job> = files
        .iter()
        .zip(&names)
        .map(|(remote, name)| {
            let mut job = fetch::listed_job(remote, name, extract);
            let placed = lone
                .as_deref()
                .filter(|_| !job.fetch.unpack)
                .map(|lone| folder::within(lone, name));
            job.fetch.held = held(cache, &job, placed.as_deref());
            job
        })
        .collect();
    vet(cache, &files, &names, extract)?;
    Ok(Wanted {
        key: remembered::key(args, &parsed),
        parsed,
        listing,
        files,
        jobs,
        chosen: chosen.map(Path::to_path_buf),
        parent: parent.map(Path::to_path_buf),
        extract,
        expected,
    })
}

/// the bytes a download that asks whether its link changed may find unchanged: what the link
/// served last time, while the cache holds them, or the file at `placed`, where the download
/// would go.
fn held(cache: &Cache, job: &Job, placed: Option<&Path>) -> Option<Sha256> {
    if !job.fetch.revalidate {
        return None;
    }
    let record = cache.url_record(job.fetch.url.as_str())?;
    let there = || {
        placed.is_some_and(|path| {
            std::fs::metadata(path).is_ok_and(|found| found.is_file() && found.len() == record.size)
                && Sha256::of_file(path, Block::sized(record.size.saturating_add(1)))
                    .is_ok_and(|digest| digest == record.sha256)
        })
    };
    (job.fetch.remembered(cache).is_some() || there()).then_some(record.sha256)
}

/// hints `--no-extract` before anything downloads when the files a link keeps are numbered tar
/// shards that would unpack.
fn hint_shards(context: &Context, wanted: &Wanted) {
    if wanted.extract
        && context.output.shows_hints()
        && let Some(shards) = archive::shards(wanted.files.iter().map(|file| file.path.as_str()))
    {
        context
            .output
            .hint(&shards.hint(&wanted.parsed.reference.shown(), "--no-extract", None));
    }
}

/// refuses a hash for anything but one file, and a file whose source states another hash,
/// before it downloads.
fn one_file(files: &[Remote], expected: &Expected, parsed: &Parsed) -> Result<(), Error> {
    let [file] = files else {
        let help = match &expected.line {
            None => "keep one file with --select, or leave out --sha256".to_owned(),
            Some(line) => format!("list a link to one file on {line}, or leave its SHA-256 off"),
        };
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{} checks one file, and {} has {}",
                expected.named(),
                parsed.reference.shown(),
                units::count(files.len() as u64, "file", "files")
            ),
        )
        .help(help));
    };
    match file.sha256 {
        Some(stated) if stated != expected.sha256 => Err(Error::new(
            Kind::IntegrityMismatch,
            format!("{} is not the file {} names", file.path, expected.named()),
        )
        .detail(format!(
            "expected sha256 {}, the source states {}",
            expected.sha256.hex(),
            stated.hex()
        ))
        .help(expected.help())),
        _ => Ok(()),
    }
}

/// the downloads of one of several links put in place, or every one of its files that failed.
async fn settle(
    context: &Context,
    cache: &Cache,
    wanted: Wanted,
    fetched: Vec<Result<Fetched, Error>>,
    started: Instant,
    taken: (&mut Taken, bool),
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
    finish(context, cache, wanted, found, started, taken).await
}

/// checks what arrived, unpacks archives and puts every file in the link's folder, unless an
/// earlier link of the run took that folder. `last` says this is the run's last link, whose
/// placement marks the run's work as finishing.
async fn finish(
    context: &Context,
    cache: &Cache,
    wanted: Wanted,
    fetched: Vec<Fetched>,
    started: Instant,
    (taken, last): (&mut Taken, bool),
) -> Result<Done, Error> {
    let Wanted {
        parsed,
        listing,
        files,
        chosen,
        parent,
        extract,
        expected,
        key,
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
    if chosen.is_none() {
        folder::check_generated_root(&folder)?;
    }
    tell_fetched(context, &paths, &fetched, &files, started);
    vouch(&folder, &paths, &fetched, expected.as_ref(), taken)?;
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
    let unpacked = unpack(context, cache, &mut locked, &folder, extract).await?;
    let planned = planned(cache, &locked, unpacked.trees)?;
    let placed: Vec<String> = planned
        .targets
        .iter()
        .map(|target| target.path.clone())
        .collect();
    check(&folder, Some(&placed))?;
    let named = (chosen.is_some(), last);
    let outcomes = put_all(context, cache, &folder, &planned.targets, named)?;
    unpacking::drop_archives(cache, &unpacked.archives, None);
    if let (Some(key), [remote], [fetched]) = (&key, files.as_slice(), fetched.as_slice()) {
        remembered::keep(cache, key, &remote.path, fetched.digest, fetched.size);
    }
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
        .map(|(target, outcome)| {
            // get places downloads alone, never a recipe's result
            let file = target.file();
            Got {
                path: target.path.clone(),
                size: target.size,
                sha256: target.sha256,
                url: file.map(|file| file.url.clone()).unwrap_or_default(),
                origin: file
                    .and_then(|file| origins.get(file.path.as_str()))
                    .copied()
                    .unwrap_or(Origin::Cache),
                method: match outcome {
                    Outcome::Placed(method) | Outcome::Replaced(method) => Some(method.into()),
                    Outcome::AlreadyThere => None,
                },
                unpacked_from: file
                    .filter(|file| file.unpacks_to.is_some())
                    .map(|file| file.path.clone()),
            }
        })
        .collect();
    Ok(Done { folder, files })
}

/// the checks between a download and its placing: the hash `--sha256` or the link's line asks
/// for, and a folder no earlier link of the run took, which then counts as taken.
fn vouch(
    folder: &Path,
    paths: &[String],
    fetched: &[Fetched],
    expected: Option<&Expected>,
    taken: &mut Taken,
) -> Result<(), Error> {
    if let ([path], [found], Some(expected)) = (paths, fetched, expected) {
        matches_sha256(path, &found.digest, expected)?;
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
        .help("fetch these links in separate runs, each with its own --dir folder"));
    }
    Ok(())
}

/// refuses the bytes of the file at `path` when the hash `expected` names others.
fn matches_sha256(path: &str, digest: &Sha256, expected: &Expected) -> Result<(), Error> {
    if expected.sha256 == *digest {
        return Ok(());
    }
    Err(Error::new(
        Kind::IntegrityMismatch,
        format!("{path} does not match {}", expected.named()),
    )
    .detail(format!(
        "expected sha256 {}, got {}",
        expected.sha256.hex(),
        digest.hex()
    ))
    .help(expected.help()))
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

/// says at once that one of several links failed before the downloads, so the others' story
/// goes on around it.
fn tell_failed(context: &Context, link: &str, error: &Error) {
    context
        .output
        .step("Failed", &format!("{} · {}", shown(link), error.message));
}

/// one error for the links that failed: the first one's kind, and lines that name each link
/// once with what happened to it, in the order given, links that failed alike under one line.
/// every link keeps its own error in the report.
fn some_failed(links: &[String], failed: &[(usize, Error)]) -> Error {
    let Some((_, first)) = failed.first() else {
        return Error::new(Kind::LocalIo, "a link failed without a reason");
    };
    let shared_help = failed
        .iter()
        .all(|(_, error)| error.help == first.help)
        .then(|| first.help.clone())
        .flatten();
    let failed_links = format!(
        "{} of {} could not be fetched",
        units::grouped(failed.len() as u64),
        units::count(links.len() as u64, "link", "links")
    );
    // a stop says first that it stopped, as every stop does
    let message = if first.kind == Kind::Interrupted {
        format!("stopped, and {failed_links}")
    } else {
        failed_links
    };
    let mut error = Error::new(first.kind, message);
    error.failures = failed
        .iter()
        .map(|(index, failure)| FileFailure {
            file: shown(&links[*index]),
            kind: failure.kind.name(),
            message: failure.message.clone(),
            details: failure.details.clone(),
            help: failure.help.as_deref().map(str::to_owned),
        })
        .collect();
    let told: Vec<fetch::Told<'_>> = failed
        .iter()
        .map(|(index, failure)| {
            let label = shown(&links[*index]);
            fetch::Told {
                alone: if failure.message.contains(&label) {
                    failure.message.clone()
                } else {
                    format!("{label}: {}", failure.message)
                },
                label,
                error: failure,
            }
        })
        .collect();
    error.details = fetch::alike(&told, shared_help.is_some());
    error.help(shared_help.unwrap_or_else(|| {
        "fetched links stay in place and downloaded files stay in the cache. fix the errors above, then run the same command again"
            .into()
    }))
}

/// asks before the run's downloads when they are large. nothing is placed yet, so a cancel there
/// changed nothing. a download that may find its bytes unchanged asks only once its server sends
/// them, so an unchanged rerun asks nothing.
fn approve(context: &Context, cache: &Cache, jobs: &[Job]) -> Result<(), Error> {
    if context.offline.enabled() {
        return Ok(());
    }
    let missing: Vec<&Job> = jobs
        .iter()
        .filter(|job| job.fetch.held.is_none() && job.fetch.cached(cache).is_none())
        .collect();
    crate::space::cache_holds(
        cache.root(),
        missing
            .iter()
            .filter_map(|job| Some((job.label.as_str(), job.fetch.expect.size?))),
    )
    .map_err(Error::unchanged)?;
    let (size, room) = needs(&missing);
    let what = units::count(missing.len() as u64, "file", "files");
    let doing = if room > size {
        format!("downloading and unpacking {what}")
    } else {
        format!("downloading {what}")
    };
    super::confirm_doing(
        &context.output,
        context.yes,
        context.settings.ask_above.value,
        &what,
        size,
        Some((cache.root(), room)),
        &doing,
    )
    .map(|_| ())
    .map_err(Error::unchanged)
}

/// what the downloads `missing` need: the bytes they move, which the question about their size
/// counts, and the room on the cache's disk, which counts each archive they unpack once more,
/// since its files land beside it before it can go. the room is a lower bound: an archive may
/// unpack to more than its own size, and a size nothing stated counts nothing.
fn needs(missing: &[&Job]) -> (u64, u64) {
    let sized = || {
        missing
            .iter()
            .filter_map(|job| Some((job.fetch.expect.size?, job.fetch.unpack)))
    };
    let moved = sized().map(|(size, _)| size).fold(0, u64::saturating_add);
    let unpacked = sized()
        .filter(|(_, unpack)| *unpack)
        .map(|(size, _)| size)
        .fold(0, u64::saturating_add);
    (moved, moved.saturating_add(unpacked))
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
    let marks = cache.doubts()?;
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

type Trees = HashMap<(String, Sha256, Sha256), Vec<crate::cache::Member>>;

#[derive(Default)]
struct UnpackedFiles {
    archives: Vec<Sha256>,
    trees: Trees,
}

/// unpacks each archive among the fetched files that this machine has not unpacked before, and
/// returns every archive whose files are now in the cache, so its own bytes can go. an archive
/// fetchloom leaves packed is told with how to unpack it from `folder`, where it lands.
async fn unpack(
    context: &Context,
    cache: &Cache,
    files: &mut [LockedFile],
    folder: &Path,
    extract: bool,
) -> Result<UnpackedFiles, Error> {
    let mut done = UnpackedFiles::default();
    let mut slots = Vec::new();
    let mut todo = Vec::new();
    let tell_packed = |path: &str, said: String| {
        let placed = format!("{}/{path}", folder.display());
        context.output.warn_with(
            &said,
            &archive::unpack_elsewhere(System::RUNNING, &placed, "", "--no-extract"),
        );
    };
    for (index, file) in files.iter_mut().enumerate() {
        match archive::by_name(&file.path) {
            Some(Packed::Archive) if extract => {}
            Some(Packed::Unsupported(format)) if extract => {
                let said = format!(
                    "{} stays packed, since fetchloom does not unpack {format}",
                    file.path
                );
                tell_packed(&file.path, said);
                continue;
            }
            _ => continue,
        }
        if let Some((tree, members)) = cache.unpacked_with_tree(&file.sha256)
            && members
                .iter()
                .all(|member| cache.find(&member.sha256, Some(member.size)).is_some())
        {
            file.unpacks_to = Some(tree);
            done.archives.push(file.sha256);
            done.trees
                .insert((file.path.clone(), file.sha256, tree), members);
            continue;
        }
        slots.push(index);
        todo.push(Unpack::whole(
            file.sha256,
            file.size,
            file.path.clone(),
            None,
        ));
    }
    let results = unpacking::unpack_all(context, cache, todo).await?;
    for (index, result) in slots.into_iter().zip(results) {
        let file = &files[index];
        if result.is_none()
            && let Some(format) = archive::unsupported(cache, &file.sha256, file.size)
        {
            let said = format!(
                "{} stays packed: its bytes are a {format}, which fetchloom does not unpack",
                file.path
            );
            tell_packed(&file.path, said);
        }
        if let Some(result) = result {
            files[index].unpacks_to = Some(result.tree);
            done.archives.push(files[index].sha256);
            done.trees.insert(
                (files[index].path.clone(), files[index].sha256, result.tree),
                result.members,
            );
        }
    }
    Ok(done)
}

/// where every file lands in the folder, each archive standing for the files it unpacked to.
fn planned<'a>(cache: &Cache, files: &'a [LockedFile], trees: Trees) -> Result<Plan<'a>, Error> {
    let mut planned = Plan::default();
    let trees = std::cell::RefCell::new(trees);
    let members = |archive: &crate::project::lock::archives::Archive<'_>| {
        let file = archive.file;
        let tree = file.unpacks_to?;
        trees
            .borrow_mut()
            .remove(&(file.path.clone(), file.sha256, tree))
            .or_else(|| cache.unpacked(&file.sha256, Some(&tree)))
    };
    plan::expand(
        &mut planned,
        "",
        "",
        false,
        files,
        &crate::project::manifest::Members::new(),
        &members,
    )?;
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

/// the step after a download for a person at a terminal: pinning the same link in a project,
/// this one when the download ran inside one.
fn tell_next(context: &Context, args: &Get, link: &str) {
    if let Some(command) = pin_command(args, link) {
        let inside = std::env::current_dir()
            .ok()
            .and_then(|here| crate::project::Project::find(&here))
            .is_some();
        context.output.next(&if inside {
            format!("to pin it in this project instead, run `{command}`")
        } else {
            format!("to pin this dataset in a project, run `{command}` there")
        });
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

/// the select patterns of a download: `--select`, or else the one file or folder a pasted link
/// names.
fn wanted_patterns(args: &Get, parsed: &Parsed) -> Vec<String> {
    if args.filter.select.is_empty() {
        parsed.select()
    } else {
        args.filter.select.clone()
    }
}

/// a link resolved to its listing, and the files of it this get keeps.
type Resolved = (Parsed, Listing, Vec<Remote>);

/// the files a link its source listed keeps, told on the Resolved line, which counts them among
/// all the listing holds: one file of a repository's four reads as one of four.
fn told(
    context: &Context,
    args: &Get,
    parsed: &Parsed,
    listing: &Listing,
) -> Result<Vec<Remote>, Error> {
    let files = selected(args, parsed, listing);
    let kept = files.as_deref().unwrap_or(listing.files.as_slice());
    engine::tell_kept(&context.output, listing, kept);
    files
}

/// the files `--select` and `--exclude` keep, or the one file or folder a pasted link names.
fn selected(args: &Get, parsed: &Parsed, listing: &Listing) -> Result<Vec<Remote>, Error> {
    if let Some(file) = &parsed.file
        && args.filter.select.is_empty()
        && !listing.paths().any(|path| path == file)
    {
        return Err(missing_file(parsed, listing, file));
    }
    let select = wanted_patterns(args, parsed);
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
    match entry.source() {
        Some(source) => engine::filter(&parsed.reference.shown(), &entry, source, listing),
        None => Ok(listing.files.clone()),
    }
}

/// the error for a pasted link to `file`, which its listing lacks. it names the file the link
/// most likely meant: one whose name differs only in case, as hugging face's `Iris.csv` from a
/// typed `iris.csv`, else a near typo.
fn missing_file(parsed: &Parsed, listing: &Listing, file: &str) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!("{} has no file {file}", parsed.reference.shown()),
    );
    let folded = name::fold(file);
    if let Some(meant) = listing.paths().find(|path| name::fold(path) == folded) {
        return error.help(format!("its name is {meant}: write that in the link"));
    }
    if let Some(meant) = name::closest(file, listing.paths()) {
        return error.help(format!("did you mean {meant}? write that in the link"));
    }
    let error = match name::nearest(file, listing.paths(), 1).first() {
        Some(nearest) => error.detail(format!(
            "{} considered, the nearest {nearest}",
            units::count(listing.paths().count() as u64, "file was", "files were")
        )),
        None => error,
    };
    error.help("check the file's name in the link")
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
            .naming_title()
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

/// tells what one link's downloads brought, as [`fetched_step`] words it, with each file's full
/// hash for `-v`.
fn tell_fetched(
    context: &Context,
    paths: &[String],
    fetched: &[Fetched],
    files: &[Remote],
    started: Instant,
) {
    let (verb, text) = fetched_step(paths, fetched, files, started);
    context.output.step(verb, &text);
    if let [fetched] = fetched {
        context.output.detail(&format!("sha256 {}", fetched.digest));
    }
}

/// the step that tells what one link's downloads brought: downloaded, checked with the server or
/// found in the cache, with the time since `started` their bytes took to arrive. the time a slow disk
/// takes to keep them comes after, so it does not count against the link.
fn fetched_step(
    paths: &[String],
    fetched: &[Fetched],
    files: &[Remote],
    started: Instant,
) -> (&'static str, String) {
    let took = Fetched::transfer(fetched, started);
    if let ([path], [fetched], [remote]) = (paths, fetched, files) {
        return match fetched.origin {
            Origin::Network => (
                "Downloaded",
                format!(
                    "{path} · {}",
                    units::downloaded([(fetched.size, fetched.moved)], took)
                ),
            ),
            // a 304 confirms the bytes downloaded last, which a one-off may have moved out of the
            // cache and a destination an earlier run filled may not hold
            Origin::NotModified => (
                "Checked",
                format!(
                    "{path} · {} still serves the bytes downloaded last time",
                    net::host(&remote.url)
                ),
            ),
            Origin::Cache => ("Found", format!("{path} in the cache")),
        };
    }
    let downloaded: Vec<&Fetched> = fetched
        .iter()
        .filter(|fetched| fetched.origin == Origin::Network)
        .collect();
    if downloaded.is_empty() {
        return (
            "Found",
            format!(
                "{} · in the cache or unchanged",
                units::count(fetched.len() as u64, "file", "files")
            ),
        );
    }
    (
        "Downloaded",
        format!(
            "{} · {}",
            units::count(downloaded.len() as u64, "file", "files"),
            units::downloaded(
                downloaded
                    .iter()
                    .map(|fetched| (fetched.size, fetched.moved)),
                took
            )
        ),
    )
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
            within: Vec::new(),
            access: crate::source::Access::Unknown,
            resolved: parsed.reference.clone(),
            title: Some("Aux".to_owned()),
            license: None,
            doi: None,
            files: Vec::new(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
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

    /// a download is told by when its last byte arrived, so the time a slow disk takes to keep
    /// the file, as `-o` waits for, does not count against the link.
    #[test]
    fn a_download_is_told_by_when_its_last_byte_arrived() {
        let started = Instant::now();
        let fetched = Fetched {
            digest: crate::digest::sha256(b"iris"),
            size: 3072,
            moved: 3072,
            name: None,
            origin: Origin::Network,
            version: None,
            arrived: Some(started + std::time::Duration::from_secs(3)),
        };
        let remote = Remote {
            path: "iris.csv".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: url::Url::parse("https://example.org/iris.csv").unwrap(),
            named_by_server: false,
            etag: None,
        };
        assert_eq!(
            fetched_step(&["iris.csv".to_owned()], &[fetched], &[remote], started),
            (
                "Downloaded",
                "iris.csv · 3.0 KiB in 3.0 s · 1.0 KiB/s".to_owned()
            )
        );
    }

    /// the room a get asks for counts each archive it unpacks once more, since the files land
    /// beside it before it can go, while the question about the size counts the bytes moved. a
    /// file kept packed, a file that is no archive and an archive whose size nothing stated add
    /// nothing, and a sum too large to count stays the largest.
    #[test]
    fn the_room_a_get_needs_counts_each_archive_it_unpacks_once_more() {
        let job = |path: &str, size: Option<u64>, extract: bool| {
            let remote = Remote {
                path: path.to_owned(),
                size,
                sha256: None,
                md5: None,
                url: url::Url::parse(&format!("https://example.org/{path}")).unwrap(),
                named_by_server: false,
                etag: None,
            };
            fetch::listed_job(&remote, path, extract)
        };
        let zip = job("a.zip", Some(1_000_000), true);
        assert_eq!(needs(&[&zip]), (1_000_000, 2_000_000));
        let packed = job("b.zip", Some(1_000_000), false);
        let csv = job("c.csv", Some(500), true);
        let unstated = job("d.zip", None, true);
        assert_eq!(needs(&[&packed, &csv, &unstated]), (1_000_500, 1_000_500));
        let huge = job("e.tar", Some(u64::MAX - 1), true);
        assert_eq!(needs(&[&huge, &zip]), (u64::MAX, u64::MAX));
    }

    /// links that a stop kept from their folders end the run as that stop, which says first that
    /// it stopped, as every stop does.
    #[test]
    fn links_a_stop_kept_from_their_folders_end_as_a_stop() {
        let links = [
            "https://example.org/a.csv".to_owned(),
            "zenodo:1".to_owned(),
        ];
        let stopped = some_failed(&links, &[(1, crate::stop::Stop::stopped())]);
        assert_eq!(stopped.kind, Kind::Interrupted);
        assert_eq!(
            stopped.message,
            "stopped, and 1 of 2 links could not be fetched"
        );
        let failed = some_failed(&links, &[(0, Error::new(Kind::SourceInvalid, "x"))]);
        assert_eq!(failed.message, "1 of 2 links could not be fetched");
    }

    /// the count of links that failed has its thousands apart, as every count a person reads.
    #[test]
    fn many_failed_links_are_counted_with_thousands_apart() {
        let links: Vec<String> = (0..1200)
            .map(|n| format!("https://a.example/{n}"))
            .collect();
        let failed: Vec<(usize, Error)> = (0..1200)
            .map(|n| (n, Error::new(Kind::ReferenceUnresolved, "missing")))
            .collect();
        assert_eq!(
            some_failed(&links, &failed).message,
            "1,200 of 1,200 links could not be fetched"
        );
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
            size: Some(crate::settings::ASK_ABOVE + 1),
            sha256: Some(digest),
            md5: None,
            url: url::Url::parse("https://example.org/large.zip").unwrap(),
            named_by_server: false,
            etag: None,
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
            size: Some(crate::settings::ASK_ABOVE),
            sha256: Some(digest),
            md5: None,
            url: url::Url::parse("https://example.org/cached.zip").unwrap(),
            named_by_server: false,
            etag: None,
        };
        let mut jobs = vec![fetch::listed_job(&remote, &remote.path, true)];
        remote.path = "missing.bin".to_owned();
        remote.sha256 = Some(crate::digest::sha256(b"missing"));
        jobs.push(fetch::listed_job(&remote, &remote.path, false));
        let context = approval_context();
        approve(&context, &cache, &jobs).unwrap();
        jobs[1].fetch.expect.size = Some(crate::settings::ASK_ABOVE + 1);
        assert_eq!(
            approve(&context, &cache, &jobs).unwrap_err().kind,
            Kind::Unconfirmed
        );
    }

    /// a large file whose bytes may turn out unchanged is asked about only once its server sends
    /// it, so a rerun answered 304 asks nothing.
    #[test]
    fn get_approval_leaves_a_file_that_may_be_unchanged_to_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let remote = Remote {
            path: "large.bin".to_owned(),
            size: Some(crate::settings::ASK_ABOVE + 1),
            sha256: None,
            md5: None,
            url: url::Url::parse("https://example.org/large.bin").unwrap(),
            named_by_server: false,
            etag: None,
        };
        let mut jobs = vec![fetch::listed_job(&remote, &remote.path, false)];
        let context = approval_context();
        assert_eq!(
            approve(&context, &cache, &jobs).unwrap_err().kind,
            Kind::Unconfirmed
        );
        jobs[0].fetch.held = Some(crate::digest::sha256(b"the bytes fetched before"));
        approve(&context, &cache, &jobs).unwrap();
    }

    /// a link to a file the listing lacks names the file it most likely meant, such as hugging
    /// face's `Iris.csv` for a typed `iris.csv`, rather than a pattern nobody typed.
    #[test]
    fn a_link_to_a_missing_file_names_the_one_meant() {
        let listing = |paths: &[&str]| Listing {
            resolved: Reference::parse("hf:datasets/scikit-learn/iris@main")
                .unwrap()
                .reference,
            title: None,
            license: None,
            doi: None,
            access: crate::source::Access::Unknown,
            files: paths
                .iter()
                .map(|path| Remote {
                    path: (*path).to_owned(),
                    size: Some(1),
                    sha256: None,
                    md5: None,
                    url: url::Url::parse(&format!("https://example.org/{path}")).unwrap(),
                    named_by_server: false,
                    etag: None,
                })
                .collect(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
            within: Vec::new(),
        };
        let args = Get {
            links: Vec::new(),
            output: None,
            force: false,
            dir: None,
            input_file: None,
            sha256: None,
            filter: Filter {
                select: Vec::new(),
                exclude: Vec::new(),
            },
            no_extract: false,
        };
        let pasted = |file: &str| {
            Reference::parse(&format!(
                "https://huggingface.co/datasets/scikit-learn/iris/resolve/main/{file}"
            ))
            .unwrap()
        };
        let files = [".gitattributes", "Iris.csv", "README.md", "database.sqlite"];
        for (typed, details, help) in [
            (
                "iris.csv",
                &[][..],
                "its name is Iris.csv: write that in the link",
            ),
            (
                "Iris.cvs",
                &[],
                "did you mean Iris.csv? write that in the link",
            ),
            (
                "flowers.csv",
                &["4 files were considered, the nearest Iris.csv"],
                "check the file's name in the link",
            ),
        ] {
            let error = selected(&args, &pasted(typed), &listing(&files)).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(
                error.message,
                format!("hf:datasets/scikit-learn/iris@main has no file {typed}")
            );
            assert_eq!(error.details, details, "{typed}");
            assert_eq!(error.help.as_deref(), Some(help), "{typed}");
        }
        let kept = selected(&args, &pasted("Iris.csv"), &listing(&files)).unwrap();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn the_pin_hint_keeps_filters_and_extraction_settings() {
        let args = Get {
            links: vec!["https://example.org/data.zip".to_owned()],
            output: None,
            force: false,
            dir: Some(PathBuf::from("chosen")),
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
    #[test]
    fn get_plans_from_the_member_record_it_already_checked() {
        use crate::output::{Level, Output, Style};
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let archive = crate::cache::tests::store(&cache, b"packed");
        let member = crate::cache::tests::store(&cache, b"member");
        let members = [crate::cache::Member {
            path: "a.csv".into(),
            size: member.size,
            sha256: member.digest,
        }];
        let tree = cache.record_unpacked(&archive.digest, &members).unwrap();
        let mut files = [LockedFile {
            path: "files.zip".into(),
            size: archive.size,
            sha256: archive.digest,
            md5: None,
            url: "https://example.org/files.zip".into(),
            unpacks_to: None,
        }];
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        let known = crate::source::recorded::block_on(unpack(
            &context,
            &cache,
            &mut files,
            Path::new("files"),
            true,
        ))
        .unwrap();
        assert_eq!(files[0].unpacks_to, Some(tree));
        std::fs::remove_file(cache.root().join("unpacked").join(archive.digest.hex())).unwrap();
        let plan = planned(&cache, &files, known.trees).unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].path, "a.csv");
        assert_eq!(plan.targets[0].sha256, member.digest);
    }
}
