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
    Done, Got, Written, approve, folder_for, matches_sha256, one_file, selected, tell_fetched, vet,
};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Fetched, Net, Origin};
use crate::place::{Method, Order, Placer};
use crate::source::{Listing, Parsed, Remote};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::volume::Volume;
use crate::work::Work;

/// refuses an `-o` that cannot name the one file to write, before anything is asked of a source:
/// stdout, several links or a list of them, a folder, or a name this machine's disks cannot
/// store.
pub(super) fn usage(path: &Path, links: usize, listed: bool) -> Result<(), Error> {
    if path == Path::new("-") {
        return Err(Error::new(
            Kind::Usage,
            "get checks a file before it appears, so -o cannot be - for stdout",
        )
        .help("give -o a file name, then read that file; get prints its path"));
    }
    if listed || links > 1 {
        let several = if listed {
            "reads its links from a list".to_owned()
        } else {
            format!("has {links} links")
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
        return Err(Error::new(
            Kind::Usage,
            format!("-o needs a file name, and {shown} names a folder"),
        )
        .help(format!(
            "use --dir {shown} to put the download in that folder"
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
    Ok(())
}

/// what is at the path `-o` names.
#[derive(Debug)]
enum Existing {
    Missing,
    /// a file, or a link to one, by the hash of its bytes.
    Holds(Sha256),
    /// a link that leads nowhere, or a special file such as a pipe.
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

/// looks at `path`, hashing a file there. a link is followed for its bytes, so a file reached
/// through one that holds them stays; `--force` replaces the link itself.
fn existing(path: &Path) -> Result<Existing, Error> {
    let unreadable =
        |err: &io::Error| Error::io(format_args!("could not read {}", path.display()), err);
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(Error::new(
            Kind::LocalNotEmpty,
            format!("{} became a folder while downloading", path.display()),
        )
        .help("choose another name with -o")),
        Ok(metadata) if metadata.is_file() => Sha256::of_file(path)
            .map(Existing::Holds)
            .map_err(|err| unreadable(&err)),
        Ok(_) => Ok(Existing::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(path) {
            Ok(_) => Ok(Existing::Other),
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

/// fetches the one file `listing` keeps and writes it at `path`, unless a file there holds its
/// bytes already. returns the file's absolute path and what it holds.
pub(super) async fn one(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    args: &Get,
    path: &Path,
    parsed: &Parsed,
    listing: &Listing,
) -> Result<(PathBuf, Done), Error> {
    let remote = only(selected(args, parsed, listing)?, parsed, listing)?;
    if let Some(expected) = args.sha256 {
        one_file(std::slice::from_ref(&remote), expected, parsed)?;
    }
    let shown = path.display().to_string();
    let destination = Destination {
        path: std::path::absolute(path)
            .map_err(|err| Error::io(format_args!("could not find where {shown} is"), &err))?,
        shown,
    };
    if let Some(kept) = decided_early(context, cache, &remote, args, &destination)? {
        return Ok(kept);
    }
    let fetched = download(context, net, cache, &remote).await?;
    matches_sha256(&remote.path, &fetched.digest, args.sha256)?;
    refuse_zipped(cache, &remote, &fetched.digest)?;
    settle(context, cache, &remote, &fetched, args.force, &destination)
}

/// what a hash known before downloading decides about a file already at the path, with no
/// request: none when it decides nothing, as when the path is free or the file must go.
fn decided_early(
    context: &Context,
    cache: &Cache,
    remote: &Remote,
    args: &Get,
    destination: &Destination,
) -> Result<Option<(PathBuf, Done)>, Error> {
    let recorded = || {
        context
            .offline
            .enabled()
            .then(|| cache.url_record(remote.url.as_str()))
            .flatten()
            .map(|record| (record.sha256, Wanted::Recorded))
    };
    let known = match (args.sha256, remote.sha256) {
        (Some(named), _) => Some((named, Wanted::Named)),
        (None, Some(stated)) => Some((stated, Wanted::Stated)),
        (None, None) => recorded(),
    };
    let Some((digest, wanted)) = known else {
        return Ok(None);
    };
    let found = existing(&destination.path)?;
    match decide(&found, &digest, args.force) {
        Step::Keep => {
            let size = fs::metadata(&destination.path)
                .map_err(|err| {
                    Error::io(format_args!("could not read {}", destination.shown), &err)
                })?
                .len();
            let got = got(destination, remote, digest, size, Origin::Cache, None);
            Ok(Some(kept(context, wanted, destination, got)))
        }
        Step::Refuse => Err(refused(&destination.shown, &found, wanted)),
        Step::Write | Step::Replace => Ok(None),
    }
}

/// downloads the one file into the cache, or finds it there, and says so.
async fn download(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    remote: &Remote,
) -> Result<Fetched, Error> {
    let job = fetch::listed_job(remote, &remote.path, false);
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
        started.elapsed(),
    );
    Ok(fetched)
}

/// keeps, refuses or writes the file once the bytes the link serves are in the cache.
fn settle(
    context: &Context,
    cache: &Cache,
    remote: &Remote,
    fetched: &Fetched,
    force: bool,
    destination: &Destination,
) -> Result<(PathBuf, Done), Error> {
    let found = existing(&destination.path)?;
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
        return Err(refused(&destination.shown, &found, Wanted::Served));
    }
    if step == Step::Keep {
        if one_off(context, cache, &fetched.digest) {
            forget(cache, &fetched.digest);
        }
        return Ok(kept(context, Wanted::Served, destination, record(None)));
    }
    let object = cache
        .find(&fetched.digest, Some(fetched.size))
        .ok_or_else(|| {
            Error::new(
                Kind::LocalIo,
                format!("the cache lost {} while writing it", remote.path),
            )
            .help("run the same command again to download it")
        })?;
    let replace = step == Step::Replace;
    let one_off = one_off(context, cache, &fetched.digest);
    let moved = one_off
        && take(&object, &destination.path, replace)
            .map_err(|err| unwritten(&destination.shown, &err))?;
    let how = if moved {
        Written::Move
    } else {
        let method = publish(
            &object,
            &destination.path,
            replace,
            context.threads,
            &context.stop,
        )
        .map_err(|err| unwritten(&destination.shown, &err))?;
        let counter = match method {
            Method::Reflink => &context.work.reflinks,
            Method::Hardlink => &context.work.hardlinks,
            Method::Copy => &context.work.copies,
        };
        Work::add(counter, 1);
        if context.work.cached_before(&fetched.digest) {
            Work::add(&context.work.placed_from_cache, 1);
        }
        if one_off {
            forget(cache, &fetched.digest);
        }
        Written::from(method)
    };
    context.output.done(
        if replace { "Replaced" } else { "Saved" },
        &destination.shown,
    );
    Ok(written(destination, record(Some(how))))
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
        "use --dir {} to unpack it into a folder",
        crate::name::folder_for(&remote.path)
    )))
}

/// the one file a link keeps for `-o`, or why `--dir` fits it instead.
fn only(mut files: Vec<Remote>, parsed: &Parsed, listing: &Listing) -> Result<Remote, Error> {
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
        "keep one with --select, or use --dir {} for all of them",
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

fn refused(shown: &str, existing: &Existing, wanted: Wanted) -> Error {
    let what = match existing {
        Existing::Other => "is no file fetchloom can read".to_owned(),
        Existing::Missing | Existing::Holds(_) => {
            format!("holds other bytes than {}", wanted.said())
        }
    };
    Error::new(
        Kind::LocalNotEmpty,
        format!("{shown} already exists and {what}"),
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
        Ok(Some(needed)) if !needed.objects.contains(digest) => true,
        Ok(_) => {
            tracing::debug!("kept the download, since a project may need it");
            false
        }
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
/// costs no second copy of its bytes, and says whether it did. it does not across disks, for an
/// object with another name, as a project's hardlink is, which the file made writable would let
/// an edit reach, or on windows, where fetchloom cannot count a file's names; the caller copies
/// then. a name the file cannot take gives the object back to the cache as it was.
#[cfg(unix)]
fn take(object: &Path, path: &Path, replace: bool) -> io::Result<bool> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let Some(folder) = path.parent() else {
        return Ok(false);
    };
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
fn take(_object: &Path, _path: &Path, _replace: bool) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Reference;

    #[test]
    fn output_takes_one_link_and_a_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("downloads");
        fs::create_dir(&folder).unwrap();
        let folder = folder.display().to_string();
        let ending = format!("downloads{}", std::path::MAIN_SEPARATOR);
        for (path, links, listed, message, help) in [
            (
                "-",
                1,
                false,
                "get checks a file before it appears, so -o cannot be - for stdout",
                "give -o a file name",
            ),
            (
                "a.csv",
                2,
                false,
                "-o writes one file, and this get has 2 links",
                "use --dir <folder> instead",
            ),
            (
                "a.csv",
                0,
                true,
                "-o writes one file, and this get reads its links from a list",
                "use --dir <folder> instead",
            ),
            (&ending, 1, false, "names a folder", "use --dir downloads"),
            (".", 1, false, "names a folder", "use --dir ."),
            ("..", 1, false, "names a folder", "use --dir .."),
            (&folder, 1, false, "names a folder", "use --dir "),
        ] {
            let error = usage(Path::new(path), links, listed).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{path}");
            assert!(error.message.contains(message), "{path}: {}", error.message);
            assert!(
                error
                    .help
                    .as_deref()
                    .is_some_and(|said| said.contains(help)),
                "{path}: {:?}",
                error.help
            );
        }
        let long = "x".repeat(300);
        assert_eq!(
            usage(Path::new(&long), 1, false).unwrap_err().kind,
            Kind::Usage
        );
        for path in ["a.csv", "new/a.csv", ".hidden"] {
            usage(Path::new(path), 1, false).unwrap();
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
        };
        let error = only(vec![remote("a.csv"), remote("b.csv")], &parsed, &listing).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "-o writes one file, and s3://bucket/iris/ has 2 files"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("keep one with --select, or use --dir iris for all of them")
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
        };
        let error = refuse_zipped(&cache, &remote, &zipped.digest).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.help.as_deref(),
            Some("use --dir train to unpack it into a folder")
        );
        refuse_zipped(&cache, &remote, &plain.digest).unwrap();
    }
}
