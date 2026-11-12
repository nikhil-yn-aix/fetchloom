//! `get -o <file>`: the one file a link resolves to, written at exactly that path. the file is
//! the person's own, writable and never unpacked, and it takes its name only once its bytes are
//! checked, never in place of another file unless `--force` asks. bytes this run downloaded leave
//! the cache once the file holds them, unless a project on this machine needs them.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::super::Context;
use super::super::args::Get;
use super::super::fetch;
use super::{
    Done, Expected, Got, Written, approve, folder_for, matches_sha256, one_file, tell_fetched, vet,
};
use crate::archive::{self, Packed};
use crate::cache::{Cache, Claim, Partial, UrlRecord};
use crate::digest::{Block, Sha256};
use crate::error::{Error, Kind};
use crate::net::{Fetched, Net, Origin};
use crate::output::Warning;
use crate::place::{Method, Order, Placer};
use crate::source::{Listing, Parsed, Reference, Remote};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::volume::Volume;
use crate::work::Work;

/// refuses an `-o` that cannot name the one file to write, before anything is asked of a source:
/// stdout, several links or a list of them, a folder, a name this machine's disks cannot store,
/// or a path beneath a file.
pub(super) fn usage(path: &Path, links: &[String], listed: bool) -> Result<(), Error> {
    if path == Path::new("-") {
        return Err(Error::new(
            Kind::Usage,
            "get checks a file before it appears, so -o cannot be - for stdout",
        )
        .help("give -o a file name, then read that file; get prints its path"));
    }
    if listed || links.len() > 1 {
        let several = if listed {
            "reads its links from a list".to_owned()
        } else {
            format!("has {} links", crate::units::grouped(links.len() as u64))
        };
        return Err(Error::new(
            Kind::Usage,
            format!("-o writes one file, and this get {several}"),
        )
        .help("use --dir <folder> instead, which gets a folder of its own for each link"));
    }
    let shown = path.display();
    let last = path.as_os_str().to_string_lossy();
    let last = last
        .rsplit(std::path::is_separator)
        .next()
        .unwrap_or_default();
    let folder = matches!(last, "" | "." | "..")
        || !matches!(path.components().next_back(), Some(Component::Normal(_)))
        || fs::metadata(path).is_ok_and(|metadata| metadata.is_dir());
    if folder {
        // --dir would refuse a folder that holds files of its own, as most do
        let name = links
            .first()
            .and_then(|link| named_in(link))
            .unwrap_or_else(|| "<file>".to_owned());
        return Err(Error::new(
            Kind::Usage,
            format!("-o needs a file name, and {shown} names a folder"),
        )
        .help(format!(
            "name the file inside it, as in `-o {}`",
            path.join(name).display()
        )));
    }
    if let Some(reason) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(crate::name::unstorable)
    {
        return Err(
            Error::new(Kind::Usage, format!("-o cannot write {shown}: {reason}"))
                .help("choose another name for -o"),
        );
    }
    if let Some((blocked, what)) = blocked(path) {
        let instead = blocked
            .parent()
            .unwrap_or(Path::new(""))
            .join(path.file_name().unwrap_or_default());
        return Err(Error::new(
            Kind::LocalIo,
            format!("cannot write {shown}: {} is {what}", blocked.display()),
        )
        .help(format!(
            "choose another name with -o, such as `-o {}`, or rename {} so a folder can take its name",
            instead.display(),
            blocked.display()
        )));
    }
    Ok(())
}

/// what stands where a folder of `path` must be, as the file `data.csv` does for
/// `data.csv/x.csv`, and what it is: nothing can be written at `path` then.
fn blocked(path: &Path) -> Option<(&Path, &'static str)> {
    for folder in path.ancestors().skip(1) {
        match fs::metadata(folder) {
            Ok(metadata) if metadata.is_dir() => return None,
            Ok(metadata) if metadata.is_file() => return Some((folder, "a file, not a folder")),
            Ok(_) => return Some((folder, "not a folder")),
            Err(_) => {}
        }
    }
    None
}

/// the name of the file a link points at, as its path ends, when that shows before any source
/// lists it: `iris.csv` for a link to `.../iris.csv`, none for an id such as a DOI.
fn named_in(link: &str) -> Option<String> {
    let parsed = Reference::parse(link).ok()?;
    match (&parsed.file, &parsed.reference) {
        (Some(file), _) => file.rsplit('/').next().and_then(crate::name::sanitize),
        (None, Reference::Url(url)) => crate::name::from_url(url),
        _ => None,
    }
}

/// what is at the path `-o` names.
#[derive(Clone, Debug)]
enum Existing {
    Missing,
    /// a file, or a link to one, by the hash of its bytes.
    Holds(Sha256),
    /// a link that leads nowhere, by where it points.
    Dangling(PathBuf),
    /// a special file such as a pipe.
    Other,
}

/// what says which bytes the file should hold.
#[derive(Clone, Copy)]
enum Wanted {
    /// the bytes the link served this run.
    Served,
    /// the hash `--sha256` names.
    Named,
    /// the hash the link's source states.
    Stated,
    /// what the link served the last time it was fetched here, all an offline run knows.
    Recorded,
}

impl Wanted {
    fn said(self) -> &'static str {
        match self {
            Wanted::Served => "the link serves now",
            Wanted::Named => "--sha256 names",
            Wanted::Stated => "its source states",
            Wanted::Recorded => "the link served last time",
        }
    }
}

/// what to do about the path `-o` names, given the bytes the link serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// nothing is there, so the file takes the name.
    Write,
    /// the file there holds these bytes already and stays as it is.
    Keep,
    /// something else is there, which `--force` replaces.
    Replace,
    /// something else is there, and nothing replaces it without `--force`.
    Refuse,
}

fn decide(existing: &Existing, wanted: &Sha256, force: bool) -> Step {
    match existing {
        Existing::Missing => Step::Write,
        Existing::Holds(found) if found == wanted => Step::Keep,
        _ if force => Step::Replace,
        _ => Step::Refuse,
    }
}

