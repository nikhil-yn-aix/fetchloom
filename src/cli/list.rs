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
        output.answer(&table(&report.datasets));
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

/// the datasets as aligned columns, one per line, for a person or for `grep`.
fn table(datasets: &[Listed]) -> String {
    let rows: Vec<[String; 4]> = datasets
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
            [
                dataset.name.clone(),
                dataset.reference.clone(),
                contents,
                dataset.folder.clone(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..3)
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
        text.push_str(&row[3]);
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
            table(&report.datasets),
            "iris          zenodo:3608990            not locked yet  data/iris\n\
             mnist-digits  hf:datasets/ylecun/mnist  not locked yet  data/mnist-digits"
        );
    }
}
