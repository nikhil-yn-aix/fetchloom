//! whether the data a run asked for is ready to use: each file data.lock places is in place with
//! the bytes it pins, each recipe's pinned result is here, and data.lock pins what data.toml
//! asks. `sync --strict` and `status --check` share it: when the data is not ready, both fail
//! with `data.not_ready`, with a line for each file or dataset in the way, saying what to do.

use super::Status;
use crate::cli::project::command;
use crate::cli::sync::Scope;
use crate::cli::sync::place::{Placement, Reason};
use crate::error::{Error, Kind};
use crate::project::Project;

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
    /// a file data.lock places that is not there, as in a fresh clone.
    Missing(String),
    /// a file placed for an older data.lock, which sync replaces.
    Outdated(String),
    /// a placed file that no longer holds the pinned bytes though no edit of this project's
    /// shows, which only hashing found when `hashed`.
    Damaged { path: String, hashed: bool },
    /// an archive this machine never unpacked, so none of its files is in place.
    Unpacked(String),
    /// a recipe whose pinned result is neither on this machine nor in a folder mirror.
    Unbuilt(String),
    /// a recipe data.lock does not pin as building it now would make, with what `moved`.
    Stale { name: String, moved: String },
    /// a fetched dataset data.lock does not pin as data.toml asks, or every dataset when there
    /// is no data.lock.
    Unpinned(Option<String>),
}

impl Unready {
    /// the line naming it and what makes it ready.
    fn said(&self) -> String {
        match self {
            Unready::Kept { path, placed } => {
                let what = if *placed {
                    "changed since fetchloom placed it"
                } else {
                    "holds other bytes than data.lock pins"
                };
                format!(
                    "{path} {what}: save what you need, then `{}` replaces it",
                    command("sync --force")
                )
            }
            Unready::Blocked(path) => format!(
                "{path} is not in place, since something else is where it belongs: move that, then `{}` places it",
                command("sync")
            ),
            Unready::Missing(path) => format!("{path} is missing: `{}` places it", command("sync")),
            Unready::Outdated(path) => format!(
                "{path} is from an older data.lock: `{}` replaces it",
                command("sync")
            ),
            Unready::Damaged { path, hashed } => format!(
                "{path} no longer holds the pinned bytes: `{}` puts them back",
                command(if *hashed { "sync --verify" } else { "sync" })
            ),
            Unready::Unpacked(archive) => format!(
                "{archive} is not unpacked on this machine, so its files are not in place: `{}` unpacks it",
                command("sync")
            ),
            Unready::Unbuilt(name) => format!(
                "{name}'s pinned result is not on this machine or a mirror: `{}` makes it",
                command(&format!("build {name}"))
            ),
            Unready::Stale { name, moved } => format!(
                "{name} needs a build ({moved}): `{}` makes it",
                command(&format!("build {name}"))
            ),
            Unready::Unpinned(Some(name)) => format!(
                "{name} is not pinned as data.toml asks: `{}` pins and places it",
                command("sync")
            ),
            Unready::Unpinned(None) => format!(
                "there is no data.lock, so nothing pins the files yet: ask whoever shares the project to commit it, or `{}` pins what the sources serve today",
                command("sync")
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
        )
    }
}

/// fails with `data.not_ready` when anything in `unready` keeps the data from being ready,
/// naming each with what to do. only when a plain `fetchloom sync` makes all of it ready does
/// the help name it, so `fix` never replaces an edit, repins or runs a build.
pub(in crate::cli) fn ready(unready: &[Unready]) -> Result<(), Error> {
    if unready.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = unready.iter().map(Unready::said).collect();
    let help = if unready.iter().all(Unready::placed_by_sync) {
        format!("run `{}` to put them in place", command("sync"))
    } else {
        "each line says what makes it ready".to_owned()
    };
    Err(Error::new(Kind::NotReady, "the data is not ready to use")
        .details_up_to(&lines, NAMED)
        .help(help))
}

/// what keeps the data of `scope` from being ready once a sync left the data folder as
/// `placement` says: the files it kept, except those no dataset needs, the recipes whose results
/// it left out, and the recipes data.lock does not pin as building them now would make, which
/// only a build changes.
pub(in crate::cli) fn after_sync(
    project: &Project,
    scope: &Scope,
    placement: &Placement,
) -> Result<Vec<Unready>, Error> {
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
            Reason::Unneeded => None,
        })
        .collect();
    unready.extend(placement.unbuilt.iter().cloned().map(Unready::Unbuilt));
    let (_, manifest) = project.manifest()?;
    let recipes = manifest
        .datasets
        .iter()
        .any(|(name, entry)| entry.recipe().is_some() && scope.includes(name));
    if recipes && let Some(lock) = project.lock()? {
        for (name, moved) in super::unbuilt(project, &manifest, &lock)? {
            if scope.includes(&name) {
                unready.push(Unready::Stale { name, moved });
            }
        }
    }
    Ok(unready)
}

/// what keeps the data of every dataset from being ready, as `status` found the data folder.
pub(super) fn of_status(status: &Status) -> Vec<Unready> {
    let mut unready = Vec::new();
    if status.lockless {
        unready.push(Unready::Unpinned(None));
    } else {
        unready.extend(
            status
                .unlocked
                .iter()
                .filter(|name| !status.building.contains_key(*name))
                .map(|name| Unready::Unpinned(Some(name.clone()))),
        );
    }
    unready.extend(status.building.iter().map(|(name, moved)| Unready::Stale {
        name: name.clone(),
        moved: moved.clone(),
    }));
    let each = |paths: &[String], make: fn(String) -> Unready| {
        paths.iter().cloned().map(make).collect::<Vec<_>>()
    };
    unready.extend(each(&status.missing, Unready::Missing));
    unready.extend(status.edited.iter().map(|path| Unready::Kept {
        path: path.clone(),
        placed: true,
    }));
    unready.extend(status.untracked.iter().map(|path| Unready::Kept {
        path: path.clone(),
        placed: false,
    }));
    unready.extend(each(&status.outdated, Unready::Outdated));
    unready.extend(each(&status.not_unpacked, Unready::Unpacked));
    unready.extend(each(&status.unbuilt, Unready::Unbuilt));
    unready.extend(
        status
            .blocked
            .iter()
            .map(|blocker| Unready::Blocked(blocker.path.clone())),
    );
    unready.extend(status.damaged.iter().map(|path| Unready::Damaged {
        path: path.clone(),
        hashed: status.hashed_only.contains(path),
    }));
    unready
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a file a sync keeps is no fix for a script to run, since replacing it is the person's
    /// call, and neither is a build, while what a plain sync puts in place is.
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
            vec![missing(), Unready::Unbuilt("clean".to_owned())],
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
