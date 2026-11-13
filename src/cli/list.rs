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
use crate::project::lock::{Lock, Locked};
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
    /// each source file data.lock pins, named so because `files` is the count.
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
    let lock = match name {
        Some(name) => project
            .lock_only(&|other| other == name)?
            .map(|(lock, _)| lock),
        None => project.lock()?,
    }
    .unwrap_or_default();
    let output = &context.output;
    let cache = super::peek_cache().ok();
    let report = report(&manifest, &lock, cache.as_ref(), name, output.json());
    if name.is_some() {
        // `--json` prints the report alone, so the lines for a person are not built
        if let Some(dataset) = report.datasets.first()
            && !output.json()
        {
            one(output, dataset);
        }
    } else if report.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        output.hint(&format!(
            "add one with `{}`, or find one with `fetchloom search <words>`",
            super::project::command("add <link>")
        ));
    } else {
        let marks = Marks {
            dot: output.dot(),
            more: output.more(),
        };
        output.answer(&table(&report.datasets, output.answer_width(), marks));
    }
    Ok(report)
}

/// one dataset's files on stdout, a path and a size per line. the heading is an aside for a
/// person, since data.lock and `--json` keep what it says.
fn one(output: &crate::output::Output, dataset: &Listed) {
    let Some(files) = &dataset.held else {
        output.done("Ok", &format!("{} is not locked yet", dataset.name));
        output.hint(&format!(
            "run `{}` to lock it and fetch its files",
            super::project::command("sync")
        ));
        return;
    };
    output.aside(&heading(dataset));
    if files.is_empty() {
        output.done("Ok", &format!("{} has no files", dataset.name));
    } else {
        output.answer(&file_lines(files));
    }
}

fn heading(dataset: &Listed) -> String {
    let mut facts = vec![dataset.name.clone()];
    facts.extend(dataset.title.clone());
    if let (Some(files), Some(bytes)) = (dataset.files, dataset.bytes) {
        facts.push(format!(
            "{} \u{b7} {}",
            units::count(files, "file", "files"),
            units::size(bytes)
        ));
    }
    facts.extend(
        dataset
            .license
            .as_ref()
            .map(|license| format!("license {license}")),
    );
    facts.extend(dataset.doi.as_ref().map(|doi| format!("doi {doi}")));
    facts.join(" \u{b7} ")
}

