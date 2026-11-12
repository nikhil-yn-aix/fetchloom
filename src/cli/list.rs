//! `fetchloom list`: the datasets a project has, with where each comes from, how much it holds
//! and where its files go. it reads data.toml and data.lock and changes nothing.

use std::fmt::Write as _;

use serde::Serialize;

use super::Context;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::Error;
use crate::plan;
use crate::project::lock::archives::{self, Archive, Pinned};
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::{Entry, Manifest};
use crate::source::Access;
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    datasets: Vec<Listed>,
}

#[derive(Serialize)]
struct Listed {
    name: String,
    /// where the dataset is fetched from, or none when a recipe makes it.
    #[serde(rename = "ref")]
    reference: Option<String>,
    /// the datasets a recipe makes it from, or none when it is fetched.
    from: Option<Vec<String>>,
    /// the version data.lock pins, or none before the dataset is locked.
    resolved: Option<String>,
    files: Option<u64>,
    bytes: Option<u64>,
    folder: String,
    /// whether its files are placed in `folder`: false for a dataset with `place = false`, which
    /// is pinned and cached for the recipes that read it.
    placed: bool,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    /// who may read the data, as its source said: a hint for sharing it, `unknown` when the
    /// source said nothing.
    access: Option<Access>,
    /// each source file data.lock pins. `files` was already the count, so the list has its own
    /// name.
    locked_files: Option<Vec<ListedFile>>,
    /// the files `files` and `bytes` count, by their paths in the dataset's folder, with their
    /// sizes.
    #[serde(skip)]
    held: Option<Vec<(String, u64)>>,
}

#[derive(Serialize)]
struct ListedFile {
    path: String,
    size: u64,
    sha256: Sha256,
    /// the link the file downloads from, or none when it holds a login, a secret or a character
    /// no line may hold, as `export` leaves it out.
    url: Option<String>,
}

pub(super) fn run(context: &Context, name: Option<&str>) -> Result<Report, Error> {
    let project = super::project::find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    if let Some(name) = name {
        super::project::known(&manifest, name, "list")?;
    }
    let lock = project.lock()?.unwrap_or_default();
    let mut report = listed(&manifest, &lock, super::peek_cache().ok().as_ref());
    let output = &context.output;
    if let Some(name) = name {
        report.datasets.retain(|dataset| dataset.name == name);
        if let Some(dataset) = report.datasets.first() {
            one(output, dataset);
        }
    } else if report.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        output.hint(&format!(
            "add one with `{}`, or find one with `fetchloom search <words>`",
            super::project::command("add <link>")
        ));
    } else {
        output.answer(&table(&report.datasets, output.answer_width()));
    }
    Ok(report)
}

