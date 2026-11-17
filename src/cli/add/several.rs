//! several links typed for one add: a dataset for each link, resolved, downloaded and placed in
//! one add.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinSet;

use super::{
    AddProject, FromLine, RENAME, Several, Sources, choose_name, close, from_here, learn_names,
    on_lines, preview_add, private_origins, stored_reference, sync_add, tell_started, typed,
};
use crate::cli::args::Add;
use crate::cli::project::Report;
use crate::cli::sync::place::tell_marked;
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

/// several links typed: a dataset for each, resolved, downloaded and placed together, with
/// data.toml and data.lock each written once. a link data.toml has stays as it is.
#[expect(
    clippy::too_many_lines,
    reason = "every link is resolved before data.toml and data.lock are written once"
)]
pub(super) async fn add_several_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    several: Several,
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
    // each origin typed is trusted for this run, as one typed alone is
    for (_, parsed) in &several.links {
        net.trust(parsed.reference.written_origin());
    }
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let mut listings = resolve_links(context, &net, &several).await?;
    let entries = several
        .links
        .iter()
        .zip(&listings)
        .map(|((link, parsed), listing)| {
            listed_entry(link, parsed, listing, (extract, args.decompress))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lines = FromLine::each(&several);
    if dry_run {
        super::foresee_names(
            context,
            &net,
            &cache,
            listings.iter_mut().collect(),
            (extract, args.web_page),
            true,
        )
        .await
        .map_err(|error| on_lines(error, &lines))?;
    } else {
        learn_names(
            context,
            &net,
            &cache,
            listings.iter_mut().collect(),
            (extract, args.web_page),
        )
        .await
        .map_err(|error| on_lines(error, &lines))?;
    }
    let found = listings.into_iter().zip(entries).collect();
    let NewDatasets {
        text,
        manifest,
        names,
        given,
        already,
    } = new_datasets(
        &context.output,
        adding,
        old.as_ref(),
        &several,
        found,
        &mut lines,
        args,
    )?;
    // a dataset a line names that was dropped here comes back, as an add of its link brings it
    let dropped = project.dropped();
    let back: Vec<String> = already
        .iter()
        .filter(|name| dropped.contains(*name))
        .cloned()
        .collect();
    let placing: Vec<String> = names.iter().chain(&back).cloned().collect();
    // each dataset added gets one last line, so what it pinned and placed is told only with -v
    let work = placing.iter().fold(
        Loop::new(context, project, net, cache).running("this add"),
        |work, name| work.adding(name),
    );
    if dry_run {
        let nothing = "each link is in data.toml already";
        return preview_add(&work, &manifest, old, (names, given), here, Some(nothing)).await;
    }
    let updated = (!names.is_empty()).then_some(text.as_str());
    let outcome = sync_add(&work, adding, &manifest, updated, old, (&placing, given))
        .await
        .map_err(|error| on_lines(error, &lines))?;
    tell_started(&work, starting);
    tell_marked(&work.context.output, &outcome.placed);
    let private = private_origins(
        &work,
        several
            .links
            .iter()
            .zip(&lines)
            .filter(|(_, from)| from.dataset.is_some())
            .map(|((_, parsed), _)| &parsed.reference),
    );
    let added = (names.as_slice(), &outcome.placed.planned);
    tell_several(&work, &manifest, added, (&already, &back), here, &private);
    Ok(Report {
        project: project.root.clone(),
        datasets: names,
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// what several links add to data.toml: its text and datasets with them, each new dataset with
/// its listing, and the datasets it has already.
struct NewDatasets {
    text: String,
    manifest: Manifest,
    names: Vec<String>,
    given: Vec<Given>,
    already: Vec<String>,
}

/// the datasets the links of `several` add: one data.toml has stays, and each other gets a name
/// apart from every other, noted in `lines`. data.toml's text is written once however many links.
fn new_datasets(
    output: &Output,
    adding: &AddProject,
    old: Option<&Lock>,
    several: &Several,
    found: Vec<(Listing, Listed)>,
    lines: &mut [FromLine],
    args: &Add,
) -> Result<NewDatasets, Error> {
    let sources = Sources::new(&adding.manifest, old);
    let mut taken: HashSet<String> = adding.manifest.datasets.keys().cloned().collect();
    let mut written = Vec::new();
    let (mut names, mut given, mut already) = (Vec::new(), Vec::new(), Vec::new());
    for (((link, parsed), (listing, listed)), from) in several.links.iter().zip(found).zip(lines) {
        let at = crate::shown::mask_query(link);
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
                extract: !args.no_extract,
                decompress: args.decompress,
                web_page: args.web_page,
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
        already,
    })
}

/// a dataset a typed link names, as data.toml records it.
struct Listed {
    reference: String,
    select: Vec<String>,
    /// the entry as data.toml reads it back.
    entry: manifest::Entry,
}

/// how data.toml records what `link`, which reads as `parsed`, names: the file or folder the link
/// points at is selected.
fn listed_entry(
    link: &str,
    parsed: &Parsed,
    listing: &Listing,
    (extract, decompress): (bool, bool),
) -> Result<Listed, Error> {
    let select = parsed.select();
    let reference = stored_reference(link, parsed);
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
    entry.source.decompress = decompress;
    engine::filter(&parsed.reference.shown(), &entry, listing)?;
    Ok(Listed {
        reference,
        select,
        entry,
    })
}

/// refuses a typed link, `at`, that names `name`, a dataset data.toml has already, with other
/// settings than the link's own: adding it again would place the same data twice.
fn same_settings(
    existing: &manifest::Entry,
    wanted: &manifest::Entry,
    name: &str,
    at: &str,
) -> Result<(), Error> {
    if existing.source.same_settings(&wanted.source) {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!("{at} names {name}, which data.toml has already with other settings"),
    )
    .help("leave it out, or add its link on its own with --as <name> to have it twice"))
}

/// resolves every link of `several` at once, within the network's limits, in the order typed. the
/// first link that fails stops the add, so it adds all its datasets or none.
async fn resolve_links(
    context: &Context,
    net: &Arc<Net>,
    several: &Several,
) -> Result<Vec<Listing>, Error> {
    let mut tasks = JoinSet::new();
    let mut asked = 0_u64;
    for (index, (link, parsed)) in several.links.iter().enumerate() {
        engine::tell_raw(&context.output, link, &parsed.reference);
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
    let mut listings: Vec<Option<Listing>> = several.links.iter().map(|_| None).collect();
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
    if let Some((_, error)) = failed {
        return Err(typed(error, true));
    }
    Ok(listings.into_iter().flatten().collect())
}

/// the last lines of an add of several links: each added dataset and where its files are, or that
/// it has none, then each one data.toml had already, counted past a few dozen, then the `private`
/// origins.
fn tell_several(
    work: &Loop<'_>,
    manifest: &Manifest,
    (names, placed): (&[String], &std::collections::BTreeSet<String>),
    (already, back): (&[String], &[String]),
    here: &Path,
    private: &[url::Origin],
) {
    let output = &work.context.output;
    let datasets = |count: usize| units::count(count as u64, "dataset", "datasets");
    if crate::cli::fetch::one_by_one(output, names.len()) {
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
    } else {
        output.done(
            "Added",
            &format!("{} · `-v` lists them", datasets(names.len())),
        );
    }
    if crate::cli::fetch::one_by_one(output, already.len()) {
        for name in already {
            let back = if back.contains(name) {
                " and back on this machine"
            } else {
                ""
            };
            output.done("Ok", &format!("{name} is already in data.toml{back}"));
        }
    } else {
        let back = match back.len() {
            0 => String::new(),
            count => format!(", {count} of them back on this machine"),
        };
        output.done(
            "Ok",
            &format!(
                "{} are already in data.toml{back} · `-v` lists them",
                datasets(already.len())
            ),
        );
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
    close(work, private, None);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::cli::project::tests::{listed, offline_context, stated};
    use crate::project::Project;

    /// a typed link naming a dataset data.toml has with other settings is refused by its link; the
    /// same settings are no change.
    #[test]
    fn a_known_dataset_with_other_settings_is_refused() {
        let entry = |select: &str| {
            let text = format!("[datasets.iris]\nref = \"https://example.org/iris.zip\"\n{select}");
            Manifest::parse(&text)
                .unwrap()
                .datasets
                .remove("iris")
                .unwrap()
        };
        let plain = entry("");
        let at = "https://example.org/iris.zip";
        assert!(same_settings(&plain, &entry(""), "iris", at).is_ok());
        let error =
            same_settings(&plain, &entry("select = [\"*.csv\"]\n"), "iris", at).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "https://example.org/iris.zip names iris, which data.toml has already with other settings"
        );
    }

    /// `count` typed links, each to one file whose hash its source states, as an add has them once
    /// every link resolved.
    fn resolved_links(count: usize) -> (Several, Vec<(Listing, Listed)>) {
        let links = (0..count)
            .map(|index| format!("https://example.org/f{index:05}.bin"))
            .collect();
        let several = Several {
            links: crate::cli::links::typed(links).unwrap(),
        };
        let found = several
            .links
            .iter()
            .map(|(link, parsed)| {
                let file = link.rsplit('/').next().unwrap();
                let listing = listed(link, vec![stated(file, link, link.as_bytes())]);
                let entry = listed_entry(link, parsed, &listing, (true, false)).unwrap();
                (listing, entry)
            })
            .collect();
        (several, found)
    }

    /// reading data.toml again for each link made 4,000 links take a minute.
    #[test]
    fn several_links_read_data_toml_as_often_however_many_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let adding = AddProject {
            project: Project::at(dir.path().to_path_buf()),
            held: None,
            text: None,
            manifest: Manifest::parse("").unwrap(),
            old: None,
            lock_text: None,
            unrestored: AtomicBool::new(false),
            seeded: std::sync::Mutex::default(),
        };
        let context = offline_context();
        let args = Add {
            links: Vec::new(),
            name: None,
            input_file: None,
            filter: crate::cli::args::Filter {
                select: Vec::new(),
                exclude: Vec::new(),
            },
            no_extract: false,
            decompress: false,
            web_page: false,
            no_sync: false,
            dry: crate::cli::args::DryRun { dry_run: false },
        };
        let reads = |count| {
            let (several, found) = resolved_links(count);
            let mut lines = FromLine::each(&several);
            let before = manifest::tests::reads();
            let new = new_datasets(
                &context.output,
                &adding,
                None,
                &several,
                found,
                &mut lines,
                &args,
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
