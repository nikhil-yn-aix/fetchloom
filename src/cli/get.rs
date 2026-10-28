//! `fetchloom get <link>`: a dataset or a file into a new folder, once, through the same cache as
//! projects. a second run asks the server whether anything changed and moves no bytes when
//! nothing did.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::args::Get;
use super::fetch::{self, Job};
use super::sync as engine;
use super::{Context, open_cache};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{self, Expect, Fetch, Fetched, Origin};
use crate::place::{Method, Placer};
use crate::project::manifest::{self, Manifest};
use crate::source::{self, Listing, Parsed, Reference, Remote};
use crate::units;
use crate::work::Work;

/// what `get` did, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    folder: PathBuf,
    files: Vec<Got>,
}

#[derive(Serialize)]
struct Got {
    path: String,
    size: u64,
    sha256: Sha256,
    url: String,
    origin: Origin,
    /// how the file was put into the folder, or none when it was there already.
    method: Option<Method>,
}

/// what happened to one file in the folder.
#[derive(Clone, Copy)]
enum Outcome {
    Placed(Method),
    /// an earlier version, which the cache still holds, was swapped for this one.
    Replaced(Method),
    AlreadyThere,
}

pub(super) async fn run(context: &Context, args: Get) -> Result<Report, Error> {
    let parsed = Reference::parse(&args.link)?;
    let cache = open_cache()?;
    let net = Arc::new(context.net()?);
    net.trust(parsed.reference.written_host());
    let listing = parsed.reference.resolve(&*net).await?;
    engine::tell_resolved(&context.output, &listing);
    let files = selected(&args, &parsed, &listing)?;
    let chosen = args.output.as_deref();
    // the folder is checked before any byte arrives, so a refusal costs no download
    let guessed: Vec<String> = files.iter().map(|remote| remote.path.clone()).collect();
    check(&folder_for(chosen, &listing, &parsed, &guessed), &guessed)?;
    let size: u64 = files.iter().filter_map(|remote| remote.size).sum();
    super::confirm(
        &context.output,
        context.yes,
        &units::count(files.len() as u64, "file", "files"),
        size,
    )?;
    let jobs = files.iter().map(job).collect();
    let started = Instant::now();
    let fetched = fetch::all(context, &net, &cache, jobs).await?;
    let paths: Vec<String> = files
        .iter()
        .zip(&fetched)
        .map(
            |(remote, fetched)| match fetched.name.as_deref().and_then(name::sanitize) {
                Some(given) if remote.named_by_server => given,
                _ => remote.path.clone(),
            },
        )
        .collect();
    let folder = folder_for(chosen, &listing, &parsed, &paths);
    tell_fetched(context, &paths, &fetched, &files, started.elapsed());
    check(&folder, &paths)?;
    let outcomes = put_all(context, &cache, &folder, &paths, &fetched)?;
    let folder = std::path::absolute(&folder).unwrap_or(folder);
    context.output.answer(&folder.display().to_string());
    Ok(Report {
        folder,
        files: paths
            .into_iter()
            .zip(fetched)
            .zip(files)
            .zip(outcomes)
            .map(|(((path, fetched), remote), outcome)| Got {
                path,
                size: fetched.size,
                sha256: fetched.digest,
                url: remote.url.to_string(),
                origin: fetched.origin,
                method: match outcome {
                    Outcome::Placed(method) | Outcome::Replaced(method) => Some(method),
                    Outcome::AlreadyThere => None,
                },
            })
            .collect(),
    })
}

/// the files `--select` and `--exclude` keep, or the one file a pasted link names.
fn selected(args: &Get, parsed: &Parsed, listing: &Listing) -> Result<Vec<Remote>, Error> {
    let select = if args.filter.select.is_empty() {
        parsed.file.iter().cloned().collect()
    } else {
        args.filter.select.clone()
    };
    if select.is_empty() && args.filter.exclude.is_empty() {
        return Ok(listing.files.clone());
    }
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "get",
        "get",
        &select,
        &args.filter.exclude,
    )?)?;
    let mut entry = alone.datasets["get"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.to_string(), &entry, listing)
}

/// where the files go: the folder asked for, else one named after the file or the dataset.
fn folder_for(
    chosen: Option<&Path>,
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
            .and_then(name::dataset_name)
            .or_else(|| name::dataset_name(&parsed.reference.to_string()))
            .unwrap_or_else(|| "download".to_owned()),
    };
    PathBuf::from(name)
}

/// the download of one listed file. a file no checksum pins is asked about again on later runs,
/// so a changed file is noticed and an unchanged one moves no bytes.
fn job(remote: &Remote) -> Job {
    let (from, divert) = source::fetch_plan(&remote.url);
    Job {
        fetch: Fetch {
            url: remote.url.clone(),
            from,
            divert,
            expect: Expect {
                sha256: remote.sha256,
                md5: remote.md5,
                size: remote.size,
            },
            revalidate: remote.sha256.is_none() && remote.md5.is_none(),
        },
        label: remote.path.clone(),
        dataset: None,
    }
}

/// refuses a folder that holds anything but these files, or a file where the folder should be.
fn check(folder: &Path, paths: &[String]) -> Result<(), Error> {
    match fs::metadata(folder) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(local(folder, &err)),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::new(
                Kind::LocalNotEmpty,
                format!("{} already exists and is not a folder", folder.display()),
            )
            .help("choose another folder with -o"));
        }
        Ok(_) => {}
    }
    let tops: std::collections::HashSet<&str> = paths
        .iter()
        .map(|path| path.split('/').next().unwrap_or(path))
        .collect();
    let mut others: Vec<String> = fs::read_dir(folder)
        .map_err(|err| local(folder, &err))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !tops.contains(name.as_str()))
        .collect();
    if others.is_empty() {
        return Ok(());
    }
    others.sort();
    Err(Error::new(
        Kind::LocalNotEmpty,
        format!("{} already holds other files", folder.display()),
    )
    .detail(format!("such as {}", others[0]))
    .help("choose another folder with -o, or empty this one"))
}

