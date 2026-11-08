//! `fetchloom list`: the datasets a project has, with where each comes from, how much it holds
//! and where its files go. it reads data.toml and data.lock and changes nothing.

use std::fmt::Write as _;

use serde::Serialize;

use super::Context;
use crate::error::Error;
use crate::plan;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
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

/// the datasets as aligned columns, one per line, for a person or for `grep`. on a terminal
/// too narrow for every column, the source gives way, since `--json` and data.toml keep it.
fn table(datasets: &[Listed], width: Option<usize>) -> String {
    let full = columns(datasets, true);
    let widest = full
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or_default();
    match width {
        Some(width) if widest > width => columns(datasets, false),
        _ => full,
    }
}

fn columns(datasets: &[Listed], with_source: bool) -> String {
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
}
