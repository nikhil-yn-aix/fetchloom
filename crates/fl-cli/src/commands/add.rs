//! `fl add`: write a dataset into `data.toml`, lock it and sync it.

use std::path::{Component, Path, PathBuf};

use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::filter::{Filters, Pattern, Sample};
use fl_model::manifest::Manifest;
use fl_model::name::Name;
use fl_model::reference::{Reference, Target};
use fl_model::units::{Size, size};
use serde_json::json;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

use crate::args::{Add, FilterArgs, LinkMode};
use crate::commands::lock::{Relock, Totals, relock};
use crate::commands::sync::{Options, sync_project};
use crate::commands::{Context, init, plural};
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl add`.
pub(crate) async fn run(context: &Context<'_>, add: &Add) -> Result<Outcome, Failure> {
    let typed = match &add.reference {
        Some(reference) => reference.clone(),
        None => ask_for_reference(context)?,
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
    let reference = parse(&typed)?;
    let reference = project_relative(&reference, context.cwd, project.root())?;
    let name = match &add.name {
        Some(name) => Name::new(name).map_err(|error| {
            Failure::new(ErrorKind::Usage, error.to_string())
                .help("names use a-z, 0-9, `-` and `_`")
        })?,
        None => name_for(&reference)?,
    };
    let _held = context.hold(&project)?;
    let before = project.manifest_text()?;
    let existed = Manifest::from_toml(&before)?.datasets.contains_key(&name);
    let text = edit(&before, &name, &reference, add)?;
    let manifest = Manifest::from_toml(&text)?;
    let data_dir = project.data_dir(&manifest)?;
    let how = Relock {
        typed: Some(&reference),
        upgrade: None,
    };
    let relocked = relock(context, &project, &manifest, &how).await?;
    project.write_manifest(&text)?;
    let change = relocked.changes.iter().find(|change| change.name == name);
    match change.and_then(|change| change.before.map(|was| (was, change.after))) {
        Some((was, now)) => context.emit(Event::Note {
            label: "Updated".to_owned(),
            text: format!("{name}: {} before, {} now", totals(was), totals(now)),
        }),
        None if !existed => context.emit(Event::Added {
            dataset: name.clone(),
            reference: reference.clone(),
        }),
        None => {}
    }
    let unchanged = text == before;
    let mut result = json!({
        "dataset": name.as_str(),
        "ref": reference.as_str(),
        "locked": relocked.written,
        "changed": !unchanged,
    });
    let dir = data_dir.join(name.as_str());
    let sentence = if add.no_sync {
        format!("Added {name} to data.toml and data.lock, not downloaded.")
    } else {
        let only = [name.clone()];
        let options = Options {
            only: &only,
            ..Options::default()
        };
        let synced = sync_project(context, &project, &options)
            .await?
            .finished(context)?;
        let mut synced_result = synced.result();
        synced_result["bytes_fetched"] = json!(relocked.fetched + synced.fetched());
        result["synced"] = synced_result;
        if unchanged {
            format!(
                "{name} is already in data.toml with these settings, nothing to change; its files are in {}.",
                dir.display()
            )
        } else {
            format!("Added {name}, its files are in {}.", dir.display())
        }
    };
    if !existed && !add.no_sync {
        context.emit(Event::Hint(format!(
            "scripts find it with `fl path {name}`"
        )));
    }
    Ok(Outcome::ok(result, sentence))
}

fn totals(totals: Totals) -> String {
    format!(
        "{}, {}",
        plural(totals.files, "file", "files"),
        size(totals.bytes)
    )
}

fn parse(text: &str) -> Result<Reference, Failure> {
    text.parse()
        .map_err(|error: fl_model::reference::ReferenceError| {
            Failure::new(error.kind(), error.to_string())
                .help("references look like zenodo:3608990, 10.5281/zenodo.3608990 or https://host/file.csv")
        })
}

/// Asks for a reference when `fl add` was given none and a terminal can answer.
fn ask_for_reference(context: &Context<'_>) -> Result<String, Failure> {
    let refusal = Failure::new(ErrorKind::Usage, "fl add needs something to add")
        .help("`fl add zenodo:3608990`, `fl add https://host/file.csv`, or `fl add search words`");
    let answer = context
        .ask
        .text("Paste a DOI, a link, or search words", refusal)?;
    if answer.is_empty() {
        return Err(Failure::new(ErrorKind::Usage, "nothing was given to add")
            .help("run `fl add` again and paste a DOI, a link, or search words"));
    }
    Ok(answer)
}

