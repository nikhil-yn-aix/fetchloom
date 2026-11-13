//! `add -i` and several links typed at once: a dataset for each line, resolved, downloaded and
//! placed in one add.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinSet;

use super::{
    AddProject, FromLine, List, Promised, RENAME, Sources, choose_name, close, from_here,
    learn_names, on_line, on_lines, preview_add, private_origins, stored_reference, sync_add,
    tell_started,
};
use crate::cli::args::Add;
use crate::cli::links::{self, Line};
use crate::cli::project::{Report, ignore_data};
use crate::cli::sync::{self as engine, Given, Loop};
use crate::cli::{Context, open_cache, peek_cache};
use crate::error::{Error, Kind};
use crate::net::Net;
use crate::output::Output;
use crate::plan;
use crate::project::lock::Lock;
use crate::project::manifest::{self, Manifest};
use crate::source::{Listing, Parsed};
use crate::units;

/// `add -i`, or several links typed: a dataset for each, resolved, downloaded and placed together,
/// with data.toml and data.lock each written once. a link data.toml has stays as it is.
pub(super) async fn add_list_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    list: List,
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let old = adding.old.take();
    let adding = &*adding;
    let project = &adding.project;
    if !dry_run {
        project.check_writable()?;
    }
    let extract = !args.no_extract;
    let net = Arc::new(context.net());
    list.trust_typed(&net);
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let mut listings = resolve_lines(context, &net, &list).await?;
    let entries = list
        .lines
        .iter()
        .zip(&listings)
        .map(|((line, parsed), listing)| {
            listed_entry(list.at(line).as_deref(), line, parsed, listing, extract)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lines = FromLine::each(&list);
    if !dry_run {
        learn_names(
            context,
            &net,
            &cache,
            listings.iter_mut().collect(),
            extract,
        )
        .await
        .map_err(|error| on_lines(error, list.name.as_deref(), &lines))?;
    }
    let found = listings.into_iter().zip(entries).collect();
    let NewDatasets {
        text,
        manifest,
        names,
        given,
        promised,
        already,
    } = new_datasets(
        &context.output,
        adding,
        old.as_ref(),
        &list,
        found,
        &mut lines,
        extract,
    )?;
    // each dataset added gets one last line, so what it pinned and placed is told only with -v
    let work = names
        .iter()
        .fold(Loop::new(context, project, net, cache), |work, name| {
            work.telling(name)
        });
    if dry_run {
        let nothing = list.nothing_new();
        return preview_add(&work, &manifest, old, (names, given), here, Some(&nothing)).await;
    }
    let updated = (!names.is_empty()).then_some(text.as_str());
    let outcome = sync_add(
        &work,
        adding,
        &manifest,
        updated,
        old,
        (&names, given),
        &promised,
    )
    .await
    .map_err(|error| on_lines(error, list.name.as_deref(), &lines))?;
    tell_started(&work, starting);
    ignore_data(&work, &manifest)?;
    let private = private_origins(
        &work,
        list.lines
            .iter()
            .zip(&lines)
            .filter(|(_, from)| from.dataset.is_some())
            .map(|((_, parsed), _)| &parsed.reference),
    );
    let added = (names.as_slice(), &outcome.placed.planned);
    tell_list(&work, &manifest, added, &already, here, &private);
    Ok(Report {
        project: project.root.clone(),
        datasets: names,
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// what the lines of a list add to data.toml: its text and datasets with them, each new dataset
/// with its listing and any SHA-256 its line gave, and the datasets it has already.
struct NewDatasets {
    text: String,
    manifest: Manifest,
    names: Vec<String>,
    given: Vec<Given>,
    promised: Vec<Promised>,
    already: Vec<String>,
}

/// the datasets the lines of `list` add: one data.toml has stays, and each other gets a name apart
/// from every other, noted in `lines`. data.toml's text is written once however long the list.
fn new_datasets(
    output: &Output,
    adding: &AddProject,
    old: Option<&Lock>,
    list: &List,
    found: Vec<(Listing, Listed)>,
    lines: &mut [FromLine],
    extract: bool,
) -> Result<NewDatasets, Error> {
    let sources = Sources::new(&adding.manifest, old);
    let mut taken: HashSet<String> = adding.manifest.datasets.keys().cloned().collect();
    let mut written = Vec::new();
    let (mut names, mut given, mut promised, mut already) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (((line, parsed), (listing, listed)), from) in list.lines.iter().zip(found).zip(lines) {
        let at = list.subject(line);
        if let Some(name) = sources.present(&listed.entry, &listing) {
            same_settings(&adding.manifest.datasets[&name], &listed.entry, &name, &at)?;
            already.push(name);
            continue;
        }
        let name = choose_name(
            output,
            &listing,
            parsed,
            &|name| taken.contains(name),
            RENAME,
        )?;
        taken.insert(name.clone());
        if let Some(sha256) = line.sha256 {
            promised.push(Promised {
                dataset: name.clone(),
                sha256,
                line: at,
            });
        }
        written.push((name.clone(), listed));
        given.push(Given::Listing(name.clone(), listing));
        from.dataset = Some(name.clone());
        names.push(name);
    }
    let text = adding.text.clone().unwrap_or_default();
    let (text, manifest) = if written.is_empty() {
        (text, adding.manifest.clone())
    } else {
        let added: Vec<manifest::NewDataset<'_>> = written
            .iter()
            .map(|(name, listed)| manifest::NewDataset {
                name,
                reference: &listed.reference,
                select: &listed.select,
                exclude: &[],
                extract,
            })
            .collect();
        let text = manifest::with_datasets(&text, &added)?;
        let manifest = Manifest::parse(&text)?;
        (text, manifest)
    };
    Ok(NewDatasets {
        text,
        manifest,
        names,
        given,
        promised,
        already,
    })
}

/// a dataset a line of a list names, as data.toml records it.
struct Listed {
    reference: String,
    select: Vec<String>,
    /// the entry as data.toml reads it back.
    entry: manifest::Entry,
}

/// how data.toml records what `line`, `at` in its list, names: the file or folder its link points
/// at is selected, and a SHA-256 on the line needs one file left, whose stated hash must match.
fn listed_entry(
    at: Option<&str>,
    line: &Line,
    parsed: &Parsed,
    listing: &Listing,
    extract: bool,
) -> Result<Listed, Error> {
    let select = parsed.select();
    let reference = stored_reference(&line.link, parsed);
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "new",
        &reference,
        &select,
        &[],
        extract,
    )?)?;
    let mut entry = alone.datasets["new"].clone();
    entry.lines.clear();
    let files = match entry.source() {
        Some(source) => engine::filter(&parsed.reference.shown(), &entry, source, listing),
        None => Ok(listing.files.clone()),
    }
    .map_err(|error| match at {
        Some(at) => links::on_line(error, at),
        None => error,
    })?;
    if let (Some(sha256), Some(at)) = (line.sha256, at) {
        let [file] = files.as_slice() else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "the SHA-256 on {at} checks one file, and {} has {}",
                    parsed.reference.shown(),
                    units::count(files.len() as u64, "file", "files")
                ),
            )
            .help("list a link to one file there, or leave its SHA-256 off"));
        };
        if let Some(stated) = file.sha256.filter(|stated| *stated != sha256) {
            return Err(Error::new(
                Kind::IntegrityMismatch,
                format!("{} is not the file the SHA-256 on {at} names", file.path),
            )
            .detail(format!(
                "expected sha256 {}, the source states {}",
                sha256.hex(),
                stated.hex()
            ))
            .help(format!(
                "check the hash on {at}, or ask the file's owner which bytes are right"
            )));
        }
    }
    Ok(Listed {
        reference,
        select,
        entry,
    })
}

