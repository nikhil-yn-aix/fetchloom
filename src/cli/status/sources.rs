//! `status --sources`: whether a mirror or its source still serves each file data.lock pins, asked
//! without downloading any, told a line a dataset, and the check `status --sources --check` makes.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Serialize;

use super::SHOWN;
use crate::cli::Context;
use crate::cli::probe::{self, Folders, Found, Lack, Pinned};
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::project::Project;
use crate::units;

/// the most files the `--check` error names; the command's result lists them all.
const NAMED: usize = 20;

/// the most hints of distinct refusals and failures told after the lines.
const HINTS: usize = 3;

/// what to do when files could not be checked and none gave a hint of its own.
const AGAIN: &str = "ask again later: a source that gave no answer may be busy or down";

/// what `--sources` found, as `--json` gives it.
#[derive(Debug, Default, Serialize)]
pub(super) struct Sources {
    /// the files data.lock pins for the datasets data.toml fetches, and of those, how many a
    /// mirror or source still serves and how many could not be checked.
    files: u64,
    served: u64,
    unchecked: u64,
    datasets: Vec<Dataset>,
}

/// one dataset's files, by where they are served.
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
    /// the mirrors that hold any, as the run names them.
    #[serde(skip)]
    mirrors: BTreeSet<String>,
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

impl Sources {
    fn unserved(&self) -> impl Iterator<Item = &Unserved> {
        self.datasets.iter().flat_map(|dataset| &dataset.not_served)
    }

    fn unchecked(&self) -> impl Iterator<Item = &Unserved> {
        self.datasets.iter().flat_map(|dataset| &dataset.unchecked)
    }
}

/// refuses `--sources` offline, which may ask nobody.
pub(super) fn askable(context: &Context) -> Result<(), Error> {
    if !context.offline.enabled() {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        "--sources asks each mirror and source over the network, which offline mode does not allow",
    )
    .help(context.offline.retry()))
}

/// asks the run's mirrors, then each source, whether they still serve each file data.lock pins
/// for the datasets data.toml fetches; a recipe's result is not asked about, nor an archive's
/// files, only the archive. fails offline, which asks nobody, or at a stop.
pub(super) fn check(context: &Context, project: &Project) -> Result<Sources, Error> {
    let (_, manifest) = project.manifest()?;
    let Some(lock) = project.lock()? else {
        return Ok(Sources::default());
    };
    // a sync carries a pin over to the name data.toml gives it now, as status reads it
    let (lock, _) = crate::cli::sync::rename::forecast(&manifest, lock);
    let mut pinned = Vec::new();
    for (name, entry) in &manifest.datasets {
        let Some(source) = entry.source() else {
            continue;
        };
        if let Some(locked) = lock.get(name) {
            let files = locked.files.iter();
            pinned.extend(files.map(|file| Pinned::locked(name, file, source.web_page)));
        } else if let Some(stated) = lock.stated(name) {
            let files = stated.files.iter();
            pinned.extend(files.map(|file| Pinned::stated(name, file, source.web_page)));
        }
    }
    if pinned.is_empty() {
        return Ok(Sources::default());
    }
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|err| Error::new(Kind::LocalIo, format!("cannot ask the sources: {err}")))?;
    let net = Arc::new(context.net());
    let found = {
        let places = if context.mirrors.mirrors.list.is_empty() {
            "their sources"
        } else {
            "their mirrors and sources"
        };
        let _waiting = context.output.wait(
            "Checking",
            format!(
                "{} at {places}",
                units::count(pinned.len() as u64, "file", "files")
            ),
        );
        runtime.block_on(probe::find(context, &net, &pinned, Folders::Sized))?
    };
    Ok(gathered(context, &pinned, found))
}

