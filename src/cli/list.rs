//! `fetchloom list`: the datasets a project has, with where each comes from, how much it holds
//! and where its files go. it reads data.toml and data.lock and changes nothing.

use std::fmt::Write as _;

use serde::Serialize;

use super::Context;
use crate::error::Error;
use crate::plan;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::source::Access;
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    datasets: Vec<Listed>,
}

#[derive(Serialize)]
struct Listed {
    name: String,
    #[serde(rename = "ref")]
    reference: String,
    /// the version data.lock pins, or none before the dataset is locked.
    resolved: Option<String>,
    files: Option<u64>,
    bytes: Option<u64>,
    folder: String,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    /// who may read the data, as its source said: a hint for sharing it, `unknown` when the
    /// source said nothing.
    access: Option<Access>,
    /// each source file data.lock pins. `files` was already the count, so the list has its own
    /// name.
    locked_files: Option<Vec<ListedFile>>,
}

#[derive(Serialize)]
struct ListedFile {
    path: String,
    size: u64,
}

pub(super) fn run(context: &Context, name: Option<&str>) -> Result<Report, Error> {
    let project = super::project::find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    if let Some(name) = name {
        super::project::known(&manifest, name, "list")?;
    }
    let lock = project.lock()?.unwrap_or_default();
    let mut report = listed(&manifest, &lock);
    let output = &context.output;
    if let Some(name) = name {
        report.datasets.retain(|dataset| dataset.name == name);
        if let Some(dataset) = report.datasets.first() {
            one(output, dataset);
        }
    } else if report.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        output.hint(
            "add one with `fetchloom add <link>`, or find one with `fetchloom search <words>`",
        );
    } else {
        output.answer(&table(&report.datasets, output.answer_width()));
    }
    Ok(report)
}

/// one dataset's files on stdout, a path and a size per line. its title, license and DOI are an
/// aside for a person, since data.lock and `--json` keep them.
fn one(output: &crate::output::Output, dataset: &Listed) {
    let Some(files) = &dataset.locked_files else {
        output.done("Ok", &format!("{} is not locked yet", dataset.name));
        output.hint("run `fetchloom sync` to lock it and fetch its files");
        return;
    };
    let about: Vec<&str> = [&dataset.title, &dataset.license, &dataset.doi]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !about.is_empty() {
        output.aside(&about.join(" \u{b7} "));
    }
    if files.is_empty() {
        output.done("Ok", &format!("{} has no files", dataset.name));
    } else {
        output.answer(&file_lines(files));
    }
}