/// refuses a line of a list, `at`, that names `name`, a dataset data.toml has already, with other
/// settings than the line's own: adding it again would place the same data twice.
fn same_settings(
    existing: &manifest::Entry,
    wanted: &manifest::Entry,
    name: &str,
    at: &str,
) -> Result<(), Error> {
    let alike = existing
        .source()
        .zip(wanted.source())
        .is_some_and(|(existing, wanted)| {
            existing.select == wanted.select
                && existing.exclude == wanted.exclude
                && existing.extract == wanted.extract
        });
    if alike {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!("{at} names {name}, which data.toml has already with other settings"),
    )
    .help("leave it out, or add its link on its own with --as <name> to have it twice"))
}

/// resolves every line of `list` at once, within the network's limits, in the lines' order. the
/// first line that fails stops the add, so a list adds all its datasets or none.
async fn resolve_lines(
    context: &Context,
    net: &Arc<Net>,
    list: &List,
) -> Result<Vec<Listing>, Error> {
    let mut tasks = JoinSet::new();
    let mut asked = 0_u64;
    for (index, (line, parsed)) in list.lines.iter().enumerate() {
        engine::tell_raw(&context.output, &line.link, &parsed.reference);
        if !engine::resolves_locally(&parsed.reference) {
            asked += 1;
        }
        let reference = parsed.reference.clone();
        let select = crate::glob::patterns_of(&parsed.select());
        let net = net.clone();
        tasks.spawn(async move {
            let listing = reference.resolve_in(&*net, &select).await;
            (index, listing)
        });
    }
    let _waiting = (asked > 0).then(|| {
        context
            .output
            .wait("Resolving", units::count(asked, "link", "links"))
    });
    let mut listings: Vec<Option<Listing>> = list.lines.iter().map(|_| None).collect();
    let mut failed: Option<(usize, Error)> = None;
    while let Some(joined) = tasks.join_next().await {
        let (index, listing) = joined.map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("resolving stopped unexpectedly: {err}"),
            )
        })?;
        match listing {
            Ok(listing) => {
                engine::tell_resolved(&context.output, &listing);
                listings[index] = Some(listing);
            }
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => {
                if failed.as_ref().is_none_or(|(first, _)| index < *first) {
                    failed = Some((index, error));
                }
            }
        }
    }
    if let Some((index, error)) = failed {
        let at = list.at(&list.lines[index].0);
        return Err(on_line(error, at.as_deref(), true));
    }
    Ok(listings.into_iter().flatten().collect())
}

