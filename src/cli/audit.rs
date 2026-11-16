//! `fetchloom audit`: whether a mirror or its source still serves each file data.lock pins, asked
//! without downloading any; whether its source lists a newer version, as `lock --upgrade` would pin
//! it; and whether its title, license or access changed. told a line a dataset, and the check
//! `audit --check` makes for ci.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::task::JoinSet;

use super::Context;
use super::args;
use super::probe::{self, Ask, Found, Lack, Pinned};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::net::Net;
use crate::output::Output;
use crate::project::lock::{Lock, Waiting, Words};
use crate::project::manifest::Manifest;
use crate::source::{Access, Listing, Reference};
use crate::units;

/// the most rows of each kind a terminal shows; a pipe, a file and `--json` get them all.
const SHOWN: usize = 20;

/// the most lines the `--check` error names; the command's result lists them all.
const NAMED: usize = 20;

/// the most hints of distinct refusals and failures told after the lines.
const HINTS: usize = 3;

/// what `audit` found, as `--json` gives it.
#[derive(Debug, Default, Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// the files data.lock pins for the datasets audited, and of those, how many a mirror or
    /// source still serves and how many could not be checked.
    files: u64,
    served: u64,
    unchecked: u64,
    datasets: Vec<Dataset>,
    /// whether the project has no data.lock, so nothing pins its files yet.
    #[serde(skip)]
    lockless: bool,
    /// for a project with no data.lock, what pins its files' bytes.
    #[serde(skip)]
    pins_lockless: String,
}

/// one dataset: where its files are served, and what its source says of it now.
#[derive(Debug, Serialize)]
struct Dataset {
    name: String,
    files: u64,
    served: u64,
    /// of those served, how many a mirror holds.
    on_mirrors: u64,
    /// of those served, how many its source answered for without saying their size.
    no_size: u64,
    /// each file no mirror or source serves.
    not_served: Vec<Unserved>,
    /// each file no mirror or source answered for clearly: refused, or no answer came.
    unchecked: Vec<Unserved>,
    /// each file a mirror serves while its source lost or changed it, as its source said.
    lost_at_source: Vec<Unserved>,
    /// the version `lock --upgrade` would pin in place of data.lock's, as `zenodo:3608991`; none
    /// when there is no other, or data.toml names the version.
    newer: Option<String>,
    /// the title, license or access its source gives the pinned version now, where data.lock
    /// pins another.
    changed: Vec<Change>,
    /// why its source did not say what it holds now; none when it did, and for an import, which
    /// no source lists.
    unlisted: Option<Unlisted>,
    /// the version data.lock pins, as `zenodo:3608990`.
    #[serde(skip)]
    pinned: String,
    /// the mirrors that hold any, as the run names them.
    #[serde(skip)]
    mirrors: BTreeSet<String>,
}

impl Dataset {
    fn new(name: &str, pinned: &str) -> Dataset {
        Dataset {
            name: name.to_owned(),
            files: 0,
            served: 0,
            on_mirrors: 0,
            no_size: 0,
            not_served: Vec::new(),
            unchecked: Vec::new(),
            lost_at_source: Vec::new(),
            newer: None,
            changed: Vec::new(),
            unlisted: None,
            pinned: pinned.to_owned(),
            mirrors: BTreeSet::new(),
        }
    }

    /// what moved at its source since data.lock pinned it, as the `--check` error names it.
    fn behind(&self) -> Vec<String> {
        let newer = self.newer.iter().map(|newer| {
            format!(
                "{} · newer version {newer}, where data.lock pins {}",
                self.name, self.pinned
            )
        });
        let changed = self
            .changed
            .iter()
            .map(|change| format!("{} · {}", self.name, change.said()));
        newer.chain(changed).collect()
    }
}

/// a pinned file no mirror or source was found to serve, and why.
#[derive(Debug, Serialize)]
struct Unserved {
    /// the file as data.lock names it, after its dataset: `iris/iris.csv`.
    file: String,
    /// `changed`, `gone` or `imported` for a file not served, `refused` or `unknown` for one
    /// not checked.
    outcome: &'static str,
    /// for `changed`, the size the source serves in its place.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    /// the kind of error a download of it would meet.
    #[serde(skip)]
    kind: Kind,
}