/// `reference` with a relative local path made relative to the project root at `root`
/// instead of `cwd`, so data.toml means the same from every directory.
fn project_relative(reference: &Reference, cwd: &Path, root: &Path) -> Result<Reference, Failure> {
    let (Target::Path(path) | Target::File { path }) = reference.target() else {
        return Ok(reference.clone());
    };
    let written = Path::new(path);
    if written.is_absolute() {
        return Ok(reference.clone());
    }
    let cwd =
        normal(&std::path::absolute(cwd).map_err(|error| Failure::io("resolve", cwd, &error))?);
    if cwd == normal(root) {
        return Ok(reference.clone());
    }
    let full = normal(&cwd.join(written));
    let Some(relative) = relative_to(&full, root) else {
        return parse(&full.display().to_string());
    };
    let text = format!("./{}", relative.display().to_string().replace('\\', "/"));
    parse(&text)
}

fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let root = normal(root);
    let mut shared = 0;
    let parts: Vec<Component<'_>> = path.components().collect();
    let base: Vec<Component<'_>> = root.components().collect();
    while shared < parts.len() && shared < base.len() && parts[shared] == base[shared] {
        shared += 1;
    }
    if shared == 0 {
        return None;
    }
    let mut relative = PathBuf::new();
    for _ in shared..base.len() {
        relative.push("..");
    }
    for part in &parts[shared..] {
        relative.push(part);
    }
    Some(relative)
}

/// `text` with the dataset `name` set to `reference` and the settings of `add`. An existing
/// entry with the same reference is updated; one with another reference is an error.
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
    let entry = datasets
        .entry(name.as_str())
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            Failure::new(
                ErrorKind::ProjectInvalidManifest,
                format!("data.toml: `datasets.{name}` is not a table"),
            )
        })?;
    entry.insert("ref", value(reference.as_str()));
    Chosen::new(&add.filters)?.write(entry)?;
    match add.link {
        Some(LinkMode::Copy) => {
            entry.insert("link", value("copy"));
        }
        Some(LinkMode::Auto) => {
            entry.remove("link");
        }
        None => {}
    }
    Ok(document.to_string())
}

/// The filters of `fl add` and `fl get`, checked.
pub(crate) struct Chosen {
    select: Vec<Pattern>,
    exclude: Vec<Pattern>,
    max_file_size: Option<(String, Size)>,
    sample: Option<Sample>,
}

