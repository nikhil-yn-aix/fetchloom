//! `fl get`: one reference fetched into a directory, through the store, without a project.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fl_model::digest::Digest;
use fl_model::document::Status;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::path::DataPath;
use fl_model::reference::Reference;
use fl_store::fingerprint;
use serde_json::json;

use crate::commands::{Context, everything, fetch_reported, plural, want};
use crate::failure::Failure;
use crate::report::Outcome;

/// `fl get`.
pub(crate) async fn run(
    context: &Context<'_>,
    reference: &str,
    output: Option<&Path>,
) -> Result<Outcome, Failure> {
    let reference: Reference =
        reference
            .parse()
            .map_err(|error: fl_model::reference::ReferenceError| {
                Failure::new(error.kind(), error.to_string())
            })?;
    let name = super::add::name_for(&reference)?;
    let target = match output {
        Some(dir) => context.cwd.join(dir),
        None => context.cwd.join(name.as_str()),
    };
    let target =
        std::path::absolute(&target).map_err(|error| Failure::io("resolve", &target, &error))?;
    let (root, dir) = split(&target)?;
    let listing = context.engine.resolve(&reference, &everything()?).await?;
    context.emit(Event::Resolved {
        dataset: name.clone(),
        resolved: listing.resolved.clone(),
        title: listing.meta.title.clone(),
        files: listing.entries.len() as u64,
        bytes: listing.entries.iter().filter_map(|entry| entry.size).sum(),
        license: listing.meta.license.clone(),
    });
    let wants = listing
        .entries
        .iter()
        .map(|entry| want(entry, &std::collections::HashMap::new()))
        .collect::<Result<Vec<_>, _>>()?;
    let gots = fetch_reported(context, &name, wants).await?;
    let started = Instant::now();
    let files: Vec<(DataPath, [u8; 32])> = listing
        .entries
        .iter()
        .zip(&gots)
        .map(|(entry, got)| (entry.path.clone(), got.blake3))
        .collect();
    refuse_others(&target, &files)?;
    let placed = context.engine.link(&root, &dir, &files)?;
    context.emit(Event::Linked {
        dataset: name.clone(),
        path: target.display().to_string(),
        elapsed: started.elapsed(),
        extra_bytes: placed.iter().map(|(_, linked)| linked.bytes_copied).sum(),
    });
    let shown = target.display().to_string();
    Ok(Outcome {
        status: Status::Ok,
        result: json!({ "path": shown, "files": placed.len() }),
        answer: Some(shown.clone()),
        sentence: format!(
            "Got {} into {shown}.",
            plural(placed.len() as u64, "file", "files")
        ),
    })
}

/// Refuses a target directory that already holds other bytes at any of the files' paths, so
/// `get` never overwrites what is there.
fn refuse_others(target: &Path, files: &[(DataPath, [u8; 32])]) -> Result<(), Failure> {
    for (path, blake3) in files {
        let place = target.join(path.as_str());
        if place.exists() && fingerprint::hash_file(&place)? != Digest::Blake3(*blake3) {
            return Err(Failure::new(
                ErrorKind::LocalModified,
                format!("`{}` already holds other bytes", place.display()),
            )
            .subject(place.display().to_string())
            .help("move it away or fetch into another directory with `-o DIR`"));
        }
    }
    Ok(())
}

fn split(target: &Path) -> Result<(PathBuf, DataPath), Failure> {
    let refuse = || {
        Failure::new(
            ErrorKind::Usage,
            format!("`{}` cannot hold the files", target.display()),
        )
        .help("name a directory with `-o DIR`")
    };
    let parent = target.parent().ok_or_else(refuse)?;
    let last = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(refuse)?;
    let dir = DataPath::new(last).map_err(|_| refuse())?;
    Ok((parent.to_path_buf(), dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_splits_into_its_parent_and_last_name() {
        let target = Path::new("work").join("iris");
        let (root, dir) = split(&target).unwrap();
        assert_eq!(root, Path::new("work"));
        assert_eq!(dir.as_str(), "iris");
    }

    #[test]
    fn a_root_cannot_hold_the_files() {
        assert_eq!(split(Path::new("/")).unwrap_err().kind(), ErrorKind::Usage);
    }
}