impl Unserved {
    /// the file and why no place serves it, as a row and the `--check` error say it.
    fn said(&self) -> String {
        match self.outcome {
            "imported" => format!("{} · {}", self.file, self.message),
            outcome => format!("{} · {outcome}: {}", self.file, self.message),
        }
    }
}

/// a fact of a dataset that its source gives otherwise now than data.lock pins it.
#[derive(Debug, Serialize)]
struct Change {
    /// `title`, `license` or `access`.
    field: &'static str,
    pinned: String,
    now: String,
}

impl Change {
    fn said(&self) -> String {
        if self.field == "title" {
            format!("title \"{}\" → \"{}\"", self.pinned, self.now)
        } else {
            format!("{} {} → {}", self.field, self.pinned, self.now)
        }
    }
}

/// why a source did not say what a dataset holds now.
#[derive(Debug, Serialize)]
struct Unlisted {
    /// `gone` when it has nothing there, as 404 or 410 says, `refused`, or `unknown` for no
    /// answer or another one.
    outcome: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip)]
    kind: Kind,
}

impl Unlisted {
    fn of(error: Error) -> Unlisted {
        let outcome = match error.kind {
            Kind::ReferenceUnresolved => "gone",
            Kind::AuthRequired => "refused",
            _ => "unknown",
        };
        Unlisted {
            outcome,
            message: error.message,
            help: error.help.map(String::from),
            kind: error.kind,
        }
    }
}

impl Report {
    fn unserved(&self) -> impl Iterator<Item = &Unserved> {
        self.datasets.iter().flat_map(|dataset| &dataset.not_served)
    }

    fn unchecked(&self) -> impl Iterator<Item = &Unserved> {
        self.datasets.iter().flat_map(|dataset| &dataset.unchecked)
    }

    /// the datasets whose source no longer lists them, with why.
    fn gone(&self) -> impl Iterator<Item = (&str, &Unlisted)> {
        self.unlisted()
            .filter(|(_, unlisted)| unlisted.outcome == "gone")
    }

    /// the datasets whose source could not say whether it changed, with why.
    fn unasked(&self) -> impl Iterator<Item = (&str, &Unlisted)> {
        self.unlisted()
            .filter(|(_, unlisted)| unlisted.outcome != "gone")
    }

    fn unlisted(&self) -> impl Iterator<Item = (&str, &Unlisted)> {
        self.datasets.iter().filter_map(|dataset| {
            let unlisted = dataset.unlisted.as_ref()?;
            Some((dataset.name.as_str(), unlisted))
        })
    }

    /// the datasets with a newer version or a change at their source.
    fn moved(&self) -> Vec<&str> {
        self.datasets
            .iter()
            .filter(|dataset| dataset.newer.is_some() || !dataset.changed.is_empty())
            .map(|dataset| dataset.name.as_str())
            .collect()
    }
}

/// what to ask a dataset's source, as `lock` and `lock --upgrade` ask it.
struct Asked {
    /// the version data.lock pins, as its `resolved` names it.
    pinned: String,
    /// data.toml's ref, which `lock --upgrade` resolves, when it may name another version.
    upgrade: Option<String>,
    select: Vec<Glob>,
    words: Words,
    access: Access,
}

/// what a source lists now: at the pinned version, and at the version `lock --upgrade` would pin
/// when that may be another.
type Listed = (Result<Listing, Error>, Option<Result<Listing, Error>>);

