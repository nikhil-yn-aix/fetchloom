//! `fl restore`: changed or missing files put back from the store, never from the network.

use std::path::{Component, Path, PathBuf};

use fl_model::document::Status;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_store::fingerprint::Fingerprint;
use serde_json::json;

use crate::commands::{Context, Found, dataset_dir, ladder, plural, some_of, survey};
use crate::failure::Failure;
use crate::project::{Placed, Project};
use crate::report::Outcome;

/// `fl restore`.
pub(crate) fn run(context: &Context<'_>, paths: &[PathBuf]) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let _held = context.hold(&project)?;
    let manifest = project.manifest()?;
    let lock = project.lock()?.ok_or_else(|| {
        Failure::new(
            ErrorKind::LockStale,
            "there is no data.lock to restore from",
        )
        .help("run `fl sync` to lock and download the datasets")
    })?;
    let root = project.data_dir(&manifest)?;
    let within: Vec<String> = paths
        .iter()
        .map(|path| key_of(&root, &context.cwd.join(path)))
        .collect::<Result<_, _>>()?;
    let shown_root = manifest.project.data_dir.to_string();
    let mut state = project.state()?;
    let mut restored = 0;
    let mut replaced = Vec::new();
    let mut unavailable = Vec::new();
    let mut plans = Vec::new();
    for locked in lock.datasets() {
        let dir = dataset_dir(&manifest, &locked.name)?;
        let found = survey(context, &root, &shown_root, &dir, locked, &state)?;
        let mut files = Vec::new();
        for slot in &found.slots {
            let wanted = within.is_empty()
                || within.iter().any(|key| {
                    key.is_empty() || slot.key == *key || slot.key.starts_with(&format!("{key}/"))
                });
            if slot.found == Found::Clean || !wanted {
                continue;
            }
            if !context.engine.reuse(&slot.file.blake3) {
                unavailable.push((locked.name.clone(), slot.shown.clone()));
                continue;
            }
            if slot.found == Found::Modified {
                replaced.push(slot.shown.clone());
            }
            files.push((slot.key.clone(), slot.file.path.clone(), slot.file.blake3));
        }
        plans.push((dir, ladder(&manifest, &locked.name, context.link), files));
    }
    agree_to_replace(context, &replaced)?;
    for (dir, ladder, files) in &plans {
        if files.is_empty() {
            continue;
        }
        let pairs: Vec<(DataPath, [u8; 32])> = files
            .iter()
            .map(|(_, path, blake3)| (path.clone(), *blake3))
            .collect();
        let placed = context.engine.link(&root, dir, &pairs, *ladder)?;
        for ((destination, _), (key, path, blake3)) in placed.iter().zip(files) {
            let print = Fingerprint::of(destination.path())?;
            state.files.insert(key.clone(), Placed::new(blake3, &print));
            context.emit(Event::Note {
                label: "Restored".to_owned(),
                text: format!("{shown_root}/{dir}/{path}"),
            });
            restored += 1;
        }
    }
    project.write_state(&state, false)?;
    let result = json!({
        "restored": restored,
        "unavailable": unavailable.iter().map(|(_, shown)| shown.as_str()).collect::<Vec<_>>(),
    });
    let sentence = match restored {
        0 if unavailable.is_empty() => {
            "Nothing to restore, every file matches data.lock.".to_owned()
        }
        n => format!(
            "Restored {} from the downloads kept on this machine.",
            plural(n, "file", "files")
        ),
    };
    if let Some(failure) = unavailable_failure(&unavailable) {
        if restored == 0 {
            return Err(failure);
        }
        context.emit(Event::Failed(failure.into_problem()));
        return Ok(Outcome {
            status: Status::Partial,
            exit: Some(ErrorKind::OfflineMissing),
            ..Outcome::ok(result, sentence)
        });
    }
    Ok(Outcome::ok(result, sentence))
}

/// Asks before files the user changed are replaced, when there are any.
fn agree_to_replace(context: &Context<'_>, replaced: &[String]) -> Result<(), Failure> {
    if replaced.is_empty() {
        return Ok(());
    }
    context.agree(
        &format!(
            "Replace {} you changed with the downloaded {}? ({})",
            plural(replaced.len() as u64, "file", "files"),
            if replaced.len() == 1 { "one" } else { "ones" },
            some_of(replaced, 3)
        ),
        &format!(
            "{} you changed would be replaced",
            plural(replaced.len() as u64, "file", "files")
        ),
        &format!(
            "replacing {}",
            plural(replaced.len() as u64, "changed file", "changed files")
        ),
    )
}

/// The failure of the files no store holds, which restore cannot put back.
fn unavailable_failure(unavailable: &[(Name, String)]) -> Option<Failure> {
    let (name, _) = unavailable.first()?;
    let shown: Vec<String> = unavailable.iter().map(|(_, shown)| shown.clone()).collect();
    Some(
        Failure::new(
            ErrorKind::OfflineMissing,
            format!(
                "{} {} not kept on this machine, and restore never downloads: {}",
                plural(shown.len() as u64, "file", "files"),
                if shown.len() == 1 { "is" } else { "are" },
                some_of(&shown, 3)
            ),
        )
        .help(format!("`fl sync {name}` downloads them")),
    )
}

/// The key under the data directory at `root` that `path` names: its components joined by `/`,
/// empty for the data directory itself.
fn key_of(root: &Path, path: &Path) -> Result<String, Failure> {
    let absolute =
        std::path::absolute(path).map_err(|error| Failure::io("resolve", path, &error))?;
    let root = std::path::absolute(root).map_err(|error| Failure::io("resolve", root, &error))?;
    let outside = || {
        Failure::new(
            ErrorKind::Usage,
            format!("`{}` is not under {}", path.display(), root.display()),
        )
        .help("name files or folders inside the data folder, like data/NAME/file.csv")
    };
    let inside = absolute.strip_prefix(&root).map_err(|_| outside())?;
    let mut parts = Vec::new();
    for component in inside.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str().ok_or_else(outside)?.to_owned()),
            _ => return Err(outside()),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_the_data_directory_becomes_its_key() {
        let root = Path::new("/p/data");
        assert_eq!(
            key_of(root, &root.join("eeg").join("a.edf")).unwrap(),
            "eeg/a.edf"
        );
        assert_eq!(key_of(root, root).unwrap(), "");
        assert_eq!(
            key_of(root, Path::new("/p/other")).unwrap_err().kind(),
            ErrorKind::Usage
        );
    }
}
