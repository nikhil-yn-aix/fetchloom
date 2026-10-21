//! `fl add`: write a dataset into `data.toml`, lock it and sync it.

use fl_model::document::Status;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::manifest::Manifest;
use fl_model::name::Name;
use fl_model::reference::{Reference, Target};
use serde_json::json;
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::args::Add;
use crate::commands::{Context, init, relock, sync_project};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl add`.
pub(crate) async fn run(context: &Context<'_>, add: &Add) -> Result<Outcome, Failure> {
    let reference: Reference =
        add.reference
            .parse()
            .map_err(|error: fl_model::reference::ReferenceError| {
                Failure::new(error.kind(), error.to_string())
                    .help("references look like zenodo:3608990 or https://host/file.csv")
            })?;
    let name = match &add.name {
        Some(name) => Name::new(name).map_err(|error| {
            Failure::new(ErrorKind::Usage, error.to_string())
                .help("names use a-z, 0-9, `-` and `_`")
        })?,
        None => name_for(&reference)?,
    };
    let project = match Project::find(context.cwd) {
        Ok(project) => project,
        Err(failure) if failure.kind() == ErrorKind::ProjectMissing => {
            init::create(context, context.cwd, None)?;
            Project::at(
                std::path::absolute(context.cwd)
                    .map_err(|error| Failure::io("resolve", context.cwd, &error))?,
            )
        }
        Err(failure) => return Err(failure),
    };
    let _held = context.hold(&project)?;
    let text = edit(&project.manifest_text()?, &name, &reference, add)?;
    let manifest = Manifest::from_toml(&text)?;
    project.write_manifest(&text)?;
    context.emit(Event::Added {
        dataset: name.clone(),
        reference: reference.clone(),
    });
    let (_, written) = relock(context, &project, &manifest).await?;
    let mut result =
        json!({ "dataset": name.as_str(), "ref": reference.as_str(), "locked": written });
    let sentence = if add.no_sync {
        format!("Added {name} to data.toml and data.lock, not synced.")
    } else {
        let synced = sync_project(context, &project, Some(&name)).await?;
        result["synced"] = synced.outcome().result;
        format!(
            "Added {name}, its files are in {}.",
            project.data_dir(&manifest).join(name.as_str()).display()
        )
    };
    Ok(Outcome {
        status: Status::Ok,
        result,
        answer: None,
        sentence,
    })
}

/// `text` with the dataset `name` set to `reference` and the filters of `add`. An existing
/// entry with the same reference is replaced; one with another reference is an error.
fn edit(text: &str, name: &Name, reference: &Reference, add: &Add) -> Result<String, Failure> {
    let mut document: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        Failure::new(
            ErrorKind::ProjectInvalidManifest,
            format!("data.toml: {}", error.message()),
        )
        .subject("data.toml")
    })?;
    let datasets = document
        .entry("datasets")
        .or_insert_with(|| {
            let mut datasets = Table::new();
            datasets.set_implicit(true);
            Item::Table(datasets)
        })
        .as_table_mut()
        .ok_or_else(|| {
            Failure::new(
                ErrorKind::ProjectInvalidManifest,
                "data.toml: `datasets` is not a table",
            )
        })?;
    if let Some(existing) = datasets
        .get(name.as_str())
        .and_then(|item| item.get("ref"))
        .and_then(Item::as_str)
        && existing != reference.as_str()
    {
        return Err(Failure::new(
            ErrorKind::Usage,
            format!("`{name}` already names {existing} in data.toml"),
        )
        .help(format!(
            "choose another name: `fl add {reference} --as NAME`"
        )));
    }
    let mut entry = Table::new();
    entry.insert("ref", value(reference.as_str()));
    for (key, globs) in [("select", &add.select), ("exclude", &add.exclude)] {
        if !globs.is_empty() {
            entry.insert(key, value(globs.iter().collect::<Array>()));
        }
    }
    datasets.insert(name.as_str(), Item::Table(entry));
    Ok(document.to_string())
}

/// The last meaningful segment of `reference`, lowercased, with every other character as `-`.
pub(crate) fn name_for(reference: &Reference) -> Result<Name, Failure> {
    let segment = match reference.target() {
        Target::Url { path, .. }
        | Target::File { path }
        | Target::Path(path)
        | Target::Scheme { body: path, .. }
        | Target::Doi { suffix: path, .. } => last_segment(path),
        Target::Remote { name, path } => {
            if path.is_empty() {
                name.as_str()
            } else {
                last_segment(path)
            }
        }
        Target::Name(name) => name.as_str(),
    };
    let mut name = String::with_capacity(segment.len());
    for character in segment.chars().flat_map(char::to_lowercase) {
        let kept = character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_';
        let next = if kept { character } else { '-' };
        if !(next == '-' && name.ends_with('-')) {
            name.push(next);
        }
    }
    Name::new(name.trim_matches('-')).map_err(|_| {
        Failure::new(
            ErrorKind::Usage,
            format!("no name can be made from `{reference}`"),
        )
        .help(format!("name it yourself: `fl add {reference} --as NAME`"))
    })
}

fn last_segment(text: &str) -> &str {
    let text = text.split(['?', '#']).next().unwrap_or_default();
    let text = text.split('@').next().unwrap_or_default();
    text.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(text: &str) -> String {
        name_for(&text.parse().unwrap())
            .unwrap()
            .as_str()
            .to_owned()
    }

    fn adding(select: &[&str]) -> Add {
        Add {
            reference: String::new(),
            name: None,
            select: select.iter().map(|glob| (*glob).to_owned()).collect(),
            exclude: Vec::new(),
            no_sync: false,
        }
    }

    #[test]
    fn names_come_from_the_last_meaningful_segment() {
        assert_eq!(name_of("zenodo:3608990"), "3608990");
        assert_eq!(name_of("https://host/a/Iris.CSV?x=1"), "iris-csv");
        assert_eq!(name_of("https://host/a/b/"), "b");
        assert_eq!(name_of("hf:datasets/org/My.Set@v2"), "my-set");
    }

    #[test]
    fn a_reference_with_no_usable_segment_asks_for_a_name() {
        let failure = name_for(&"https://host/".parse().unwrap()).unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::Usage);
        assert!(failure.0.help.unwrap().contains("--as"));
    }

    #[test]
    fn an_edit_keeps_comments_and_adds_filters() {
        let text = "# mine\n[project]\nname = \"p\"\n";
        let edited = edit(
            text,
            &Name::new("iris").unwrap(),
            &"zenodo:3608990".parse().unwrap(),
            &adding(&["*.csv"]),
        )
        .unwrap();
        assert_eq!(
            edited,
            "# mine\n[project]\nname = \"p\"\n\n[datasets.iris]\nref = \"zenodo:3608990\"\nselect = [\"*.csv\"]\n"
        );
    }

    #[test]
    fn the_same_name_for_another_reference_is_refused() {
        let text = "[datasets.iris]\nref = \"zenodo:1\"\n";
        let failure = edit(
            text,
            &Name::new("iris").unwrap(),
            &"zenodo:2".parse().unwrap(),
            &adding(&[]),
        )
        .unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::Usage);
        assert!(failure.to_string().contains("already names zenodo:1"));
    }
}
