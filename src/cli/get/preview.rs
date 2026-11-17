//! `get --dry-run`: what a get would fetch and where its files would land, from the sources'
//! listings and the cache, writing nothing, not even the cache folder. an unstated size, or an
//! archive never unpacked here, stays unknown, and offline it fails where the get would download.

use std::path::PathBuf;
use std::sync::Arc;

use super::file::{self, Foreseen};
use super::folder::{self, check};
use super::{
    Asked, Dry, Expected, Item, Report, Resolved, Taken, beside, folder_for, foresee, held, known,
    one_file, resolve_all, shown, tell_failed, unclaimed, unwrapped,
};
use crate::cache::{Cache, decompressed_key};
use crate::cli::args::Get;
use crate::cli::fetch::{self, Job};
use crate::cli::project::dry_run_done;
use crate::cli::sync::place::{Changes, Download};
use crate::cli::{Context, search};
use crate::digest::Sha256;
use crate::error::Error;
use crate::net::{self, Net};
use crate::source::search::is_search;
use crate::units;

/// what one link would do.
struct Planned {
    /// the folder its files would go in, or the file `-o` names.
    place: PathBuf,
    /// the files it would download, by their listed paths, with the size each source states.
    download: Vec<Download>,
    /// the host each of those downloads starts at, with its size, in the same order.
    hosts: Vec<(String, crate::net::estimate::Size)>,
    /// files whose bytes the cache holds from the last get, whose server is asked whether they
    /// changed.
    check: Vec<String>,
    /// archives never unpacked here, whose files only unpacking tells.
    unpack: Vec<String>,
    /// each file that would decompress, and the file it would decompress to.
    decompress: Vec<(String, String)>,
    /// the paths its files would take in the folder, once every one is known.
    paths: Option<Vec<String>>,
    /// with `-o`, what writing the file would do.
    written: Option<Foreseen>,
}

/// previews every link `asked` names, as [`super::run`] would get them.
pub(super) async fn run(
    context: &Context,
    net: &Arc<Net>,
    args: &Get,
    asked: &[Asked],
) -> Result<Report, Error> {
    let cache = crate::cli::peek_cache()?;
    let mut links: Vec<String> = asked.iter().map(|asked| asked.link.clone()).collect();
    let lone = args.input_file.is_none() && links.len() == 1;
    if lone {
        search::refuse_local(&links[0], false)?;
        if is_search(&links[0]) {
            links[0] = search::pick(context, net.as_ref(), &links[0], "get").await?;
        }
    }
    let mut resolved = resolve_all(context, net, &cache, args, &links).await?;
    // as a dry add does, each lone link's server is asked for the file's first byte, where the
    // get downloads it, so the file is named as the get names it, and fails where the get would
    for found in &mut resolved {
        let Ok((_, listing, files)) = found else {
            continue;
        };
        if !matches!(files.as_slice(), [remote] if remote.named_by_server) {
            continue;
        }
        let extract = (!args.no_extract, args.web_page);
        match crate::cli::add::foresee_names(context, net, &cache, vec![listing], extract, true)
            .await
        {
            Ok(()) => files.clone_from(&listing.files),
            Err(error) => *found = Err(error),
        }
    }
    let mut taken = Taken::default();
    let mut plans = Vec::with_capacity(links.len());
    for (asked, resolved) in asked.iter().zip(resolved) {
        let planned = resolved.and_then(|found| {
            plan(
                context,
                &cache,
                args,
                (lone, &mut taken),
                found,
                asked.expected.as_ref(),
            )
        });
        if lone {
            plans.push(planned?);
            continue;
        }
        plans.push(planned.inspect_err(|error| tell_failed(context, &asked.link, error))?);
    }
    let total = fetch::planned(
        context,
        plans
            .iter()
            .flat_map(|planned| planned.hosts.iter().cloned()),
    );
    tell(context, &plans, &total);
    let mut report = report(args, lone, &links, &plans);
    if let Some(dry) = &mut report.dry {
        dry.changes.to_download = Some(total);
    }
    dry_run_done(&context.output, None);
    Ok(report)
}

