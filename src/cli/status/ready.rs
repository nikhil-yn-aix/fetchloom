//! whether the data a run asked for is ready to use: every placed file holds its pinned bytes, and
//! data.lock pins what data.toml asks. `sync --strict` and
//! `status --check` share it, failing with `data.not_ready`.

use std::collections::HashSet;

use super::Status;
use crate::cli::project::command;
use crate::cli::sync::Scope;
use crate::cli::sync::disk::is_inside;
use crate::cli::sync::place::{ChangedOutput, Placement, Reason};
use crate::error::{Error, Kind};
use crate::project::lock::Waiting;
use crate::project::manifest::Manifest;

/// the most files and datasets the error names; the command's result lists them all.
const NAMED: usize = 20;

/// a file or dataset that keeps the data from being ready.
#[derive(Debug)]
pub(in crate::cli) enum Unready {
    /// a file at a path data.lock places that holds other bytes, which sync keeps: one fetchloom
    /// `placed` that changed since, or one it did not place.
    Kept { path: String, placed: bool },
    /// something else stands where a file data.lock places belongs.
    Blocked(String),
    /// a file at a path data.lock places that this account cannot read, which no sync reads either.
    Unreadable(String),
    /// a file data.lock places that is not there, as in a fresh clone.
    Missing(String),
    /// a file placed for an older data.lock, which sync replaces.
    Outdated(String),
    /// a placed file that no longer holds the pinned bytes though no edit of this project's
    /// shows, which only hashing found when `hashed`.
    Damaged { path: String, hashed: bool },
    /// an archive this machine never unpacked, so none of its files is in place.
    Unpacked(String),
    /// a fetched dataset data.lock does not pin as data.toml asks.
    Unpinned(String),
    /// every dataset, since there is no data.lock, with what pins them.
    Lockless(String),
    /// an import no sync pins, since no source lists its files: one whose entry changed since
    /// data.lock `pinned` it, or one data.lock holds no pin of. an output's folder `at`, as a
    /// command names it, is pinned again by its add.
    Import {
        name: String,
        pinned: bool,
        at: Option<String>,
    },
    /// a dataset its source's digests pin, `by` naming them, that no sync has downloaded yet.
    NotDownloaded { name: String, by: &'static str },
    /// a dataset whose file list, which data.lock names, is not beside it.
    Waiting(Box<Waiting>),
    /// an output whose folder changed since it was pinned, which sync leaves whole.
    Output(Box<ChangedOutput>),
    /// an output whose files this machine lacks, as a clone of the project that made it does,
    /// which sync leaves out: its name, and its folder as a command names it.
    Elsewhere {
        name: String,
        shown: String,
        here: bool,
    },
    /// an import with files this machine lacks, missing or damaged, which no link fetches, so no
    /// sync brings them: its name, and its folder.
    Imported { name: String, folder: String },
    /// what a sync refuses before it follows a data folder link the first time, as it says it.
    Refused(String),
}

impl Unready {
    /// the file or archive it names, if it names one.
    pub(super) fn path(&self) -> Option<&str> {
        match self {
            Unready::Kept { path, .. }
            | Unready::Damaged { path, .. }
            | Unready::Blocked(path)
            | Unready::Unreadable(path)
            | Unready::Missing(path)
            | Unready::Outdated(path)
            | Unready::Unpacked(path) => Some(path),
            _ => None,
        }
    }

    /// the line naming it and what makes it ready, `sync` being the words of the sync that does.
    fn said(&self, sync: &str) -> String {
        match self {
            Unready::Kept { path, placed } => {
                let what = if *placed {
                    "changed since fetchloom placed it"
                } else {
                    "holds other bytes than data.lock pins"
                };
                format!(
                    "{path} {what}: save what you need, then `{}` replaces it",
                    command(&format!("{sync} --force"))
                )
            }
            Unready::Blocked(path) => format!(
                "{path} is not in place, since something else is where it belongs: move that, then `{}` places it",
                command(sync)
            ),
            Unready::Unreadable(path) => format!(
                "{path} cannot be read, so what it holds is unknown: check its permissions, then run `{}`",
                command(sync)
            ),
            Unready::Missing(path) => format!("{path} is missing: `{}` places it", command(sync)),
            Unready::Outdated(path) => format!(
                "{path} is from an older data.lock: `{}` replaces it",
                command(sync)
            ),
            Unready::Damaged { path, hashed } => format!(
                "{path} no longer holds the pinned bytes: `{}` puts them back",
                command(&if *hashed {
                    format!("{sync} --verify")
                } else {
                    sync.to_owned()
                })
            ),
            Unready::Unpacked(archive) => format!(
                "{archive} is not unpacked on this machine, so its files are not in place: `{}` unpacks it",
                command(sync)
            ),
            Unready::Unpinned(name) => format!(
                "{name} is not pinned as data.toml asks: `{}` pins and places it",
                command(sync)
            ),
            Unready::Import { name, pinned, at } => line(&if *pinned {
                crate::source::local::changed(name, at.as_deref())
            } else {
                crate::source::local::unpinned(name, at.as_deref())
            }),
            Unready::Imported { name, folder } => line(&crate::source::local::imported_elsewhere(
                name, folder, false,
            )),
            Unready::Refused(said) => said.clone(),
            Unready::NotDownloaded { name, by } => format!(
                "{name} is pinned by its source's {by}, not downloaded yet: `{}` downloads and checks it",
                command(&format!("sync {name}"))
            ),
            Unready::Lockless(pins) => {
                format!("there is no data.lock, so nothing pins the files yet: {pins}")
            }
            Unready::Waiting(waiting) => format!(
                "{} waits for its file list {}, which is not there: commit data.lock.d where it was pinned",
                waiting.name, waiting.part
            ),
            Unready::Output(changed) => changed.said(),
            Unready::Elsewhere { name, shown, here } => line(
                &crate::source::local::made_elsewhere(&[(name, shown)], false, *here),
            ),
        }
    }

