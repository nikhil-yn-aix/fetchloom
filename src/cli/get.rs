//! `fetchloom get <link>`: one download into a new folder, through the same cache as projects.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use url::Url;

use super::Context;
use super::args::Get;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{self, Event, Fetched, Net, Origin};
use crate::place::{Method, Placer};
use crate::reference::Reference;
use crate::units;
use crate::work::Work;

/// a download larger than this asks first.
const ASK_ABOVE: u64 = 1024 * 1024 * 1024;

/// what `get` did, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    folder: PathBuf,
    files: Vec<Placed>,
}

#[derive(Serialize)]
struct Placed {
    path: String,
    size: u64,
    sha256: Sha256,
    url: String,
    origin: Origin,
    /// how the file was put into the folder, or none when it was there already.
    method: Option<Method>,
}

/// what happened to the file in the folder.
enum Outcome {
    Placed(Method),
    /// an earlier version, which the cache still holds, was swapped for this one.
    Replaced(Method),
    AlreadyThere,
}

pub(super) async fn run(context: &Context, args: Get) -> Result<Report, Error> {
    let Reference::Url(url) = Reference::parse(&args.link)?;
    let cache = super::open_cache()?;
    let net = Net::new(context.offline, context.work.clone(), context.stop.clone())?;
    net.typed(&url);
    let guess = name::from_url(&url).unwrap_or_else(|| "download".to_owned());
    let chosen = args.output.as_deref();
    let bar = context
        .output
        .bar(format!("{guess} from {}", net::host(&url)));
    let asked = AtomicBool::new(false);
    let on = |event: Event| -> Result<(), Error> {
        match event {
            Event::Waiting => {
                bar.message(format!("waiting for another fetchloom to download {guess}"));
            }
            Event::Started {
                total,
                resumed,
                name,
            } => {
                let file = file_name(name.as_deref(), &guess);
                // the folder is checked before any byte arrives, so a refusal costs no download
                let folder = folder_for(&file, chosen);
                check(&folder, &file)?;
                if let Some(total) = total {
                    refuse_early(&folder.join(&file), total, &cache)?;
                }
                if let Some(total) = total
                    && total.saturating_sub(resumed) > ASK_ABOVE
                    && !context.yes
                    && !asked.swap(true, Ordering::Relaxed)
                {
                    confirm(context, &file, total - resumed)?;
                }
                if resumed > 0 {
                    context
                        .output
                        .detail(&format!("continuing from {}", units::size(resumed)));
                }
                bar.message(format!("{file} from {}", net::host(&url)));
                bar.start(total, resumed);
            }
            Event::Bytes(bytes) => bar.advance(bytes),
            Event::Retrying {
                attempt,
                wait,
                reason,
            } => context.output.warn(&format!(
                "{reason}. trying again in {} (try {} of {})",
                units::duration(wait),
                attempt + 1,
                net::ATTEMPTS
            )),
        }
        Ok(())
    };
    let started = Instant::now();
    let fetched = net.download(&url, &cache, &on).await?;
    drop(bar);
    let file = file_name(fetched.name.as_deref(), &guess);
    let folder = folder_for(&file, chosen);
    tell_fetched(context, &url, &file, &fetched, started.elapsed());
    let (outcome, hint) = put(context, &cache, &fetched, &folder, &file)?;
    let shown = folder.join(&file);
    let method = match outcome {
        Outcome::Placed(method) | Outcome::Replaced(method) => {
            context
                .output
                .done(&format!("fetched {file} into {}", folder.display()));
            Some(method)
        }
        Outcome::AlreadyThere => {
            context
                .output
                .done(&format!("{} is already up to date", shown.display()));
            None
        }
    };
    if let Some(hint) = hint {
        context.output.hint(&hint);
    }
    let folder = std::path::absolute(&folder).unwrap_or(folder);
    context.output.answer(&folder.display().to_string());
    Ok(Report {
        folder,
        files: vec![Placed {
            path: file,
            size: fetched.size,
            sha256: fetched.digest,
            url: url.to_string(),
            origin: fetched.origin,
            method,
        }],
    })
}

/// the name the server gave, else the one the link suggests.
fn file_name(given: Option<&str>, guess: &str) -> String {
    given
        .and_then(name::sanitize)
        .unwrap_or_else(|| guess.to_owned())
}

fn folder_for(file: &str, chosen: Option<&Path>) -> PathBuf {
    chosen.map_or_else(|| PathBuf::from(name::folder_for(file)), Path::to_path_buf)
}

/// refuses a folder that holds anything but this download.
fn check(folder: &Path, file: &str) -> Result<(), Error> {
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
    let mut others: Vec<String> = fs::read_dir(folder)
        .map_err(|err| local(folder, &err))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != file)
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