/// looks at the destination's path, hashing a file there. a link is followed for its bytes, so a
/// file reached through one that holds them stays; `--force` replaces the link itself.
fn existing(destination: &Destination) -> Result<Existing, Error> {
    let path = destination.path.as_path();
    let unreadable =
        |err: &io::Error| Error::io(format_args!("could not read {}", path.display()), err);
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(Error::new(
            Kind::LocalNotEmpty,
            format!("{} became a folder while downloading", destination.shown),
        )
        .help("choose another name with -o")),
        Ok(metadata) if metadata.is_file() => {
            Sha256::of_file(path, Block::sized(metadata.len().saturating_add(1)))
                .map(Existing::Holds)
                .map_err(|err| unreadable(&err))
        }
        Ok(_) => Ok(Existing::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(path) {
            Ok(_) => Ok(Existing::Dangling(fs::read_link(path).unwrap_or_default())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Existing::Missing),
            Err(err) => Err(unreadable(&err)),
        },
        Err(err) => Err(unreadable(&err)),
    }
}

/// the path `-o` names: as the person typed it, for messages, and made absolute.
struct Destination {
    shown: String,
    path: PathBuf,
}

/// fetches the one file of `files`, what the link keeps of `listing`, and writes it at `path`,
/// unless a file there holds its bytes already. returns the file's absolute path and what it
/// holds. `key` remembers what the link kept, for a link only its source can list.
pub(super) async fn one(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    args: &Get,
    path: &Path,
    (parsed, listing, files): (&Parsed, &Listing, Vec<Remote>),
    key: Option<&str>,
) -> Result<(PathBuf, Done), Error> {
    let remote = only(files, parsed, listing)?;
    put(context, net, cache, args, path, parsed, &remote, None, key).await
}

/// `-o` offline for a link only its source can list: the file the link kept the last time this
/// machine fetched it, `kept`, stands in for the listing, as do the bytes `--sha256` names. none
/// when neither says what the file holds.
pub(super) async fn again_offline(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    args: &Get,
    path: &Path,
    parsed: &Parsed,
    key: &str,
) -> Result<Option<(PathBuf, Done)>, Error> {
    let kept = cache.url_record(key);
    let name = kept
        .as_ref()
        .and_then(|kept| kept.name.clone())
        .or_else(|| parsed.file.clone())
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let Some(remote) = super::remembered::file(parsed, name, kept.as_ref(), args.sha256) else {
        return Ok(None);
    };
    put(
        context,
        net,
        cache,
        args,
        path,
        parsed,
        &remote,
        kept.as_ref(),
        Some(key),
    )
    .await
    .map(Some)
}

/// writes `remote` at `path` as [`one`] does. `last` is what the link kept last time, which
/// decides offline where its listing cannot.
#[expect(
    clippy::too_many_arguments,
    reason = "the run's context and the file, as get resolved it"
)]
async fn put(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    args: &Get,
    path: &Path,
    parsed: &Parsed,
    remote: &Remote,
    last: Option<&UrlRecord>,
    key: Option<&str>,
) -> Result<(PathBuf, Done), Error> {
    let expected = args.sha256.map(Expected::flag);
    if let Some(expected) = &expected {
        one_file(std::slice::from_ref(remote), expected, parsed)?;
    }
    let shown = path.display().to_string();
    let destination = Destination {
        path: std::path::absolute(path)
            .map_err(|err| Error::io(format_args!("could not find where {shown} is"), &err))?,
        shown,
    };
    let mut early = decided_early(context, cache, remote, args, &destination, last)?;
    if matches!(early, Early::Download(_)) && !context.offline.enabled() {
        // once another run's download of the link is over, the run decides again, as a rerun
        // would, holding the link meanwhile so that run's bytes stay where it put them
        let (_link, waited) = claimed(context, cache, remote, true).await?;
        if waited {
            early = decided_early(context, cache, remote, args, &destination, last)?;
        }
    }
    let done = match early {
        Early::Kept(kept) => kept,
        Early::Download(known) => {
            let fetched = download(context, net, cache, remote, known.held).await?;
            // bytes this run wrote may leave the cache for the file, so it holds the link until
            // they are there, for a run waiting for the link to find them in one or the other
            let _link = if context.work.cached_before(&fetched.digest) {
                None
            } else {
                Some(claimed(context, cache, remote, false).await?.0)
            };
            if let Some(expected) = &expected {
                matches_sha256(&remote.path, &fetched.digest, expected)?;
            }
            refuse_zipped(cache, remote, &fetched.digest)?;
            settle(
                context,
                cache,
                remote,
                &fetched,
                args.force,
                &destination,
                &known,
            )?
        }
    };
    if let (Some(key), [got]) = (key, done.1.files.as_slice()) {
        super::remembered::keep(cache, key, &remote.path, got.sha256, got.size);
    }
    Ok(done)
}

/// what a run decided before downloading: the file there kept, or what it learned for the
/// download.
enum Early {
    Kept((PathBuf, Done)),
    Download(Known),
}

/// what a run learned before downloading.
#[derive(Default)]
struct Known {
    /// what is at the path, when it was looked at.
    found: Option<Existing>,
    /// the bytes the link served last time, when the cache or the file at the path still holds
    /// them, so the server is asked whether they changed.
    held: Option<Sha256>,
}