/// what `found` says of each of `pinned`, by dataset in their order.
fn gathered(context: &Context, pinned: &[Pinned<'_>], found: Vec<Found>) -> Sources {
    let mut sources = Sources::default();
    for (pinned, found) in pinned.iter().zip(found) {
        if sources
            .datasets
            .last()
            .is_none_or(|dataset| dataset.name != pinned.dataset)
        {
            sources.datasets.push(Dataset {
                name: pinned.dataset.to_owned(),
                files: 0,
                served: 0,
                on_mirrors: 0,
                no_size: 0,
                not_served: Vec::new(),
                unchecked: Vec::new(),
                mirrors: BTreeSet::new(),
            });
        }
        let Some(dataset) = sources.datasets.last_mut() else {
            continue;
        };
        dataset.files += 1;
        sources.files += 1;
        match found {
            Found::Mirror(index) => {
                dataset.served += 1;
                dataset.on_mirrors += 1;
                sources.served += 1;
                dataset
                    .mirrors
                    .insert(context.mirrors.mirrors.list[index].to_string());
            }
            Found::Source => {
                dataset.served += 1;
                sources.served += 1;
            }
            // the source gave the file, as it did to the download, only not its size
            Found::Nowhere(Lack::Unsized(_)) => {
                dataset.served += 1;
                dataset.no_size += 1;
                sources.served += 1;
            }
            Found::Nowhere(lack) if lack.lost() => dataset.not_served.push(unserved(pinned, lack)),
            Found::Nowhere(lack) => {
                sources.unchecked += 1;
                dataset.unchecked.push(unserved(pinned, lack));
            }
        }
    }
    sources
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
pub(super) fn tell(output: &Output, sources: &Sources) {
    let shown = output
        .answer_width()
        .filter(|_| !output.shows_details())
        .map(|_| SHOWN);
    for dataset in &sources.datasets {
        tell_dataset(output, dataset, shown);
    }
    let unserved = sources.files - sources.served - sources.unchecked;
    if unserved == 0 && sources.unchecked == 0 {
        output.done(
            "Ok",
            &match sources.files {
                0 => "data.lock pins no file to ask about".to_owned(),
                1 => "the file in data.lock is still served".to_owned(),
                files => format!(
                    "all {} files in data.lock are still served",
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
            units::count(sources.files, "file", "files")
        )
    };
    if unserved > 0 {
        output.mark(
            "Found",
            &format!("{} that no mirror or source serves", of_all(unserved)),
        );
    }
    if sources.unchecked > 0 {
        output.mark(
            "Unchecked",
            &format!("{}, which may still be served", of_all(sources.unchecked)),
        );
    }
    let mut helps: Vec<&str> = Vec::new();
    for help in sources
        .unserved()
        .chain(sources.unchecked())
        .filter_map(|file| file.help.as_deref())
    {
        if !helps.contains(&help) {
            helps.push(help);
        }
    }
    for help in helps.iter().take(HINTS) {
        output.hint(help);
    }
    if unserved > 0 {
        output.hint(&next(sources));
    } else if helps.is_empty() {
        output.hint(AGAIN);
    }
}

/// a step on stderr for `dataset` when every file is served, else a row on stdout for its files
/// not served and one for those not checked, each with a row under it for each file, at most
/// `shown` but for `-v`.
fn tell_dataset(output: &Output, dataset: &Dataset, shown: Option<usize>) {
    let files = units::count(dataset.files, "file", "files");
    if dataset.not_served.is_empty() && dataset.unchecked.is_empty() {
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
        return;
    }
    for (label, listed) in [
        ("Not served", &dataset.not_served),
        ("Unchecked", &dataset.unchecked),
    ] {
        if listed.is_empty() {
            continue;
        }
        let count = units::grouped(listed.len() as u64);
        output.report(label, &format!("{} · {count} of {files}", dataset.name));
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
                    crate::cli::project::command("status --sources -v")
                ),
            );
        }
    }
}

/// what to do about the files no mirror or source serves: keep a copy on a mirror, or pin what a
/// source that lost or changed one serves now.
fn next(sources: &Sources) -> String {
    let push = format!(
        "`{}` from a machine whose cache holds them keeps a copy on a mirror",
        crate::cli::project::command("push")
    );
    let moved: Vec<&str> = sources
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
        crate::cli::project::command(&format!("lock --upgrade {}", moved.join(" ")))
    )
}

/// fails for `--check`: with `data.not_ready` naming each file no mirror or source serves, else,
/// when some could not be checked, with the error a download of one would meet, a refusal's before
/// no answer's, since asking again later does not end a refusal.
pub(super) fn ready(sources: &Sources) -> Result<(), Error> {
    let lines: Vec<String> = sources.unserved().map(Unserved::said).collect();
    if !lines.is_empty() {
        return Err(Error::new(
            Kind::NotReady,
            format!(
                "no mirror or source serves {} data.lock pins",
                units::count(lines.len() as u64, "file", "files")
            ),
        )
        .details_up_to(&lines, NAMED)
        .help(next(sources)));
    }
    let Some(first) = sources.unchecked().min_by_key(|file| match file.kind {
        Kind::AuthRequired => 0,
        Kind::NetworkUnreachable
        | Kind::NetworkStatus
        | Kind::NetworkRateLimited
        | Kind::NetworkRefused
        | Kind::OfflineMissing => 2,
        _ => 1,
    }) else {
        return Ok(());
    };
    let lines: Vec<String> = sources.unchecked().map(Unserved::said).collect();
    Err(Error::new(
        first.kind,
        format!(
            "could not check {} data.lock pins, which may still be served",
            units::count(lines.len() as u64, "file", "files")
        ),
    )
    .details_up_to(&lines, NAMED)
    .help(first.help.as_deref().unwrap_or(AGAIN)))
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

    /// the check fails as not ready only for files served nowhere, naming each; else, for files it
    /// could not check, with the error a download would meet, a refusal's first; and it passes a
    /// project whose every file a mirror or source serves.
    #[test]
    fn the_check_fails_as_not_ready_only_for_files_served_nowhere() {
        let mut sources = Sources {
            files: 4,
            served: 4,
            unchecked: 0,
            datasets: vec![Dataset {
                name: "pets".to_owned(),
                files: 4,
                served: 4,
                on_mirrors: 1,
                no_size: 1,
                not_served: Vec::new(),
                unchecked: Vec::new(),
                mirrors: BTreeSet::new(),
            }],
        };
        assert!(ready(&sources).is_ok());
        let pets = &mut sources.datasets[0];
        pets.not_served = vec![unserved("gone", Kind::ReferenceUnresolved)];
        pets.unchecked = vec![
            unserved("unknown", Kind::NetworkStatus),
            unserved("refused", Kind::AuthRequired),
        ];
        let error = ready(&sources).unwrap_err();
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
        sources.datasets[0].not_served = vec![unserved("imported", Kind::ReferenceUnresolved)];
        assert_eq!(
            ready(&sources).unwrap_err().help.as_deref(),
            Some("`fetchloom push` from a machine whose cache holds them keeps a copy on a mirror")
        );
        // with no file lost, those not checked fail it as a download would, a refusal first
        sources.datasets[0].not_served.clear();
        let error = ready(&sources).unwrap_err();
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
        sources.datasets[0].unchecked.pop();
        let error = ready(&sources).unwrap_err();
        assert_eq!(
            (error.kind, error.help.as_deref()),
            (Kind::NetworkStatus, Some(AGAIN))
        );
    }
}
