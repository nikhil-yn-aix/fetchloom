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
}

pub(super) fn run(context: &Context) -> Result<Report, Error> {
    let project = super::project::find()?;
    let (_, manifest) = project.manifest()?;
    let lock = project.lock()?.unwrap_or_default();
    let report = listed(&manifest, &lock);
    let output = &context.output;
    if report.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        output.hint(
            "add one with `fetchloom add <link>`, or find one with `fetchloom search <words>`",
        );
    } else {
        output.answer(&table(&report.datasets, output.answer_width()));
    }
    Ok(report)
}

fn listed(manifest: &Manifest, lock: &Lock) -> Report {
    let datasets = manifest
        .datasets
        .iter()
        .map(|(name, entry)| {
            let locked = lock.get(name);
            Listed {
                name: name.clone(),
                reference: entry.reference.clone(),
                resolved: locked.map(|locked| locked.resolved.clone()),
                files: locked.map(|locked| locked.files.len() as u64),
                bytes: locked.map(|locked| locked.files.iter().map(|file| file.size).sum()),
                folder: plan::folder(manifest, name),
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