/// audits the datasets `args` names, or every one, failing as `--check` asks. it asks nobody
/// offline.
pub(super) async fn run(context: &Context, args: args::Audit) -> Result<Report, Error> {
    if context.offline.enabled() {
        return Err(Error::new(
            Kind::OfflineMissing,
            "audit asks each source and mirror over the network, which offline mode does not allow",
        )
        .help(context.offline.retry()));
    }
    let project = super::project::find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        super::project::known(&manifest, name, "audit")?;
    }
    let wanted = |name: &str| args.names.is_empty() || args.names.iter().any(|named| named == name);
    let output = &context.output;
    let mut report = Report {
        project: project.root.clone(),
        ..Report::default()
    };
    let lock = project.lock()?;
    // a dataset waiting for its file list has no file to ask about
    if let Some(lock) = &lock {
        lock.refuse_waiting(args.names.iter().map(String::as_str))?;
    }
    let Some(lock) = lock else {
        report.lockless = true;
        report.pins_lockless = super::project::unpinned(&manifest);
        tell(output, &report);
        if args.check {
            ready(&report).map_err(|error| error.with_report(&report))?;
        }
        return Ok(report);
    };
    let waiting: Vec<Waiting> = lock
        .still_waiting()
        .filter(|waiting| wanted(&waiting.name))
        .cloned()
        .collect();
    super::project::tell_waiting(output, &waiting, &super::sync::Scope::All);
    // a sync carries a pin over to the name data.toml gives it now, as status reads it
    let (lock, _) = super::sync::rename::forecast(&manifest, lock);
    let (pinned, asks) = pinned(&manifest, &lock, &wanted, &mut report);
    if !report.datasets.is_empty() {
        let net = Arc::new(context.net());
        let places = if context.mirrors.mirrors.list.is_empty() {
            "their sources"
        } else {
            "their sources and mirrors"
        };
        let _waiting = output.wait(
            "Checking",
            format!(
                "{} at {places}",
                units::count(pinned.len() as u64, "file", "files")
            ),
        );
        let (found, listed) = tokio::join!(
            probe::find(context, &net, &pinned, Ask::Sources),
            listings(&net, &asks),
        );
        gathered(context, &mut report, &pinned, found?);
        for ((index, asked), (now, newest)) in asks.iter().zip(listed?) {
            judge(&mut report.datasets[*index], asked, now, newest);
        }
    }
    tell(output, &report);
    if args.check {
        ready(&report).map_err(|error| error.with_report(&report))?;
    }
    Ok(report)
}

/// the files data.lock pins for the datasets `wanted` names, to ask about, and what to ask each
/// dataset's source, by its place in `report`, which gets a record for each.
fn pinned<'l>(
    manifest: &'l Manifest,
    lock: &'l Lock,
    wanted: &dyn Fn(&str) -> bool,
    report: &mut Report,
) -> (Vec<Pinned<'l>>, Vec<(usize, Asked)>) {
    let mut pinned = Vec::new();
    let mut asks = Vec::new();
    for (name, entry) in &manifest.datasets {
        if !wanted(name) {
            continue;
        }
        let source = &entry.source;
        let (pin, words) = if let Some(locked) = lock.get(name) {
            let files = locked.files.iter();
            pinned.extend(files.map(|file| Pinned::locked(name, file, source.web_page)));
            (locked, Words::of(locked))
        } else if let Some(stated) = lock.stated(name) {
            let files = stated.files.iter();
            pinned.extend(files.map(|file| Pinned::stated(name, file, source.web_page)));
            (&stated.pin, stated.written.clone())
        } else {
            continue;
        };
        report.datasets.push(Dataset::new(name, &pin.resolved));
        if entry.is_import() {
            continue;
        }
        // only a pin that answers data.toml's entry is one an upgrade replaces
        let upgrade = (pin.drift(entry).is_none() && source.reference != pin.resolved)
            .then(|| source.reference.clone());
        asks.push((
            report.datasets.len() - 1,
            Asked {
                pinned: pin.resolved.clone(),
                upgrade,
                select: source.select.clone(),
                words,
                access: pin.access,
            },
        ));
    }
    (pinned, asks)
}