/// puts every fetched file into the folder, and says how.
fn put_all(
    context: &Context,
    cache: &Cache,
    folder: &Path,
    paths: &[String],
    fetched: &[Fetched],
) -> Result<Vec<Outcome>, Error> {
    let mut placer = Placer::new(folder);
    let mut outcomes = Vec::with_capacity(paths.len());
    for (path, fetched) in paths.iter().zip(fetched) {
        if let Some(reason) = name::unstorable(path) {
            return Err(Error::new(
                Kind::LocalIo,
                format!("{path} cannot be stored on this machine: {reason}"),
            )
            .help("leave it out with --exclude"));
        }
        outcomes.push(put(
            &context.work,
            cache,
            &mut placer,
            fetched,
            &folder.join(path),
        )?);
    }
    tell_placed(context, &placer, folder, paths, fetched, &outcomes);
    Ok(outcomes)
}

fn put(
    work: &Work,
    cache: &Cache,
    placer: &mut Placer,
    fetched: &Fetched,
    destination: &Path,
) -> Result<Outcome, Error> {
    let mut replaced = false;
    if fs::symlink_metadata(destination).is_ok() {
        if holds(destination, fetched)? {
            return Ok(Outcome::AlreadyThere);
        }
        if !replaceable(destination, cache)? {
            return Err(Error::new(
                Kind::LocalNotEmpty,
                format!(
                    "{} already exists with other contents",
                    destination.display()
                ),
            )
            .help("move it away to fetch this file, or choose another folder with -o"));
        }
        fs::remove_file(destination).map_err(|err| local(destination, &err))?;
        replaced = true;
    }
    let method = placer
        .place(&fetched.path, destination, false)
        .map_err(|err| local(destination, &err))?;
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
    Ok(if replaced {
        Outcome::Replaced(method)
    } else {
        Outcome::Placed(method)
    })
}

/// whether the file at `path` already holds the fetched bytes. a link or a copy keeps the
/// object's size and modification time, so only a mismatch costs a hash.
fn holds(path: &Path, fetched: &Fetched) -> Result<bool, Error> {
    let there = fs::metadata(path).map_err(|err| local(path, &err))?;
    if !there.is_file() || there.len() != fetched.size {
        return Ok(false);
    }
    let object = fs::metadata(&fetched.path).map_err(|err| local(&fetched.path, &err))?;
    if let (Ok(a), Ok(b)) = (there.modified(), object.modified())
        && a == b
    {
        return Ok(true);
    }
    Ok(Sha256::of_file(path).map_err(|err| local(path, &err))? == fetched.digest)
}

/// whether the file at `path` may be replaced: only when the cache holds its bytes, so nothing
/// is ever lost.
fn replaceable(path: &Path, cache: &Cache) -> Result<bool, Error> {
    let size = fs::metadata(path).map_err(|err| local(path, &err))?.len();
    let digest = Sha256::of_file(path).map_err(|err| local(path, &err))?;
    Ok(cache.find(&digest, Some(size)).is_some())
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
            Origin::Cache => output.step("Found", &format!("{path} · in the cache")),
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

fn tell_placed(
    context: &Context,
    placer: &Placer,
    folder: &Path,
    paths: &[String],
    fetched: &[Fetched],
    outcomes: &[Outcome],
) {
    let output = &context.output;
    let copied: u64 = outcomes
        .iter()
        .zip(fetched)
        .filter(|(outcome, _)| {
            matches!(
                outcome,
                Outcome::Placed(Method::Copy) | Outcome::Replaced(Method::Copy)
            )
        })
        .map(|(_, fetched)| fetched.size)
        .sum();
    let how = if copied > 0 {
        format!("copied, {}", units::size(copied))
    } else {
        "linked, no extra disk".to_owned()
    };
    let changed = outcomes
        .iter()
        .filter(|outcome| !matches!(outcome, Outcome::AlreadyThere))
        .count();
    match (paths, outcomes) {
        (_, _) if changed == 0 => {
            let shown = match paths {
                [only] => folder.join(only).display().to_string(),
                _ => folder.display().to_string(),
            };
            output.done(&format!("{shown} is already up to date"));
            return;
        }
        ([only], [outcome]) => {
            let verb = if matches!(outcome, Outcome::Replaced(_)) {
                "Replaced"
            } else {
                "Placed"
            };
            output.step(verb, &format!("{} · {how}", folder.join(only).display()));
        }
        _ => output.step(
            "Placed",
            &format!(
                "{} · {} {how}",
                folder.display(),
                units::count(changed as u64, "file", "files")
            ),
        ),
    }
    let hint = if copied == 0 {
        None
    } else if let Some(client) = placer.synced() {
        let things = if paths.len() == 1 {
            "the file was"
        } else {
            "files were"
        };
        output.warn(&format!(
            "{} is inside {client}, which breaks links, so {things} copied",
            folder.display()
        ));
        Some(format!(
            "a folder outside {client} gets links that use no extra disk"
        ))
    } else if placer.other_volume() {
        Some("the cache is on another drive. set FETCHLOOM_CACHE to a folder on this drive to link files instead of copying them".to_owned())
    } else {
        None
    };
    let what = match paths {
        [only] => only.clone(),
        _ => units::count(paths.len() as u64, "file", "files"),
    };
    output.done(&format!("fetched {what} into {}", folder.display()));
    if let Some(hint) = hint {
        output.hint(&hint);
    }
}

fn local(path: &Path, err: &io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not write {}: {err}", path.display()),
    )
}
