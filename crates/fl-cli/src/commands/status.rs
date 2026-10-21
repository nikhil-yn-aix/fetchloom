//! `fl status`: how the data directory and the lock compare, saying nothing about what is clean.

use fl_model::document::Status;
use fl_model::event::Event;
use fl_model::lock::Lock;
use serde_json::json;

use crate::commands::{Context, Found, dataset_dir, is_current, plural, survey};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl status`.
pub(crate) fn run(context: &Context<'_>) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let manifest = project.manifest()?;
    let lock = project.lock()?;
    let current = lock
        .as_ref()
        .is_some_and(|lock| is_current(&manifest, lock));
    if !current {
        context.emit(Event::Warning(
            "data.lock is out of date with data.toml, `fl lock` brings it in line".to_owned(),
        ));
    }
    let root = project.data_dir(&manifest);
    let state = project.state()?;
    let mut missing = Vec::new();
    let mut modified = Vec::new();
    for locked in lock.iter().flat_map(Lock::datasets) {
        let Ok(dir) = dataset_dir(&manifest, &locked.name) else {
            continue;
        };
        for slot in survey(
            context,
            &root,
            &manifest.project.data_dir,
            &dir,
            locked,
            &state,
        )? {
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
    }
    let differ = (missing.len() + modified.len()) as u64;
    let sentence = match (differ, current) {
        (0, true) => "Everything matches data.lock.".to_owned(),
        (0, false) => "The files match data.lock, but data.lock is out of date.".to_owned(),
        _ => format!(
            "{} {} from data.lock, `fl sync` places what is missing.",
            plural(differ, "file", "files"),
            if differ == 1 { "differs" } else { "differ" }
        ),
    };
    Ok(Outcome {
        status: Status::Ok,
        result: json!({ "lock_current": current, "missing": missing, "modified": modified }),
        answer: None,
        sentence,
    })
}