/// what each of `asks` lists now, in their order: at its pinned version, and at the version
/// `lock --upgrade` would pin when that may be another. fails at a stop.
async fn listings(net: &Arc<Net>, asks: &[(usize, Asked)]) -> Result<Vec<Listed>, Error> {
    let mut now: Vec<Option<Result<Listing, Error>>> = asks.iter().map(|_| None).collect();
    let mut newest: Vec<Option<Result<Listing, Error>>> = asks.iter().map(|_| None).collect();
    let mut tasks = JoinSet::new();
    for (at, (_, asked)) in asks.iter().enumerate() {
        for (upgrade, text) in [(false, Some(&asked.pinned)), (true, asked.upgrade.as_ref())] {
            let Some(text) = text else {
                continue;
            };
            let (net, text, select) = (Arc::clone(net), text.clone(), asked.select.clone());
            tasks.spawn(async move {
                let listing = match Reference::parse(&text) {
                    Ok(parsed) => parsed.reference.resolve_in(&*net, &select).await,
                    Err(error) => Err(error),
                };
                (at, upgrade, listing)
            });
        }
    }
    while let Some(joined) = tasks.join_next().await {
        let (at, upgrade, listing) = joined.map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("a check of a source stopped unexpectedly: {err}"),
            )
        })?;
        let listing = match listing {
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error);
            }
            listing => listing,
        };
        if upgrade {
            newest[at] = Some(listing);
        } else {
            now[at] = Some(listing);
        }
    }
    Ok(now
        .into_iter()
        .zip(newest)
        .map(|(now, newest)| {
            let now = now.unwrap_or_else(|| Err(Error::new(Kind::LocalIo, "not asked")));
            (now, newest)
        })
        .collect())
}

/// fills in what `dataset`'s source lists now: `now` at the pinned version, `newest` at the
/// version `lock --upgrade` would pin.
fn judge(
    dataset: &mut Dataset,
    asked: &Asked,
    now: Result<Listing, Error>,
    newest: Option<Result<Listing, Error>>,
) {
    match now {
        Ok(listing) => dataset.changed = changes(asked, &listing),
        Err(error) => dataset.unlisted = Some(Unlisted::of(error)),
    }
    match newest {
        Some(Ok(listing)) => {
            let newest = listing.resolved.to_string();
            if newest != asked.pinned {
                dataset.newer = Some(newest);
            }
        }
        Some(Err(error)) if dataset.unlisted.is_none() => {
            dataset.unlisted = Some(Unlisted::of(error));
        }
        _ => {}
    }
}

/// the title, license and access `listing` gives otherwise than data.lock pins them. a fact
/// either side leaves unsaid is no change, so a source that starts to state one raises nothing.
fn changes(asked: &Asked, listing: &Listing) -> Vec<Change> {
    let mut changed = Vec::new();
    for (field, pinned, now) in [
        ("title", &asked.words.title, &listing.title),
        ("license", &asked.words.license, &listing.license),
    ] {
        if let (Some(pinned), Some(now)) = (pinned, now)
            && pinned != now
        {
            changed.push(Change {
                field,
                pinned: pinned.clone(),
                now: now.clone(),
            });
        }
    }
    let known = |access: Access| access != Access::Unknown;
    if known(asked.access) && known(listing.access) && asked.access != listing.access {
        changed.push(Change {
            field: "access",
            pinned: asked.access.word().to_owned(),
            now: listing.access.word().to_owned(),
        });
    }
    changed
}