/// one dataset's files on stdout, a path and a size per line. its title, license and DOI are an
/// aside for a person, since data.lock and `--json` keep them.
fn one(output: &crate::output::Output, dataset: &Listed) {
    let Some(files) = &dataset.held else {
        output.done("Ok", &format!("{} is not locked yet", dataset.name));
        output.hint(&format!(
            "run `{}` to lock it and fetch its files",
            super::project::command("sync")
        ));
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

fn file_lines(files: &[(String, u64)]) -> String {
    let width = files
        .iter()
        .map(|(path, _)| path.chars().count())
        .max()
        .unwrap_or_default();
    files
        .iter()
        .map(|(path, size)| {
            let pad = width - path.chars().count();
            format!("{path}{:pad$}  {}", "", units::size(*size))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// the files the fetched dataset `name`, pinned as `locked`, puts in its folder, by their paths
/// there: each archive stands for the files `cache` recorded it unpacked to that its members
/// table keeps, or for itself, all its parts, while the cache has no record of it. a members
/// table that does not fit the pin leaves every pinned file as it is.
fn held(
    manifest: &Manifest,
    name: &str,
    locked: &Locked,
    cache: Option<&Cache>,
) -> Vec<(String, u64)> {
    let as_pinned = |files: &[&LockedFile]| -> Vec<(String, u64)> {
        files
            .iter()
            .map(|file| (file.path.clone(), file.size))
            .collect()
    };
    let Some(source) = manifest.datasets.get(name).and_then(Entry::source) else {
        return as_pinned(&locked.files.iter().collect::<Vec<_>>());
    };
    let folder = plan::folder(manifest, name);
    let members = |archive: &Archive<'_>| cache.and_then(|cache| archive.recorded(cache));
    let mut planned = plan::Plan::default();
    let expanded = plan::expand(
        &mut planned,
        name,
        &folder,
        false,
        &locked.files,
        &source.members,
        &members,
    );
    if expanded.is_err() {
        return as_pinned(&locked.files.iter().collect::<Vec<_>>());
    }
    let inside = |path: &str| {
        path.strip_prefix(folder.as_str())
            .and_then(|path| path.strip_prefix('/'))
            .unwrap_or(path)
            .to_owned()
    };
    let mut held: Vec<(String, u64)> = planned
        .targets
        .iter()
        .map(|target| (inside(&target.path), target.size))
        .collect();
    for pinned in archives::pinned(&locked.files) {
        if let Pinned::Archive(archive) = pinned
            && planned
                .unknown
                .iter()
                .any(|(_, file)| file.path == archive.file.path)
        {
            held.extend(as_pinned(&archive.files()));
        }
    }
    held.sort();
    held
}

fn listed(manifest: &Manifest, lock: &Lock, cache: Option<&Cache>) -> Report {
    let datasets = manifest
        .datasets
        .iter()
        .map(|(name, entry)| {
            let locked = lock.get(name).filter(|_| entry.source().is_some());
            let result = lock.result(name).filter(|_| entry.recipe().is_some());
            let held: Option<Vec<(String, u64)>> = match (locked, result) {
                (Some(locked), _) => Some(held(manifest, name, locked, cache)),
                (None, Some(result)) => Some(
                    result
                        .files
                        .iter()
                        .map(|file| (file.path.clone(), file.size))
                        .collect(),
                ),
                (None, None) => None,
            };
            let sizes: Option<Vec<u64>> = held
                .as_ref()
                .map(|held| held.iter().map(|(_, size)| *size).collect());
            Listed {
                name: name.clone(),
                reference: entry
                    .source()
                    .map(|source| crate::net::mask_query(&source.reference).into_owned()),
                from: entry.recipe().map(|recipe| recipe.from.clone()),
                resolved: locked.map(|locked| locked.resolved.clone()),
                files: sizes.as_ref().map(|sizes| sizes.len() as u64),
                bytes: sizes.map(|sizes| {
                    sizes
                        .into_iter()
                        .fold(0, |total: u64, size| total.saturating_add(size))
                }),
                folder: plan::folder(manifest, name),
                placed: entry.place,
                title: locked.and_then(|locked| locked.title.clone()),
                license: locked.and_then(|locked| locked.license.clone()),
                doi: locked.and_then(|locked| locked.doi.clone()),
                access: locked.map(|locked| locked.access),
                locked_files: match (locked, result) {
                    (Some(locked), _) => Some(
                        locked
                            .files
                            .iter()
                            .map(|file| ListedFile {
                                path: file.path.clone(),
                                size: file.size,
                                sha256: file.sha256,
                                url: super::export::link(&file.url).ok(),
                            })
                            .collect(),
                    ),
                    (None, Some(result)) => Some(
                        result
                            .files
                            .iter()
                            .map(|file| ListedFile {
                                path: file.path.clone(),
                                size: file.size,
                                sha256: file.sha256,
                                url: None,
                            })
                            .collect(),
                    ),
                    (None, None) => None,
                },
                held,
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
                _ if dataset.from.is_some() => "not built yet".to_owned(),
                _ => "not locked yet".to_owned(),
            };
            let mut row = vec![dataset.name.clone()];
            if with_source {
                row.push(match (&dataset.reference, &dataset.from) {
                    (Some(reference), _) => reference.clone(),
                    (None, Some(from)) => format!("made from {}", from.join(", ")),
                    (None, None) => String::new(),
                });
            }
            let folder = if dataset.placed {
                dataset.folder.clone()
            } else {
                "not placed".to_owned()
            };
            row.extend([contents, folder]);
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
            ("iris.csv".to_owned(), 4551),
            ("docs/readme.txt".to_owned(), 12),
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
        let report = listed(&manifest, &Lock::default(), None);
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

    /// a dataset with `place = false` lists where its folder would be in JSON, beside `placed`,
    /// and as not placed in the table.
    #[test]
    fn an_unplaced_dataset_lists_as_not_placed() {
        let manifest = Manifest::parse(
            "[datasets.iris]\nref = \"zenodo:3608990\"\nplace = false\n\n[datasets.wine]\nref = \"zenodo:1\"\n",
        )
        .unwrap();
        let report = listed(&manifest, &Lock::default(), None);
        assert_eq!(
            table(&report.datasets, None),
            "iris  zenodo:3608990  not locked yet  not placed\n\
             wine  zenodo:1        not locked yet  data/wine"
        );
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["datasets"][0]["placed"], false);
        assert_eq!(value["datasets"][0]["folder"], "data/iris");
        assert_eq!(value["datasets"][1]["placed"], true);
    }

    #[test]
    fn the_license_comes_last_and_gives_way_after_the_source() {
        let listed = |name: &str, license: Option<&str>| Listed {
            name: name.to_owned(),
            reference: Some(format!("zenodo:{}", name.len())),
            from: None,
            resolved: Some(format!("zenodo:{}@1", name.len())),
            files: Some(1),
            bytes: Some(4551),
            folder: format!("data/{name}"),
            placed: true,
            title: None,
            license: license.map(str::to_owned),
            doi: None,
            access: Some(Access::Unknown),
            locked_files: Some(Vec::new()),
            held: Some(Vec::new()),
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

    /// an archive counts as the files it puts in its dataset's folder, as this machine's cache
    /// recorded what it unpacked to, less those a members table leaves out, and `list <name>`
    /// names them by their paths there. one never unpacked here counts as itself until then.
    #[test]
    fn an_archive_counts_as_the_files_it_places() {
        use crate::cache::Member;
        use crate::project::lock::LockedFile;
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let member = |path: &str, size| Member {
            path: path.to_owned(),
            size,
            sha256: crate::digest::sha256(path.as_bytes()),
        };
        let zip = crate::digest::sha256(b"shapes.zip");
        let members = [
            member("shapes/a.png", 1000),
            member("shapes/b.png", 2000),
            member("shapes/labels.csv", 24),
        ];
        let tree = cache.record_unpacked(&zip, &members).unwrap();
        let file = |path: &str, size, sha256, unpacks_to| LockedFile {
            path: path.to_owned(),
            size,
            sha256,
            md5: None,
            url: String::new(),
            unpacks_to,
        };
        let locked = |name: &str, files| Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}.zip"),
            spec: String::new(),
            extract: true,
            resolved: String::new(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: String::new(),
            files,
        };
        let never = crate::digest::sha256(b"never unpacked");
        let lock = Lock {
            datasets: vec![
                locked("images", vec![file("shapes.zip", 3776, zip, Some(tree))]),
                locked(
                    "other",
                    vec![file(
                        "other.zip",
                        512,
                        crate::digest::sha256(b"o"),
                        Some(never),
                    )],
                ),
            ],
            ..Lock::default()
        };
        let manifest = Manifest::parse(
            "[datasets.images]\nref = \"https://example.org/images.zip\"\n\n[datasets.images.members.\"shapes.zip\"]\nexclude = [\"shapes/b.png\"]\n\n[datasets.other]\nref = \"https://example.org/other.zip\"\n",
        )
        .unwrap();
        let report = listed(&manifest, &lock, Some(&cache));
        assert_eq!(
            table(&report.datasets, None),
            "images  https://example.org/images.zip  2 files · 1.0 KiB  data/images\n\
             other   https://example.org/other.zip   1 file · 512 B     data/other"
        );
        let images = report.datasets[0].held.as_deref().unwrap_or_default();
        assert_eq!(file_lines(images), "a.png       1000 B\nlabels.csv  24 B");
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            (
                &value["datasets"][0]["files"],
                &value["datasets"][0]["bytes"]
            ),
            (&serde_json::json!(2), &serde_json::json!(1024))
        );
        assert_eq!(
            value["datasets"][0]["locked_files"][0]["path"],
            "shapes.zip"
        );
    }
}
