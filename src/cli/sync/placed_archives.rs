//! archives whose files a project's data folder already holds where placing puts them, as when a
//! project moves to another machine with its data folder but not its cache. an archive's list of
//! files is rebuilt from those files, and stands for the archive only when it hashes to the tree
//! data.lock pins as its `unpacks_to`: then the files are taken as placed, and the archive itself
//! is fetched only once a file of it is needed again.
//!
//! the files of a data folder anyone could have filled prove only that tree, never what an
//! archive's bytes unpack to, so a list found here is recorded by the tree's own key
//! ([`tree_key`]), which only a lookup that pins the tree finds.
//!
//! when the files as they lie prove no list, as when the person's own sit beside them, the files
//! a copy's state records as placed for the archive's dataset are tried alone. anyone could have
//! written that state too, so it only says which files to try: each is still hashed, and the
//! list must still hash to the tree.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::disk::{Survey, hash_plain};
use crate::cache::{Cache, Member, tree_hash, tree_key};
use crate::digest::{Block, Sha256};
use crate::error::Error;
use crate::output::{Output, Warning};
use crate::project::lock::Lock;
use crate::project::lock::archives::{self, Archive, Pinned};
use crate::project::manifest::Manifest;
use crate::project::{Project, native};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::{name, plan, units};

/// the lists of files found in place, by the tree each hashes to, and the archives whose files
/// sit in place but prove no list.
#[derive(Default)]
pub(crate) struct PlacedArchives {
    trees: HashMap<Sha256, Found>,
    unproven: Vec<Unproven>,
}

/// an archive whose folder holds files that cannot stand for it, so it is downloaded.
struct Unproven {
    /// the archive, as `<dataset>/<path>`.
    label: String,
    /// the archive's path in the project, as status names it.
    path: String,
    /// the folder its files land below, relative to the project.
    folder: String,
    /// a members table placed only some of its files, which no tree can prove.
    filtered: bool,
    /// the files below `folder`, which say nothing of it if another archive's list took them.
    placed: Vec<String>,
}

impl Unproven {
    /// why the files in place cannot stand for the archive.
    fn why(&self) -> String {
        if self.filtered {
            "its members table placed only some of its files, which cannot prove the list of files data.lock pins for it".to_owned()
        } else {
            format!(
                "the files in {} cannot prove the list of files data.lock pins for it",
                self.folder
            )
        }
    }
}

/// an archive's list of files found in place.
struct Found {
    members: Vec<Member>,
    /// the archive, as `<dataset>/<path>`.
    label: String,
    /// the folder its files sit below, relative to the project.
    folder: String,
}

/// a way the files below an archive's folder may be its files, as placing puts them: each file's
/// path in the project with its path in the archive.
type Guess = Vec<(String, String)>;

/// a guess whose files hash to the tree: the paths of its files in the project, with the members
/// they make.
type Proven = (Vec<String>, Vec<Member>);