/// what one resolved link would do: where its files would land, the bytes that would download for
/// them and the archives that would unpack, with nothing downloaded or written.
fn plan(
    context: &Context,
    cache: &Cache,
    args: &Get,
    (lone, taken): (bool, &mut Taken),
    (parsed, listing, files): Resolved,
    expected: Option<&Expected>,
) -> Result<Planned, Error> {
    if let Some(expected) = expected {
        one_file(&files, expected, &parsed)?;
    }
    if let Some(path) = &args.output {
        let remote = file::only(files, &parsed, &listing)?;
        let written = file::foreseen(context, cache, args, &remote, path)?;
        let came = args.decompress.then(|| file::coming_name(cache, &remote));
        let to = came
            .as_deref()
            .and_then(|came| crate::archive::decompressed_as(true, came)?.ok());
        let mut planned = Planned {
            place: path.clone(),
            download: Vec::new(),
            hosts: Vec::new(),
            check: Vec::new(),
            unpack: Vec::new(),
            decompress: Vec::new(),
            paths: None,
            written: Some(written),
        };
        if written != Foreseen::Keep {
            let mut job = fetch::listed_job(&remote, &remote.path, false);
            job.fetch.decompressed.clone_from(&to);
            fetching(context, cache, &job, None, &mut planned)?;
            if let (Some(came), Some(_)) = (came, &to) {
                planned.decompress.push((came, path.display().to_string()));
            }
        }
        return Ok(planned);
    }
    let (extract, decompress) = (!args.no_extract, args.decompress);
    let (names, paths) = foresee(cache, &files, (extract, decompress));
    let (chosen, parent) = if lone {
        (args.dir.as_deref(), None)
    } else {
        (None, args.dir.as_deref())
    };
    let mut place = folder_for(
        chosen,
        parent,
        &listing,
        &parsed,
        &unwrapped(&files, &names),
    );
    if chosen.is_none() {
        place = unclaimed(place, taken);
    }
    taken.claim(&place);
    check(&place, paths.as_deref())?;
    let mut planned = Planned {
        place,
        download: Vec::new(),
        hosts: Vec::new(),
        check: Vec::new(),
        unpack: Vec::new(),
        decompress: Vec::new(),
        paths,
        written: None,
    };
    for (remote, name) in files.iter().zip(&names) {
        let mut job = fetch::listed_job(remote, name, extract);
        job.fetch.listed = listing.listed(remote);
        job.fetch.decompressed =
            crate::archive::decompressed_as(decompress, name).and_then(Result::ok);
        // an archive or a file never unpacked here, whose files only unpacking makes; the bytes are
        // looked up only for those
        let bytes = || {
            known(
                cache,
                remote,
                cache.url_record(remote.url.as_str()).as_ref(),
            )
        };
        let unpacked = |key: Sha256| cache.unpacked(&key, None).is_some();
        let unpacks = job.fetch.unpack && !bytes().is_some_and(|(digest, _)| unpacked(digest));
        let decompresses = job.fetch.decompressed.as_deref().filter(|to| {
            !bytes().is_some_and(|(digest, _)| unpacked(decompressed_key(&digest, to)))
        });
        let at = (!job.fetch.unpacks()).then(|| folder::within(&planned.place, name));
        fetching(context, cache, &job, at.as_deref(), &mut planned)?;
        if unpacks {
            planned.unpack.push(name.clone());
        }
        if let Some(to) = decompresses {
            planned.decompress.push((name.clone(), beside(name, to)));
        }
    }
    Ok(planned)
}