/// what a hash known before downloading decides about a file already at the path, with no
/// request, or what the run learned for the download, as when the path is free or the file must
/// go. `last`, what the link kept last time, decides offline in place of its listing. a link
/// whose source states no hash stands for the bytes it served last time: offline they decide,
/// and online the server is asked whether they changed.
fn decided_early(
    context: &Context,
    cache: &Cache,
    remote: &Remote,
    args: &Get,
    destination: &Destination,
    last: Option<&UrlRecord>,
) -> Result<Early, Error> {
    let (digest, wanted) = match (args.sha256, last, stated(cache, remote)) {
        (Some(named), _, _) => (named, Wanted::Named),
        (None, Some(last), _) => (last.sha256, Wanted::Recorded),
        (None, None, Some(stated)) => (stated, Wanted::Stated),
        (None, None, None) => {
            let Some(record) = recorded(cache, remote) else {
                unhashed(context, remote, args.force, destination)?;
                return Ok(Early::Download(Known::default()));
            };
            if !context.offline.enabled() {
                return held(cache, &record, destination).map(Early::Download);
            }
            (record.sha256, Wanted::Recorded)
        }
    };
    let found = existing(destination)?;
    match decide(&found, &digest, args.force) {
        Step::Keep => {
            let size = fs::metadata(&destination.path)
                .map_err(|err| {
                    Error::io(format_args!("could not read {}", destination.shown), &err)
                })?
                .len();
            let got = got(destination, remote, digest, size, Origin::Cache, None);
            Ok(Early::Kept(kept(context, wanted, destination, got)))
        }
        Step::Refuse => Err(refused(&destination.shown, &found, wanted, &digest)),
        Step::Write | Step::Replace => Ok(Early::Download(Known {
            found: Some(found),
            held: None,
        })),
    }
}

/// looks, before downloading a link whose bytes no hash names, at a file already at the path: one
/// of another size than the source states holds other bytes, so it is refused at once, and of any
/// other only the download tells, so the person is told that it comes first. `--force` replaces
/// it either way.
fn unhashed(
    context: &Context,
    remote: &Remote,
    force: bool,
    destination: &Destination,
) -> Result<(), Error> {
    if !there_unhashed(remote, force, destination)? {
        return Ok(());
    }
    if !context.offline.enabled() {
        context.output.step(
            "Checking",
            &format!(
                "{} · already there, so the link downloads first to compare",
                destination.shown
            ),
        );
    }
    Ok(())
}

/// what writing `remote` at `path` would do, as a get decides it before downloading, with
/// nothing downloaded or written: by the hash `--sha256` or its source gives, or offline by what
/// the link served last time. a file already there that no hash speaks for is compared once the
/// download is in. a file the get would refuse is refused the same way.
pub(super) fn foreseen(
    context: &Context,
    cache: &Cache,
    args: &Get,
    remote: &Remote,
    path: &Path,
) -> Result<Foreseen, Error> {
    let shown = path.display().to_string();
    let destination = Destination {
        path: std::path::absolute(path)
            .map_err(|err| Error::io(format_args!("could not find where {shown} is"), &err))?,
        shown,
    };
    let known = match (args.sha256, stated(cache, remote)) {
        (Some(named), _) => Some((named, Wanted::Named)),
        (None, Some(stated)) => Some((stated, Wanted::Stated)),
        (None, None) => recorded(cache, remote)
            .filter(|_| context.offline.enabled())
            .map(|record| (record.sha256, Wanted::Recorded)),
    };
    let found = existing(&destination)?;
    let Some((digest, wanted)) = known else {
        return Ok(match found {
            Existing::Missing => Foreseen::Write,
            _ if args.force => Foreseen::Replace,
            _ => {
                there_unhashed(remote, false, &destination)?;
                Foreseen::Compare
            }
        });
    };
    match decide(&found, &digest, args.force) {
        Step::Write => Ok(Foreseen::Write),
        Step::Keep => Ok(Foreseen::Keep),
        Step::Replace => Ok(Foreseen::Replace),
        Step::Refuse => Err(refused(&destination.shown, &found, wanted, &digest)),
    }
}

/// what a get would do with the file `-o` names, as [`foreseen`] finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Foreseen {
    /// nothing is there, so the file takes the name.
    Write,
    /// the file there holds these bytes already and stays.
    Keep,
    /// `--force` replaces what is there.
    Replace,
    /// a file is there whose bytes only the download can compare.
    Compare,
}

/// whether a file is at the path that no hash speaks for, which only the download can compare,
/// refusing one of another size than the source states, which holds other bytes. `--force`
/// replaces whatever is there.
fn there_unhashed(remote: &Remote, force: bool, destination: &Destination) -> Result<bool, Error> {
    let size = match fs::metadata(&destination.path) {
        Ok(metadata) if metadata.is_file() && !force => metadata.len(),
        _ => return Ok(false),
    };
    if let Some(stated) = remote.size.filter(|stated| *stated != size) {
        return Err(
            other_bytes(&destination.shown, Wanted::Served).detail(format!(
                "{} is {}, and the link's file {}",
                destination.shown,
                crate::units::size(size),
                crate::units::size(stated)
            )),
        );
    }
    Ok(true)
}

/// what the link served the last time it was downloaded here, for a link whose source states
/// no hash.
fn recorded(cache: &Cache, remote: &Remote) -> Option<UrlRecord> {
    if remote.sha256.is_some() || remote.md5.is_some() {
        return None;
    }
    cache.url_record(remote.url.as_str())
}

/// what the run learned of the bytes the link served last time, `record`, which the server is
/// asked about while the cache or the file at the path still holds them.
fn held(cache: &Cache, record: &UrlRecord, destination: &Destination) -> Result<Known, Error> {
    if cache.find(&record.sha256, Some(record.size)).is_some() {
        return Ok(Known {
            found: None,
            held: Some(record.sha256),
        });
    }
    let found = existing(destination)?;
    let held = matches!(found, Existing::Holds(digest) if digest == record.sha256)
        .then_some(record.sha256);
    Ok(Known {
        found: Some(found),
        held,
    })
}

/// the SHA-256 the link's source states for its file: itself, or through the MD5 it states, the
/// hash of the bytes an earlier download found to match it.
fn stated(cache: &Cache, remote: &Remote) -> Option<Sha256> {
    remote.sha256.or_else(|| {
        let md5 = remote.md5?;
        cache
            .stated_md5(&md5, remote.url.as_str())
            .map(|(digest, _)| digest)
    })
}

/// how often a run waiting for another fetchloom's hold on its link asks again, as a download
/// does.
const WAITING: std::time::Duration = std::time::Duration::from_millis(250);