impl PlacedArchives {
    /// the lists of files of the archives of `unknown` whose files `survey` finds where placing
    /// puts them, each checked by hashing those files against the tree data.lock pins.
    pub(crate) fn find(
        project: &Project,
        (manifest, lock): (&Manifest, &Lock),
        survey: &Survey,
        unknown: &[(String, Archive<'_>)],
        (threads, stop): (Threads, &Stop),
    ) -> Result<PlacedArchives, Error> {
        let mut found = PlacedArchives::default();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut hashed: HashMap<String, Option<Sha256>> = HashMap::new();
        let mut recorded: Option<HashMap<String, HashSet<String>>> = None;
        let temporary: HashSet<&String> = survey.leftovers().collect();
        // built once for all the archives: each archive building every dataset's folder and
        // passing over every file was quadratic in archive datasets
        let folders = DatasetFolderSet::of(manifest);
        let paths = in_order(
            survey
                .disk
                .keys()
                .filter(|placed| !temporary.contains(placed))
                .map(String::as_str),
        );
        for (dataset, archive) in unknown {
            let Some(tree) = archive.file.unpacks_to else {
                continue;
            };
            if found.trees.contains_key(&tree) {
                continue;
            }
            let Some(layout) = Layout::of((manifest, lock), dataset, archive) else {
                continue;
            };
            let listed = layout.listed(&paths, &folders);
            if listed.is_empty() {
                continue;
            }
            let below: Vec<(&str, &str)> = listed
                .iter()
                .map(|(placed, inside)| (*placed, inside.as_str()))
                .collect();
            let placed: Vec<String> = below
                .iter()
                .map(|(placed, _)| (*placed).to_owned())
                .collect();
            let unproven = |filtered: bool| Unproven {
                label: format!("{dataset}/{}", archive.file.path),
                path: name::joined(&layout.folder, &archive.file.path),
                folder: layout.base.clone(),
                filtered,
                placed: placed.clone(),
            };
            if layout.filtered {
                found.unproven.push(unproven(true));
                continue;
            }
            let disk = (project.root.as_path(), survey);
            let mut proven = first_proven(
                &guesses(&layout, &below),
                &tree,
                &mut hashed,
                disk,
                (threads, stop),
            )?;
            if proven.is_none() && layout.decompressed.is_none() {
                let recorded = recorded.get_or_insert_with(|| copied(survey));
                let named = recorded.get(dataset.as_str());
                if let Some(hinted) = named.and_then(|named| hinted(&below, &layout.base, named)) {
                    tracing::debug!(
                        archive = archive.file.path,
                        files = hinted.len(),
                        "trying the files a copy's state names"
                    );
                    let guesses = placings(&layout, &hinted);
                    proven = first_proven(&guesses, &tree, &mut hashed, disk, (threads, stop))?;
                }
            }
            let Some((taken, members)) = proven else {
                found.unproven.push(unproven(false));
                continue;
            };
            claimed.extend(taken);
            let label = format!("{dataset}/{}", archive.file.path);
            found.trees.insert(
                tree,
                Found {
                    members,
                    label,
                    folder: layout.base,
                },
            );
        }
        // files another archive's list took say nothing of an archive left unproven
        found.unproven.retain(|unproven| {
            unproven
                .placed
                .iter()
                .any(|placed| !claimed.contains(placed))
        });
        found
            .unproven
            .sort_unstable_by(|a, b| a.label.cmp(&b.label));
        Ok(found)
    }

    /// says once of each archive whose folder holds files that cannot stand for it that it is
    /// downloaded, and why; `offline`, that it must be.
    pub(crate) fn tell_unproven(&self, output: &Output, offline: bool) {
        let will = if offline { "must" } else { "will" };
        for unproven in &self.unproven {
            output.warn(
                Warning::ArchiveUnproven,
                &format!(
                    "{} {will} be downloaded: {}",
                    unproven.label,
                    unproven.why()
                ),
            );
        }
    }

    /// why each archive's files in place cannot stand for it, by its path in the project.
    pub(crate) fn unproven(&self) -> HashMap<String, String> {
        self.unproven
            .iter()
            .map(|unproven| (unproven.path.clone(), unproven.why()))
            .collect()
    }

    /// whether no list was found.
    pub(crate) fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }

    /// the plan for `lock` with each archive's files as `cache` records them, or as found here.
    pub(crate) fn plan<'l>(
        &self,
        manifest: &Manifest,
        lock: &'l Lock,
        cache: &Cache,
    ) -> Result<plan::Plan<'l>, Error> {
        plan::plan(manifest, lock, &|archive| {
            archive.recorded(cache).or_else(|| {
                let tree = archive.file.unpacks_to.as_ref()?;
                self.trees.get(tree).map(|found| found.members.clone())
            })
        })
    }

    /// records each list in `cache` by its tree's key, and says which archives need no download.
    pub(crate) fn record(&self, cache: &Cache, output: &Output) -> Result<(), Error> {
        let mut trees: Vec<(&Sha256, &Found)> = self.trees.iter().collect();
        trees.sort_unstable_by(|(_, a), (_, b)| a.label.cmp(&b.label));
        for (tree, found) in trees {
            let text = crate::cache::tree(&found.members);
            let recorded = cache
                .record_tree_bytes(&tree_key(tree), tree, text.as_bytes())
                .map_err(|err| {
                    Error::io(
                        format_args!("could not record what {} holds", found.label),
                        &err,
                    )
                })?;
            if !recorded {
                continue;
            }
            let count = found.members.len() as u64;
            output.step(
                "Checked",
                &format!(
                    "{} \u{b7} {} in {} {} what data.lock pins, so it needs no download",
                    found.label,
                    units::count(count, "file", "files"),
                    found.folder,
                    if count == 1 { "holds" } else { "hold" }
                ),
            );
        }
        Ok(())
    }
}