    /// whether a plain sync makes it ready, changing no file a person made and no pin.
    fn placed_by_sync(&self) -> bool {
        matches!(
            self,
            Unready::Missing(_)
                | Unready::Outdated(_)
                | Unready::Damaged { hashed: false, .. }
                | Unready::Unpacked(_)
                | Unready::NotDownloaded { .. }
        )
    }
}

/// `refused`, a refusal, as one line: what is wrong, then the ways on.
pub(super) fn line(refused: &Error) -> String {
    match &refused.help {
        Some(help) => format!("{}: {help}", refused.message),
        None => refused.message.clone(),
    }
}

/// fails with `data.not_ready` naming each of `unready` with what to do; only when a plain sync
/// makes all of it ready does the help name sync, so it never points at replacing an edit or
/// repinning.
pub(in crate::cli) fn ready(unready: &[Unready]) -> Result<(), Error> {
    ready_by(unready, &|_| None)
}

/// [`ready`], with the sync each line names limited to the dataset `only` gives for it, where a
/// plain sync is refused.
pub(super) fn ready_by(
    unready: &[Unready],
    only: &dyn Fn(&Unready) -> Option<String>,
) -> Result<(), Error> {
    if unready.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = unready
        .iter()
        .map(|line| match only(line) {
            Some(dataset) => line.said(&format!("sync {dataset}")),
            None => line.said("sync"),
        })
        .collect();
    let help = if unready.iter().all(Unready::placed_by_sync) {
        format!("run `{}` to put them in place", command("sync"))
    } else {
        "each line says what makes it ready".to_owned()
    };
    Err(Error::new(Kind::NotReady, "the data is not ready to use")
        .details_up_to(&lines, NAMED)
        .help(help))
}

/// what keeps the data of `scope` from being ready after a sync left `placement`: the files it kept
/// but unneeded ones, and the datasets of `waiting`, whose file lists are not there.
pub(in crate::cli) fn after_sync(
    scope: &Scope,
    placement: &Placement,
    waiting: &[Waiting],
) -> Vec<Unready> {
    let mut unready: Vec<Unready> = placement
        .kept
        .iter()
        .filter_map(|kept| match kept.reason {
            Reason::Changed | Reason::Shared => Some(Unready::Kept {
                path: kept.path.clone(),
                placed: true,
            }),
            Reason::Foreign => Some(Unready::Kept {
                path: kept.path.clone(),
                placed: false,
            }),
            Reason::Blocked => Some(Unready::Blocked(kept.path.clone())),
            // named with its output's other files git tracks, below
            Reason::Unneeded | Reason::Tracked => None,
        })
        .collect();
    unready.extend(
        placement
            .tracked
            .iter()
            .map(|held| Unready::Output(Box::new(held.clone()))),
    );
    unready.extend(
        placement
            .changed_outputs
            .iter()
            .map(|changed| Unready::Output(Box::new(changed.clone()))),
    );
    unready.extend(elsewhere(
        placement
            .elsewhere
            .iter()
            .map(|left| (&left.name, &left.shown, left.here)),
    ));
    unready.extend(
        waiting
            .iter()
            .filter(|waiting| scope.includes(&waiting.name))
            .map(|waiting| Unready::Waiting(Box::new(waiting.clone()))),
    );
    unready
}

/// a line for each output of `outputs`, whose files this machine lacks, with its folder as a
/// command names it and whether the state records its capture here.
fn elsewhere<'o>(
    outputs: impl IntoIterator<Item = (&'o String, &'o String, bool)>,
) -> impl Iterator<Item = Unready> {
    outputs
        .into_iter()
        .map(|(name, shown, here)| Unready::Elsewhere {
            name: name.clone(),
            shown: shown.clone(),
            here,
        })
}