/// what `found` says of each of `pinned`, under its dataset in `report`.
fn gathered(context: &Context, report: &mut Report, pinned: &[Pinned<'_>], found: Vec<Found>) {
    let at: HashMap<String, usize> = report
        .datasets
        .iter()
        .enumerate()
        .map(|(index, dataset)| (dataset.name.clone(), index))
        .collect();
    for (pinned, found) in pinned.iter().zip(found) {
        let Some(&index) = at.get(pinned.dataset) else {
            continue;
        };
        let dataset = &mut report.datasets[index];
        dataset.files += 1;
        report.files += 1;
        match found {
            Found::Mirror { index, source } => {
                dataset.served += 1;
                dataset.on_mirrors += 1;
                report.served += 1;
                dataset
                    .mirrors
                    .insert(context.mirrors.mirrors.list[index].to_string());
                // an import has no source that could lose it: the mirror simply serves it
                if let Some(lack) =
                    source.filter(|lack| lack.lost() && !matches!(lack, Lack::Imported))
                {
                    dataset.lost_at_source.push(unserved(pinned, lack));
                }
            }
            Found::Source => {
                dataset.served += 1;
                report.served += 1;
            }
            // the source gave the file, as it did to the download, only not its size
            Found::Nowhere(Lack::Unsized(_)) => {
                dataset.served += 1;
                dataset.no_size += 1;
                report.served += 1;
            }
            Found::Nowhere(lack) if lack.lost() => dataset.not_served.push(unserved(pinned, lack)),
            Found::Nowhere(lack) => {
                report.unchecked += 1;
                dataset.unchecked.push(unserved(pinned, lack));
            }
        }
    }
}

/// `pinned`, which no mirror or source serves for `lack`.
fn unserved(pinned: &Pinned<'_>, lack: Lack) -> Unserved {
    let (outcome, size) = (lack.outcome(), lack.size());
    let error = lack.error(pinned.size);
    Unserved {
        file: pinned.label(),
        outcome,
        size,
        message: error.message,
        help: error.help.map(String::from),
        kind: error.kind,
    }
}

/// a line for each dataset, as [`tell_dataset`] says it; then the closing lines, and what to do.
fn tell(output: &Output, report: &Report) {
    if report.lockless {
        output.done(
            "Ok",
            "there is no data.lock, so nothing pins files to audit",
        );
        output.hint(&format!(
            "nothing pins the files' bytes yet: {}",
            report.pins_lockless
        ));
        return;
    }
    let shown = output
        .answer_width()
        .filter(|_| !output.shows_details())
        .map(|_| SHOWN);
    for dataset in &report.datasets {
        tell_dataset(output, dataset, shown);
    }
    let unserved = report.files - report.served - report.unchecked;
    let gone = report.gone().count();
    let unasked = report.unasked().count();
    let moved = report.moved();
    if unserved == 0 && report.unchecked == 0 && gone + unasked + moved.len() == 0 {
        output.done(
            "Ok",
            &match report.files {
                0 => "data.lock pins no file to ask about".to_owned(),
                1 => "the file in data.lock is still served, and its source changed nothing"
                    .to_owned(),
                files => format!(
                    "all {} files in data.lock are still served, and their sources changed nothing",
                    units::grouped(files)
                ),
            },
        );
        return;
    }
    let of_all = |count: u64| {
        format!(
            "{} of {} in data.lock",
            units::grouped(count),
            units::count(report.files, "file", "files")
        )
    };
    let datasets = |count: usize| units::count(count as u64, "dataset", "datasets");
    if unserved > 0 {
        output.mark(
            "Found",
            &format!("{} that no mirror or source serves", of_all(unserved)),
        );
    }
    if gone > 0 {
        output.mark(
            "Found",
            &format!("{} that their source no longer lists", datasets(gone)),
        );
    }
    if !moved.is_empty() {
        output.mark(
            "Found",
            &format!(
                "{} with a newer version or a change at their source",
                datasets(moved.len())
            ),
        );
    }
    if report.unchecked > 0 {
        output.mark(
            "Unchecked",
            &format!("{}, which may still be served", of_all(report.unchecked)),
        );
    }
    if unasked > 0 {
        output.mark(
            "Unchecked",
            &format!(
                "{} whose source could not say whether it changed",
                datasets(unasked)
            ),
        );
    }
    let helps = helps(report);
    for help in helps.iter().take(HINTS) {
        output.hint(help);
    }
    if unserved > 0 {
        output.hint(&next(report));
    }
    if !moved.is_empty() {
        output.hint(&upgrade(&moved));
    }
    if unserved == 0 && gone + moved.len() == 0 && helps.is_empty() {
        output.hint(probe::AGAIN);
    }
}

/// the distinct helps of the files and datasets no source served or answered for clearly.
fn helps(report: &Report) -> Vec<&str> {
    let mut helps: Vec<&str> = Vec::new();
    let unlisted = report
        .unlisted()
        .map(|(_, unlisted)| unlisted.help.as_deref());
    for help in report
        .unserved()
        .chain(report.unchecked())
        .map(|file| file.help.as_deref())
        .chain(unlisted)
        .flatten()
    {
        if !helps.contains(&help) {
            helps.push(help);
        }
    }
    helps
}

/// a step on stderr for `dataset` when its sources serve every file, else a row on stdout for its
/// files not served, one for those not checked and one for those only a mirror serves, each with a
/// row under it for each file, at most `shown` but for `-v`; then a row for a newer version, one
/// for what changed and one for why its source did not list it.
fn tell_dataset(output: &Output, dataset: &Dataset, shown: Option<usize>) {
    let files = units::count(dataset.files, "file", "files");
    if dataset.not_served.is_empty()
        && dataset.unchecked.is_empty()
        && dataset.lost_at_source.is_empty()
    {
        let mirrored = match (dataset.on_mirrors, dataset.mirrors.len()) {
            (0, _) => String::new(),
            (on, 1) => format!(
                " · {} on {}",
                units::grouped(on),
                dataset.mirrors.first().map_or("", String::as_str)
            ),
            (on, mirrors) => format!(" · {} on {mirrors} mirrors", units::grouped(on)),
        };
        let size_note = match dataset.no_size {
            0 => String::new(),
            count => format!(" · {} answered without a size", units::grouped(count)),
        };
        output.step(
            "Served",
            &format!("{} · {files}{mirrored}{size_note}", dataset.name),
        );
    }
    for (label, listed, tail) in [
        ("Not served", &dataset.not_served, ""),
        ("Unchecked", &dataset.unchecked, ""),
        (
            "Source lost",
            &dataset.lost_at_source,
            ", which only a mirror serves now",
        ),
    ] {
        if listed.is_empty() {
            continue;
        }
        let count = units::grouped(listed.len() as u64);
        output.report(
            label,
            &format!("{} · {count} of {files}{tail}", dataset.name),
        );
        let most = shown.unwrap_or(usize::MAX);
        for file in listed.iter().take(most) {
            output.report("", &file.said());
        }
        let hidden = listed.len().saturating_sub(most);
        if hidden > 0 {
            output.report(
                "",
                &format!(
                    "and {} more · `{}` names each",
                    units::grouped(hidden as u64),
                    super::project::command("audit -v")
                ),
            );
        }
    }
    if let Some(newer) = &dataset.newer {
        output.report(
            "Newer",
            &format!(
                "{} · {newer}, where data.lock pins {}",
                dataset.name, dataset.pinned
            ),
        );
    }
    if !dataset.changed.is_empty() {
        let changed: Vec<String> = dataset.changed.iter().map(Change::said).collect();
        output.report(
            "Changed",
            &format!("{} · {}", dataset.name, changed.join(" · ")),
        );
    }
    if let Some(unlisted) = &dataset.unlisted {
        let label = if unlisted.outcome == "gone" {
            "Not listed"
        } else {
            "Unchecked"
        };
        output.report(
            label,
            &format!(
                "{} · {}: {}",
                dataset.name, unlisted.outcome, unlisted.message
            ),
        );
    }
}

/// what to do about the files no mirror or source serves: keep a copy on a mirror, or pin what a
/// source that lost or changed one serves now.
fn next(report: &Report) -> String {
    let push = format!(
        "`{}` from a machine whose cache holds them keeps a copy on a mirror",
        super::project::command("push")
    );
    let moved: Vec<&str> = report
        .datasets
        .iter()
        .filter(|dataset| {
            dataset
                .not_served
                .iter()
                .any(|file| matches!(file.outcome, "changed" | "gone"))
        })
        .map(|dataset| dataset.name.as_str())
        .collect();
    if moved.is_empty() {
        return push;
    }
    format!(
        "{push}, or `{}` pins what the source serves now",
        super::project::command(&format!("lock --upgrade {}", moved.join(" ")))
    )
}

/// the command that pins what the sources of `moved` list now.
fn upgrade(moved: &[&str]) -> String {
    format!(
        "`{}` pins what their sources list now; check the change before you commit data.lock",
        super::project::command(&format!("lock --upgrade {}", moved.join(" ")))
    )
}

/// fails for `--check`: with `data.not_ready` naming each file no mirror or source serves and each
/// dataset its source no longer lists, else with `lock.outdated` naming each newer version and
/// change, else, when some could not be checked, with the error a download would meet, a refusal's
/// before no answer's, since asking again later does not end a refusal.
pub(super) fn ready(report: &Report) -> Result<(), Error> {
    if report.lockless {
        return Err(Error::new(
            Kind::NotReady,
            "there is no data.lock, so nothing pins files to audit",
        )
        .help(report.pins_lockless.clone()));
    }
    let behind: Vec<String> = report.datasets.iter().flat_map(Dataset::behind).collect();
    let files: Vec<String> = report.unserved().map(Unserved::said).collect();
    let gone: Vec<String> = report
        .gone()
        .map(|(name, unlisted)| format!("{name} · gone: {}", unlisted.message))
        .collect();
    if !files.is_empty() || !gone.is_empty() {
        let message = if files.is_empty() {
            format!(
                "the sources no longer list {} data.lock pins",
                units::count(gone.len() as u64, "dataset", "datasets")
            )
        } else {
            format!(
                "no mirror or source serves {} data.lock pins",
                units::count(files.len() as u64, "file", "files")
            )
        };
        let lines: Vec<String> = files.into_iter().chain(gone).chain(behind).collect();
        return Err(Error::new(Kind::NotReady, message)
            .details_up_to(&lines, NAMED)
            .help(next(report)));
    }
    let moved = report.moved();
    if !moved.is_empty() {
        return Err(Error::new(
            Kind::Outdated,
            format!(
                "data.lock is behind the sources of {}",
                units::listed(&moved, "and")
            ),
        )
        .details_up_to(&behind, NAMED)
        .help(upgrade(&moved)));
    }
    let files: Vec<(String, Kind, Option<&str>)> = report
        .unchecked()
        .map(|file| (file.said(), file.kind, file.help.as_deref()))
        .collect();
    let datasets: Vec<(String, Kind, Option<&str>)> = report
        .unasked()
        .map(|(name, unlisted)| {
            let line = format!("{name} · {}: {}", unlisted.outcome, unlisted.message);
            (line, unlisted.kind, unlisted.help.as_deref())
        })
        .collect();
    let Some((_, kind, help)) = files
        .iter()
        .chain(&datasets)
        .min_by_key(|(_, kind, _)| probe::urgency(*kind))
    else {
        return Ok(());
    };
    let message = if files.is_empty() {
        format!(
            "could not ask the sources of {} whether they changed",
            units::count(datasets.len() as u64, "dataset", "datasets")
        )
    } else {
        format!(
            "could not check {} data.lock pins, which may still be served",
            units::count(files.len() as u64, "file", "files")
        )
    };
    let lines: Vec<String> = files
        .iter()
        .chain(&datasets)
        .map(|(line, _, _)| line.clone())
        .collect();
    Err(Error::new(*kind, message)
        .details_up_to(&lines, NAMED)
        .help(help.unwrap_or(probe::AGAIN)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unserved(outcome: &'static str, kind: Kind) -> Unserved {
        Unserved {
            file: format!("pets/{outcome}.png"),
            outcome,
            size: None,
            message: format!("{outcome} at data.example.org"),
            help: None,
            kind,
        }
    }

    fn pets() -> Report {
        let mut pets = Dataset::new("pets", "zenodo:1");
        (pets.files, pets.served, pets.on_mirrors, pets.no_size) = (4, 4, 1, 1);
        Report {
            files: 4,
            served: 4,
            datasets: vec![pets],
            ..Report::default()
        }
    }

    /// the check fails as not ready only for files served nowhere, naming each; else, for files it
    /// could not check, with the error a download would meet, a refusal's first; and it passes a
    /// project whose every file a mirror or source serves.
    #[test]
    fn the_check_fails_as_not_ready_only_for_files_served_nowhere() {
        let mut report = pets();
        assert!(ready(&report).is_ok());
        let pets = &mut report.datasets[0];
        pets.not_served = vec![unserved("gone", Kind::ReferenceUnresolved)];
        pets.unchecked = vec![
            unserved("unknown", Kind::NetworkStatus),
            unserved("refused", Kind::AuthRequired),
        ];
        let error = ready(&report).unwrap_err();
        assert_eq!(error.kind, Kind::NotReady);
        assert_eq!(
            error.message,
            "no mirror or source serves 1 file data.lock pins"
        );
        assert_eq!(
            error.details,
            ["pets/gone.png · gone: gone at data.example.org"]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "`fetchloom push` from a machine whose cache holds them keeps a copy on a mirror, or `fetchloom lock --upgrade pets` pins what the source serves now"
            )
        );
        // pinning again brings back no file that only this machine had
        report.datasets[0].not_served = vec![unserved("imported", Kind::ReferenceUnresolved)];
        assert_eq!(
            ready(&report).unwrap_err().help.as_deref(),
            Some("`fetchloom push` from a machine whose cache holds them keeps a copy on a mirror")
        );
        // with no file lost, those not checked fail it as a download would, a refusal first
        report.datasets[0].not_served.clear();
        let error = ready(&report).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.message,
            "could not check 2 files data.lock pins, which may still be served"
        );
        assert_eq!(
            error.details,
            [
                "pets/unknown.png · unknown: unknown at data.example.org",
                "pets/refused.png · refused: refused at data.example.org"
            ]
        );
        report.datasets[0].unchecked.pop();
        let error = ready(&report).unwrap_err();
        assert_eq!(
            (error.kind, error.help.as_deref()),
            (Kind::NetworkStatus, Some(probe::AGAIN))
        );
    }

    /// a newer version or a changed fact fails the check as data.lock out of date, naming the
    /// dataset and what moved, with the upgrade that pins it; a lost file still comes first.
    #[test]
    fn a_source_that_moved_on_fails_the_check_as_outdated() {
        let mut report = pets();
        let pets = &mut report.datasets[0];
        pets.newer = Some("zenodo:2".to_owned());
        pets.changed = vec![Change {
            field: "license",
            pinned: "cc-by-4.0".to_owned(),
            now: "cc0-1.0".to_owned(),
        }];
        let error = ready(&report).unwrap_err();
        assert_eq!(error.kind, Kind::Outdated);
        assert_eq!(error.message, "data.lock is behind the sources of pets");
        assert_eq!(
            error.details,
            [
                "pets · newer version zenodo:2, where data.lock pins zenodo:1",
                "pets · license cc-by-4.0 → cc0-1.0"
            ]
        );
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.starts_with("`fetchloom lock --upgrade pets` pins")),
            "{:?}",
            error.help
        );
        report.datasets[0].unlisted = Some(Unlisted::of(Error::new(
            Kind::ReferenceUnresolved,
            "zenodo:1 does not exist, or is not public",
        )));
        let error = ready(&report).unwrap_err();
        assert_eq!(error.kind, Kind::NotReady);
        assert_eq!(
            error.message,
            "the sources no longer list 1 dataset data.lock pins"
        );
        assert_eq!(
            error.details[0],
            "pets · gone: zenodo:1 does not exist, or is not public"
        );
    }

    /// a fact only one side states is no change, so a source that starts to say it raises
    /// nothing; a title, a license and an access both state differently each are.
    #[test]
    fn only_facts_both_sides_state_can_change() {
        let asked = Asked {
            pinned: "zenodo:1".to_owned(),
            upgrade: None,
            select: Vec::new(),
            words: Words {
                title: Some("Iris".to_owned()),
                license: None,
                doi: None,
            },
            access: Access::Unknown,
        };
        let listing = |title: &str, license: Option<&str>, access: Access| Listing {
            resolved: Reference::parse("zenodo:1").unwrap().reference,
            title: Some(title.to_owned()),
            license: license.map(str::to_owned),
            doi: None,
            access,
            files: Vec::new(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
            within: Vec::new(),
        };
        let same = listing("Iris", Some("cc0-1.0"), Access::Public);
        assert!(changes(&asked, &same).is_empty());
        let asked = Asked {
            words: Words {
                license: Some("cc-by-4.0".to_owned()),
                ..asked.words
            },
            access: Access::Public,
            ..asked
        };
        let said: Vec<String> = changes(&asked, &listing("Irises", Some("cc0-1.0"), Access::Login))
            .iter()
            .map(Change::said)
            .collect();
        assert_eq!(
            said,
            [
                "title \"Iris\" → \"Irises\"",
                "license cc-by-4.0 → cc0-1.0",
                "access public → login"
            ]
        );
    }
}