impl Chosen {
    /// Checks `args`.
    pub(crate) fn new(args: &FilterArgs) -> Result<Self, Failure> {
        let patterns = |globs: &[String]| {
            globs
                .iter()
                .map(|glob| {
                    glob.parse::<Pattern>().map_err(|error| {
                        Failure::new(ErrorKind::Usage, format!("`{glob}`: {error}"))
                            .help("patterns match the whole path inside the dataset, like **/*.csv")
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let max_file_size = args
            .max_file_size
            .as_ref()
            .map(|limit| {
                limit
                    .parse::<Size>()
                    .map(|size| (limit.clone(), size))
                    .map_err(|error| {
                        Failure::new(
                            ErrorKind::Usage,
                            format!("--max-file-size {limit}: {error}"),
                        )
                        .help("write a size like 500MiB or 2GiB")
                    })
            })
            .transpose()?;
        let seed = args.seed.unwrap_or(0);
        let sample = args
            .sample
            .as_deref()
            .map(|sample| match sample.strip_suffix('%') {
                Some(percent) => percent
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|percent| *percent > 0.0 && *percent <= 100.0)
                    .map(|percent| Sample::Fraction {
                        fraction: percent / 100.0,
                        seed,
                    })
                    .ok_or_else(|| bad_sample(sample)),
                None => sample
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .map(|n| Sample::Count { n, seed })
                    .ok_or_else(|| bad_sample(sample)),
            })
            .transpose()?;
        Ok(Self {
            select: patterns(&args.select)?,
            exclude: patterns(&args.exclude)?,
            max_file_size,
            sample,
        })
    }

    /// The filters themselves.
    pub(crate) fn filters(&self) -> Result<Filters, Failure> {
        Filters::new(
            self.select.clone(),
            self.exclude.clone(),
            self.max_file_size.as_ref().map(|(_, size)| *size),
            self.sample,
        )
        .map_err(|error| Failure::new(ErrorKind::Usage, error.to_string()))
    }

    /// Sets the filter keys of `entry` to these, removing the ones not given.
    fn write(&self, entry: &mut Table) -> Result<(), Failure> {
        for (key, patterns) in [("select", &self.select), ("exclude", &self.exclude)] {
            if patterns.is_empty() {
                entry.remove(key);
            } else {
                entry.insert(
                    key,
                    value(patterns.iter().map(Pattern::as_str).collect::<Array>()),
                );
            }
        }
        match &self.max_file_size {
            Some((written, _)) => {
                entry.insert("max_file_size", value(written.as_str()));
            }
            None => {
                entry.remove("max_file_size");
            }
        }
        match self.sample {
            Some(sample) => {
                let mut table = InlineTable::new();
                let seed = match sample {
                    Sample::Count { n, seed } => {
                        table.insert(
                            "n",
                            i64::try_from(n).map_err(|_| too_large("--sample"))?.into(),
                        );
                        seed
                    }
                    Sample::Fraction { fraction, seed } => {
                        table.insert("fraction", fraction.into());
                        seed
                    }
                };
                if seed != 0 {
                    table.insert(
                        "seed",
                        i64::try_from(seed).map_err(|_| too_large("--seed"))?.into(),
                    );
                }
                entry.insert("sample", value(table));
            }
            None => {
                entry.remove("sample");
            }
        }
        Ok(())
    }
}

fn too_large(flag: &str) -> Failure {
    Failure::new(ErrorKind::Usage, format!("{flag} is too large"))
        .help("choose a number below 2^63")
}

fn bad_sample(sample: &str) -> Failure {
    Failure::new(
        ErrorKind::Usage,
        format!("--sample {sample} is not a count or a percentage"),
    )
    .help("write a number of files like 20, or a percentage like 10%")
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
            filters: FilterArgs {
                select: select.iter().map(|glob| (*glob).to_owned()).collect(),
                ..FilterArgs::default()
            },
            ..Add::default()
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
    fn adding_again_keeps_the_entry_s_other_keys_and_replaces_its_filters() {
        let text =
            "[datasets.iris]\nref = \"zenodo:1\"\npath = \"flowers\"\nselect = [\"*.csv\"]\n";
        let edited = edit(
            text,
            &Name::new("iris").unwrap(),
            &"zenodo:1".parse().unwrap(),
            &adding(&[]),
        )
        .unwrap();
        assert_eq!(
            edited,
            "[datasets.iris]\nref = \"zenodo:1\"\npath = \"flowers\"\n"
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

    #[test]
    fn every_filter_flag_lands_in_the_entry_and_bad_ones_are_refused() {
        let add = Add {
            filters: FilterArgs {
                exclude: vec!["*.tmp".to_owned()],
                max_file_size: Some("2GiB".to_owned()),
                sample: Some("10%".to_owned()),
                seed: Some(7),
                ..FilterArgs::default()
            },
            link: Some(LinkMode::Copy),
            ..Add::default()
        };
        let edited = edit(
            "",
            &Name::new("a").unwrap(),
            &"zenodo:1".parse().unwrap(),
            &add,
        )
        .unwrap();
        let manifest = Manifest::from_toml(&edited).unwrap();
        let spec = &manifest.datasets[&Name::new("a").unwrap()];
        assert_eq!(spec.link, fl_model::manifest::Link::Copy);
        assert!(
            edited.contains("sample = { fraction = 0.1, seed = 7 }"),
            "{edited}"
        );
        assert!(edited.contains("max_file_size = \"2GiB\""), "{edited}");
        for sample in ["0", "x", "0%", "101%"] {
            let add = Add {
                filters: FilterArgs {
                    sample: Some(sample.to_owned()),
                    ..FilterArgs::default()
                },
                ..Add::default()
            };
            let failure = edit(
                "",
                &Name::new("a").unwrap(),
                &"zenodo:1".parse().unwrap(),
                &add,
            )
            .unwrap_err();
            assert_eq!(failure.kind(), ErrorKind::Usage, "{sample}");
        }
        let add = Add {
            filters: FilterArgs {
                max_file_size: Some("big".to_owned()),
                ..FilterArgs::default()
            },
            ..Add::default()
        };
        assert!(
            edit(
                "",
                &Name::new("a").unwrap(),
                &"zenodo:1".parse().unwrap(),
                &add
            )
            .is_err()
        );
    }

    #[test]
    fn a_relative_path_is_made_relative_to_the_project_root() {
        let root = std::path::absolute("/p").unwrap();
        let below = root.join("sub");
        let moved = project_relative(&"./raw".parse().unwrap(), &below, &root).unwrap();
        assert_eq!(moved.as_str(), "./sub/raw");
        let up = project_relative(&"../raw".parse().unwrap(), &below, &root).unwrap();
        assert_eq!(up.as_str(), "./raw");
        let same = project_relative(&"./raw".parse().unwrap(), &root, &root).unwrap();
        assert_eq!(same.as_str(), "./raw");
        let url = project_relative(&"zenodo:1".parse().unwrap(), &below, &root).unwrap();
        assert_eq!(url.as_str(), "zenodo:1");
    }
}
