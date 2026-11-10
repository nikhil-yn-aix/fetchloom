//! `fetchloom remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the
//! data folder. it downloads nothing.

use super::args::Remove;
use super::project::{Outcome, Report, committed, dry_run_done, find, hold, known};
use super::sync::disk::Survey;
use super::sync::place;
use super::{Context, open_cache, peek_cache};
use crate::error::Error;
use crate::project::lock;
use crate::project::manifest::{self, Manifest};

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "remove").await?)
    };
    let (text, manifest) = project.manifest()?;
    known(&manifest, &args.name, "remove")?;
    let updated = manifest::without_dataset(&text, &args.name)?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    lock.datasets.retain(|dataset| dataset.name != args.name);
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let survey = Survey::read(&project, &new_manifest.data_dir, context.threads);
    let placing = place::Placing::new(&project, &new_manifest, &lock, &cache, false, survey)?;
    if dry_run {
        let preview = placing.preview_removal();
        context.output.step(
            "Would drop",
            &format!("{} from data.toml and data.lock", args.name),
        );
        place::tell_preview(&context.output, &new_manifest, &preview);
        dry_run_done(&context.output, None);
        let outcome = Outcome {
            lock_changed: true,
            pins: Vec::new(),
            placed: preview,
        };
        return Ok(Report::dry(project.root, vec![args.name], outcome));
    }
    // a stop asked while the data folder was read leaves the project as it was; once the files
    // change, the removal runs to its end
    context.stop.check()?;
    context.stop.finish();
    project.write(manifest::FILE, updated.as_bytes())?;
    project.write_lists(&lock)?;
    project.write(lock::FILE, lock.render().as_bytes())?;
    project.sweep_lists(&lock);
    let placement = placing.remove_only(&context.output).map_err(|error| {
        error.detail("data.toml and data.lock were updated; after fixing the error, `fetchloom sync` finishes the removal")
    })?;
    context.output.done("Removed", &args.name);
    context.output.next(&format!(
        "commit {}; after pulling, other clones remove unchanged files with `fetchloom sync`",
        committed(&project)
    ));
    Ok(Report {
        project: project.root,
        datasets: vec![args.name],
        lock_changed: true,
        placement: Some(placement),
        ..Report::default()
    })
}