/// the first of `guesses` whose files hash to `tree`, each of its files `hashed` lacks hashed
/// first, a few at once.
fn first_proven(
    guesses: &[Guess],
    tree: &Sha256,
    hashed: &mut HashMap<String, Option<Sha256>>,
    (root, survey): (&Path, &Survey),
    (threads, stop): (Threads, &Stop),
) -> Result<Option<Proven>, Error> {
    let fresh: Vec<&str> = guesses
        .iter()
        .flatten()
        .map(|(placed, _)| placed.as_str())
        .filter(|placed| !hashed.contains_key(*placed))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let digests = crate::place::run_all(&fresh, threads, |placed| {
        let size = survey.disk.get(*placed)?.size;
        stop.check_io().ok()?;
        hash_plain(&native(root, placed), Block::sized(size.saturating_add(1))).ok()
    });
    stop.check()?;
    hashed.extend(fresh.into_iter().map(str::to_owned).zip(digests));
    let member = |(placed, path): &(String, String)| {
        Some(Member {
            path: path.clone(),
            size: survey.disk.get(placed)?.size,
            sha256: (*hashed.get(placed)?)?,
        })
    };
    Ok(guesses.iter().find_map(|guess| {
        let members: Vec<Member> = guess.iter().map(member).collect::<Option<_>>()?;
        (tree_hash(&members) == *tree).then(|| {
            (
                guess.iter().map(|(placed, _)| placed.clone()).collect(),
                members,
            )
        })
    }))
}

/// the files the state a copy of the project brought records as placed, by path in the project, in
/// sets by dataset, as the survey read them.
fn copied(survey: &Survey) -> HashMap<String, HashSet<String>> {
    let mut by_dataset: HashMap<String, HashSet<String>> = HashMap::new();
    for (path, dataset) in &survey.state.copied.files {
        by_dataset
            .entry(dataset.clone())
            .or_default()
            .insert(path.clone());
    }
    by_dataset
}

/// the files of `below`, the files below the folder `base` with their paths there, that `named`
/// names by path in the project, when they leave out some file: those alone may be the archive's,
/// and the rest the person's own.
fn hinted<'b>(
    below: &[(&'b str, &'b str)],
    base: &str,
    named: &HashSet<String>,
) -> Option<Vec<(&'b str, &'b str)>> {
    let hinted: Vec<(&str, &str)> = below
        .iter()
        .copied()
        .filter(|(_, inside)| named.contains(&name::joined(base, inside)))
        .collect();
    (!hinted.is_empty() && hinted.len() < below.len()).then_some(hinted)
}

/// where placing put the files of an archive, as data.toml and data.lock lay its dataset out.
struct Layout {
    dataset: String,
    /// the dataset's folder, relative to the project.
    folder: String,
    /// the folder the archive's files land below.
    base: String,
    /// the archive's name without its ending.
    stem: String,
    /// whether the archive is the dataset's only pinned file, whose files lose the top folder
    /// they share.
    alone: bool,
    /// the name a single compressed file decompresses to.
    decompressed: Option<String>,
    /// a members table placed only some of its files.
    filtered: bool,
    /// the paths of the dataset's files pinned whole, none of them the archive's.
    whole: HashSet<String>,
}

impl Layout {
    /// where `archive`, a file of `dataset`, puts its files.
    fn of(
        (manifest, lock): (&Manifest, &Lock),
        dataset: &str,
        archive: &Archive<'_>,
    ) -> Option<Layout> {
        let entry = manifest.datasets.get(dataset)?;
        let filtered = entry.source.members.get(&archive.file.path).is_some();
        let pinned = archives::pinned(&lock.get(dataset)?.files);
        let folder = plan::folder(manifest, dataset);
        let whole = pinned
            .iter()
            .filter_map(|pinned| match pinned {
                Pinned::File(file) => Some(name::joined(&folder, &file.path)),
                Pinned::Archive(_) => None,
            })
            .collect();
        let path = archive.path();
        let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
        let base = plan::unpacked_folder(manifest, dataset, path);
        Some(Layout {
            dataset: dataset.to_owned(),
            folder,
            base,
            stem: crate::archive::stem(name).to_owned(),
            alone: pinned.len() == 1,
            decompressed: archive.decompressed(),
            filtered,
            whole,
        })
    }

    /// the files of `sorted`, the disk's paths in byte order, that [`Layout::below`] gives: the
    /// ones below the archive's folder are a run of them.
    fn listed<'p>(&self, sorted: &[&'p str], folders: &DatasetFolderSet) -> Vec<(&'p str, String)> {
        self.below(paths_below(sorted, &self.base), folders)
    }

    /// each file of `files`, paths in the project as the disk lists them, below the folder the
    /// archive's files land below, pinned by none of the dataset's whole files and below no
    /// other dataset's folder of `folders`, with its path there in NFC, as an archive's members
    /// are kept, in order. a file a system writes by itself, which unpacking never keeps, is left
    /// out, unless it is the file one decompresses to.
    fn below<'f>(
        &self,
        files: impl Iterator<Item = &'f str>,
        folders: &DatasetFolderSet,
    ) -> Vec<(&'f str, String)> {
        let prefix = format!("{}/", self.base);
        let mut below: Vec<(&str, String)> = files
            .filter(|placed| !self.whole.contains(*placed))
            .filter(|placed| !folders.hold(placed, &self.folder))
            .filter_map(|placed| Some((placed, name::composed(placed.strip_prefix(&prefix)?))))
            .filter(|(_, inside)| {
                self.decompressed.as_ref() == Some(inside)
                    || !inside.split('/').any(name::made_by_system)
            })
            .collect();
        below.sort_unstable();
        below
    }
}