/// the link's claim, which its download takes too, once no other fetchloom on this machine
/// holds it, and whether the run waited for it, saying so first when `tell` asks. while it is
/// held, no download of the link starts and no `-o` run moves the link's bytes out of the cache.
async fn claimed(
    context: &Context,
    cache: &Cache,
    remote: &Remote,
    tell: bool,
) -> Result<(Partial, bool), Error> {
    let mut waiting = None;
    let mut waited = false;
    loop {
        match cache.claim(remote.url.as_str()) {
            Ok(Claim::Owned(link)) => return Ok((link, waited)),
            Ok(Claim::Busy) => waited = true,
            Err(err) => return Err(Error::io("could not write to the cache", &err)),
        }
        if tell && waiting.is_none() {
            waiting = Some(context.output.wait(
                "Waiting",
                format!("{} · another fetchloom is downloading it", remote.path),
            ));
        }
        tokio::select! {
            () = tokio::time::sleep(WAITING) => {}
            () = context.stop.asked() => return Err(Stop::stopped()),
        }
    }
}

/// downloads the one file into the cache, or finds it there, and says so.
async fn download(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    remote: &Remote,
    held: Option<Sha256>,
) -> Result<Fetched, Error> {
    let mut job = fetch::listed_job(remote, &remote.path, false);
    job.fetch.held = held;
    vet(
        cache,
        std::slice::from_ref(remote),
        std::slice::from_ref(&remote.path),
        false,
    )?;
    approve(context, cache, std::slice::from_ref(&job))?;
    let started = Instant::now();
    // nothing is written before the download is in, so a download cancelled at its question
    // changed nothing
    let [fetched] = <[Fetched; 1]>::try_from(
        fetch::all(context, net, cache, vec![job])
            .await
            .map_err(Error::unchanged)?,
    )
    .map_err(|_| Error::new(Kind::LocalIo, "the download went missing"))?;
    tell_fetched(
        context,
        std::slice::from_ref(&remote.path),
        std::slice::from_ref(&fetched),
        std::slice::from_ref(remote),
        started,
    );
    Ok(fetched)
}

/// keeps, refuses or writes the file once the bytes the link serves are in the cache, or the
/// server said those the file holds are unchanged. a file looked at before a download that took
/// its time is looked at again.
fn settle(
    context: &Context,
    cache: &Cache,
    remote: &Remote,
    fetched: &Fetched,
    force: bool,
    destination: &Destination,
    known: &Known,
) -> Result<(PathBuf, Done), Error> {
    let found = match &known.found {
        Some(found) if fetched.origin != Origin::Network => found.clone(),
        _ => existing(destination)?,
    };
    let step = decide(&found, &fetched.digest, force);
    let record = |method| {
        got(
            destination,
            remote,
            fetched.digest,
            fetched.size,
            fetched.origin,
            method,
        )
    };
    if step == Step::Refuse {
        if matches!(found, Existing::Holds(digest) if Some(digest) == known.held) {
            return Err(Error::new(
                Kind::LocalNotEmpty,
                format!(
                    "{} already exists and holds what the link served last time, and its source changed it since",
                    destination.shown
                ),
            )
            .help("add --force to replace it with what the link serves now, or choose another name with -o to keep both"));
        }
        return Err(refused(
            &destination.shown,
            &found,
            Wanted::Served,
            &fetched.digest,
        ));
    }
    if step == Step::Keep {
        if one_off(context, cache, &fetched.digest) {
            forget(cache, &fetched.digest);
        }
        return Ok(kept(context, Wanted::Served, destination, record(None)));
    }
    // another run of the link may write the file between that look and this write, moving the
    // bytes out of the cache to do so, and a file it wrote with them is kept as a rerun keeps it
    let holds_them = || {
        matches!(
            existing(destination),
            Ok(Existing::Holds(found)) if found == fetched.digest
        )
    };
    let Some(object) = cache.find(&fetched.digest, Some(fetched.size)) else {
        if holds_them() {
            return Ok(kept(context, Wanted::Served, destination, record(None)));
        }
        return Err(Error::new(
            Kind::LocalIo,
            format!("the cache lost {} while writing it", remote.path),
        )
        .help("run the same command again to download it"));
    };
    // the last look for a stop: once the file is being written, the get ends as it would
    context.stop.check().map_err(Error::unchanged)?;
    context.stop.finish();
    let replace = step == Step::Replace;
    let one_off = one_off(context, cache, &fetched.digest);
    let how = match write(
        context,
        cache,
        (&object, &fetched.digest),
        &destination.path,
        replace,
        one_off,
    ) {
        Ok(how) => how,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && holds_them() => {
            if one_off {
                forget(cache, &fetched.digest);
            }
            return Ok(kept(context, Wanted::Served, destination, record(None)));
        }
        Err(err) => return Err(unwritten(&destination.shown, &err)),
    };
    context.output.done(
        if replace { "Replaced" } else { "Saved" },
        &destination.shown,
    );
    warn_packed(context, remote, fetched, destination);
    Ok(written(destination, record(Some(how))))
}

/// gives `path` the bytes of `object`, the cache's copy of `digest`: moved out of the cache for a
/// one-off, else published beside it and counted, the one-off's copy then dropped.
fn write(
    context: &Context,
    cache: &Cache,
    (object, digest): (&Path, &Sha256),
    path: &Path,
    replace: bool,
    one_off: bool,
) -> io::Result<Written> {
    if one_off && take(object, path, replace)? {
        return Ok(Written::Move);
    }
    let method = publish(object, path, replace, context.threads, &context.stop)?;
    let counter = match method {
        Method::Reflink => &context.work.reflinks,
        Method::Hardlink => &context.work.hardlinks,
        Method::Copy => &context.work.copies,
    };
    Work::add(counter, 1);
    if context.work.cached_before(digest) {
        Work::add(&context.work.placed_from_cache, 1);
    }
    if one_off {
        forget(cache, digest);
    }
    Ok(Written::from(method))
}

