//! `fl path`: the absolute path of a dataset or of something inside it, synced first.

use fl_model::error::ErrorKind;
use fl_model::path::DataPath;
use serde_json::json;

use crate::commands::sync::{Options, sync_project};
use crate::commands::{Context, dataset_dir, dataset_named};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl path`.
pub(crate) async fn run(
    context: &Context<'_>,
    wanted: &str,
    no_sync: bool,
) -> Result<Outcome, Failure> {
    let (name, inside) = wanted.split_once('/').unwrap_or((wanted, ""));
    let inside = if inside.is_empty() {
        None
    } else {
        Some(DataPath::new(inside).map_err(|error| {
            Failure::new(
                ErrorKind::Usage,
                format!("`{inside}` is not a path inside a dataset: {error}"),
            )
            .help("write it with `/` and without `..`, like NAME/sub/file.csv")
        })?)
    };
    let project = Project::find(context.cwd)?;
    let held = if no_sync {
        None
    } else {
        Some(context.hold(&project)?)
    };
    let manifest = project.manifest()?;
    let name = dataset_named(&manifest, name)?;
    let mut path = project
        .data_dir(&manifest)?
        .join(dataset_dir(&manifest, &name)?.as_str());
    if let Some(inside) = &inside {
        path = path.join(inside.as_str());
    }
    if held.is_some() {
        let only = [name.clone()];
        let options = Options {
            only: &only,
            ..Options::default()
        };
        sync_project(context, &project, &options)
            .await?
            .finished(context)?;
    }
    drop(held);
    if !path.exists() {
        let failure = if no_sync {
            Failure::new(
                ErrorKind::OfflineMissing,
                format!("`{}` is not there and --no-sync was given", path.display()),
            )
            .help(format!("run `fl sync` or `fl path {wanted}`"))
        } else {
            Failure::new(
                ErrorKind::Usage,
                format!("dataset `{name}` holds nothing at `{wanted}`"),
            )
            .help("run `fl status` to see the dataset's files")
        };
        return Err(failure.subject(name.as_str()));
    }
    let shown = path.display().to_string();
    Ok(Outcome {
        answer: Some(shown.clone()),
        ..Outcome::ok(
            json!({ "dataset": name.as_str(), "path": shown }),
            String::new(),
        )
    })
}
