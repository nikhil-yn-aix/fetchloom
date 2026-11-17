//! archives whose files a project's data folder already holds where placing puts them, as when a
//! project moves to another machine with its data folder but not its cache. an archive's list of
//! files is rebuilt from those files, and stands for the archive only when it hashes to the tree
//! data.lock pins as its `unpacks_to`: then the files are taken as placed, and the archive itself
//! is fetched only once a file of it is needed again.
//!
//! the files of a data folder anyone could have filled prove only that tree, never what an
//! archive's bytes unpack to, so a list found here is recorded by the tree's own key
//! ([`tree_key`]), which only a lookup that pins the tree finds.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::disk::{Survey, hash_plain};
use crate::cache::{Cache, Member, tree_hash, tree_key};
use crate::digest::{Block, Sha256};
use crate::error::Error;
use crate::output::Output;
use crate::project::lock::Lock;
use crate::project::lock::archives::{self, Archive, Pinned};
use crate::project::manifest::Manifest;
use crate::project::native;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::{name, plan, units};

/// the lists of files found in place, by the tree each hashes to.
#[derive(Default)]
pub(crate) struct InPlace {
    trees: HashMap<Sha256, Found>,
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

impl InPlace {
    /// the lists of files of the archives of `unknown` whose files `survey` finds where placing
    /// puts them, each checked by hashing those files against the tree data.lock pins.
    pub(crate) fn find(
        root: &Path,
        (manifest, lock): (&Manifest, &Lock),
        survey: &Survey,
        unknown: &[(String, Archive<'_>)],
        (threads, stop): (Threads, &Stop),
    ) -> Result<InPlace, Error> {
        let mut found = InPlace::default();
        let mut hashed: HashMap<String, Option<Sha256>> = HashMap::new();
        let temporary: HashSet<&String> = survey.leftovers().collect();
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
            let files = survey
                .disk
                .keys()
                .filter(|placed| !temporary.contains(placed));
            let guesses = guesses(&layout, &layout.below(files.map(String::as_str)));
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
            let proven = guesses.iter().find_map(|guess| {
                let members: Vec<Member> = guess.iter().map(member).collect::<Option<_>>()?;
                (tree_hash(&members) == tree).then_some(members)
            });
            if let Some(members) = proven {
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
        }
        Ok(found)
    }

    /// whether no list was found.
    pub(crate) fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }

    /// the files `archive` unpacks to, as found in place for the tree data.lock pins it to.
    pub(crate) fn members(&self, archive: &Archive<'_>) -> Option<Vec<Member>> {
        let tree = archive.file.unpacks_to.as_ref()?;
        self.trees.get(tree).map(|found| found.members.clone())
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

/// where placing put the files of an archive with no members table, as data.toml and data.lock
/// lay its dataset out.
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
    /// the paths of the dataset's files pinned whole, none of them the archive's.
    whole: HashSet<String>,
    /// the folders of the other datasets, each ending in `/`, whose files are none of the
    /// archive's, as when its dataset is the data folder itself.
    others: Vec<String>,
}

impl Layout {
    /// where `archive`, a file of `dataset`, puts its files: none when a members table places only
    /// some of them, which no tree can prove.
    fn of(
        (manifest, lock): (&Manifest, &Lock),
        dataset: &str,
        archive: &Archive<'_>,
    ) -> Option<Layout> {
        let entry = manifest.datasets.get(dataset)?;
        if entry.source.members.get(&archive.file.path).is_some() {
            return None;
        }
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
        let (beside, name) = path.rsplit_once('/').unwrap_or(("", path));
        let base = if beside.is_empty() {
            folder.clone()
        } else {
            name::joined(&folder, beside)
        };
        let others = manifest
            .datasets
            .keys()
            .map(|other| plan::folder(manifest, other))
            .filter(|other| *other != folder)
            .map(|other| format!("{other}/"))
            .collect();
        Some(Layout {
            dataset: dataset.to_owned(),
            folder,
            base,
            stem: crate::archive::stem(name).to_owned(),
            alone: pinned.len() == 1,
            decompressed: archive.decompressed(),
            whole,
            others,
        })
    }

    /// each file of `files`, paths in the project, below the folder the archive's files land
    /// below and pinned by none of the dataset's whole files, with its path there, in order.
    fn below<'f>(&self, files: impl Iterator<Item = &'f str>) -> Vec<(&'f str, &'f str)> {
        let prefix = format!("{}/", self.base);
        let mut below: Vec<(&str, &str)> = files
            .filter(|placed| !self.whole.contains(*placed))
            .filter(|placed| {
                !self
                    .others
                    .iter()
                    .any(|other| placed.starts_with(other.as_str()))
            })
            .filter_map(|placed| Some((placed, placed.strip_prefix(&prefix)?)))
            .collect();
        below.sort_unstable();
        below
    }
}

/// the ways `below`, the files below an archive's folder with their paths there, may be the
/// archive's own as placing put them, the likeliest first. a dataset that is one archive loses the
/// top folder its files share, which only the archive's or the dataset's name can give back.
fn guesses(layout: &Layout, below: &[(&str, &str)]) -> Vec<Guess> {
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
    if let Some(to) = &layout.decompressed {
        if let Some((placed, _)) = below.iter().find(|(_, inside)| inside == to) {
            guesses.push(vec![((*placed).to_owned(), to.clone())]);
        }
        return guesses;
    }
    // the .gitignore fetchloom keeps a dataset's folder out of git with, unless the archive holds it
    let marker = format!("{}/.gitignore", layout.folder);
    let unmarked: Vec<(&str, &str)> = below
        .iter()
        .copied()
        .filter(|(placed, _)| *placed != marker)
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
    // a folder with no files proves nothing, though an archive of none hashes to the empty tree
    guesses.retain(|guess| !guess.is_empty());
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
            whole: HashSet::new(),
            others: Vec::new(),
        }
    }

    /// the paths in the archive each guess gives the files of `placed`, paths in the project.
    fn guessed(layout: &Layout, placed: &[&str]) -> Vec<Vec<String>> {
        let below = layout.below(placed.iter().copied());
        guesses(layout, &below)
            .into_iter()
            .map(|guess| guess.into_iter().map(|(_, path)| path).collect())
            .collect()
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

    #[test]
    fn a_dataset_in_the_data_folder_itself_leaves_out_the_other_datasets_files() {
        let mut layout = layout("all", "all.zip", true);
        layout.folder = "data".to_owned();
        layout.base = "data".to_owned();
        layout.others = vec!["data/iris/".to_owned()];
        assert_eq!(
            guessed(&layout, &["data/a.csv", "data/iris/iris.csv"])[0],
            ["a.csv"]
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
        let found = InPlace {
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