/// warns of an archive saved under a name that does not say so, as a zip saved as `data.csv`:
/// `-o` keeps it packed, which a reader of that name would not expect.
fn warn_packed(context: &Context, remote: &Remote, fetched: &Fetched, destination: &Destination) {
    let came = fetched.name.as_deref().unwrap_or(&remote.path);
    let saved = destination
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if archive::by_name(came) != Some(Packed::Archive) || archive::by_name(&saved).is_some() {
        return;
    }
    let file = came.rsplit('/').next().unwrap_or(came);
    context.output.warn_with(
        Warning::ArchivePacked,
        &format!(
            "{} holds the archive {file}, kept packed as it came",
            destination.shown
        ),
        &format!(
            "use `--dir {}` to unpack it into a folder",
            crate::name::folder_for(file)
        ),
    );
}

/// refuses a file its source sent inside a zip, as kaggle may send a competition's file: `-o`
/// writes a file as it comes, and those bytes are not the file the name promises.
fn refuse_zipped(cache: &Cache, remote: &Remote, digest: &Sha256) -> Result<(), Error> {
    if fetch::wrapped(cache, remote, remote.path.clone(), digest) == remote.path {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!(
            "{} came inside a zip, and -o writes a file as it comes",
            remote.path
        ),
    )
    .help(format!(
        "use `--dir {}` to unpack it into a folder",
        crate::name::folder_for(&remote.path)
    )))
}

/// the one file a link keeps for `-o`, or why `--dir` fits it instead.
pub(super) fn only(
    mut files: Vec<Remote>,
    parsed: &Parsed,
    listing: &Listing,
) -> Result<Remote, Error> {
    if files.len() == 1
        && let Some(remote) = files.pop()
    {
        return Ok(remote);
    }
    let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
    let folder = folder_for(None, None, listing, parsed, &paths);
    Err(Error::new(
        Kind::Usage,
        format!(
            "-o writes one file, and {} has {}",
            parsed.reference.shown(),
            crate::units::count(files.len() as u64, "file", "files")
        ),
    )
    .help(format!(
        "keep one with --select, or use `--dir {}` for all of them",
        folder.display()
    )))
}

fn got(
    destination: &Destination,
    remote: &Remote,
    sha256: Sha256,
    size: u64,
    origin: Origin,
    method: Option<Written>,
) -> Got {
    Got {
        path: destination
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size,
        sha256,
        // what get reports, so a secret in the link stays masked
        url: crate::net::Shown(&remote.url).to_string(),
        origin,
        method,
        unpacked_from: None,
    }
}

/// the file and the record of its folder, as the report gives them.
fn written(destination: &Destination, got: Got) -> (PathBuf, Done) {
    let folder = destination
        .path
        .parent()
        .map_or_else(|| destination.path.clone(), Path::to_path_buf);
    (
        destination.path.clone(),
        Done {
            folder,
            files: vec![got],
        },
    )
}

/// says that the file holds the bytes already, and leaves it as it is.
fn kept(context: &Context, wanted: Wanted, destination: &Destination, got: Got) -> (PathBuf, Done) {
    context.output.done(
        "Ok",
        &format!("{} already holds what {}", destination.shown, wanted.said()),
    );
    written(destination, got)
}

/// refuses the file at the path, which holds other bytes than `wanted` says, `digest`, or no
/// file fetchloom can read.
fn refused(shown: &str, existing: &Existing, wanted: Wanted, digest: &Sha256) -> Error {
    if let (Wanted::Named, Existing::Holds(found)) = (wanted, existing) {
        // a person typed the hash, so it is as likely to be wrong as the file
        return Error::new(
            Kind::LocalNotEmpty,
            format!(
                "{shown} already exists, and its bytes are not the ones --sha256 names: the hash or the file may be wrong"
            ),
        )
        .detail(format!(
            "--sha256 names {}, {shown} holds {}",
            digest.hex(),
            found.hex()
        ))
        .help("check the hash first. if it is right, choose another name with -o, or add --force to replace the file");
    }
    let (what, it) = match existing {
        Existing::Dangling(target) => (
            format!("is a link to {}, which does not exist", target.display()),
            "the link",
        ),
        Existing::Other => ("is a special file, such as a pipe".to_owned(), "it"),
        Existing::Missing | Existing::Holds(_) => return other_bytes(shown, wanted),
    };
    Error::new(
        Kind::LocalNotEmpty,
        format!("{shown} already exists and {what}"),
    )
    .help(format!(
        "add --force to replace {it}, or choose another name with -o"
    ))
}

/// refuses the file at the path, which holds other bytes than `wanted` says.
fn other_bytes(shown: &str, wanted: Wanted) -> Error {
    Error::new(
        Kind::LocalNotEmpty,
        format!(
            "{shown} already exists and holds other bytes than {}",
            wanted.said()
        ),
    )
    .help("add --force to replace it, or choose another name with -o")
}

fn unwritten(shown: &str, err: &io::Error) -> Error {
    if err.kind() == io::ErrorKind::AlreadyExists {
        return Error::new(
            Kind::LocalNotEmpty,
            format!("{shown} appeared while downloading, so it stays as it was"),
        )
        .help("add --force to replace it, or choose another name with -o");
    }
    Error::io(format_args!("could not write {shown}"), err)
}

