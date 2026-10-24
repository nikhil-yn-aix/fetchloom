//! `fl status`: how the data directory and the lock compare, saying nothing about what is clean.

use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::lock::Lock;
use serde_json::json;

use crate::commands::lock::{is_current, stale};
use crate::commands::{Context, Found, dataset_dir, datasets_named, plural, survey};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl status`.
pub(crate) fn run(
    context: &Context<'_>,
    names: &[String],
    check: bool,
) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let manifest = project.manifest()?;
    let only = datasets_named(&manifest, names)?;
    let lock = project.lock()?;
    let current = lock
        .as_ref()
        .is_some_and(|lock| is_current(&manifest, lock));
    if !current {
        context.emit(Event::Warning(
            if lock.is_some() {
                "data.lock is out of date with data.toml, `fl lock` brings it in line"
            } else {
                "there is no data.lock yet, `fl sync` writes it and downloads the datasets"
            }
            .to_owned(),
        ));
    }
    let root = project.data_dir(&manifest)?;
    let shown_root = manifest.project.data_dir.to_string();
    let state = project.state()?;
    let mut missing = Vec::new();
    let mut modified = Vec::new();
    let mut extra = Vec::new();
    for locked in lock.iter().flat_map(Lock::datasets) {
        if !only.is_empty() && !only.contains(&locked.name) {
            continue;
        }
        let dir = dataset_dir(&manifest, &locked.name)?;
        let found = survey(context, &root, &shown_root, &dir, locked, &state)?;
        for slot in found.slots {
            match slot.found {
                Found::Clean => {}
                Found::Missing => {
                    context.emit(Event::Missing {
                        path: slot.shown.clone(),
                    });
                    missing.push(slot.shown);
                }
                Found::Modified => {
                    context.emit(Event::Modified {
                        path: slot.shown.clone(),
                    });
                    modified.push(slot.shown);
                }
            }
        }
        for key in found.stale {
            context.emit(Event::Note {
                label: "Stale".to_owned(),
                text: format!(
                    "{shown_root}/{key}, which data.lock no longer lists; `fl sync` removes it"
                ),
            });
        }
        for key in found.extras {
            let shown = format!("{shown_root}/{key}");
            context.emit(Event::Note {
                label: "Extra".to_owned(),
                text: shown.clone(),
            });
            extra.push(shown);
        }
    }
    let differ = (missing.len() + modified.len()) as u64;
    let sentence = sentence(differ, current, extra.len() as u64, modified.is_empty());
    if check && !current {
        return Err(stale("data.lock is out of date with data.toml"));
    }
    if check && differ > 0 {
        return Err(Failure::new(
            ErrorKind::StatusDirty,
            format!(
                "{} under {shown_root} {} from data.lock",
                plural(differ, "file", "files"),
                if differ == 1 { "differs" } else { "differ" }
            ),
        )
        .help("`fl sync` downloads what is missing, `fl restore` puts changed files back"));
    }
    Ok(Outcome::ok(
        json!({
            "lock_current": current,
            "missing": missing,
            "modified": modified,
            "extra": extra,
        }),
        sentence,
    ))
}

fn sentence(differ: u64, current: bool, extra: u64, none_modified: bool) -> String {
    match (differ, current) {
        (0, true) if extra == 0 => "Everything matches data.lock.".to_owned(),
        (0, true) => format!(
            "The downloaded files match data.lock; {} you added {} kept as {}.",
            plural(extra, "file", "files"),
            if extra == 1 { "is" } else { "are" },
            if extra == 1 { "it is" } else { "they are" }
        ),
        (0, false) => "The files match data.lock, but data.lock is out of date.".to_owned(),
        _ if none_modified => format!(
            "{} missing, `fl sync` downloads {}.",
            plural(differ, "file is", "files are"),
            if differ == 1 { "it" } else { "them" }
        ),
        _ => format!(
            "{} {} from data.lock; `fl restore` puts the downloaded {} back.",
            plural(differ, "file", "files"),
            if differ == 1 { "differs" } else { "differ" },
            if differ == 1 { "one" } else { "ones" }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_project_says_everything_matches() {
        assert_eq!(sentence(0, true, 0, true), "Everything matches data.lock.");
    }

    #[test]
    fn files_the_user_added_are_said_to_be_kept() {
        assert_eq!(
            sentence(0, true, 1, true),
            "The downloaded files match data.lock; 1 file you added is kept as it is."
        );
        assert_eq!(
            sentence(0, true, 2, true),
            "The downloaded files match data.lock; 2 files you added are kept as they are."
        );
    }

    #[test]
    fn matching_files_under_a_stale_lock_say_so() {
        assert_eq!(
            sentence(0, false, 0, true),
            "The files match data.lock, but data.lock is out of date."
        );
        assert_eq!(
            sentence(0, false, 3, true),
            "The files match data.lock, but data.lock is out of date."
        );
    }

    #[test]
    fn missing_files_point_at_sync_and_changed_ones_at_restore() {
        assert_eq!(
            sentence(1, true, 0, true),
            "1 file is missing, `fl sync` downloads it."
        );
        assert_eq!(
            sentence(2, false, 0, true),
            "2 files are missing, `fl sync` downloads them."
        );
        assert_eq!(
            sentence(1, true, 0, false),
            "1 file differs from data.lock; `fl restore` puts the downloaded one back."
        );
        assert_eq!(
            sentence(2, true, 0, false),
            "2 files differ from data.lock; `fl restore` puts the downloaded ones back."
        );
    }
}