/// the outputs of `status` whose files this machine lacks, made on another machine, each with its
/// folder as a command names it. a changed output is none, as sync leaves it whole.
pub(super) fn lacking_outputs(status: &Status) -> Vec<(&String, &String)> {
    status
        .outputs
        .iter()
        .filter(|(name, _)| {
            status.imported_elsewhere.contains_key(*name)
                && !status
                    .changed_outputs
                    .iter()
                    .any(|changed| changed.name == **name)
        })
        .collect()
}

/// what keeps the data of every dataset from being ready, as `status` found the data folder of
/// `manifest`'s datasets: each output whose folder holds other files than it pins is one line for
/// all its files, as sync leaves such a folder whole, and so is each import, output or not, whose
/// files this machine lacks, as no sync brings them.
pub(super) fn of_status(status: &Status, manifest: &Manifest) -> Vec<Unready> {
    let outputs = &status.changed_outputs;
    let made = lacking_outputs(status);
    let lacking: HashSet<&String> = status.imported_elsewhere.values().flatten().collect();
    let mut unready = Vec::new();
    if status.lockless {
        unready.push(Unready::Lockless(status.pins_lockless.clone()));
    } else {
        unready.extend(
            status
                .unlocked
                .iter()
                .filter(|name| !status.waiting.contains_key(*name))
                .map(|name| match status.imports.get(name) {
                    Some(&pinned) => Unready::Import {
                        name: name.clone(),
                        pinned,
                        at: status.outputs.get(name).cloned(),
                    },
                    None => Unready::Unpinned(name.clone()),
                }),
        );
    }
    unready.extend(
        status
            .waiting
            .values()
            .map(|waiting| Unready::Waiting(Box::new(waiting.clone()))),
    );
    unready.extend(
        status
            .not_downloaded
            .iter()
            .map(|name| Unready::NotDownloaded {
                name: name.clone(),
                by: status.pinned_by.get(name).copied().unwrap_or("digests"),
            }),
    );
    let alone = |path: &&String| {
        !lacking.contains(*path) && !outputs.iter().any(|output| is_inside(path, &output.at))
    };
    let each = |paths: &[String], make: fn(String) -> Unready| {
        paths
            .iter()
            .filter(alone)
            .cloned()
            .map(make)
            .collect::<Vec<_>>()
    };
    unready.extend(each(&status.missing, Unready::Missing));
    unready.extend(each(&status.edited, |path| Unready::Kept {
        path,
        placed: true,
    }));
    unready.extend(each(&status.untracked, |path| Unready::Kept {
        path,
        placed: false,
    }));
    unready.extend(each(&status.unreadable, Unready::Unreadable));
    unready.extend(each(&status.outdated, Unready::Outdated));
    unready.extend(each(&status.not_unpacked, Unready::Unpacked));
    unready.extend(
        status
            .blocked
            .iter()
            .filter(|blocker| alone(&&blocker.path))
            .map(|blocker| Unready::Blocked(blocker.path.clone())),
    );
    unready.extend(
        status
            .damaged
            .iter()
            .filter(alone)
            .map(|path| Unready::Damaged {
                path: path.clone(),
                hashed: status.hashed_only.contains(path),
            }),
    );
    unready.extend(
        outputs
            .iter()
            .map(|changed| Unready::Output(Box::new(changed.clone()))),
    );
    // a capture here whose files went is a changed output, so these were made elsewhere
    unready.extend(elsewhere(
        made.into_iter().map(|(name, shown)| (name, shown, false)),
    ));
    unready.extend(
        status
            .imported_elsewhere
            .keys()
            .filter(|name| !status.outputs.contains_key(*name))
            .map(|name| Unready::Imported {
                name: name.clone(),
                folder: crate::plan::folder(manifest, name),
            }),
    );
    unready
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the help names sync alone only for what a plain sync puts in place: replacing a file a
    /// sync keeps is the person's call.
    #[test]
    fn only_what_a_plain_sync_puts_in_place_gets_it_as_the_next_step() {
        let missing = || Unready::Missing("data/iris/iris.csv".to_owned());
        let placed = ready(&[missing()]).unwrap_err();
        assert_eq!(placed.kind, Kind::NotReady);
        assert_eq!(
            placed.help.as_deref(),
            Some("run `fetchloom sync` to put them in place")
        );
        let edited = Unready::Kept {
            path: "data/iris/notes.csv".to_owned(),
            placed: true,
        };
        for unready in [
            vec![edited],
            vec![missing(), Unready::Blocked("data/x".to_owned())],
        ] {
            let error = ready(&unready).unwrap_err();
            assert_eq!(
                error.help.as_deref(),
                Some("each line says what makes it ready"),
                "{unready:?}"
            );
            assert_eq!(error.details.len(), unready.len());
        }
        assert!(ready(&[]).is_ok());
    }
}