fn file_lines(files: &[ListedFile]) -> String {
    let width = files
        .iter()
        .map(|file| file.path.chars().count())
        .max()
        .unwrap_or_default();
    files
        .iter()
        .map(|file| {
            let pad = width - file.path.chars().count();
            format!("{}{:pad$}  {}", file.path, "", units::size(file.size))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn listed(manifest: &Manifest, lock: &Lock) -> Report {
    let datasets = manifest
        .datasets
        .iter()
        .map(|(name, entry)| {
            let locked = lock.get(name);
            Listed {
                name: name.clone(),
                reference: crate::net::mask_query(&entry.reference).into_owned(),
                resolved: locked.map(|locked| locked.resolved.clone()),
                files: locked.map(|locked| locked.files.len() as u64),
                bytes: locked.map(|locked| {
                    locked
                        .files
                        .iter()
                        .fold(0, |total: u64, file| total.saturating_add(file.size))
                }),
                folder: plan::folder(manifest, name),
                title: locked.and_then(|locked| locked.title.clone()),
                license: locked.and_then(|locked| locked.license.clone()),
                doi: locked.and_then(|locked| locked.doi.clone()),
                access: locked.map(|locked| locked.access),
                locked_files: locked.map(|locked| {
                    locked
                        .files
                        .iter()
                        .map(|file| ListedFile {
                            path: file.path.clone(),
                            size: file.size,
                        })
                        .collect()
                }),
            }
        })
        .collect();
    Report { datasets }
}

/// a license longer than this is cut, so one long statement does not push the table off screen.
const LICENSE: usize = 32;

/// the datasets as aligned columns, one per line, for a person or for `grep`. on a terminal
/// too narrow for every column, the source gives way, then the license, since `--json` and
/// `list <name>` keep them.
fn table(datasets: &[Listed], width: Option<usize>) -> String {
    let widest = |text: &str| {
        text.lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or_default()
    };
    let full = columns(datasets, true, true);
    let Some(width) = width.filter(|width| widest(&full) > *width) else {
        return full;
    };
    let licensed = columns(datasets, false, true);
    if widest(&licensed) <= width {
        return licensed;
    }
    columns(datasets, false, false)
}

fn columns(datasets: &[Listed], with_source: bool, with_license: bool) -> String {
    let licensed = with_license && datasets.iter().any(|dataset| dataset.license.is_some());
    let rows: Vec<Vec<String>> = datasets
        .iter()
        .map(|dataset| {
            let contents = match (dataset.files, dataset.bytes) {
                (Some(files), Some(bytes)) => format!(
                    "{} \u{b7} {}",
                    units::count(files, "file", "files"),
                    units::size(bytes)
                ),
                _ => "not locked yet".to_owned(),
            };
            let mut row = vec![dataset.name.clone()];
            if with_source {
                row.push(dataset.reference.clone());
            }
            row.extend([contents, dataset.folder.clone()]);
            if licensed {
                let license = dataset.license.as_deref().unwrap_or_default();
                row.push(units::clip(license, LICENSE, "\u{2026}"));
            }
            row
        })
        .collect();
    let last = rows.first().map_or(0, Vec::len).saturating_sub(1);
    let widths: Vec<usize> = (0..last)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or_default()
        })
        .collect();
    let mut text = String::new();
    for row in &rows {
        if !text.is_empty() {
            text.push('\n');
        }
        for (column, width) in widths.iter().enumerate() {
            let cell = &row[column];
            let pad = width - cell.chars().count();
            let _ = write!(text, "{cell}{:pad$}  ", "");
        }
        text.push_str(&row[last]);
        // a dataset with no license leaves no spaces at the end of its line
        let kept = text.trim_end_matches(' ').len();
        text.truncate(kept);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_dataset_lists_a_path_and_a_size_per_line() {
        let files = [
            ListedFile {
                path: "iris.csv".to_owned(),
                size: 4551,
            },
            ListedFile {
                path: "docs/readme.txt".to_owned(),
                size: 12,
            },
        ];
        assert_eq!(
            file_lines(&files),
            "iris.csv         4.4 KiB\ndocs/readme.txt  12 B"
        );
    }

    #[test]
    fn locked_and_unlocked_datasets_line_up() {
        let manifest = Manifest::parse(
            "[datasets.iris]\nref = \"zenodo:3608990\"\n\n[datasets.mnist-digits]\nref = \"hf:datasets/ylecun/mnist\"\n",
        )
        .unwrap();
        let report = listed(&manifest, &Lock::default());
        assert_eq!(
            table(&report.datasets, None),
            "iris          zenodo:3608990            not locked yet  data/iris\n\
             mnist-digits  hf:datasets/ylecun/mnist  not locked yet  data/mnist-digits"
        );
        assert_eq!(
            table(&report.datasets, Some(50)),
            "iris          not locked yet  data/iris\n\
             mnist-digits  not locked yet  data/mnist-digits",
            "a narrow terminal drops the source"
        );
    }

    #[test]
    fn the_license_comes_last_and_gives_way_after_the_source() {
        let listed = |name: &str, license: Option<&str>| Listed {
            name: name.to_owned(),
            reference: format!("zenodo:{}", name.len()),
            resolved: Some(format!("zenodo:{}@1", name.len())),
            files: Some(1),
            bytes: Some(4551),
            folder: format!("data/{name}"),
            title: None,
            license: license.map(str::to_owned),
            doi: None,
            access: Some(Access::Unknown),
            locked_files: Some(Vec::new()),
        };
        let datasets = [
            listed("iris", Some("cc-by-4.0")),
            listed(
                "tweets",
                Some("Data files © Original Authors, as the description says"),
            ),
            listed("wine", None),
        ];
        assert_eq!(
            table(&datasets, None),
            "iris    zenodo:4  1 file · 4.4 KiB  data/iris    cc-by-4.0\n\
             tweets  zenodo:6  1 file · 4.4 KiB  data/tweets  Data files © Original Authors…\n\
             wine    zenodo:4  1 file · 4.4 KiB  data/wine"
        );
        assert_eq!(
            table(&datasets, Some(70)),
            "iris    1 file · 4.4 KiB  data/iris    cc-by-4.0\n\
             tweets  1 file · 4.4 KiB  data/tweets  Data files © Original Authors…\n\
             wine    1 file · 4.4 KiB  data/wine",
            "the source gives way first"
        );
        assert_eq!(
            table(&datasets, Some(40)),
            "iris    1 file · 4.4 KiB  data/iris\n\
             tweets  1 file · 4.4 KiB  data/tweets\n\
             wine    1 file · 4.4 KiB  data/wine",
            "then the license"
        );
    }
}