/// notes what `job` needs before its file is in: nothing when cached, a question to its server for
/// what the link served last time, else a download, which offline is the get's error.
fn fetching(
    context: &Context,
    cache: &Cache,
    job: &Job,
    at: Option<&std::path::Path>,
    planned: &mut Planned,
) -> Result<(), Error> {
    if job.fetch.cached(cache).is_some() {
        return Ok(());
    }
    if held(cache, job, at).is_some() {
        planned.check.push(job.label.clone());
        return Ok(());
    }
    if context.offline.enabled() {
        let expect = &job.fetch.expect;
        let pinned = expect.sha256.is_some() || expect.md5.is_some();
        return Err(net::offline_missing(
            &job.fetch.url,
            context.offline,
            pinned,
        ));
    }
    planned.download.push(Download {
        path: job.label.clone(),
        size: job.fetch.expect.size,
    });
    planned
        .hosts
        .push((fetch::host_of(job), fetch::size_of(job)));
    Ok(())
}

/// tells the plans as a dry run of add does: each download with its size, each archive that
/// would unpack, then where each link's files would land.
fn tell(context: &Context, plans: &[Planned], total: &crate::net::estimate::Total) {
    let output = &context.output;
    let downloads: Vec<&Download> = plans.iter().flat_map(|planned| &planned.download).collect();
    fetch::tell_would_fetch(output, &downloads, total);
    for name in plans.iter().flat_map(|planned| &planned.check) {
        output.step(
            "Would check",
            &format!("{name} · whether it changed since the last get"),
        );
    }
    for archive in plans.iter().flat_map(|planned| &planned.unpack) {
        output.step("Would open", archive);
    }
    for (file, to) in plans.iter().flat_map(|planned| &planned.decompress) {
        output.step("Would decompress", &format!("{file} to {to}"));
    }
    if plans.iter().any(|planned| !planned.unpack.is_empty()) {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    for planned in plans {
        let place = planned.place.display();
        match planned.written {
            Some(Foreseen::Write) => output.step("Would write", &place.to_string()),
            Some(Foreseen::Replace) => output.step("Would replace", &place.to_string()),
            Some(Foreseen::Keep) => output.step(
                "Would keep",
                &format!("{place} · it holds these bytes already"),
            ),
            Some(Foreseen::Compare) => output.step(
                "Would check",
                &format!("{place} · already there, so the link downloads first to compare"),
            ),
            None => {
                let files = planned.paths.as_ref().map_or_else(
                    || "its files".to_owned(),
                    |paths| units::count(paths.len() as u64, "file", "files"),
                );
                output.step("Would put", &format!("{files} in {place}"));
            }
        }
    }
}

/// the report of a dry run, which gets nothing: where the files of each link would land, and the
/// changes, shaped as a dry run of add shapes them.
fn report(args: &Get, lone: bool, links: &[String], plans: &[Planned]) -> Report {
    let whole = |path: &PathBuf| std::path::absolute(path).unwrap_or_else(|_| path.clone());
    let mut changes = Changes::default();
    for planned in plans {
        changes
            .download
            .extend(planned.download.iter().map(|download| Download {
                path: download.path.clone(),
                size: download.size,
            }));
        changes.unpack.extend(planned.unpack.iter().cloned());
        let shown = planned.place.display().to_string();
        match planned.written {
            Some(Foreseen::Replace) => changes.replace.push(shown),
            Some(Foreseen::Write | Foreseen::Compare) => changes.place.push(shown),
            Some(Foreseen::Keep) => {}
            None => changes.place.extend(
                planned
                    .paths
                    .iter()
                    .flatten()
                    .map(|path| folder::within(&planned.place, path).display().to_string()),
            ),
        }
    }
    let items = links
        .iter()
        .zip(plans)
        .map(|(link, planned)| Item {
            reference: shown(link),
            status: "ok",
            folder: planned.written.is_none().then(|| whole(&planned.place)),
            files: Vec::new(),
            error: None,
        })
        .collect();
    let folder = plans
        .first()
        .filter(|planned| lone && planned.written.is_none())
        .map(|planned| whole(&planned.place));
    Report {
        file: args.output.as_ref().map(whole),
        folder,
        files: Vec::new(),
        items,
        dry: Some(Dry {
            dry_run: true,
            changes,
        }),
    }
}