/// a path and a size per line, the sizes right-aligned so their units line up.
fn file_lines(files: &[(String, u64)]) -> String {
    let width = files
        .iter()
        .map(|(path, _)| units::columns(path))
        .max()
        .unwrap_or_default();
    let sizes: Vec<String> = files.iter().map(|(_, size)| units::size(*size)).collect();
    let widest = sizes.iter().map(String::len).max().unwrap_or_default();
    files
        .iter()
        .zip(&sizes)
        .map(|((path, _), size)| {
            let pad = width - units::columns(path);
            format!("{path}{:pad$}  {size:>widest$}", "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// the files the fetched dataset `name` puts in its folder, by their paths there: an archive as
/// the files its members table keeps of what `cache` recorded, else as itself, every part. a
/// members table that does not fit the pin leaves every pinned file as it is.
fn held(
    manifest: &Manifest,
    name: &str,
    locked: &Locked,
    cache: Option<&Cache>,
) -> Vec<(String, u64)> {
    let mut held = Vec::new();
    each_held(manifest, name, locked, cache, &mut |path, size| {
        held.push((path.to_owned(), size));
    });
    held.sort();
    held
}

/// how many files [`held`] lists, and their bytes, without their paths.
fn held_total(
    manifest: &Manifest,
    name: &str,
    locked: &Locked,
    cache: Option<&Cache>,
) -> (u64, u64) {
    let mut totals = (0, 0);
    each_held(manifest, name, locked, cache, &mut |_, size| {
        totals = counted(totals, size);
    });
    totals
}

fn counted((files, bytes): (u64, u64), size: u64) -> (u64, u64) {
    (files + 1, bytes.saturating_add(size))
}

/// calls `each` for every file [`held`] lists, with its path in the folder and size, in no order.
fn each_held(
    manifest: &Manifest,
    name: &str,
    locked: &Locked,
    cache: Option<&Cache>,
    each: &mut dyn FnMut(&str, u64),
) {
    let as_pinned = |each: &mut dyn FnMut(&str, u64)| {
        for file in &locked.files {
            each(&file.path, file.size);
        }
    };
    let Some(source) = manifest.datasets.get(name).and_then(Entry::source) else {
        as_pinned(each);
        return;
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
        as_pinned(each);
        return;
    }
    for target in &planned.targets {
        let path = target.path.as_str();
        let inside = path
            .strip_prefix(folder.as_str())
            .and_then(|path| path.strip_prefix('/'))
            .unwrap_or(path);
        each(inside, target.size);
    }
    for pinned in archives::pinned(&locked.files) {
        if let Pinned::Archive(archive) = pinned
            && planned
                .unknown
                .iter()
                .any(|(_, file)| file.path == archive.file.path)
        {
            for file in archive.files() {
                each(&file.path, file.size);
            }
        }
    }
}

/// what a list shows, so that it builds only that.
#[derive(Clone, Copy)]
struct View<'n> {
    /// the one dataset `list <name>` shows, or none for every dataset.
    name: Option<&'n str>,
    /// whether each dataset's files go in by path, as `list <name>` shows them to a person.
    paths: bool,
    /// whether each dataset's pinned files go in, as `--json` gives them.
    pinned: bool,
}

fn report(
    manifest: &Manifest,
    lock: &Lock,
    cache: Option<&Cache>,
    name: Option<&str>,
    json: bool,
) -> Report {
    let view = View {
        name,
        paths: name.is_some() && !json,
        pinned: json,
    };
    listed(manifest, lock, cache, view)
}

fn listed(manifest: &Manifest, lock: &Lock, cache: Option<&Cache>, view: View<'_>) -> Report {
    let chosen: Vec<(&String, &Entry)> = match view.name {
        Some(name) => manifest.datasets.get_key_value(name).into_iter().collect(),
        None => manifest.datasets.iter().collect(),
    };
    let datasets = chosen
        .into_iter()
        .map(|(name, entry)| {
            let locked = lock.get(name).filter(|_| entry.source().is_some());
            let result = lock.result(name).filter(|_| entry.recipe().is_some());
            let (held, totals) = match (locked, result) {
                (Some(locked), _) if view.paths => {
                    let held = held(manifest, name, locked, cache);
                    let totals = held.iter().map(|(_, size)| *size).fold((0, 0), counted);
                    (Some(held), Some(totals))
                }
                (Some(locked), _) => (None, Some(held_total(manifest, name, locked, cache))),
                (None, Some(result)) => (
                    view.paths.then(|| {
                        result
                            .files
                            .iter()
                            .map(|file| (file.path.clone(), file.size))
                            .collect()
                    }),
                    Some(
                        result
                            .files
                            .iter()
                            .map(|file| file.size)
                            .fold((0, 0), counted),
                    ),
                ),
                (None, None) => (None, None),
            };
            Listed {
                name: name.clone(),
                reference: entry
                    .source()
                    .map(|source| crate::net::mask_query(&source.reference).into_owned()),
                from: entry.recipe().map(|recipe| recipe.from.clone()),
                resolved: locked.map(|locked| locked.resolved.clone()),
                files: totals.map(|(files, _)| files),
                bytes: totals.map(|(_, bytes)| bytes),
                folder: plan::folder(manifest, name),
                placed: entry.place,
                title: locked.and_then(|locked| locked.title.clone()),
                license: locked.and_then(|locked| locked.license.clone()),
                doi: locked.and_then(|locked| locked.doi.clone()),
                access: locked.map(|locked| locked.access),
                locked_files: match (locked, result) {
                    _ if !view.pinned => None,
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

/// the marks the table writes itself, ascii where the terminal or locale shows no unicode: what
/// parts a file count from its size, and what ends a license cut short.
#[derive(Clone, Copy)]
struct Marks {
    dot: &'static str,
    more: &'static str,
}

/// the datasets as aligned columns, one per line for `grep`. on a terminal too narrow for them all,
/// the source gives way, then the license, since `--json` and `list <name>` keep them.
fn table(datasets: &[Listed], width: Option<usize>, marks: Marks) -> String {
    let widest = |text: &str| text.lines().map(units::columns).max().unwrap_or_default();
    let full = columns(datasets, true, true, marks);
    let Some(width) = width.filter(|width| widest(&full) > *width) else {
        return full;
    };
    let licensed = columns(datasets, false, true, marks);
    if widest(&licensed) <= width {
        return licensed;
    }
    columns(datasets, false, false, marks)
}

fn columns(datasets: &[Listed], with_source: bool, with_license: bool, marks: Marks) -> String {
    let licensed = with_license && datasets.iter().any(|dataset| dataset.license.is_some());
    let rows: Vec<Vec<String>> = datasets
        .iter()
        .map(|dataset| {
            let contents = match (dataset.files, dataset.bytes) {
                (Some(files), Some(bytes)) => format!(
                    "{} {} {}",
                    units::count(files, "file", "files"),
                    marks.dot,
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
                row.push(units::clip(license, LICENSE, marks.more));
            }
            row
        })
        .collect();
    let last = rows.first().map_or(0, Vec::len).saturating_sub(1);
    let widths: Vec<usize> = (0..last)
        .map(|column| {
            rows.iter()
                .map(|row| units::columns(&row[column]))
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
            let pad = width - units::columns(cell);
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

    const UNICODE: Marks = Marks {
        dot: "\u{b7}",
        more: "\u{2026}",
    };

    #[test]
    fn one_dataset_lists_a_path_and_a_size_per_line() {
        let files = [
            ("iris.csv".to_owned(), 4551),
            ("docs/readme.txt".to_owned(), 12),
        ];
        assert_eq!(
            file_lines(&files),
            "iris.csv         4.4 KiB\ndocs/readme.txt     12 B"
        );
        let wide = [
            ("鸢尾花/数据.csv".to_owned(), 4551),
            ("docs/readme.txt".to_owned(), 12),
        ];
        assert_eq!(
            file_lines(&wide),
            "鸢尾花/数据.csv  4.4 KiB\ndocs/readme.txt     12 B",
            "a wide character takes two columns"
        );
    }

    #[test]
    fn the_heading_names_the_dataset_beside_its_license() {
        let mut dataset = Listed {
            name: "iris".to_owned(),
            reference: Some("hf:datasets/scikit-learn/iris".to_owned()),
            from: None,
            resolved: None,
            files: Some(4),
            bytes: Some(18_534),
            folder: "data/iris".to_owned(),
            placed: true,
            title: None,
            license: Some("cc0-1.0".to_owned()),
            doi: None,
            access: None,
            locked_files: None,
            held: None,
        };
        assert_eq!(
            heading(&dataset),
            "iris · 4 files · 18.1 KiB · license cc0-1.0"
        );
        dataset.title = Some("Iris Species".to_owned());
        dataset.doi = Some("10.5281/zenodo.1".to_owned());
        assert_eq!(
            heading(&dataset),
            "iris · Iris Species · 4 files · 18.1 KiB · license cc0-1.0 · doi 10.5281/zenodo.1"
        );
    }

    #[test]
    fn locked_and_unlocked_datasets_line_up() {
        let manifest = Manifest::parse(
            "[datasets.iris]\nref = \"zenodo:3608990\"\n\n[datasets.mnist-digits]\nref = \"hf:datasets/ylecun/mnist\"\n",
        )
        .unwrap();
        let report = report(&manifest, &Lock::default(), None, None, true);
        assert_eq!(
            table(&report.datasets, None, UNICODE),
            "iris          zenodo:3608990            not locked yet  data/iris\n\
             mnist-digits  hf:datasets/ylecun/mnist  not locked yet  data/mnist-digits"
        );
        assert_eq!(
            table(&report.datasets, Some(50), UNICODE),
            "iris          not locked yet  data/iris\n\
             mnist-digits  not locked yet  data/mnist-digits",
            "a narrow terminal drops the source"
        );
    }

    /// the JSON still says where the dataset's folder would be, beside `placed`.
    #[test]
    fn an_unplaced_dataset_lists_as_not_placed() {
        let manifest = Manifest::parse(
            "[datasets.iris]\nref = \"zenodo:3608990\"\nplace = false\n\n[datasets.wine]\nref = \"zenodo:1\"\n",
        )
        .unwrap();
        let report = report(&manifest, &Lock::default(), None, None, true);
        assert_eq!(
            table(&report.datasets, None, UNICODE),
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
            table(&datasets, None, UNICODE),
            "iris    zenodo:4  1 file · 4.4 KiB  data/iris    cc-by-4.0\n\
             tweets  zenodo:6  1 file · 4.4 KiB  data/tweets  Data files © Original Authors…\n\
             wine    zenodo:4  1 file · 4.4 KiB  data/wine"
        );
        assert_eq!(
            table(&datasets, Some(70), UNICODE),
            "iris    1 file · 4.4 KiB  data/iris    cc-by-4.0\n\
             tweets  1 file · 4.4 KiB  data/tweets  Data files © Original Authors…\n\
             wine    1 file · 4.4 KiB  data/wine",
            "the source gives way first"
        );
        assert_eq!(
            table(&datasets, Some(40), UNICODE),
            "iris    1 file · 4.4 KiB  data/iris\n\
             tweets  1 file · 4.4 KiB  data/tweets\n\
             wine    1 file · 4.4 KiB  data/wine",
            "then the license"
        );
        let ascii = Marks {
            dot: "-",
            more: "...",
        };
        assert_eq!(
            table(&datasets[1..2], None, ascii),
            "tweets  zenodo:6  1 file - 4.4 KiB  data/tweets  Data files © Original Authors...",
            "a terminal that shows only ascii gets its marks"
        );
        let mut wide = listed("iris", Some("cc-by-4.0"));
        wide.folder = "data/鸢尾花".to_owned();
        let datasets = [wide, listed("wine", Some("cc-by-4.0"))];
        let shown = table(&datasets, None, UNICODE);
        let starts: Vec<usize> = shown
            .lines()
            .map(|line| console::measure_text_width(&line[..line.find("cc-by").unwrap()]))
            .collect();
        assert_eq!(
            starts[0], starts[1],
            "a wide character takes two columns:\n{shown}"
        );
    }

    /// files a members table leaves out do not count, `list <name>` names the rest by their paths
    /// there, and an archive not yet unpacked here counts as itself.
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
        let report = report(&manifest, &lock, Some(&cache), None, true);
        assert_eq!(
            table(&report.datasets, None, UNICODE),
            "images  https://example.org/images.zip  2 files · 1.0 KiB  data/images\n\
             other   https://example.org/other.zip   1 file · 512 B     data/other"
        );
        let one = super::report(&manifest, &lock, Some(&cache), Some("images"), false);
        let images = one.datasets[0].held.as_deref().unwrap_or_default();
        assert_eq!(file_lines(images), "a.png       1000 B\nlabels.csv    24 B");
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

    /// a project with one dataset of each kind a list shows: a recipe's result, one not placed,
    /// one whose link holds a secret, one not locked, an archive this machine recorded with a
    /// members table, and one it never unpacked; with the cache that recorded it.
    fn every_kind() -> (Manifest, Lock, tempfile::TempDir, Cache) {
        use crate::project::lock::LockedFile;
        let manifest = Manifest::parse(concat!(
            "[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]\n\n",
            "[datasets.hidden]\nref = \"zenodo:4\"\nplace = false\n\n",
            "[datasets.iris]\nref = \"https://example.org/iris.csv?token=abc\"\n\n",
            "[datasets.later]\nref = \"zenodo:3\"\n\n",
            "[datasets.pets]\nref = \"zenodo:2\"\n\n",
            "[datasets.pets.members.\"pets.zip\"]\nexclude = [\"notes.txt\"]\n\n",
            "[datasets.scans]\nref = \"zenodo:5\"\n",
        ))
        .unwrap();
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let member = |path: &str, size: u64| crate::cache::Member {
            path: path.to_owned(),
            size,
            sha256: crate::digest::sha256(path.as_bytes()),
        };
        let pets_zip = crate::digest::sha256(b"pets.zip");
        let tree = cache
            .record_unpacked(
                &pets_zip,
                &[
                    member("images/cat.jpg", 3_000),
                    member("images/dog.jpg", 5_000),
                    member("notes.txt", 20),
                ],
            )
            .unwrap();
        let file = |path: &str, size: u64, query: &str| LockedFile {
            path: path.to_owned(),
            size,
            sha256: crate::digest::sha256(path.as_bytes()),
            md5: None,
            url: format!("https://example.org/{path}{query}"),
            unpacks_to: None,
        };
        let pinned = |name: &str, files: Vec<LockedFile>| {
            let source = manifest.datasets[name].source().unwrap();
            Locked {
                name: name.to_owned(),
                reference: source.reference.clone(),
                spec: crate::project::lock::spec(source),
                extract: source.extract,
                resolved: format!("{name}-v1"),
                title: Some(format!("the {name} set")),
                license: Some("cc-by-4.0".to_owned()),
                doi: None,
                access: Access::Unknown,
                timestamp: "2026-01-01T00:00:00Z".to_owned(),
                files,
            }
        };
        let recorded = LockedFile {
            sha256: pets_zip,
            unpacks_to: Some(tree),
            ..file("pets.zip", 4_000, "")
        };
        let never = LockedFile {
            unpacks_to: Some(crate::digest::sha256(b"never unpacked")),
            ..file("scans.zip", 60_000, "")
        };
        let result_files = vec![crate::cache::Member {
            sha256: crate::digest::sha256(b"clean"),
            ..member("iris-clean.csv", 700)
        }];
        let lock = Lock {
            datasets: vec![
                pinned("hidden", vec![file("h.bin", 9, "")]),
                pinned(
                    "iris",
                    vec![
                        file("iris.csv", 4_551, "?token=abc"),
                        file("readme.txt", 12, ""),
                    ],
                ),
                pinned("pets", vec![recorded, file("labels.csv", 30, "")]),
                pinned("scans", vec![never]),
            ],
            derived: vec![crate::project::lock::derived::Derived {
                name: "clean".to_owned(),
                from: Vec::new(),
                run: vec!["python3".to_owned()],
                code: Vec::new(),
                env: std::collections::BTreeMap::new(),
                key: crate::digest::sha256(b"key"),
                timestamp: "2026-01-02T00:00:00Z".to_owned(),
                tree: crate::cache::tree_hash(&result_files),
                files: result_files,
            }],
        };
        (manifest, lock, folder, cache)
    }

    /// the views are every dataset or one, each for a person or as `--json`.
    #[test]
    fn each_view_lists_what_the_whole_list_has() {
        let (manifest, lock, _folder, cache) = every_kind();
        let shown =
            |name: Option<&str>, json: bool| report(&manifest, &lock, Some(&cache), name, json);
        let each = |report: &Report| -> Vec<String> {
            let value = serde_json::to_value(report).unwrap();
            value["datasets"]
                .as_array()
                .unwrap()
                .iter()
                .map(serde_json::Value::to_string)
                .collect()
        };
        let expected = [
            r#"{"access":null,"bytes":700,"doi":null,"files":1,"folder":"data/clean","from":["iris"],"license":null,"locked_files":[{"path":"iris-clean.csv","sha256":"3b066804f6d1d077173cfe4d06002e6a61e6f21c2b2e648417962115f1afcd8e","size":700,"url":null}],"name":"clean","placed":true,"ref":null,"resolved":null,"title":null}"#,
            r#"{"access":"unknown","bytes":9,"doi":null,"files":1,"folder":"data/hidden","from":null,"license":"cc-by-4.0","locked_files":[{"path":"h.bin","sha256":"0925728359a07a9b354226d3f74b9438c96b02f383533dae71cf688daf5ace47","size":9,"url":"https://example.org/h.bin"}],"name":"hidden","placed":false,"ref":"zenodo:4","resolved":"hidden-v1","title":"the hidden set"}"#,
            r#"{"access":"unknown","bytes":4563,"doi":null,"files":2,"folder":"data/iris","from":null,"license":"cc-by-4.0","locked_files":[{"path":"iris.csv","sha256":"ab7cabb5c193f27616d6c479b5880441ab13117bebe91fdddc797bdfe88d3e08","size":4551,"url":null},{"path":"readme.txt","sha256":"9e6e4772050998a5c0dc3c61acf3dab0a7e594566171fa5746d6b62f9598efb6","size":12,"url":"https://example.org/readme.txt"}],"name":"iris","placed":true,"ref":"https://example.org/iris.csv?token=***","resolved":"iris-v1","title":"the iris set"}"#,
            r#"{"access":null,"bytes":null,"doi":null,"files":null,"folder":"data/later","from":null,"license":null,"locked_files":null,"name":"later","placed":true,"ref":"zenodo:3","resolved":null,"title":null}"#,
            r#"{"access":"unknown","bytes":8030,"doi":null,"files":3,"folder":"data/pets","from":null,"license":"cc-by-4.0","locked_files":[{"path":"pets.zip","sha256":"fa70fd5c3721e72a45715d3e82a49fcd15c1ed46aa714627a6fc0582b95c50c5","size":4000,"url":"https://example.org/pets.zip"},{"path":"labels.csv","sha256":"6bf6d3cb41de5ffae32a537c49ee7c4d8667314c5245cfdcd37399e1046d530e","size":30,"url":"https://example.org/labels.csv"}],"name":"pets","placed":true,"ref":"zenodo:2","resolved":"pets-v1","title":"the pets set"}"#,
            r#"{"access":"unknown","bytes":60000,"doi":null,"files":1,"folder":"data/scans","from":null,"license":"cc-by-4.0","locked_files":[{"path":"scans.zip","sha256":"662d753124e3d540e95f3e4c988a0beb71357f542a7fa8358107c3cdb54e2d04","size":60000,"url":"https://example.org/scans.zip"}],"name":"scans","placed":true,"ref":"zenodo:5","resolved":"scans-v1","title":"the scans set"}"#,
        ];
        let table_text = "clean   made from iris                          1 file · 700 B     data/clean\nhidden  zenodo:4                                1 file · 9 B       not placed  cc-by-4.0\niris    https://example.org/iris.csv?token=***  2 files · 4.5 KiB  data/iris   cc-by-4.0\nlater   zenodo:3                                not locked yet     data/later\npets    zenodo:2                                3 files · 7.8 KiB  data/pets   cc-by-4.0\nscans   zenodo:5                                1 file · 58.6 KiB  data/scans  cc-by-4.0";
        let views: [(&str, &str, Option<&str>); 6] = [
            (
                "clean",
                "clean · 1 file · 700 B",
                Some("iris-clean.csv  700 B"),
            ),
            (
                "hidden",
                "hidden · the hidden set · 1 file · 9 B · license cc-by-4.0",
                Some("h.bin  9 B"),
            ),
            (
                "iris",
                "iris · the iris set · 2 files · 4.5 KiB · license cc-by-4.0",
                Some("iris.csv    4.4 KiB\nreadme.txt     12 B"),
            ),
            ("later", "later", None),
            (
                "pets",
                "pets · the pets set · 3 files · 7.8 KiB · license cc-by-4.0",
                Some(
                    "labels.csv              30 B\npets/images/cat.jpg  2.9 KiB\npets/images/dog.jpg  4.9 KiB",
                ),
            ),
            (
                "scans",
                "scans · the scans set · 1 file · 58.6 KiB · license cc-by-4.0",
                Some("scans.zip  58.6 KiB"),
            ),
        ];
        assert_eq!(each(&shown(None, true)), expected);
        assert_eq!(
            table(&shown(None, false).datasets, None, UNICODE),
            table_text
        );
        for ((name, heading_text, lines), expected) in views.into_iter().zip(expected) {
            assert_eq!(each(&shown(Some(name), true)), [expected], "{name}");
            let one = shown(Some(name), false);
            let [dataset] = one.datasets.as_slice() else {
                panic!("{name}");
            };
            let listed = dataset.held.as_deref().map(file_lines);
            assert_eq!(
                (heading(dataset).as_str(), listed.as_deref()),
                (heading_text, lines),
                "{name}"
            );
        }
    }
}