/// the folder of every dataset data.toml names, built once for all the archives a run looks for.
/// a file below the folder of another dataset is none of an archive's, as when the archive's
/// dataset is the data folder itself.
#[derive(Default)]
struct DatasetFolderSet(HashSet<String>);

impl DatasetFolderSet {
    fn of(manifest: &Manifest) -> DatasetFolderSet {
        DatasetFolderSet(
            manifest
                .datasets
                .keys()
                .map(|name| plan::folder(manifest, name))
                .collect(),
        )
    }

    /// whether `path` starts with the folder of a dataset and `/`, that folder not `own`: a
    /// look at each folder above it in the path, not a pass over every dataset's.
    fn hold(&self, path: &str, own: &str) -> bool {
        path.match_indices('/').any(|(at, _)| {
            let folder = &path[..at];
            folder != own && self.0.contains(folder)
        })
    }
}

/// `paths` in byte order, in which the paths below a folder are one run ([`paths_below`]).
fn in_order<'p>(paths: impl Iterator<Item = &'p str>) -> Vec<&'p str> {
    let mut paths: Vec<&str> = paths.collect();
    paths.sort_unstable();
    paths
}

/// the paths of `sorted`, a list in byte order, that start with `folder` and `/`: found by a
/// search for the first of them, as they follow one another.
fn paths_below<'s, 'p>(
    sorted: &'s [&'p str],
    folder: &str,
) -> impl Iterator<Item = &'p str> + use<'s, 'p> {
    let prefix = format!("{folder}/");
    let start = sorted.partition_point(|path| *path < prefix.as_str());
    sorted[start..]
        .iter()
        .copied()
        .take_while(move |path| path.starts_with(prefix.as_str()))
}

/// the ways `below`, the files below an archive's folder with their paths there, may be the
/// archive's own as placing put them, the likeliest first: without what a person's tools leave
/// beside them, then with it, as an archive may hold such files too.
fn guesses(layout: &Layout, below: &[(&str, &str)]) -> Vec<Guess> {
    if let Some(to) = &layout.decompressed {
        return below
            .iter()
            .find(|(_, inside)| inside == to)
            .map(|(placed, _)| vec![((*placed).to_owned(), to.clone())])
            .into_iter()
            .collect();
    }
    let untooled: Vec<(&str, &str)> = below
        .iter()
        .copied()
        .filter(|(_, inside)| !inside.split('/').any(name::made_by_tool))
        .collect();
    let mut guesses = placings(layout, &untooled);
    if untooled.len() != below.len() {
        guesses.extend(placings(layout, below));
    }
    // a folder with no files proves nothing, though an archive of none hashes to the empty tree
    guesses.retain(|guess| !guess.is_empty());
    guesses
}