/// puts the fetched file into the folder, and says how, with a hint when it cost disk space.
fn put(
    context: &Context,
    cache: &Cache,
    fetched: &Fetched,
    folder: &Path,
    file: &str,
) -> Result<(Outcome, Option<String>), Error> {
    let _span = tracing::debug_span!("place").entered();
    check(folder, file)?;
    let destination = folder.join(file);
    let mut replaced = false;
    if fs::symlink_metadata(&destination).is_ok() {
        if holds(&destination, fetched)? {
            return Ok((Outcome::AlreadyThere, None));
        }
        if !replaceable(&destination, cache)? {
            return Err(yours(&destination));
        }
        fs::remove_file(&destination).map_err(|err| local(&destination, &err))?;
        replaced = true;
    }
    let mut placer = Placer::new(folder);
    let method = placer
        .place(&fetched.path, &destination, false)
        .map_err(|err| local(&destination, &err))?;
    count(&context.work, method);
    let verb = if replaced { "Replaced" } else { "Placed" };
    let hint = match method {
        Method::Reflink | Method::Hardlink => {
            context.output.step(
                verb,
                &format!("{} · linked, no extra disk", destination.display()),
            );
            None
        }
        Method::Copy => {
            context.output.step(
                verb,
                &format!(
                    "{} · copied, {}",
                    destination.display(),
                    units::size(fetched.size)
                ),
            );
            if let Some(client) = placer.synced() {
                context.output.warn(&format!(
                    "{} is inside {client}, which breaks links, so the file was copied",
                    folder.display()
                ));
                Some(format!(
                    "a folder outside {client} gets links that use no extra disk"
                ))
            } else if placer.other_volume() {
                Some("the cache is on another drive. set FETCHLOOM_CACHE to a folder on this drive to link files instead of copying them".to_owned())
            } else {
                None
            }
        }
    };
    context
        .output
        .detail(&format!("placed by {}", method_name(method)));
    if replaced {
        context
            .output
            .detail("the earlier version stays in the cache");
    }
    let outcome = if replaced {
        Outcome::Replaced(method)
    } else {
        Outcome::Placed(method)
    };
    Ok((outcome, hint))
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
    Ok(cache.find(&digest, size).is_some())
}

/// refuses before downloading when the file already in the folder is someone's own and cannot
/// be what is about to arrive, because its size differs.
fn refuse_early(destination: &Path, total: u64, cache: &Cache) -> Result<(), Error> {
    match fs::metadata(destination) {
        Ok(there) if there.len() != total && !replaceable(destination, cache)? => {
            Err(yours(destination))
        }
        _ => Ok(()),
    }
}

fn yours(path: &Path) -> Error {
    Error::new(
        Kind::LocalNotEmpty,
        format!("{} already exists with other contents", path.display()),
    )
    .help("move it away to fetch this file, or choose another folder with -o")
}

fn tell_fetched(context: &Context, url: &Url, file: &str, fetched: &Fetched, elapsed: Duration) {
    let output = &context.output;
    match fetched.origin {
        Origin::Network => {
            let mut text = format!(
                "{file} · {} in {}",
                units::size(fetched.size),
                units::duration(elapsed)
            );
            let downloaded = context.work.counts().bytes_down;
            if elapsed >= Duration::from_secs(1) {
                let _ = write!(text, " · {}", units::rate(downloaded, elapsed));
            }
            output.step("Downloaded", &text);
        }
        Origin::NotModified => output.step(
            "Checked",
            &format!("{file} · unchanged on {}", net::host(url)),
        ),
        Origin::Cache => output.step("Found", &format!("{file} · in the cache")),
    }
    output.detail(&format!("sha256 {}", fetched.digest));
    output.detail(&format!("object {}", fetched.path.display()));
}

fn confirm(context: &Context, file: &str, bytes: u64) -> Result<(), Error> {
    let size = units::size(bytes);
    match context
        .output
        .confirm(&format!("download {file} ({size})?"))
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not download {file}"),
        )),
        None => Err(Error::new(
            Kind::Unconfirmed,
            format!("{file} is {size}, and there is no terminal to ask before downloading it"),
        )
        .help("add --yes to download it")),
    }
}

fn count(work: &Work, method: Method) {
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
}

fn method_name(method: Method) -> &'static str {
    match method {
        Method::Reflink => "reflink",
        Method::Hardlink => "hardlink",
        Method::Copy => "copy",
    }
}

fn local(path: &Path, err: &io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not write {}: {err}", path.display()),
    )
}