/// gives `path` a writable file of its own holding `object`'s bytes: a clone or a copy, flushed
/// under a hidden name beside it, then named in a step that fails rather than replace a file that
/// appeared meanwhile. with `replace`, the new file takes the name of the one there in one rename,
/// and a drive that cannot rename over a file keeps the old one aside until the new one has the
/// name.
fn publish(
    object: &Path,
    path: &Path,
    replace: bool,
    threads: Threads,
    stop: &Arc<Stop>,
) -> io::Result<Method> {
    let folder = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a file to write needs a folder",
        )
    })?;
    // the person named the path, so its folder may be a link
    let mut placer = Placer::in_named_folder(folder)
        .with_threads(threads)
        .with_stop(Arc::clone(stop));
    let temporary =
        replace.then(|| folder.join(format!(".fetchloom-{:016x}.copy", fastrand::u64(..))));
    let destination = temporary.as_deref().unwrap_or(path);
    let size = fs::metadata(object)?.len();
    let outcome = placer.place_all(
        1,
        |_| Order {
            object: object.to_path_buf(),
            destination: destination.to_path_buf(),
            writable: true,
            size,
        },
        |_| (),
    );
    let (method, ()) = outcome
        .into_iter()
        .next()
        .unwrap_or_else(|| Err(io::Error::other("the file was not written")))?;
    placer.flush_copies()?;
    if let Some(temporary) = &temporary
        && let Err(err) = Volume::new(folder.to_path_buf()).replace(temporary, path)
    {
        let _ = placer.delete_file(temporary);
        return Err(err);
    }
    Ok(method)
}

/// whether the cache's copy of `digest` is this run's alone: downloaded by it and needed by no
/// project on this machine, so the file may take it, or the cache drop it once the file holds
/// its bytes. a later run downloads it again if it must.
fn one_off(context: &Context, cache: &Cache, digest: &Sha256) -> bool {
    if context.work.cached_before(digest) {
        return false;
    }
    match super::super::cache::needed_by_all(cache, None, &context.stop) {
        Ok(needed) => !needed.objects.contains(digest),
        Err(err) => {
            tracing::debug!(err = %err.message, "kept the download, since a lock could not be read");
            false
        }
    }
}

/// drops the cache's copy of `digest`, which the file now holds.
fn forget(cache: &Cache, digest: &Sha256) {
    if let Err(err) = cache.drop_object(digest) {
        tracing::debug!(%err, "could not drop the download from the cache");
    }
}