/// the last lines of `add -i`: each added dataset and where its files are, or that it has none,
/// then each one data.toml had already, then the closing lines naming the `private` origins.
fn tell_list(
    work: &Loop<'_>,
    manifest: &Manifest,
    (names, placed): (&[String], &std::collections::BTreeSet<String>),
    already: &[String],
    here: &Path,
    private: &[url::Origin],
) {
    let output = &work.context.output;
    for name in names {
        if placed.contains(name) {
            let folder = from_here(work.project, here, &plan::folder(manifest, name));
            output.done("Added", &format!("{name}, its files are in {folder}"));
        } else {
            output.done(
                "Added",
                &format!("{name}, its archives contain no regular files"),
            );
        }
    }
    for name in already {
        output.done("Ok", &format!("{name} is already in data.toml"));
    }
    match names {
        [] => return,
        [name] => output.hint(&format!(
            "scripts can find it with `{}`",
            crate::cli::project::command(&format!("path {name}"))
        )),
        _ => output.hint(&format!(
            "scripts can find each with `{}`",
            crate::cli::project::command("path <name>")
        )),
    }
    close(work, private);
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::cli::project::tests::{listed, offline_context, stated};
    use crate::project::Project;

    /// `count` links in links.txt, each to one file whose hash its source states, as `add -i` has
    /// them once every line resolved.
    fn resolved_list(count: usize) -> (List, Vec<(Listing, Listed)>) {
        let text = (0..count).fold(String::new(), |mut text, index| {
            let _ = writeln!(text, "https://example.org/f{index:05}.bin");
            text
        });
        let lines = links::parse("links.txt", &text).unwrap();
        let list = List {
            name: Some("links.txt".to_owned()),
            lines: links::checked("links.txt", lines, true).unwrap(),
        };
        let found = list
            .lines
            .iter()
            .map(|(line, parsed)| {
                let file = line.link.rsplit('/').next().unwrap();
                let listing = listed(
                    &line.link,
                    vec![stated(file, &line.link, line.link.as_bytes())],
                );
                let entry =
                    listed_entry(list.at(line).as_deref(), line, parsed, &listing, true).unwrap();
                (listing, entry)
            })
            .collect();
        (list, found)
    }

    /// reading data.toml again for each link made 4,000 links take a minute.
    #[test]
    fn a_list_reads_data_toml_as_often_however_many_links_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let adding = AddProject {
            project: Project {
                root: dir.path().to_path_buf(),
            },
            held: None,
            text: None,
            manifest: Manifest::parse("").unwrap(),
            old: None,
            lock_text: None,
            unrestored: AtomicBool::new(false),
        };
        let context = offline_context();
        let reads = |count| {
            let (list, found) = resolved_list(count);
            let mut lines = FromLine::each(&list);
            let before = manifest::tests::reads();
            let new = new_datasets(
                &context.output,
                &adding,
                None,
                &list,
                found,
                &mut lines,
                true,
            )
            .unwrap();
            let reads = manifest::tests::reads() - before;
            assert_eq!(new.manifest.datasets.len(), count);
            assert_eq!(new.names.len(), count);
            reads
        };
        assert_eq!(reads(30), reads(3), "data.toml is read again for each link");
    }
}
