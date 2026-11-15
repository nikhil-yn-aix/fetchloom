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

/// what `--sources` found, as `--json` gives it.
#[derive(Debug, Default, Serialize)]
pub(super) struct Sources {
    /// the files data.lock pins for the datasets data.toml fetches, and of those, how many a
    /// mirror or source still serves.
    files: u64,
    served: u64,
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
    /// each file no mirror or source serves.
    not_served: Vec<Unserved>,
    /// the mirrors that hold any, as the run names them.
    #[serde(skip)]
    mirrors: BTreeSet<String>,
}

/// a pinned file no mirror or source serves, and why.
#[derive(Debug, Serialize)]
struct Unserved {
    /// the file as data.lock names it, after its dataset: `iris/iris.csv`.
    file: String,
    /// `changed`, `gone`, `refused`, `unknown` or `imported`.
    outcome: &'static str,
    /// for `changed`, the size the source serves in its place.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
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
        let (Some(source), Some(locked)) = (entry.source(), lock.get(name)) else {
            continue;
        };
        pinned.extend(locked.files.iter().map(|file| Pinned {
            dataset: name,
            file,
            page: source.web_page,
        }));
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
                not_served: Vec::new(),
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
            Found::Nowhere(lack) => dataset.not_served.push(unserved(pinned, lack)),
        }
    }
    sources
}

/// `pinned`, which no mirror or source serves for `lack`.
fn unserved(pinned: &Pinned<'_>, lack: Lack) -> Unserved {
    let (outcome, size) = (lack.outcome(), lack.size());
    let error = lack.error(pinned.file.size);
    Unserved {
        file: pinned.label(),
        outcome,
        size,
        message: error.message,
        help: error.help.map(String::from),
    }
}

/// a line for each dataset: a step on stderr for one served whole, else a row on stdout with a row
/// under it for each file not served, at most [`SHOWN`] at a terminal but for `-v`; then the closing
/// line, and what to do.
pub(super) fn tell(output: &Output, sources: &Sources) {
    let shown = output
        .answer_width()
        .filter(|_| !output.shows_details())
        .map(|_| SHOWN);
    for dataset in &sources.datasets {
        let files = units::count(dataset.files, "file", "files");
        if dataset.not_served.is_empty() {
            let mirrored = match (dataset.on_mirrors, dataset.mirrors.len()) {
                (0, _) => String::new(),
                (on, 1) => format!(
                    " · {} on {}",
                    units::grouped(on),
                    dataset.mirrors.first().map_or("", String::as_str)
                ),
                (on, mirrors) => format!(" · {} on {mirrors} mirrors", units::grouped(on)),
            };
            output.step("Served", &format!("{} · {files}{mirrored}", dataset.name));
            continue;
        }
        output.report(
            "Not served",
            &format!(
                "{} · {} of {files}",
                dataset.name,
                units::grouped(dataset.not_served.len() as u64)
            ),
        );
        let most = shown.unwrap_or(usize::MAX);
        for file in dataset.not_served.iter().take(most) {
            output.report("", &file.said());
        }
        let hidden = dataset.not_served.len().saturating_sub(most);
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
    let unserved = sources.files - sources.served;
    if unserved == 0 {
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
    output.mark(
        "Found",
        &format!(
            "{} of {} in data.lock that no mirror or source serves",
            units::grouped(unserved),
            units::count(sources.files, "file", "files")
        ),
    );
    let mut helps: Vec<&str> = Vec::new();
    for help in sources.unserved().filter_map(|file| file.help.as_deref()) {
        if !helps.contains(&help) {
            helps.push(help);
        }
    }
    for help in helps.iter().take(HINTS) {
        output.hint(help);
    }
    output.hint(&next(sources));
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

/// fails with `data.not_ready` naming each file no mirror or source serves, for `--check`.
pub(super) fn ready(sources: &Sources) -> Result<(), Error> {
    let lines: Vec<String> = sources.unserved().map(Unserved::said).collect();
    if lines.is_empty() {
        return Ok(());
    }
    Err(Error::new(
        Kind::NotReady,
        format!(
            "no mirror or source serves {} data.lock pins",
            units::count(lines.len() as u64, "file", "files")
        ),
    )
    .details_up_to(&lines, NAMED)
    .help(next(sources)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unserved(outcome: &'static str) -> Unserved {
        Unserved {
            file: format!("pets/{outcome}.png"),
            outcome,
            size: None,
            message: format!("{outcome} at data.example.org"),
            help: None,
        }
    }

    /// the check fails only for files served nowhere, naming each, and passes a project whose
    /// every file a mirror or source serves.
    #[test]
    fn the_check_fails_only_for_files_served_nowhere() {
        let mut sources = Sources {
            files: 3,
            served: 3,
            datasets: vec![Dataset {
                name: "pets".to_owned(),
                files: 3,
                served: 3,
                on_mirrors: 1,
                not_served: Vec::new(),
                mirrors: BTreeSet::new(),
            }],
        };
        assert!(ready(&sources).is_ok());
        sources.served = 1;
        sources.datasets[0].served = 1;
        sources.datasets[0].not_served = vec![unserved("gone"), unserved("refused")];
        let error = ready(&sources).unwrap_err();
        assert_eq!(error.kind, Kind::NotReady);
        assert_eq!(
            error.message,
            "no mirror or source serves 2 files data.lock pins"
        );
        assert_eq!(
            error.details,
            [
                "pets/gone.png · gone: gone at data.example.org",
                "pets/refused.png · refused: refused at data.example.org"
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "`fetchloom push` from a machine whose cache holds them keeps a copy on a mirror, or `fetchloom lock --upgrade pets` pins what the source serves now"
            )
        );
        // pinning again brings back no file a source refuses
        sources.datasets[0].not_served.remove(0);
        assert_eq!(
            ready(&sources).unwrap_err().help.as_deref(),
            Some("`fetchloom push` from a machine whose cache holds them keeps a copy on a mirror")
        );
    }
}
