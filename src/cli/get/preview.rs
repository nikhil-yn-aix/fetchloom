//! `get --dry-run`: what a get would fetch and where its files would land, from the sources'
//! listings and the cache, writing nothing, not even the cache folder. an unstated size, or an
//! archive never unpacked here, stays unknown, and offline it fails where the get would download.

use std::path::PathBuf;
use std::sync::Arc;

use super::file::{self, Foreseen};
use super::folder::{self, check};
use super::{
    Asked, Dry, Expected, Item, Report, Resolved, Taken, folder_for, foresee, held, known,
    one_file, resolve_all, shown, tell_failed, unclaimed, unwrapped,
};
use crate::cache::Cache;
use crate::cli::args::Get;
use crate::cli::fetch::{self, Job};
use crate::cli::project::dry_run_done;
use crate::cli::sync::place::{Changes, Download};
use crate::cli::{Context, search};
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
    /// files whose bytes the cache holds from the last get, whose server is asked whether they
    /// changed.
    check: Vec<String>,
    /// archives never unpacked here, whose files only unpacking tells.
    unpack: Vec<String>,
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
    let resolved = resolve_all(context, net, &cache, args, &links).await?;
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
    tell(context, &plans);
    let report = report(args, lone, &links, &plans);
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
        let mut planned = Planned {
            place: path.clone(),
            download: Vec::new(),
            check: Vec::new(),
            unpack: Vec::new(),
            paths: None,
            written: Some(written),
        };
        if written != Foreseen::Keep {
            fetching(
                context,
                cache,
                &fetch::listed_job(&remote, &remote.path, false),
                None,
                &mut planned,
            )?;
        }
        return Ok(planned);
    }
    let extract = !args.no_extract;
    let (names, paths) = foresee(cache, &files, extract);
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
        check: Vec::new(),
        unpack: Vec::new(),
        paths,
        written: None,
    };
    for (remote, name) in files.iter().zip(&names) {
        let job = fetch::listed_job(remote, name, extract);
        let unpacks = job.fetch.unpack
            && known(
                cache,
                remote,
                cache.url_record(remote.url.as_str()).as_ref(),
            )
            .and_then(|(digest, _)| cache.unpacked(&digest, None))
            .is_none();
        let at = (!job.fetch.unpack).then(|| folder::within(&planned.place, name));
        fetching(context, cache, &job, at.as_deref(), &mut planned)?;
        if unpacks {
            planned.unpack.push(name.clone());
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
    Ok(())
}

/// tells the plans as a dry run of add does: each download with its size, each archive that
/// would unpack, then where each link's files would land.
fn tell(context: &Context, plans: &[Planned]) {
    let output = &context.output;
    for download in plans.iter().flat_map(|planned| &planned.download) {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
    for name in plans.iter().flat_map(|planned| &planned.check) {
        output.step(
            "Would check",
            &format!("{name} · whether it changed since the last get"),
        );
    }
    for archive in plans.iter().flat_map(|planned| &planned.unpack) {
        output.step("Would open", archive);
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
