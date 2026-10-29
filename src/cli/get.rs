//! `fetchloom get <link>`: a dataset or a file into a new folder, once, through the same cache as
//! projects. archives unpack into the folder unless `--no-extract` keeps them packed. a second run
//! asks the server whether anything changed and moves no bytes when nothing did.

mod folder;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use self::folder::{Outcome, check, put_all};
use super::args::Get;
use super::fetch::{self, Job};
use super::sync as engine;
use super::sync::unpack::{self as unpacking, Unpack};
use super::{Context, open_cache};
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{self, Expect, Fetch, Fetched, Origin};
use crate::place::Method;
use crate::plan::{self, Patterns, Plan};
use crate::project::lock::LockedFile;
use crate::project::manifest::{self, Manifest};
use crate::source::{self, Listing, Parsed, Reference, Remote};
use crate::units;

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
    /// the archive the file was unpacked from, by its path in the download.
    unpacked_from: Option<String>,
}

pub(super) async fn run(context: &Context, args: Get) -> Result<Report, Error> {
    let parsed = Reference::parse(&args.link)?;
    let cache = open_cache()?;
    let net = Arc::new(context.net()?);
    net.trust(parsed.reference.written_host());
    let listing = engine::resolve(&context.output, &parsed.reference, &net).await?;
    engine::tell_resolved(&context.output, &listing);
    let files = selected(&args, &parsed, &listing)?;
    let chosen = args.output.as_deref();
    let extract = !args.no_extract;
    // the folder is checked before any byte arrives, so a refusal costs no download. an archive
    // never unpacked here is checked once its files are known, after the download
    let (names, foreseen) = foresee(&cache, &files, extract);
    check(
        &folder_for(chosen, &listing, &parsed, &names),
        foreseen.as_deref(),
    )?;
    let size: u64 = files.iter().filter_map(|remote| remote.size).sum();
    super::confirm(
        &context.output,
        context.yes,
        &units::count(files.len() as u64, "file", "files"),
        size,
    )?;
    let jobs = files
        .iter()
        .zip(&names)
        .map(|(remote, name)| job(remote, name, extract))
        .collect();
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
    let unpacked = unpack(context, &cache, &mut locked, extract).await?;
    let planned = planned(&cache, &locked)?;
    let placed: Vec<String> = planned
        .targets
        .iter()
        .map(|target| target.path.clone())
        .collect();
    check(&folder, Some(&placed))?;
    let outcomes = put_all(context, &cache, &folder, &planned.targets)?;
    unpacking::drop_archives(&cache, &unpacked, None);
    let folder = std::path::absolute(&folder).unwrap_or(folder);
    context.output.answer(&folder.display().to_string());
    let origins: HashMap<&str, Origin> = locked
        .iter()
        .zip(&fetched)
        .map(|(file, fetched)| (file.path.as_str(), fetched.origin))
        .collect();
    Ok(Report {
        folder,
        files: planned
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
            .collect(),
    })
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
        if extract && archive::by_name(&name) == Some(Packed::Archive) {
            let members = remote
                .sha256
                .or_else(|| {
                    let md5 = remote.md5?;
                    cache.stated_md5(&md5).map(|(digest, _)| digest)
                })
                .or_else(|| record.map(|record| record.sha256))
                .and_then(|digest| cache.unpacked(&digest, None));
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
        true,
    )?)?;
    let mut entry = alone.datasets["get"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.shown(), &entry, listing)
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

/// the download of one listed file, which will be called `name`. a file no checksum pins is asked
/// about again on later runs, so a changed file is noticed and an unchanged one moves no bytes. a
/// file whose name unpacks may be found by the files it unpacked to, once its own bytes are gone.
fn job(remote: &Remote, name: &str, extract: bool) -> Job {
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
            unpack: extract && archive::by_name(name) == Some(Packed::Archive),
        },
        label: remote.path.clone(),
        dataset: None,
        again: None,
    }
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