/// moves `object`, a download nothing else needs, from the cache to `path`, so a file of any size
/// costs no second copy of its bytes, and says whether it did. a folder of `path` that is not
/// there yet is made first, as [`publish`] makes it. it does not move across disks, for an
/// object with another name, as a project's hardlink is, which the file made writable would let
/// an edit reach, or on windows, where fetchloom cannot count a file's names; the caller copies
/// then. a name the file cannot take gives the object back to the cache as it was.
#[cfg(unix)]
fn take(object: &Path, path: &Path, replace: bool) -> io::Result<bool> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let Some(folder) = path.parent() else {
        return Ok(false);
    };
    Placer::in_named_folder(folder).check_folders(path, true)?;
    let taken = folder.join(format!(".fetchloom-{:016x}.take", fastrand::u64(..)));
    if fs::rename(object, &taken).is_err() {
        return Ok(false);
    }
    let give_back = |sealed: Option<fs::Permissions>| {
        if let Some(sealed) = sealed {
            let _ = fs::set_permissions(&taken, sealed);
        }
        if fs::rename(&taken, object).is_err() {
            let _ = fs::remove_file(&taken);
        }
    };
    // a link made to the object before it moved shows here, and none can be made after
    let sealed = match fs::symlink_metadata(&taken) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => metadata.permissions(),
        _ => {
            give_back(None);
            return Ok(false);
        }
    };
    let writable = fs::Permissions::from_mode(sealed.mode() | 0o200);
    if fs::set_permissions(&taken, writable).is_err() {
        give_back(Some(sealed));
        return Ok(false);
    }
    let volume = Volume::new(folder.to_path_buf());
    let named = if replace {
        volume.replace(&taken, path)
    } else {
        volume.rename_new(&taken, path)
    };
    if let Err(err) = named {
        give_back(Some(sealed));
        return Err(err);
    }
    Ok(true)
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "windows always copies, where unix may fail to name the file"
)]
fn take(_object: &Path, _path: &Path, _replace: bool) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_takes_one_link_and_a_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("downloads");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("notes.txt"), "mine").unwrap();
        let folder = folder.display().to_string();
        let separator = std::path::MAIN_SEPARATOR;
        let ending = format!("downloads{separator}");
        let iris = "https://example.org/data/iris.csv";
        let hugging = "https://huggingface.co/datasets/scikit-learn/iris/blob/main/Iris.csv";
        for (path, links, listed, message, help) in [
            (
                "-",
                &[iris][..],
                false,
                "get checks a file before it appears, so -o cannot be - for stdout".to_owned(),
                "give -o a file name".to_owned(),
            ),
            (
                "a.csv",
                &[iris, iris],
                false,
                "-o writes one file, and this get has 2 links".to_owned(),
                "use --dir <folder> instead".to_owned(),
            ),
            (
                "a.csv",
                &[],
                true,
                "-o writes one file, and this get reads its links from a list".to_owned(),
                "use --dir <folder> instead".to_owned(),
            ),
            // a folder holding files of its own would refuse --dir, so the file is named in it
            (
                &ending,
                &[iris],
                false,
                format!("-o needs a file name, and {ending} names a folder"),
                format!("name the file inside it, as in `-o downloads{separator}iris.csv`"),
            ),
            (
                ".",
                &[iris],
                false,
                "names a folder".to_owned(),
                format!("as in `-o .{separator}iris.csv`"),
            ),
            (
                "..",
                &["zenodo:3608990"],
                false,
                "names a folder".to_owned(),
                format!("as in `-o ..{separator}<file>`"),
            ),
            (
                &folder,
                &[hugging],
                false,
                "names a folder".to_owned(),
                format!("as in `-o {folder}{separator}Iris.csv`"),
            ),
        ] {
            let links: Vec<String> = links.iter().map(|&link| link.to_owned()).collect();
            let error = usage(Path::new(path), &links, listed).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{path}");
            assert!(
                error.message.contains(&message),
                "{path}: {}",
                error.message
            );
            assert!(
                error
                    .help
                    .as_deref()
                    .is_some_and(|said| said.contains(&help)),
                "{path}: {:?}",
                error.help
            );
        }
        let one = [iris.to_owned()];
        let long = "x".repeat(300);
        assert_eq!(
            usage(Path::new(&long), &one, false).unwrap_err().kind,
            Kind::Usage
        );
        for path in ["a.csv", "new/a.csv", ".hidden"] {
            usage(Path::new(path), &one, false).unwrap();
        }
    }

    /// a file at the path whose size is not the one the source states holds other bytes, so it
    /// is refused before any download. one of the same or an unknown size waits for the download,
    /// and `--force` replaces whatever is there.
    #[test]
    fn a_file_of_another_size_than_stated_is_refused_before_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.csv");
        fs::write(&path, b"mine").unwrap();
        let destination = Destination {
            shown: "data.csv".to_owned(),
            path,
        };
        let context = crate::cli::project::tests::offline_context();
        let mut remote = crate::cli::project::tests::stated(
            "iris.csv",
            "https://example.org/iris.csv",
            b"0123456789",
        );
        remote.sha256 = None;
        let Err(error) = unhashed(&context, &remote, false, &destination) else {
            panic!("a file of another size went to the download");
        };
        assert_eq!(
            (error.kind, error.message.as_str(), error.details.as_slice()),
            (
                Kind::LocalNotEmpty,
                "data.csv already exists and holds other bytes than the link serves now",
                &["data.csv is 4 B, and the link's file 10 B".to_owned()][..]
            )
        );
        for (size, force) in [(Some(10), true), (Some(4), false), (None, false)] {
            remote.size = size;
            assert!(
                unhashed(&context, &remote, force, &destination).is_ok(),
                "{size:?} {force}"
            );
        }
    }

    #[test]
    fn an_existing_file_stays_unless_force_replaces_other_bytes() {
        let wanted = crate::digest::sha256(b"served");
        let other = crate::digest::sha256(b"mine");
        for (existing, force, step) in [
            (Existing::Missing, false, Step::Write),
            (Existing::Missing, true, Step::Write),
            (Existing::Holds(wanted), false, Step::Keep),
            (Existing::Holds(wanted), true, Step::Keep),
            (Existing::Holds(other), false, Step::Refuse),
            (Existing::Holds(other), true, Step::Replace),
            (
                Existing::Dangling(PathBuf::from("nowhere")),
                false,
                Step::Refuse,
            ),
            (
                Existing::Dangling(PathBuf::from("nowhere")),
                true,
                Step::Replace,
            ),
            (Existing::Other, false, Step::Refuse),
            (Existing::Other, true, Step::Replace),
        ] {
            assert_eq!(
                decide(&existing, &wanted, force),
                step,
                "{existing:?} {force}"
            );
        }
    }

    #[test]
    fn a_dataset_of_several_files_names_dir() {
        let parsed = Reference::parse("s3://bucket/iris/").unwrap();
        let listing = Listing {
            within: Vec::new(),
            access: crate::source::Access::Unknown,
            resolved: parsed.reference.clone(),
            title: Some("Iris".to_owned()),
            license: None,
            doi: None,
            files: Vec::new(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
        };
        let remote = |path: &str| Remote {
            path: path.to_owned(),
            size: Some(1),
            sha256: None,
            md5: None,
            url: url::Url::parse(&format!("https://example.org/{path}")).unwrap(),
            named_by_server: false,
            etag: None,
        };
        let error = only(vec![remote("a.csv"), remote("b.csv")], &parsed, &listing).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "-o writes one file, and s3://bucket/iris/ has 2 files"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("keep one with --select, or use `--dir iris` for all of them")
        );
        assert_eq!(
            only(vec![remote("a.csv")], &parsed, &listing).unwrap().path,
            "a.csv"
        );
    }

    #[test]
    fn writing_never_replaces_a_file_that_appeared_and_force_leaves_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let object = dir.path().join("object");
        fs::write(&object, b"served").unwrap();
        let folder = dir.path().join("out");
        fs::create_dir(&folder).unwrap();
        let path = folder.join("data.csv");
        fs::write(&path, b"mine").unwrap();
        let stop = Arc::new(Stop::default());
        let error = publish(&object, &path, false, Threads::default(), &stop).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"mine");
        let names = || -> Vec<String> {
            fs::read_dir(&folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names(), ["data.csv"], "no temporary is left behind");
        publish(&object, &path, true, Threads::default(), &stop).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"served");
        assert!(!fs::metadata(&path).unwrap().permissions().readonly());
        assert_eq!(names(), ["data.csv"]);
        assert_eq!(fs::read(&object).unwrap(), b"served", "the object stays");
    }

    /// --force replaces a link at the path with the file, never the file it leads to.
    #[cfg(unix)]
    #[test]
    fn force_replaces_a_link_and_leaves_what_it_led_to() {
        let dir = tempfile::tempdir().unwrap();
        let object = dir.path().join("object");
        fs::write(&object, b"served").unwrap();
        let elsewhere = dir.path().join("elsewhere.csv");
        fs::write(&elsewhere, b"mine").unwrap();
        let path = dir.path().join("data.csv");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        let stop = Arc::new(Stop::default());
        publish(&object, &path, true, Threads::default(), &stop).unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(fs::read(&path).unwrap(), b"served");
        assert_eq!(fs::read(&elsewhere).unwrap(), b"mine");
    }

    /// the names in `folder`, sorted.
    #[cfg(unix)]
    fn names_in(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// an object as the cache keeps one, sealed read-only, in `objects/` below `dir`.
    #[cfg(unix)]
    fn sealed(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let objects = dir.join("objects");
        fs::create_dir(&objects).unwrap();
        let object = objects.join("object");
        fs::write(&object, b"served").unwrap();
        fs::set_permissions(&object, fs::Permissions::from_mode(0o444)).unwrap();
        object
    }

    /// a download nothing else needs becomes the file itself, so its bytes are never written a
    /// second time, and the file is the person's to edit.
    #[cfg(unix)]
    #[test]
    fn a_one_off_moves_out_of_the_cache_rather_than_being_copied() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().unwrap();
        let object = sealed(dir.path());
        let inode = fs::metadata(&object).unwrap().ino();
        let path = dir.path().join("data.csv");
        assert!(take(&object, &path, false).unwrap());
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.ino(), inode, "the same bytes, never copied");
        assert!(!metadata.permissions().readonly());
        assert_eq!(fs::read(&path).unwrap(), b"served");
        assert_eq!(names_in(dir.path()), ["data.csv", "objects"]);
        assert_eq!(names_in(&dir.path().join("objects")), Vec::<String>::new());
    }

    /// a folder `-o` names that is not there yet is made before the move, so the download still
    /// moves rather than being copied once a copy has made the folder.
    #[cfg(unix)]
    #[test]
    fn a_one_off_moves_into_a_folder_made_for_it() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().unwrap();
        let object = sealed(dir.path());
        let inode = fs::metadata(&object).unwrap().ino();
        let folder = dir.path().join("new").join("sub");
        let path = folder.join("data.csv");
        assert!(take(&object, &path, false).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(names_in(&folder), ["data.csv"]);
        assert_eq!(names_in(&dir.path().join("objects")), Vec::<String>::new());
    }

    /// with --force the download takes the name of the file there, in one rename.
    #[cfg(unix)]
    #[test]
    fn force_moves_a_one_off_over_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let object = sealed(dir.path());
        let path = dir.path().join("data.csv");
        fs::write(&path, b"mine").unwrap();
        assert!(take(&object, &path, true).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"served");
        assert_eq!(names_in(dir.path()), ["data.csv", "objects"]);
    }

    /// an object with another name, as a project's hardlink is, stays in the cache sealed: the
    /// file made writable would let an edit reach that project.
    #[cfg(unix)]
    #[test]
    fn an_object_with_another_name_stays_in_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let object = sealed(dir.path());
        fs::hard_link(&object, dir.path().join("placed.csv")).unwrap();
        let path = dir.path().join("data.csv");
        assert!(!take(&object, &path, false).unwrap());
        assert!(fs::metadata(&object).unwrap().permissions().readonly());
        assert_eq!(names_in(dir.path()), ["objects", "placed.csv"]);
    }

    /// a file that appeared at the path keeps its bytes, and the object goes back sealed, so a
    /// later run still finds the download.
    #[cfg(unix)]
    #[test]
    fn a_file_that_appeared_meanwhile_stays_and_the_object_goes_back() {
        let dir = tempfile::tempdir().unwrap();
        let object = sealed(dir.path());
        let path = dir.path().join("data.csv");
        fs::write(&path, b"mine").unwrap();
        let error = take(&object, &path, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"mine");
        assert_eq!(fs::read(&object).unwrap(), b"served");
        assert!(fs::metadata(&object).unwrap().permissions().readonly());
        assert_eq!(names_in(dir.path()), ["data.csv", "objects"]);
    }

    /// kaggle may send a competition's file inside a zip, which `-o` would write under the
    /// file's name.
    #[test]
    fn a_file_sent_inside_a_zip_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let mut zip = ::zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("train.csv", ::zip::write::SimpleFileOptions::default())
            .unwrap();
        io::Write::write_all(&mut zip, b"a,b\n1,2\n").unwrap();
        let zipped = crate::cache::tests::store(&cache, &zip.finish().unwrap().into_inner());
        let plain = crate::cache::tests::store(&cache, b"a,b\n1,2\n");
        let remote = Remote {
            path: "train.csv".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: url::Url::parse(
                "https://api.kaggle.com/v1/competitions/data/download/titanic/train.csv",
            )
            .unwrap(),
            named_by_server: false,
            etag: None,
        };
        let error = refuse_zipped(&cache, &remote, &zipped.digest).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.help.as_deref(),
            Some("use `--dir train` to unpack it into a folder")
        );
        refuse_zipped(&cache, &remote, &plain.digest).unwrap();
    }

    /// another run of the same link may write the file between this run's look at the path and
    /// its own write, taking the bytes out of the cache or not: a file holding the same bytes is
    /// kept, as a rerun keeps it, and one holding others stays as it is.
    #[test]
    fn a_file_another_run_wrote_meanwhile_is_kept_only_with_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = Context::new(crate::output::Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            crate::output::Level::Quiet,
            false,
            false,
        ));
        let served = b"served";
        let remote = Remote {
            path: "data.csv".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: url::Url::parse("https://example.org/data.csv").unwrap(),
            named_by_server: false,
            etag: None,
        };
        let fetched = Fetched {
            digest: crate::digest::sha256(served),
            size: served.len() as u64,
            moved: 0,
            name: None,
            origin: Origin::Cache,
            version: None,
            arrived: None,
        };
        // the path was free when the run looked, before it waited for the other run
        let known = Known {
            found: Some(Existing::Missing),
            held: None,
        };
        for (index, (cached, there)) in [
            (true, &served[..]),
            (false, &served[..]),
            (true, &b"mine"[..]),
            (false, &b"mine"[..]),
        ]
        .into_iter()
        .enumerate()
        {
            if cached {
                crate::cache::tests::store(&cache, served);
            } else if cache.find(&fetched.digest, None).is_some() {
                cache.drop_object(&fetched.digest).unwrap();
            }
            let path = dir.path().join(index.to_string()).join("data.csv");
            fs::create_dir(path.parent().unwrap()).unwrap();
            fs::write(&path, there).unwrap();
            let destination = Destination {
                shown: path.display().to_string(),
                path: path.clone(),
            };
            let settled = settle(
                &context,
                &cache,
                &remote,
                &fetched,
                false,
                &destination,
                &known,
            );
            if there == served {
                let (written, done) = settled.unwrap();
                assert_eq!(written, path, "{index}");
                assert_eq!(done.files[0].method, None, "{index}: nothing was written");
            } else {
                assert!(settled.is_err(), "{index}");
            }
            assert_eq!(fs::read(&path).unwrap(), there, "{index}");
        }
    }
}
