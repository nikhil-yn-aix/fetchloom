//! `fl remove`: datasets dropped from `data.toml`, the lock and the data directory. Their
//! downloads stay in the store, so adding one again costs nothing.

use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::manifest::{Manifest, Run};
use fl_model::name::Name;
use serde_json::json;
use toml_edit::DocumentMut;

use crate::commands::lock::{Relock, relock};
use crate::commands::sync::drop_removed;
use crate::commands::{Context, dataset_dir, datasets_named};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl remove`.
pub(crate) async fn run(context: &Context<'_>, names: &[String]) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let _held = context.hold(&project)?;
    let manifest = project.manifest()?;
    let names = datasets_named(&manifest, names)?;
    for name in &names {
        if let Some(step) = step_using(&manifest, name) {
            return Err(Failure::new(
                ErrorKind::Usage,
                format!("the step `{step}` takes `{name}` as an input"),
            )
            .subject(name.as_str())
            .help(format!("remove the step first: `fl remove {step}`")));
        }
    }
    let text = drop_entries(&project.manifest_text()?, &names)?;
    let kept = Manifest::from_toml(&text)?;
    let how = Relock {
        typed: None,
        upgrade: None,
    };
    relock(context, &project, &kept, &how).await?;
    project.write_manifest(&text)?;
    let root = project.data_dir(&kept)?;
    let dirs: Vec<String> = kept
        .datasets
        .keys()
        .map(|name| dataset_dir(&kept, name).map(|dir| format!("{dir}/")))
        .collect::<Result<_, _>>()?;
    let mut state = project.state()?;
    let removed = drop_removed(
        context,
        &root,
        kept.project.data_dir.as_str(),
        &dirs,
        &mut state,
    )?;
    project.write_state(&state, false)?;
    for name in &names {
        context.emit(Event::Note {
            label: "Removed".to_owned(),
            text: format!("{name} from data.toml and data.lock"),
        });
    }
    let listed: Vec<&str> = names.iter().map(Name::as_str).collect();
    Ok(Outcome::ok(
        json!({ "removed": listed, "files_removed": removed }),
        format!(
            "Removed {}; the downloaded files stay cached, so adding {} again is instant.",
            listed.join(", "),
            if names.len() == 1 { "it" } else { "them" }
        ),
    ))
}

/// The step that takes `name` as an input, if one does.
fn step_using(manifest: &Manifest, name: &Name) -> Option<Name> {
    manifest.steps.iter().find_map(|(step, spec)| {
        let named = spec.inputs.iter().any(|input| input == name.as_str())
            || spec.foreach.as_deref().is_some_and(|foreach| {
                foreach
                    .split_once(':')
                    .is_some_and(|(of, _)| of == name.as_str())
            })
            || matches!(&spec.run, Run::Shell(line) if line.contains(&format!("{{in.{name}}}")));
        named.then(|| step.clone())
    })
}

/// `text` without the `[datasets.NAME]` entries of `names`, everything else kept as written.
fn drop_entries(text: &str, names: &[Name]) -> Result<String, Failure> {
    let mut document: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        Failure::new(
            ErrorKind::ProjectInvalidManifest,
            format!("data.toml: {}", error.message()),
        )
        .subject("data.toml")
    })?;
    if let Some(datasets) = document
        .get_mut("datasets")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        for name in names {
            datasets.remove(name.as_str());
        }
    }
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_an_entry_keeps_the_rest_as_written() {
        let text = "# mine\n[project]\nname = \"p\"\n\n[datasets.a]\nref = \"zenodo:1\"\n\n[datasets.b]\nref = \"zenodo:2\" # keep\n";
        let dropped = drop_entries(text, &[Name::new("a").unwrap()]).unwrap();
        assert_eq!(
            dropped,
            "# mine\n[project]\nname = \"p\"\n\n[datasets.b]\nref = \"zenodo:2\" # keep\n"
        );
    }

    #[test]
    fn a_step_that_reads_a_dataset_keeps_it() {
        let manifest = Manifest::from_toml(
            "[datasets.eeg]\nref = \"zenodo:1\"\n[steps.filter]\nforeach = \"eeg:sub-*/\"\nrun = [\"x\"]\n[steps.other]\nrun = \"y {in.eeg}\"\n",
        )
        .unwrap();
        let eeg = Name::new("eeg").unwrap();
        assert_eq!(step_using(&manifest, &eeg).unwrap().as_str(), "filter");
        let none = Manifest::from_toml("[datasets.eeg]\nref = \"zenodo:1\"\n").unwrap();
        assert!(step_using(&none, &eeg).is_none());
    }
}