/// the ways `below` may be an archive's files as placing put them. a dataset that is one archive
/// loses the top folder its files share, which only the archive's or the dataset's name can give
/// back.
fn placings(layout: &Layout, below: &[(&str, &str)]) -> Vec<Guess> {
    let as_placed = |files: &[(&str, &str)], top: Option<&str>| -> Guess {
        files
            .iter()
            .map(|(placed, inside)| {
                let path =
                    top.map_or_else(|| (*inside).to_owned(), |top| format!("{top}/{inside}"));
                ((*placed).to_owned(), path)
            })
            .collect()
    };
    let mut guesses = Vec::new();
    // a .gitignore in the dataset's folder is a person's, as the survey leaves out the one
    // fetchloom keeps it out of git with, unless the archive holds it
    let ignore_file = format!("{}/.gitignore", layout.folder);
    let unmarked: Vec<(&str, &str)> = below
        .iter()
        .copied()
        .filter(|(placed, _)| *placed != ignore_file)
        .collect();
    if layout.alone {
        let mut tops = vec![None, Some(layout.stem.as_str())];
        if layout.dataset != layout.stem {
            tops.push(Some(layout.dataset.as_str()));
        }
        let mut lists = vec![unmarked.as_slice()];
        if unmarked.len() != below.len() {
            lists.push(below);
        }
        for files in lists {
            for top in &tops {
                guesses.push(as_placed(files, *top));
            }
        }
    } else {
        // beside other files, in a folder named after the archive when its files share no top
        // folder, or in the top folder they share, or as the one file it holds
        let inside = format!("{}/", layout.stem);
        let named: Vec<(&str, &str)> = unmarked
            .iter()
            .filter_map(|(placed, rest)| Some((*placed, rest.strip_prefix(&inside)?)))
            .collect();
        guesses.push(as_placed(&named, None));
        let tops: BTreeSet<&str> = unmarked
            .iter()
            .filter_map(|(_, rest)| rest.split_once('/').map(|(top, _)| top))
            .collect();
        for top in tops {
            let files: Vec<(&str, &str)> = unmarked
                .iter()
                .copied()
                .filter(|(_, rest)| rest.split_once('/').is_some_and(|(first, _)| first == top))
                .collect();
            guesses.push(as_placed(&files, None));
        }
        for (placed, rest) in unmarked.iter().filter(|(_, rest)| !rest.contains('/')) {
            guesses.push(vec![((*placed).to_owned(), (*rest).to_owned())]);
        }
    }
    guesses
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the archive `name` of the dataset `dataset` in `data/<dataset>`, `alone` in it or not.
    fn layout(dataset: &str, name: &str, alone: bool) -> Layout {
        Layout {
            dataset: dataset.to_owned(),
            folder: format!("data/{dataset}"),
            base: format!("data/{dataset}"),
            stem: crate::archive::stem(name).to_owned(),
            alone,
            decompressed: None,
            filtered: false,
            whole: HashSet::new(),
        }
    }

    /// the paths in the archive each guess gives the files of `placed`, paths in the project.
    fn guessed(layout: &Layout, placed: &[&str]) -> Vec<Vec<String>> {
        guessed_beside(layout, placed, &DatasetFolderSet::default())
    }

    /// [`guessed`], with `folders` the folders of every dataset data.toml names.
    fn guessed_beside(
        layout: &Layout,
        placed: &[&str],
        folders: &DatasetFolderSet,
    ) -> Vec<Vec<String>> {
        let listed = layout.below(placed.iter().copied(), folders);
        let below: Vec<(&str, &str)> = listed
            .iter()
            .map(|(placed, inside)| (*placed, inside.as_str()))
            .collect();
        guesses(layout, &below)
            .into_iter()
            .map(|guess| guess.into_iter().map(|(_, path)| path).collect())
            .collect()
    }

    /// an archive in a folder of its dataset unpacks below that folder, where its files are
    /// looked for.
    #[test]
    fn an_archive_in_a_folder_of_its_dataset_is_looked_for_below_it() {
        let manifest =
            Manifest::parse("[datasets.scans]\nref = \"https://example.org/scans\"\n").unwrap();
        let text = format!(
            "version = 1\n\n[[dataset]]\nname = \"scans\"\nref = \"https://example.org/scans\"\nspec = \"sha256:11\"\nresolved = \"https://example.org/scans\"\nlocked = \"2026-11-15T07:00:00Z\"\nfiles = [\n  {{ path = \"raw/scans.zip\", size = 1, sha256 = \"{0}\", url = \"https://example.org/raw/scans.zip\", unpacks_to = \"sha256:{0}\" }},\n]\n",
            "0".repeat(64)
        );
        let lock = Lock::parse(&text).unwrap();
        let pinned = archives::pinned(&lock.get("scans").unwrap().files);
        let Some(Pinned::Archive(archive)) = pinned.first() else {
            panic!("no archive pinned");
        };
        let layout = Layout::of((&manifest, &lock), "scans", archive).unwrap();
        assert_eq!(layout.base, "data/scans/raw");
    }

    #[test]
    fn a_lone_archive_is_guessed_without_a_top_folder_and_under_its_name_or_its_datasets() {
        let placed = [
            "data/scans/.gitignore",
            "data/scans/a.txt",
            "data/scans/b/c.txt",
            "data/other/x.txt",
        ];
        assert_eq!(
            guessed(&layout("scans", "cifar.tar.gz", true), &placed),
            [
                vec!["a.txt", "b/c.txt"],
                vec!["cifar/a.txt", "cifar/b/c.txt"],
                vec!["scans/a.txt", "scans/b/c.txt"],
                vec![".gitignore", "a.txt", "b/c.txt"],
                vec!["cifar/.gitignore", "cifar/a.txt", "cifar/b/c.txt"],
                vec!["scans/.gitignore", "scans/a.txt", "scans/b/c.txt"],
            ]
        );
    }

    #[test]
    fn an_archive_beside_other_files_is_guessed_in_its_folder_its_top_folders_and_its_files() {
        let mut layout = layout("set", "flowers.zip", false);
        layout.whole.insert("data/set/notes.csv".to_owned());
        let placed = [
            "data/set/notes.csv",
            "data/set/flowers/a.csv",
            "data/set/flowers/b/c.csv",
            "data/set/top/d.csv",
            "data/set/lone.csv",
        ];
        assert_eq!(
            guessed(&layout, &placed),
            [
                vec!["a.csv", "b/c.csv"],
                vec!["flowers/a.csv", "flowers/b/c.csv"],
                vec!["top/d.csv"],
                vec!["lone.csv"],
            ]
        );
    }

    /// unpacking never keeps a file a system writes by itself, so none is guessed; what a tool
    /// leaves may be an archive's own or a person's, so its files are guessed without it first.
    #[test]
    fn guesses_leave_out_what_a_system_writes_and_try_without_what_a_tool_leaves() {
        let placed = [
            "data/scans/.DS_Store",
            "data/scans/.ipynb_checkpoints/a-checkpoint.txt",
            "data/scans/__MACOSX/a.txt",
            "data/scans/a.txt",
            "data/scans/a.txt~",
            "data/scans/b/._c.txt",
            "data/scans/b/c.txt",
            "data/scans/notes.txt",
        ];
        assert_eq!(
            guessed(&layout("scans", "scans.zip", true), &placed),
            [
                vec!["a.txt", "b/c.txt", "notes.txt"],
                vec!["scans/a.txt", "scans/b/c.txt", "scans/notes.txt"],
                vec![
                    ".ipynb_checkpoints/a-checkpoint.txt",
                    "a.txt",
                    "a.txt~",
                    "b/c.txt",
                    "notes.txt"
                ],
                vec![
                    "scans/.ipynb_checkpoints/a-checkpoint.txt",
                    "scans/a.txt",
                    "scans/a.txt~",
                    "scans/b/c.txt",
                    "scans/notes.txt"
                ],
            ]
        );
    }

    /// the files a copy's state names are tried apart from the person's beside them, matched by
    /// their composed names as the state keeps them; a path it names that the folder lacks adds
    /// nothing, and a state that leaves out no file of the folder, or names none, adds no guess.
    #[test]
    fn the_files_a_copied_state_names_are_tried_without_the_persons() {
        let layout = layout("scans", "scans.zip", true);
        let placed = [
            "data/scans/a.bin",
            "data/scans/a.bin.bai",
            "data/scans/cafe\u{301}.bin",
            "data/scans/notes.txt",
        ];
        let listed = layout.below(placed.iter().copied(), &DatasetFolderSet::default());
        let below: Vec<(&str, &str)> = listed
            .iter()
            .map(|(placed, inside)| (*placed, inside.as_str()))
            .collect();
        let tried = |named: &[&str]| {
            let named: HashSet<String> = named.iter().map(|path| (*path).to_owned()).collect();
            hinted(&below, &layout.base, &named).map(|hinted| {
                hinted
                    .into_iter()
                    .map(|(placed, _)| placed)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            tried(&[
                "data/scans/a.bin",
                "data/scans/caf\u{e9}.bin",
                "data/scans/gone.bin"
            ]),
            Some(vec!["data/scans/a.bin", "data/scans/cafe\u{301}.bin"])
        );
        assert_eq!(
            tried(&[
                "data/scans/a.bin",
                "data/scans/a.bin.bai",
                "data/scans/caf\u{e9}.bin",
                "data/scans/notes.txt"
            ]),
            None
        );
        assert_eq!(tried(&["data/other/a.bin"]), None);
    }

    #[test]
    fn a_dataset_in_the_data_folder_itself_leaves_out_the_other_datasets_files() {
        let mut layout = layout("all", "all.zip", true);
        layout.folder = "data".to_owned();
        layout.base = "data".to_owned();
        let folders = DatasetFolderSet(HashSet::from(["data".to_owned(), "data/iris".to_owned()]));
        assert_eq!(
            guessed_beside(&layout, &["data/a.csv", "data/iris/iris.csv"], &folders)[0],
            ["a.csv"]
        );
    }

    /// a file is another dataset's when that dataset's folder and a `/` start its path: nested
    /// folders, siblings sharing a start (`data/a`, `data/ab`, `data/a-b`, `data/a.b`), two
    /// datasets in one folder, outputs beside and inside the data folder, and the data folder.
    #[test]
    fn a_file_below_another_datasets_folder_is_none_of_an_archives() {
        let manifest = Manifest::parse(concat!(
            "[datasets.a]\nref = \"x\"\npath = \"a\"\n",
            "[datasets.inner]\nref = \"x\"\npath = \"a/inner\"\n",
            "[datasets.ab]\nref = \"x\"\npath = \"ab\"\n",
            "[datasets.dash]\nref = \"x\"\npath = \"a-b\"\n",
            "[datasets.dot]\nref = \"x\"\npath = \"a.b\"\n",
            "[datasets.one]\nref = \"x\"\npath = \"pair\"\n",
            "[datasets.two]\nref = \"x\"\npath = \"pair\"\n",
            "[datasets.run]\nref = \"local\"\nat = \"results/run\"\n",
            "[datasets.part]\nref = \"x\"\npath = \"part\"\n",
            "[datasets.output]\nref = \"local\"\nat = \"data/part/out\"\n",
        ))
        .unwrap();
        let folders = DatasetFolderSet::of(&manifest);
        for (path, own, held) in [
            ("data/a/x.csv", "data/a", false),
            ("data/a/inner/x.csv", "data/a", true),
            // a's folder holds inner's: the rule leaves out what any other folder holds
            ("data/a/inner/x.csv", "data/a/inner", true),
            ("data/ab/x.csv", "data/ab", false),
            ("data/a-b/x.csv", "data/a-b", false),
            ("data/a.b/x.csv", "data/a.b", false),
            ("data/ab/x.csv", "data/a", true),
            ("data/pair/x.csv", "data/pair", false),
            ("results/run/x.csv", "data/a", true),
            ("data/part/out/x.csv", "data/part", true),
            ("data/part/x.csv", "data/part", false),
            ("data/a", "data/ab", false),
            ("x.csv", "data/a", false),
        ] {
            assert_eq!(folders.hold(path, own), held, "{path} beside {own}");
        }
        let top = "[datasets.top]\nref = \"x\"\npath = \".\"\n[datasets.iris]\nref = \"x\"\n";
        let top = DatasetFolderSet::of(&Manifest::parse(top).unwrap());
        assert!(!top.hold("data/a.csv", "data"));
        assert!(top.hold("data/iris/iris.csv", "data"));
        let sorted = in_order(
            [
                "data/ab/x",
                "data/a/x",
                "data/a",
                "data/a.b/x",
                "data/b/x",
                "data/a/inner/y",
                "data/a-b/x",
            ]
            .into_iter(),
        );
        assert_eq!(
            paths_below(&sorted, "data/a").collect::<Vec<_>>(),
            ["data/a/inner/y", "data/a/x"]
        );
        assert_eq!(paths_below(&sorted, "data/c").count(), 0);
    }

    /// the files below an archive's folder as each archive listed them before: every other
    /// dataset's folder built for it, and every file on the disk tried against each one.
    fn walked<'f>(
        layout: &Layout,
        manifest: &Manifest,
        files: &'f [String],
    ) -> Vec<(&'f str, String)> {
        let others: Vec<String> = manifest
            .datasets
            .keys()
            .map(|other| plan::folder(manifest, other))
            .filter(|other| *other != layout.folder)
            .map(|other| format!("{other}/"))
            .collect();
        let prefix = format!("{}/", layout.base);
        let mut below: Vec<(&str, String)> = files
            .iter()
            .map(String::as_str)
            .filter(|placed| !layout.whole.contains(*placed))
            .filter(|placed| {
                !others
                    .iter()
                    .any(|other| placed.starts_with(other.as_str()))
            })
            .filter_map(|placed| Some((placed, name::composed(placed.strip_prefix(&prefix)?))))
            .filter(|(_, inside)| {
                layout.decompressed.as_ref() == Some(inside)
                    || !inside.split('/').any(name::made_by_system)
            })
            .collect();
        below.sort_unstable();
        below
    }

    /// 300 generated data.toml texts with a disk's files: dataset folders nested, beside one
    /// another, sharing a start or a folder, at the data folder itself, and outputs in results/
    /// and in the data folder; files in them, deeper, named like a folder, and in none.
    fn generated() -> Vec<(String, Vec<String>)> {
        use std::fmt::Write as _;
        const PARTS: [&str; 8] = ["a", "ab", "a-b", "a.b", "b", "A", "a b", "a_b"];
        const NAMES: [&str; 5] = ["x.csv", ".DS_Store", "a", "x.zip", "caf\u{e9}.txt"];
        let mut seed: usize = 0x9e37_79b9;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        (0..300)
            .map(|_| {
                let data = ["data", "data/in", "d"][next(3)];
                let mut toml = format!("[project]\ndata_dir = \"{data}\"\n");
                let mut folders = vec![data.to_owned(), "results".to_owned()];
                for index in 0..=next(10) {
                    let path: Vec<&str> = (0..=next(3)).map(|_| PARTS[next(PARTS.len())]).collect();
                    let path = path.join("/");
                    let (line, folder) = match next(8) {
                        0 => ("path = \".\"".to_owned(), data.to_owned()),
                        1 => (
                            format!("at = \"results/{path}\""),
                            format!("results/{path}"),
                        ),
                        2 => (
                            format!("at = \"{data}/{path}/out\""),
                            format!("{data}/{path}/out"),
                        ),
                        _ => (format!("path = \"{path}\""), format!("{data}/{path}")),
                    };
                    let source = if line.starts_with("at") { "local" } else { "x" };
                    writeln!(toml, "[datasets.d{index}]\nref = \"{source}\"\n{line}").unwrap();
                    folders.push(folder);
                }
                let mut files: Vec<String> = (0..next(40))
                    .map(|_| {
                        let mut file = folders[next(folders.len())].clone();
                        if next(10) == 0 {
                            return file;
                        }
                        for _ in 0..next(3) {
                            file.push('/');
                            file.push_str(PARTS[next(PARTS.len())]);
                        }
                        file.push('/');
                        file.push_str(NAMES[next(NAMES.len())]);
                        file
                    })
                    .collect();
                files.sort_unstable();
                files.dedup();
                // a disk lists its files in no order
                files.reverse();
                (toml, files)
            })
            .collect()
    }

    /// the folders built once and the files searched in order list each archive's files as
    /// building every other dataset's folder for it and trying every file did.
    #[test]
    fn the_folders_built_once_list_each_archives_files_as_every_dataset_walked() {
        let mut compared = 0;
        for (table, files) in generated() {
            let manifest = Manifest::parse(&table).unwrap();
            let folders = DatasetFolderSet::of(&manifest);
            let sorted = in_order(files.iter().map(String::as_str));
            for (index, dataset) in manifest.datasets.keys().enumerate() {
                for archive in ["x.zip", "a/x.zip", "a b/x.tar.gz"] {
                    let folder = plan::folder(&manifest, dataset);
                    let layout = Layout {
                        dataset: dataset.clone(),
                        whole: files
                            .iter()
                            .filter(|file| file.len() % 3 == index % 3)
                            .cloned()
                            .collect(),
                        base: plan::unpacked_folder(&manifest, dataset, archive),
                        folder,
                        stem: crate::archive::stem(archive).to_owned(),
                        alone: index % 2 == 0,
                        decompressed: (index % 4 == 1).then(|| "x.csv".to_owned()),
                        filtered: false,
                    };
                    assert_eq!(
                        layout.listed(&sorted, &folders),
                        walked(&layout, &manifest, &files),
                        "{table}\n{dataset}, {archive}, {files:?}"
                    );
                    compared += 1;
                }
            }
        }
        assert!(compared > 1500, "{compared} archives compared");
    }

    /// a disk may list a name decomposed, as a mac can, while an archive's members are kept in NFC.
    #[test]
    fn a_file_listed_decomposed_is_guessed_by_its_composed_name() {
        assert_eq!(
            guessed(
                &layout("set", "set.zip", true),
                &["data/set/cafe\u{301}.csv"]
            )[0],
            ["caf\u{e9}.csv"]
        );
    }

    #[test]
    fn a_decompressed_file_is_guessed_only_by_its_own_name() {
        let mut layout = layout("reads", "reads.csv.gz", true);
        layout.decompressed = Some("reads.csv".to_owned());
        assert_eq!(
            guessed(&layout, &["data/reads/reads.csv", "data/reads/notes.txt"]),
            [vec!["reads.csv"]]
        );
        assert_eq!(
            guessed(&layout, &["data/reads/notes.txt"]),
            Vec::<Vec<String>>::new()
        );
        layout.decompressed = Some("Thumbs.db".to_owned());
        assert_eq!(
            guessed(&layout, &["data/reads/Thumbs.db"]),
            [vec!["Thumbs.db"]]
        );
    }

    #[test]
    fn a_folder_with_no_files_proves_nothing() {
        let none = Vec::<Vec<String>>::new();
        assert_eq!(
            guessed(&layout("empty", "empty.zip", true), &["data/x/y.txt"]),
            none
        );
        assert_eq!(guessed(&layout("empty", "empty.zip", false), &[]), none);
    }

    /// a list found in place answers only a lookup that pins its tree: never what the archive's
    /// own bytes unpack to, which a lookup by the archive alone, as get's, would trust.
    #[test]
    fn a_list_found_in_place_answers_no_lookup_by_the_archive_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        let members = vec![Member {
            path: "a.txt".to_owned(),
            size: 2,
            sha256: crate::digest::sha256(b"a\n"),
        }];
        let tree = tree_hash(&members);
        let archive = crate::digest::sha256(b"the archive's bytes");
        let found = PlacedArchives {
            unproven: Vec::new(),
            trees: HashMap::from([(
                tree,
                Found {
                    members: members.clone(),
                    label: "set/set.zip".to_owned(),
                    folder: "data/set".to_owned(),
                },
            )]),
        };
        let style = crate::output::Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let output = Output::new(style, crate::output::Level::Quiet, true, false);
        found.record(&cache, &output).unwrap();
        assert_eq!(cache.pinned_tree(&archive, &tree), Some(members));
        assert_eq!(cache.unpacked(&archive, None), None);
        assert_eq!(cache.unpacked_tree(&archive), None);
    }
}
