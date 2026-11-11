//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use super::Scope;
use super::place::local;
use crate::cache::{self, Cache};
use crate::digest::Sha256;
use crate::error::Error;
use crate::name;
use crate::place::{self, Method};
use crate::plan::Target;
use crate::project::Project;
use crate::project::state::{Look, State, nanos};
use crate::threads::Threads;

/// the data folder as fetchloom recorded placing it and as it is now.
pub(crate) struct Survey {
    pub(super) threads: Threads,
    /// the scan's start, so marks written while placing files still put them in doubt.
    pub(crate) started: u64,
    pub(crate) state: State,
    /// every file under the data folder and how it looks, by its path in the project.
    pub(crate) disk: HashMap<String, Look>,
    /// the listing's spelling of an alias, within the current data folder, for removals.
    pub(crate) listed: HashMap<String, String>,
    /// hardlinks whose bytes hashing found changed while they still share them, with the cache's
    /// copy or other projects, or are a copy a change mark records: edited through some link to
    /// that copy. sync puts them back with or without --force.
    pub(crate) elsewhere: HashSet<String>,
    /// hardlinks a mark put in doubt whose bytes hashing found intact, which sync keeps, recording
    /// the scan's start.
    pub(crate) vouched: HashSet<String>,
    /// files that still look as recorded but hold other bytes, which hashing found among those
    /// as new as the state itself.
    pub(crate) edited: HashSet<String>,
    /// copies and reflinks, and links left with no name but their own, that still look as placed
    /// but hold other bytes: a change that kept the size and time, as a failing disk or a tool
    /// that restores times makes. hashing every placed file finds them, as `--verify` asks, and a
    /// change mark puts them in doubt too. sync puts them back with or without --force.
    pub(crate) damaged: HashSet<String>,
    /// files the disk lets their owner write. a link to the cache's copy among them leaves the
    /// copy every project shares open to an edit.
    pub(crate) writable: HashSet<String>,
    /// whether hashing found files as new as the state unchanged, so a state written now settles
    /// them for later runs, or a record moved to another spelling of its path.
    pub(crate) needs_writing: bool,
    /// files under the hidden names placement stages files in, by their paths in the project.
    temporaries: Vec<String>,
    /// what else the data folder holds, by path in the project: folders, and links and special
    /// files, which the survey never follows or reads.
    pub(crate) entries: HashMap<String, Entry>,
    /// the data folder's path in the project, below which entries can stand in a file's way.
    data_dir: String,
    /// the files the state records outside the data folder that are still there, by their paths
    /// in the project, in order: what fetchloom placed before `data_dir` changed. a sync never
    /// deletes them.
    outside: Vec<String>,
}

/// the files fetchloom placed before `data_dir` changed that are still outside the data folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LeftBehind {
    /// the folder they are in, when it can be named without naming the data folder too.
    pub(crate) folder: Option<String>,
    /// their paths in the project, in order.
    pub(crate) files: Vec<String>,
}

/// a folder that held files fetchloom placed which no dataset needs now, and that holds none a
/// dataset needs, with what else is in it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Retired {
    /// its path in the project.
    pub(crate) folder: String,
    /// the files, links and special files in it that fetchloom did not place, by path in the
    /// project, in order.
    pub(crate) unplaced: Vec<String>,
}

/// what stands at a path in the data folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    File,
    Folder,
    Link,
    /// a device, a socket or a named pipe.
    Special,
}

/// an entry that keeps a file data.lock places from its path: one that is not a folder where
/// the file needs a folder on its way, or one that is not a plain file at the file's own path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blocker {
    /// the entry's path in the project.
    pub(crate) path: String,
    pub(crate) found: Entry,
    /// whether data.lock needs a folder at that path, rather than a file.
    pub(crate) folder: bool,
    /// whether `sync --force` may remove it. a folder holding anything stays, since its files
    /// may be someone's work.
    pub(crate) clearable: bool,
    /// the dataset the entry is a file of, or whose files a folder in the way holds, when the
    /// run leaves that dataset alone, as a sync of other datasets does. such an entry stays
    /// even with `--force`.
    pub(crate) dataset: Option<String>,
}

impl Survey {
    /// reads the state, lists the data folder, and hashes the files the state is too new to
    /// vouch for.
    pub(crate) fn read(project: &Project, data_dir: &str, threads: Threads) -> Survey {
        let started = nanos(SystemTime::now());
        let (state, written) = project.state();
        let Scanned {
            found: disk,
            temporaries,
            entries,
            writable,
        } = scan(&project.root, data_dir, threads);
        let mut survey = Survey {
            threads,
            started,
            state,
            disk,
            listed: HashMap::new(),
            elsewhere: HashSet::new(),
            vouched: HashSet::new(),
            edited: HashSet::new(),
            damaged: HashSet::new(),
            writable,
            needs_writing: false,
            temporaries,
            entries,
            data_dir: data_dir.to_owned(),
            outside: Vec::new(),
        };
        let other_device = !survey.state.files.is_empty()
            && apart(&project.root.join(data_dir), &project.own_folder());
        survey.hash_racy(&project.root, written, other_device);
        // after hash_racy, so a record found under another spelling is looked through too
        survey.look_through_links(&project.root);
        survey.outside = outside(&project.root, data_dir, &survey.state, &survey.disk);
        survey
    }

    /// the files among `going`, the records a run drops, that fetchloom placed before `data_dir`
    /// changed and that are still outside the data folder, with the folder they are in: the part
    /// of their paths before the path a file of `wanted` has below the data folder now, as when
    /// only `data_dir` changed, else the folder they share. none when no file was left behind.
    pub(crate) fn left_behind(
        &self,
        wanted: &HashSet<&str>,
        going: &HashSet<&str>,
    ) -> Option<LeftBehind> {
        let files: Vec<String> = self
            .outside
            .iter()
            .filter(|path| going.contains(path.as_str()))
            .cloned()
            .collect();
        let first = files.first()?;
        let below: HashSet<&str> = wanted
            .iter()
            .filter_map(|path| path.strip_prefix(self.data_dir.as_str())?.strip_prefix('/'))
            .collect();
        let before = |path: &'_ str| {
            path.match_indices('/')
                .map(|(end, _)| end)
                .find(|end| below.contains(&path[end + 1..]))
                .map(|end| path[..end].to_owned())
        };
        let folder = before(first)
            .filter(|folder| {
                files
                    .iter()
                    .all(|path| before(path).as_ref() == Some(folder))
            })
            .unwrap_or_else(|| shared_folder(&files));
        // naming the data folder's own parent would ask the person to delete the data folder too
        let holds_data = |folder: &str| {
            let (data, folder) = (name::fold(&self.data_dir), name::fold(folder));
            data == folder || data.starts_with(&format!("{folder}/"))
        };
        Some(LeftBehind {
            folder: Some(folder).filter(|folder| !folder.is_empty() && !holds_data(folder)),
            files,
        })
    }

    /// the folders that held files fetchloom placed, `going`, which no file of `wanted` needs now,
    /// each the highest below the data folder that holds none of `wanted`, with what else they
    /// hold, as the survey listed it before anything was removed. a folder holding nothing else
    /// goes with its last file, so it is not among them.
    pub(crate) fn retired(&self, wanted: &HashSet<&str>, going: &HashSet<&str>) -> Vec<Retired> {
        if going.is_empty() {
            return Vec::new();
        }
        let mut sorted: Vec<&str> = wanted.iter().copied().collect();
        sorted.sort_unstable();
        let holds_wanted = |folder: &str| {
            let below = format!("{folder}/");
            let at = sorted.partition_point(|path| *path < below.as_str());
            sorted.get(at).is_some_and(|path| path.starts_with(&below))
        };
        let start = self.data_dir.len() + 1;
        let mut folders = HashSet::new();
        for path in going {
            let Some(below) = path
                .strip_prefix(self.data_dir.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
            else {
                continue;
            };
            if let Some(folder) = below
                .match_indices('/')
                .map(|(end, _)| &path[..start + end])
                .find(|folder| !holds_wanted(folder))
            {
                folders.insert(folder.to_owned());
            }
        }
        if folders.is_empty() {
            return Vec::new();
        }
        let mut retired: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let unplaced = self
            .disk
            .keys()
            .filter(|path| !self.state.files.contains_key(*path))
            .filter(|path| !place::is_copy_temporary(path.rsplit('/').next().unwrap_or(path)))
            .chain(
                self.entries
                    .iter()
                    .filter(|(_, entry)| **entry != Entry::Folder)
                    .map(|(path, _)| path),
            );
        for path in unplaced {
            let folder = path
                .match_indices('/')
                .map(|(end, _)| &path[..end])
                .find(|folder| folders.contains(*folder));
            if let Some(folder) = folder {
                retired.entry(folder).or_default().push(path.clone());
            }
        }
        retired
            .into_iter()
            .map(|(folder, mut unplaced)| {
                unplaced.sort();
                Retired {
                    folder: folder.to_owned(),
                    unplaced,
                }
            })
            .collect()
    }

    /// on windows, a folder's listing keeps a hardlinked file's size, time and read-only flag as
    /// they were when this link last opened it, so an edit made through another project's link
    /// does not show. opening the file reads them as they are, for the files placed as hardlinks
    /// only.
    #[cfg(windows)]
    fn look_through_links(&mut self, root: &Path) {
        for (path, placed) in &self.state.files {
            if placed.method == Method::Hardlink
                && let Some(seen) = self.disk.get_mut(path)
                && let Ok(metadata) = fs::metadata(root.join(path))
            {
                *seen = Look::of(&metadata);
                if metadata.permissions().readonly() {
                    self.writable.remove(path);
                } else {
                    self.writable.insert(path.clone());
                }
            }
        }
    }

    #[cfg(not(windows))]
    #[expect(
        clippy::unused_self,
        reason = "unix listings report a hardlinked file's size and time as they are"
    )]
    fn look_through_links(&mut self, _root: &Path) {}

    /// reconciles missing recorded names, then hashes files whose recorded time is not older than the
    /// state's own, `written`. a coarse clock, such as FAT's two seconds, gives a file edited in
    /// the tick it was placed the size and time recorded for it, and both times come from that
    /// clock. git treats its racily clean index entries the same way. a data folder on an
    /// `other_device` than the state's may keep times to a coarser step than the state's, so
    /// there the state vouches only for files older than it by the coarsest step.
    fn hash_racy(&mut self, root: &Path, written: u64, other_device: bool) {
        let written = vouched_before(written, other_device);
        let mut racy = Vec::new();
        let mut missing = Vec::new();
        for (path, placed) in &self.state.files {
            match self.disk.get(path) {
                None => missing.push(path),
                Some(seen) if placed.look.modified >= written && *seen == placed.look => {
                    racy.push((path, &placed.sha256));
                }
                Some(_) => {}
            }
        }
        for (path, listed) in aliases(root, &self.disk, &missing) {
            if self.state.files.contains_key(&listed) {
                continue;
            }
            let Some(look) = self.disk.remove(&listed) else {
                continue;
            };
            self.disk.insert(path.clone(), look);
            self.listed.insert(path.clone(), listed);
            if let Some(placed) = self.state.files.get(path)
                && placed.look.modified >= written
                && look == placed.look
            {
                racy.push((path, &placed.sha256));
            }
        }
        if racy.is_empty() {
            return;
        }
        let _span = tracing::debug_span!("hash_racy", files = racy.len()).entered();
        // a time equal to the state's own cannot tell an edit in the same clock tick apart
        tracing::debug!(
            files = racy.len(),
            "hashing files placed too recently to trust their time"
        );
        let unchanged = place::run_all(&racy, self.threads, |(path, sha256)| {
            Sha256::of_file(&root.join(path)).is_ok_and(|digest| digest == **sha256)
        });
        for ((path, _), unchanged) in racy.into_iter().zip(unchanged) {
            if unchanged {
                self.needs_writing = true;
            } else {
                tracing::debug!(%path, "hashing found an edit");
                self.edited.insert(path.clone());
            }
        }
    }

    /// carries a placed record to a wanted alias before decisions or removals can treat the
    /// two spellings as separate files. edits and doubts still belong to that same file. a file
    /// nothing recorded is carried only when its listed name differs in unicode composition
    /// alone, as hfs+ lists every name decomposed, so it is the target's own file and never
    /// both missing and extra.
    pub(crate) fn retarget(&mut self, root: &Path, targets: &[Target]) {
        // an alias of another composition needs a name beyond ascii on one side
        if self.state.files.is_empty()
            && self.disk.keys().all(|path| path.is_ascii())
            && targets.iter().all(|target| target.path.is_ascii())
        {
            return;
        }
        let missing: Vec<&String> = targets
            .iter()
            .filter(|target| !self.disk.contains_key(&target.path))
            .map(|target| &target.path)
            .collect();
        for (path, previous) in aliases(root, &self.disk, &missing) {
            if self.state.files.contains_key(path) {
                continue;
            }
            let record = self.state.files.get(&previous).cloned();
            if record.is_none() && !name::same_composed(path, &previous) {
                continue;
            }
            let Some(look) = self.disk.remove(&previous) else {
                continue;
            };
            self.disk.insert(path.clone(), look);
            if self.writable.remove(&previous) {
                self.writable.insert(path.clone());
            }
            if let Some(record) = record {
                self.state.files.remove(&previous);
                self.state.files.insert(path.clone(), record);
                for paths in [
                    &mut self.edited,
                    &mut self.elsewhere,
                    &mut self.vouched,
                    &mut self.damaged,
                ] {
                    if paths.remove(&previous) {
                        paths.insert(path.clone());
                    }
                }
                self.needs_writing = true;
            }
            let listed = self.listed.remove(&previous).unwrap_or(previous);
            self.listed.insert(path.clone(), listed);
        }
    }

    /// gives each file hashing found edited a recorded size no file has. a state written now is
    /// newer than the edit and would vouch for the size and time it shares with the placed file.
    /// git smudges its racily clean index entries the same way.
    pub(crate) fn smudge_edited(&mut self) {
        for path in &self.edited {
            if let Some(placed) = self.state.files.get_mut(path) {
                placed.look.size = u64::MAX;
            }
        }
    }

    /// what keeps a file data.lock places at `path` from being placed, when no plain file stands
    /// there: a file, link or special file where a folder on its way belongs, or a folder, link
    /// or special file at the path itself. none when nothing does, as when the path is free.
    pub(crate) fn blocker(&self, path: &str) -> Option<Blocker> {
        let start = self.data_dir.len() + 1;
        let below = path
            .strip_prefix(self.data_dir.as_str())?
            .strip_prefix('/')?;
        for (end, _) in below.match_indices('/') {
            let folder = &path[..start + end];
            let found = if self.disk.contains_key(folder) {
                Entry::File
            } else {
                match self.entries.get(folder) {
                    Some(Entry::Folder) => continue,
                    Some(entry) => *entry,
                    None => return None,
                }
            };
            return Some(Blocker {
                path: folder.to_owned(),
                found,
                folder: true,
                clearable: true,
                dataset: None,
            });
        }
        let found = *self.entries.get(path)?;
        let clearable = found != Entry::Folder || self.holds_nothing_under(path);
        Some(Blocker {
            path: path.to_owned(),
            found,
            folder: false,
            clearable,
            dataset: None,
        })
    }

    /// whether removing the files in `going`, with the folders that leaves empty, takes
    /// `blocker` out of the way: a file in the way that is one of them, or a folder that holds
    /// nothing else, since a folder goes with the last file removed from it.
    pub(crate) fn frees(&self, blocker: &Blocker, going: &HashSet<String>) -> bool {
        match blocker.found {
            Entry::File => going.contains(&blocker.path),
            Entry::Folder => {
                let prefix = format!("{}/", blocker.path);
                let mut files = self
                    .disk
                    .keys()
                    .filter(|path| path.starts_with(&prefix))
                    .peekable();
                files.peek().is_some()
                    && files.all(|path| going.contains(path))
                    && self
                        .entries
                        .iter()
                        .filter(|(path, _)| path.starts_with(&prefix))
                        .all(|(path, entry)| {
                            let below = format!("{path}/");
                            *entry == Entry::Folder
                                && going.iter().any(|file| file.starts_with(&below))
                        })
            }
            Entry::Link | Entry::Special => false,
        }
    }

    /// whether the folder at `path` was empty when the survey listed it.
    fn holds_nothing_under(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        !self
            .disk
            .keys()
            .chain(self.entries.keys())
            .any(|inner| inner.starts_with(&prefix))
    }

    /// the files an interrupted placement left under the hidden names it stages files in, which
    /// no record claims. only placement makes such names.
    pub(crate) fn leftovers(&self) -> impl Iterator<Item = &String> {
        self.temporaries
            .iter()
            .filter(|path| !self.state.files.contains_key(*path))
    }

    /// files placed as hardlinks that only hashing can tell intact from changed, with the cache's
    /// copy of their bytes in doubt.
    ///
    /// one kind still shows the size and time it was placed with, while its object was marked as
    /// changed after fetchloom last knew the link to hold its bytes, so the mark is the only sign
    /// of an edit made through another project. the link may also name an older copy of the
    /// object than the one that changed. a mark within a coarse clock's step of that time is in
    /// doubt too, as its time may be cut down. a link that is the very file a repair put in place
    /// is that copy, which the cache trusts; a shared time alone proves nothing, since two files
    /// can carry the same coarse time.
    ///
    /// the other kind changed, and is still the cache's own copy, or the very copy a mark records
    /// the cache found changed. an edit through any link to that copy, in this project or
    /// another, changed what every project sharing it reads, so it is damage to the cached copy
    /// rather than this project's own file. a link replaced by a file of the project's own is
    /// another file, and stays an edit.
    pub(crate) fn doubted(&self, root: &Path, cache: &Cache) -> Result<Vec<String>, Error> {
        let marks = cache.marks()?;
        Ok(self
            .state
            .files
            .iter()
            .filter(|(path, placed)| {
                if placed.method != Method::Hardlink {
                    return false;
                }
                let Some(seen) = self.disk.get(*path) else {
                    return false;
                };
                let mark = marks.get(&placed.sha256);
                let is_object = || {
                    same_file::is_same_file(
                        root.join(path.as_str()),
                        cache.object_path(&placed.sha256),
                    )
                    .unwrap_or(false)
                };
                let changed = *seen != placed.look
                    || (!self.edited.is_empty() && self.edited.contains(*path));
                if !changed {
                    return mark.is_some_and(|mark| {
                        cache::may_follow(mark.found, placed.look.modified.max(placed.checked))
                            && !(mark.replacement == Some(placed.look.modified) && is_object())
                    });
                }
                is_object()
                    || mark.is_some_and(|mark| {
                        cache::Identity::of(&root.join(path.as_str()))
                            .is_ok_and(|copy| mark.copies.contains(&copy))
                    })
            })
            .map(|(path, _)| path.clone())
            .collect())
    }

    /// hashes the files [`Survey::doubted`] names, separating changed links from intact ones.
    /// an unreadable file stops the check.
    pub(crate) fn check_doubted(&mut self, root: &Path, cache: &Cache) -> Result<(), Error> {
        let doubted = self.doubted(root, cache)?;
        if doubted.is_empty() {
            return Ok(());
        }
        let _span = tracing::debug_span!("hash_doubted", files = doubted.len()).entered();
        tracing::debug!(
            files = doubted.len(),
            "hashing hardlinks whose cached object was marked changed"
        );
        let files = &self.state.files;
        let intact = place::run_all(&doubted, self.threads, |path| {
            Sha256::of_file(&root.join(path)).map(|digest| digest == files[path].sha256)
        });
        let marks = cache.marks()?;
        for (path, intact) in doubted.into_iter().zip(intact) {
            match intact {
                Ok(true) => {
                    self.vouched.insert(path);
                }
                Ok(false) => {
                    let file = root.join(&path);
                    let marked = marks.get(&files[&path].sha256).is_some_and(|mark| {
                        cache::Identity::of(&file).is_ok_and(|copy| mark.copies.contains(&copy))
                    });
                    if marked || place::has_other_names(&file) {
                        self.elsewhere.insert(path);
                    } else {
                        self.damaged.insert(path);
                    }
                }
                Err(err) => {
                    return Err(Error::io(
                        format_args!("could not read {path} to check it"),
                        &err,
                    ));
                }
            }
        }
        Ok(())
    }

    /// hashes every placed file of a dataset in `scope` that still looks as placed and that
    /// nothing here hashed or put in doubt already, so changed bytes that kept their size and
    /// time show too, as `--verify` asks. a hardlink found changed is the cache's copy, so it
    /// joins `elsewhere`; any other file joins `damaged`. a file that cannot be read stops the
    /// check. returns the files it found changed, which a plain sync cannot see.
    pub(crate) fn verify(&mut self, root: &Path, scope: &Scope) -> Result<HashSet<String>, Error> {
        let files: Vec<(&String, &crate::project::state::Placed)> = self
            .state
            .files
            .iter()
            .filter(|(path, placed)| {
                scope.includes(&placed.dataset)
                    && self.disk.get(*path) == Some(&placed.look)
                    && !self.edited.contains(*path)
                    && !self.elsewhere.contains(*path)
                    && !self.vouched.contains(*path)
            })
            .collect();
        if files.is_empty() {
            return Ok(HashSet::new());
        }
        let _span = tracing::debug_span!("verify", files = files.len()).entered();
        let intact = place::run_all(&files, self.threads, |(path, placed)| {
            Sha256::of_file(&root.join(path.as_str())).map(|digest| digest == placed.sha256)
        });
        let mut changed = Vec::new();
        for ((path, placed), intact) in files.into_iter().zip(intact) {
            match intact {
                Ok(true) => {}
                Ok(false) => changed.push((path.clone(), placed.method == Method::Hardlink)),
                Err(err) => {
                    return Err(Error::io(
                        format_args!("could not read {path} to check it"),
                        &err,
                    ));
                }
            }
        }
        // a link changed while it still shares its bytes changed every name it has; one left
        // with a name of its own is this project's file alone
        let mut found = HashSet::new();
        for (path, linked) in changed {
            tracing::debug!(%path, "hashing found bytes changed under an unchanged look");
            found.insert(path.clone());
            if linked && place::has_other_names(&root.join(&path)) {
                self.elsewhere.insert(path);
            } else {
                self.damaged.insert(path);
            }
        }
        Ok(found)
    }
}

/// finds only unambiguous aliases that resolve to the listed file. an ordinary scan never
/// folds names or reads files again when every recorded spelling was listed.
fn aliases<'a>(
    root: &Path,
    disk: &HashMap<String, Look>,
    missing: &[&'a String],
) -> Vec<(&'a String, String)> {
    if missing.is_empty() || disk.is_empty() {
        return Vec::new();
    }
    let _span = tracing::debug_span!("aliases", files = missing.len()).entered();
    let mut folded: HashMap<String, Option<&String>> = HashMap::new();
    for path in disk.keys() {
        folded
            .entry(name::fold(path))
            .and_modify(|listed| *listed = None)
            .or_insert(Some(path));
    }
    let mut listings = Listings::new();
    missing
        .iter()
        .filter_map(|path| {
            let listed = folded.get(&name::fold(path))?.as_ref()?;
            let look = disk.get(*listed)?;
            same_entry(root, path, listed, *look, &mut listings).then(|| (*path, (*listed).clone()))
        })
        .collect()
}

/// the names in each folder read so far, by folder.
type Listings = HashMap<std::path::PathBuf, HashSet<std::ffi::OsString>>;

fn same_entry(root: &Path, path: &str, listed: &str, look: Look, listings: &mut Listings) -> bool {
    let Ok(metadata) = fs::symlink_metadata(root.join(path)) else {
        return false;
    };
    if !metadata.is_file() || Look::of(&metadata) != look {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let _ = listings;
        let Ok(other) = fs::symlink_metadata(root.join(listed)) else {
            return false;
        };
        other.is_file()
            && Look::of(&other) == look
            && metadata.dev() == other.dev()
            && metadata.ino() == other.ino()
    }
    #[cfg(not(unix))]
    {
        // without a file id, two spellings that each appear in their folders are two files
        spelled_otherwise(root, path, listings) || spelled_otherwise(root, listed, listings)
    }
}

/// whether a folder on the way to `path`, or its file, lacks that exact name in its listing, so
/// the filesystem matched the name to another spelling.
#[cfg(any(not(unix), test))]
fn spelled_otherwise(root: &Path, path: &str, listings: &mut Listings) -> bool {
    let mut folder = root.to_path_buf();
    for part in path.split('/') {
        let names = listings.entry(folder.clone()).or_insert_with(|| {
            fs::read_dir(&folder)
                .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
                .unwrap_or_default()
        });
        if !names.contains(std::ffi::OsStr::new(part)) {
            return true;
        }
        folder.push(part);
    }
    false
}

/// the step of the coarsest clock a data folder may sit on: fat cuts a file's time down to two
/// seconds.
const COARSE: u64 = 2_000_000_000;

/// the time before which the state, written at `written`, vouches for a file that still looks as
/// it records: a file as new as the state may hold an edit made in the same tick of a coarse
/// clock. a data folder on an `other_device` than the state's may keep times to a coarser step,
/// so there the state vouches only for files older than it by the coarsest step.
pub(crate) fn vouched_before(written: u64, other_device: bool) -> u64 {
    if other_device {
        written.saturating_sub(COARSE)
    } else {
        written
    }
}

/// the files `state` records outside the data folder `data_dir`, as fetchloom placed them before
/// `data_dir` changed, that are still plain files in real folders under `root`, in order. a path
/// that names a file `disk` lists in the data folder under another spelling, as a disk that
/// ignores case reads it, is that file.
fn outside(
    root: &Path,
    data_dir: &str,
    state: &State,
    disk: &HashMap<String, Look>,
) -> Vec<String> {
    let inside = format!("{data_dir}/");
    let mut real = HashMap::new();
    state
        .files
        .keys()
        .filter(|path| !path.starts_with(&inside) && !disk.contains_key(*path))
        .filter(|path| name::escapes(path).is_none())
        .filter(|path| plain_in_real_folders(root, path, &mut real))
        .filter(|path| !same_inside(root, path, data_dir))
        .cloned()
        .collect()
}

/// the deepest folder holding every one of `paths`, by path in the project. empty when that is
/// the project itself.
fn shared_folder(paths: &[String]) -> String {
    let mut shared: Option<Vec<&str>> = None;
    for path in paths {
        let mut folder: Vec<&str> = path.split('/').collect();
        folder.pop();
        shared = Some(match shared {
            None => folder,
            Some(mut shared) => {
                let same = shared
                    .iter()
                    .zip(&folder)
                    .take_while(|(a, b)| a == b)
                    .count();
                shared.truncate(same);
                shared
            }
        });
    }
    shared.unwrap_or_default().join("/")
}

/// whether `path` below `root` is a plain file whose folders are real, never links. `real` keeps
/// what each folder was found to be.
fn plain_in_real_folders(root: &Path, path: &str, real: &mut HashMap<String, bool>) -> bool {
    let folders_real = path.match_indices('/').all(|(end, _)| {
        let folder = &path[..end];
        *real.entry(folder.to_owned()).or_insert_with(|| {
            fs::symlink_metadata(root.join(folder))
                .is_ok_and(|found| found.is_dir() && !found.file_type().is_symlink())
        })
    });
    folders_real
        && fs::symlink_metadata(root.join(path)).is_ok_and(|found| crate::volume::is_plain(&found))
}

/// whether `path`, outside the data folder `data_dir` as written, is the same file as the one at
/// its place in the data folder, as on a disk that ignores case `data/x` and `Data/x` are.
fn same_inside(root: &Path, path: &str, data_dir: &str) -> bool {
    let parts = data_dir.split('/').count();
    let mut split = path.splitn(parts + 1, '/');
    let outer: Vec<&str> = split.by_ref().take(parts).collect();
    let Some(rest) = split.next() else {
        return false;
    };
    name::fold(&outer.join("/")) == name::fold(data_dir)
        && same_file::is_same_file(root.join(path), root.join(data_dir).join(rest)).unwrap_or(false)
}

/// whether `a` and `b` sit on different devices, whose filesystems may keep times to different
/// steps. folders that cannot be read count as apart.
pub(crate) fn apart(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() != b.dev(),
            _ => true,
        }
    }
    #[cfg(windows)]
    {
        let volume = |path: &Path| {
            winapi_util::Handle::from_path_any(path)
                .and_then(|handle| winapi_util::file::information(&handle))
                .map(|information| information.volume_serial_number())
        };
        match (volume(a), volume(b)) {
            (Ok(a), Ok(b)) => a != b,
            _ => true,
        }
    }
}

/// what a scan of the data folder found.
#[derive(Debug, Default, PartialEq)]
struct Scanned {
    /// every file and how it looks, by its path in the project.
    found: HashMap<String, Look>,
    /// the paths of the files under the hidden names placement stages files in.
    temporaries: Vec<String>,
    /// every other entry: folders, links and special files.
    entries: HashMap<String, Entry>,
    /// the files the disk lets their owner write.
    writable: HashSet<String>,
}

/// every file under the data folder, how it looks and whether it is writable, by its path in the
/// project, the paths of those under the hidden names placement stages files in, and the folders,
/// links and special files beside them. links are neither followed nor touched.
fn scan(root: &Path, data_dir: &str, threads: Threads) -> Scanned {
    let span = tracing::debug_span!("scan", files = tracing::field::Empty).entered();
    let mut scanned = Scanned::default();
    let mut pending = Vec::new();
    let mut last_parent = None;
    let mut parents = 0;
    let mut entries = HashMap::new();
    visit_files(
        root,
        data_dir,
        &mut entries,
        |entry, path, temporary, parent| {
            if last_parent != Some(parent) {
                // DirEntry retains its parent handle on unix; bound distinct parents too.
                if parents == 8 {
                    scan_pending(&mut pending, threads, &mut scanned);
                    parents = 0;
                }
                parents += 1;
                last_parent = Some(parent);
            }
            pending.push((entry, path, temporary));
            if pending.len() == 4096 {
                scan_pending(&mut pending, threads, &mut scanned);
                parents = 0;
                last_parent = None;
            }
        },
    );
    scan_pending(&mut pending, threads, &mut scanned);
    scanned.entries = entries;
    span.record("files", scanned.found.len());
    scanned
}

fn scan_pending(
    pending: &mut Vec<(fs::DirEntry, String, bool)>,
    threads: Threads,
    scanned: &mut Scanned,
) {
    let metadata = |(entry, _, _): &(fs::DirEntry, String, bool)| {
        entry
            .metadata()
            .ok()
            .map(|found| (Look::of(&found), !found.permissions().readonly()))
    };
    let looks = if pending.len() < 512 {
        pending.iter().map(metadata).collect()
    } else {
        place::run_all(pending, threads, metadata)
    };
    for ((_, path, temporary), look) in pending.drain(..).zip(looks) {
        if let Some((look, writable)) = look {
            if temporary {
                scanned.temporaries.push(path.clone());
            }
            if writable {
                scanned.writable.insert(path.clone());
            }
            scanned.found.insert(path, look);
        }
    }
}

fn visit_files(
    root: &Path,
    data_dir: &str,
    others: &mut HashMap<String, Entry>,
    mut visit: impl FnMut(fs::DirEntry, String, bool, usize),
) {
    let mut folders = vec![(root.join(data_dir), data_dir.to_owned())];
    let mut parent = 0;
    while let Some((folder, relative)) = folders.pop() {
        parent += 1;
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = format!("{relative}/{name}");
            if kind.is_dir() {
                others.insert(path.clone(), Entry::Folder);
                folders.push((entry.path(), path));
            } else if kind.is_file() {
                let temporary = place::is_copy_temporary(&name);
                visit(entry, path, temporary, parent);
            } else if kind.is_symlink() {
                others.insert(path, Entry::Link);
            } else {
                others.insert(path, Entry::Special);
            }
        }
    }
}

/// hashes the objects behind hardlinks someone edited, before anything downloads. an edit
/// through a hardlink also changes the cached object, which is then dropped and fetched again,
/// so it never spreads to other projects. a link hashing found changed that is still the object
/// itself marks the object at once, since its bytes are the object's; one the cache no longer
/// holds is marked as a changed copy, so other projects that link it check it too.
pub(crate) fn audit(project: &Project, survey: &Survey, cache: &Cache) -> Result<(), Error> {
    let _span = tracing::debug_span!("audit").entered();
    for (path, placed) in &survey.state.files {
        if placed.method != Method::Hardlink {
            continue;
        }
        let file = project.root.join(path);
        if survey.elsewhere.contains(path) {
            let marked = if same_file::is_same_file(&file, cache.object_path(&placed.sha256))
                .unwrap_or(false)
            {
                cache.mark_changed(&placed.sha256)
            } else {
                cache::Identity::of(&file)
                    .and_then(|copy| cache.mark_copy_changed(&placed.sha256, copy))
            };
            marked.map_err(|err| local(&file, &err))?;
            continue;
        }
        if (survey.edited.contains(path)
            || survey
                .disk
                .get(path)
                .is_some_and(|seen| *seen != placed.look))
            && !cache
                .verify(&placed.sha256)
                .map_err(|err| local(&file, &err))?
        {
            tracing::debug!(%path, "an edit through a hardlink changed its cached object");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn parallel_scans_do_not_retain_one_descriptor_per_folder() {
        const CHILD: &str = "FETCHLOOM_TEST_SCAN_DESCRIPTOR_LIMIT";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "cli::sync::disk::tests::parallel_scans_do_not_retain_one_descriptor_per_folder", "--test-threads=1"])
                .env(CHILD, "1")
                .output().unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let old = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(64),
                maximum: old.maximum,
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        for index in 0..512 {
            let folder = dir.path().join(format!("data/folder-{index}"));
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("x"), b"data").unwrap();
        }
        let found = scan(dir.path(), "data", Threads::default()).found;
        assert_eq!(found.len(), 512);
    }

    #[test]
    fn scan_thread_limits_preserve_files_and_staging_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for index in 0..512 {
            let path = root.join(format!("data/folder-{}/file-{index}", index / 64));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, index.to_string()).unwrap();
        }
        fs::write(
            root.join("data/.fetchloom-0123456789abcdef.copy"),
            b"partial",
        )
        .unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        fs::write(root.join("outside/foreign"), b"outside").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("outside"), root.join("data/linked")).unwrap();
            std::os::unix::fs::symlink(root.join("outside/foreign"), root.join("data/linked-file"))
                .unwrap();
        }
        let serial = Threads::read(&|_| Some("1".into())).unwrap();
        let expected = scan(root, "data", serial);
        assert_eq!(expected.found.len(), 513);
        assert_eq!(
            expected.temporaries,
            ["data/.fetchloom-0123456789abcdef.copy"]
        );
        for count in ["1", "8"] {
            let threads = Threads::read(&|_| Some(count.into())).unwrap();
            let actual = scan(root, "data", threads);
            assert_eq!(actual, expected);
            assert_eq!(scan(root, "absent", threads), Scanned::default());
        }
    }

    #[test]
    fn two_spellings_both_listed_are_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let composed = "caf\u{e9}";
        let decomposed = "cafe\u{301}";
        for name in [composed, decomposed] {
            fs::create_dir_all(root.join("data").join(name)).unwrap();
            fs::write(root.join("data").join(name).join("a.txt"), name).unwrap();
        }
        // apfs and hfs+ see both spellings as one name, which leaves one folder
        if fs::read_dir(root.join("data")).unwrap().count() < 2 {
            return;
        }
        let mut listings = Listings::new();
        for name in [composed, decomposed] {
            let path = format!("data/{name}/a.txt");
            assert!(!spelled_otherwise(root, &path, &mut listings), "{name}");
        }
        assert!(spelled_otherwise(
            root,
            "Data/caf\u{e9}/a.txt",
            &mut listings
        ));
    }

    fn record(
        root: &Path,
        path: &str,
        bytes: &[u8],
        method: Method,
    ) -> crate::project::state::Placed {
        crate::project::state::Placed {
            dataset: "set".to_owned(),
            sha256: crate::digest::sha256(bytes),
            method,
            look: Look::of(&fs::metadata(root.join(path)).unwrap()),
            writable: method == Method::Copy,
            checked: 0,
            adopted: false,
        }
    }

    #[test]
    fn racy_edits_are_hashed_and_smudged_without_distrusting_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        for path in ["data/changed", "data/intact"] {
            fs::write(project.root.join(path), b"original").unwrap();
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        for path in ["data/changed", "data/intact"] {
            survey.state.files.insert(
                path.to_owned(),
                record(&project.root, path, b"original", Method::Copy),
            );
        }
        let path = project.root.join("data/changed");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"modified").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(
            Look::of(&fs::metadata(&path).unwrap()),
            survey.disk["data/changed"]
        );
        survey.hash_racy(&project.root, 0, false);
        assert_eq!(survey.edited, HashSet::from(["data/changed".to_owned()]));
        assert!(survey.needs_writing);
        survey.smudge_edited();
        assert_eq!(survey.state.files["data/changed"].look.size, u64::MAX);
        assert_eq!(survey.state.files["data/intact"].look.size, 8);
        assert_eq!(fs::read(path).unwrap(), b"modified");
    }

    /// a change that keeps a file's size and time shows in no listing; only hashing every placed
    /// file finds it. a hardlink found so is the cache's copy, any other file is damaged, and
    /// files hashing already settled are not hashed again, nor are those of a dataset the run
    /// leaves alone.
    #[test]
    fn verifying_finds_changes_that_kept_their_size_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let placed = [
            ("copy", Method::Copy),
            ("link", Method::Hardlink),
            ("lone", Method::Hardlink),
            ("intact", Method::Copy),
            ("edited", Method::Copy),
            ("other", Method::Copy),
        ];
        for (name, _) in placed {
            fs::write(project.root.join("data").join(name), b"original").unwrap();
        }
        // the cache's own name for the link's bytes; "lone" was a link once but has only its own
        let linked = fs::hard_link(
            project.root.join("data").join("link"),
            project.root.join("object"),
        )
        .is_ok();
        let mut survey = Survey::read(&project, "data", Threads::default());
        for (name, method) in placed {
            let path = format!("data/{name}");
            let mut record = record(&project.root, &path, b"original", method);
            if name == "other" {
                "another set".clone_into(&mut record.dataset);
            }
            survey.state.files.insert(path, record);
        }
        for name in ["copy", "link", "lone", "edited", "other"] {
            let path = project.root.join("data").join(name);
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            fs::write(&path, b"Original").unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        survey.edited.insert("data/edited".to_owned());

        let named = Scope::Only(std::collections::BTreeSet::from(["set".to_owned()]));
        survey.verify(&project.root, &named).unwrap();

        let (shared, alone): (Vec<_>, Vec<_>) = ["data/copy", "data/link", "data/lone"]
            .into_iter()
            .map(str::to_owned)
            .partition(|path| linked && path == "data/link");
        assert_eq!(survey.damaged, HashSet::from_iter(alone));
        assert_eq!(survey.elsewhere, HashSet::from_iter(shared));
        assert_eq!(survey.edited, HashSet::from(["data/edited".to_owned()]));
        fs::remove_file(project.root.join("data/intact")).unwrap();
        survey.disk.insert(
            "data/intact".to_owned(),
            survey.state.files["data/intact"].look,
        );
        assert!(
            survey.verify(&project.root, &Scope::All).is_err(),
            "an unreadable file stops it"
        );
    }

    #[test]
    fn a_data_folder_on_another_device_allows_for_a_coarser_clock() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let path = project.root.join("data/table.csv");
        let stamp = |bytes: &[u8], time: SystemTime| {
            fs::write(&path, bytes).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        // fat cuts times down to two seconds: a file placed at 10.3 s and edited at 11 s keeps
        // 10 s, while a state on a finer clock was written at 10.5 s
        let placed = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_010);
        stamp(b"original", placed);
        let mut survey = Survey::read(&project, "data", Threads::default());
        survey.state.files.insert(
            "data/table.csv".to_owned(),
            record(&project.root, "data/table.csv", b"original", Method::Copy),
        );
        stamp(b"modified", placed);
        let written = nanos(placed) + 500_000_000;

        survey.hash_racy(&project.root, written, false);
        assert!(survey.edited.is_empty(), "one clock dates both");
        survey.hash_racy(&project.root, written, true);
        assert_eq!(survey.edited, HashSet::from(["data/table.csv".to_owned()]));
    }

    #[test]
    fn only_folders_on_different_devices_are_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (data, own) = (dir.path().join("data"), dir.path().join(".fetchloom"));
        fs::create_dir(&data).unwrap();
        fs::create_dir(&own).unwrap();
        assert!(!apart(&data, &own));
        assert!(apart(&dir.path().join("missing"), &own));
        #[cfg(target_os = "linux")]
        assert!(apart(Path::new("/proc"), &own));
    }

    #[test]
    fn change_marks_hash_old_links_and_refresh_their_listing_metadata() {
        use crate::cache::tests::{hardlinks, store};

        let dir = tempfile::tempdir().unwrap();
        if !hardlinks(dir.path()) {
            return;
        }
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let good = store(&cache, b"intact");
        let bad = store(&cache, b"before");
        for (name, stored) in [("intact", &good), ("changed", &bad)] {
            let object = cache.object_path(&stored.digest);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
            }
            #[cfg(not(unix))]
            {
                let mut permissions = fs::metadata(&object).unwrap().permissions();
                #[expect(
                    clippy::permissions_set_readonly_false,
                    reason = "the test edits a cached object as a writable hardlink would"
                )]
                permissions.set_readonly(false);
                fs::set_permissions(&object, permissions).unwrap();
            }
            fs::File::options()
                .write(true)
                .open(&object)
                .unwrap()
                .set_modified(
                    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000),
                )
                .unwrap();
            fs::hard_link(object, project.root.join("data").join(name)).unwrap();
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        survey.state.files.insert(
            "data/intact".to_owned(),
            record(&project.root, "data/intact", b"intact", Method::Hardlink),
        );
        survey.state.files.insert(
            "data/changed".to_owned(),
            record(&project.root, "data/changed", b"before", Method::Hardlink),
        );
        fs::write(
            cache.object_path(&bad.digest),
            b"changed through another link",
        )
        .unwrap();
        cache.mark_changed(&good.digest).unwrap();
        cache.mark_changed(&bad.digest).unwrap();
        assert_eq!(survey.doubted(&project.root, &cache).unwrap().len(), 2);
        survey.check_doubted(&project.root, &cache).unwrap();
        assert_eq!(survey.elsewhere, HashSet::from(["data/changed".to_owned()]));
        assert_eq!(survey.vouched, HashSet::from(["data/intact".to_owned()]));
        assert_eq!(
            fs::read(project.root.join("data/changed")).unwrap(),
            b"changed through another link"
        );
    }

    /// a file placed as a hardlink, then replaced by a file of its own that holds other bytes
    /// under the recorded size and time, shares nothing with the cache: a change mark puts it in
    /// doubt, and hashing finds it damaged, not changed through a link it no longer is.
    #[test]
    fn a_changed_file_that_is_no_longer_a_link_is_damaged_not_changed_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/file"), b"other").unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let mut survey = Survey::read(&project, "data", Threads::default());
        let placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
        cache.mark_changed(&placed.sha256).unwrap();
        survey.state.files.insert("data/file".to_owned(), placed);

        survey.check_doubted(&project.root, &cache).unwrap();

        assert_eq!(survey.damaged, HashSet::from(["data/file".to_owned()]));
        assert!(survey.elsewhere.is_empty());
    }

    /// a changed link that other projects share, once the cache no longer holds its bytes, is
    /// still recorded as a copy found changed, so the other projects' links to it are checked.
    #[test]
    fn a_changed_shared_link_the_cache_lost_is_marked_for_the_other_projects() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let file = project.root.join("data/file");
        fs::write(&file, b"other").unwrap();
        if fs::hard_link(&file, project.root.join("another project's link")).is_err() {
            return;
        }
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let mut survey = Survey::read(&project, "data", Threads::default());
        let placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
        let digest = placed.sha256;
        survey.state.files.insert("data/file".to_owned(), placed);
        survey.elsewhere.insert("data/file".to_owned());

        audit(&project, &survey, &cache).unwrap();

        let copy = crate::cache::Identity::of(&file).unwrap();
        assert!(cache.marks().unwrap()[&digest].copies.contains(&copy));
    }

    #[test]
    fn an_unreadable_doubted_link_is_not_silently_vouched_for() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/file"), b"bytes").unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let mut survey = Survey::read(&project, "data", Threads::default());
        let mut placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
        placed.look.modified = 0;
        survey.disk.insert("data/file".to_owned(), placed.look);
        cache.mark_changed(&placed.sha256).unwrap();
        survey.state.files.insert("data/file".to_owned(), placed);
        fs::remove_file(project.root.join("data/file")).unwrap();
        let error = survey.check_doubted(&project.root, &cache).unwrap_err();
        assert_eq!(error.kind, crate::error::Kind::LocalIo);
        assert!(error.message.contains("data/file"));
        assert_eq!(survey.vouched.len(), 0);
        assert_eq!(survey.elsewhere.len(), 0);
    }

    #[test]
    fn a_mark_whose_time_a_coarse_clock_cut_down_still_doubts_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/file"), b"bytes").unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let mut survey = Survey::read(&project, "data", Threads::default());
        let mut placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
        placed.look.modified = 0;
        placed.checked = 1_700_000_000_500_000_000;
        survey.disk.insert("data/file".to_owned(), placed.look);
        cache.mark_changed(&placed.sha256).unwrap();
        let mark = project
            .root
            .join("cache")
            .join("changed")
            .join(placed.sha256.hex());
        survey.state.files.insert("data/file".to_owned(), placed);
        for (seconds, doubted) in [(1_700_000_000, true), (1_699_999_997, false)] {
            fs::File::options()
                .write(true)
                .open(&mark)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap();
            assert_eq!(
                survey.doubted(&project.root, &cache).unwrap().len(),
                usize::from(doubted),
                "{seconds}"
            );
        }
    }

    /// a link that changed is damage to the cached copy while it is still the cache's own copy,
    /// or the copy a mark records the cache found changed after a repair gave the object a new
    /// one. a file the project put at the link's name is its own edit, which no hash is needed for.
    #[test]
    fn a_changed_link_that_is_the_cached_copy_is_doubted_and_found_damaged() {
        use crate::cache::tests::{hardlinks, rewrite, seal, store};

        let dir = tempfile::tempdir().unwrap();
        if !hardlinks(dir.path()) {
            return;
        }
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        for name in ["mine", "theirs", "replaced"] {
            fs::hard_link(&object, project.root.join("data").join(name)).unwrap();
        }
        let records: Vec<_> = ["mine", "theirs", "replaced"]
            .iter()
            .map(|name| {
                let path = format!("data/{name}");
                let placed = record(&project.root, &path, b"good", Method::Hardlink);
                (path, placed)
            })
            .collect();
        let replaced = project.root.join("data/replaced");
        fs::remove_file(&replaced).unwrap();
        fs::write(&replaced, b"a file of its own").unwrap();
        rewrite(
            &project.root.join("data/mine"),
            b"good, edited through a link",
        );
        seal(&object);
        let surveyed = || {
            let mut survey = Survey::read(&project, "data", Threads::default());
            survey.state.files.extend(records.iter().cloned());
            // as reading a state that records them does, so windows reads the links as they are
            survey.look_through_links(&project.root);
            survey
        };

        let mut survey = surveyed();
        assert_eq!(
            survey.doubted(&project.root, &cache).unwrap(),
            ["data/mine", "data/theirs"]
        );
        survey.check_doubted(&project.root, &cache).unwrap();
        assert_eq!(
            survey.elsewhere,
            HashSet::from(["data/mine".to_owned(), "data/theirs".to_owned()])
        );

        assert!(!cache.verify(&stored.digest).unwrap());
        assert!(store(&cache, b"good").new);
        fs::remove_file(project.root.join("data/mine")).unwrap();
        fs::hard_link(&object, project.root.join("data/mine")).unwrap();
        let mut survey = surveyed();
        survey.state.files.get_mut("data/mine").unwrap().look =
            Look::of(&fs::metadata(project.root.join("data/mine")).unwrap());
        assert_eq!(
            survey.doubted(&project.root, &cache).unwrap(),
            ["data/theirs"]
        );
        survey.check_doubted(&project.root, &cache).unwrap();
        assert_eq!(survey.elsewhere, HashSet::from(["data/theirs".to_owned()]));
    }

    /// a repair replaces a changed object. its mark must still reach a link to the old copy that
    /// was checked before the change was found, while a link to the new copy stays trusted.
    #[test]
    fn a_repair_doubts_links_to_the_old_copy_alone() {
        use crate::cache::tests::{hardlinks, seal, store};

        let dir = tempfile::tempdir().unwrap();
        if !hardlinks(dir.path()) {
            return;
        }
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        let cache = Cache::open(project.root.join("cache")).unwrap();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let long_ago = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test edits a cached object as a writable hardlink would"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).unwrap();
        let set_time = || {
            fs::File::options()
                .write(true)
                .open(&object)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        };
        set_time();
        fs::hard_link(&object, project.root.join("data/old")).unwrap();
        let mut old = record(&project.root, "data/old", b"good", Method::Hardlink);
        // the scan of the sync that repairs, and the last check of the old link, come first
        let checked = nanos(SystemTime::now());
        old.checked = checked;
        // the old copy changes keeping its size and time, then a fresh download replaces it
        fs::write(&object, b"evil").unwrap();
        set_time();
        seal(&object);
        assert!(store(&cache, b"good").new);
        fs::hard_link(&object, project.root.join("data/new")).unwrap();
        let mut new = record(&project.root, "data/new", b"good", Method::Hardlink);
        new.checked = checked;
        let mut survey = Survey::read(&project, "data", Threads::default());
        survey.state.files.insert("data/old".to_owned(), old);
        survey.state.files.insert("data/new".to_owned(), new);
        assert_eq!(survey.doubted(&project.root, &cache).unwrap(), ["data/old"]);

        // a coarse clock can give the old copy the very time of the new one; only the file
        // itself says which copy a link names
        let old_path = project.root.join("data/old");
        let replaced = fs::metadata(&object).unwrap().modified().unwrap();
        let mut permissions = fs::metadata(&old_path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test retimes the old copy, which its seal made read-only"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&old_path, permissions.clone()).unwrap();
        fs::File::options()
            .write(true)
            .open(&old_path)
            .unwrap()
            .set_modified(replaced)
            .unwrap();
        permissions.set_readonly(true);
        fs::set_permissions(&old_path, permissions).unwrap();
        let mut old = record(&project.root, "data/old", b"good", Method::Hardlink);
        old.checked = checked;
        let mut survey = Survey::read(&project, "data", Threads::default());
        let new = record(&project.root, "data/new", b"good", Method::Hardlink);
        assert_eq!(old.look.modified, new.look.modified);
        survey.state.files.insert("data/old".to_owned(), old);
        survey.state.files.insert(
            "data/new".to_owned(),
            crate::project::state::Placed { checked, ..new },
        );
        assert_eq!(survey.doubted(&project.root, &cache).unwrap(), ["data/old"]);
    }

    // runs where the disk ignores case, as windows and macos disks do by default
    #[test]
    fn alias_retargeting_carries_edits_and_the_original_listed_spelling() {
        use crate::project::lock::LockedFile;

        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/A.txt"), b"a").unwrap();
        if !project.root.join("data/a.txt").is_file() {
            return;
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        let placed = record(&project.root, "data/a.txt", b"a", Method::Copy);
        survey
            .state
            .files
            .insert("data/a.txt".to_owned(), placed.clone());
        survey.hash_racy(&project.root, 0, false);
        assert_eq!(survey.disk.len(), 1);
        assert_eq!(survey.disk["data/a.txt"], placed.look);
        assert_eq!(survey.listed["data/a.txt"], "data/A.txt");
        for paths in [
            &mut survey.edited,
            &mut survey.elsewhere,
            &mut survey.vouched,
        ] {
            paths.insert("data/a.txt".to_owned());
        }
        let locked = LockedFile {
            path: "A.txt".to_owned(),
            size: 1,
            sha256: placed.sha256,
            md5: None,
            url: "https://example.org/a".to_owned(),
            unpacks_to: None,
        };
        let target = Target {
            dataset: "set",
            path: "data/A.txt".to_owned(),
            sha256: placed.sha256,
            size: 1,
            copy: true,
            holder: crate::plan::Holder::File(&locked),
        };
        survey.retarget(&project.root, &[target]);
        assert_eq!(survey.state.files.len(), 1);
        assert_eq!(survey.state.files["data/A.txt"], placed);
        assert_eq!(survey.listed["data/A.txt"], "data/A.txt");
        for paths in [&survey.edited, &survey.elsewhere, &survey.vouched] {
            assert_eq!(paths, &HashSet::from(["data/A.txt".to_owned()]));
        }
        assert!(survey.needs_writing);
    }

    fn target<'a>(locked: &'a crate::project::lock::LockedFile, path: &str) -> Target<'a> {
        Target {
            dataset: "set",
            path: path.to_owned(),
            sha256: locked.sha256,
            size: locked.size,
            copy: false,
            holder: crate::plan::Holder::File(locked),
        }
    }

    fn locked(path: &str, bytes: &[u8]) -> crate::project::lock::LockedFile {
        crate::project::lock::LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: crate::digest::sha256(bytes),
            md5: None,
            url: "https://example.org/a".to_owned(),
            unpacks_to: None,
        }
    }

    // runs where the disk sees composed and decomposed unicode as one name, as macos disks do
    #[test]
    fn a_name_listed_in_another_composition_is_the_targets_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/cafe\u{301}.txt"), b"a").unwrap();
        if !project.root.join("data/caf\u{e9}.txt").is_file() {
            return;
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        let listed = survey.disk.keys().next().unwrap().clone();
        let locked = locked("caf\u{e9}.txt", b"a");
        survey.retarget(&project.root, &[target(&locked, "data/caf\u{e9}.txt")]);
        assert_eq!(
            survey.disk.keys().collect::<Vec<_>>(),
            ["data/caf\u{e9}.txt"]
        );
        assert_eq!(survey.listed["data/caf\u{e9}.txt"], listed);
        assert!(survey.state.files.is_empty());
        assert!(!survey.needs_writing);
    }

    // runs where the disk sees composed and decomposed unicode as one name, as macos disks do
    #[test]
    fn a_writable_file_listed_in_another_composition_stays_writable_under_the_targets_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/cafe\u{301}.txt"), b"a").unwrap();
        if !project.root.join("data/caf\u{e9}.txt").is_file() {
            return;
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        assert_eq!(survey.writable.len(), 1);
        let locked = locked("caf\u{e9}.txt", b"a");
        survey.retarget(&project.root, &[target(&locked, "data/caf\u{e9}.txt")]);
        assert!(
            survey.writable.contains("data/caf\u{e9}.txt"),
            "{:?}",
            survey.writable
        );
    }

    // runs where the disk ignores case, as windows and macos disks do by default
    #[test]
    fn a_name_listed_in_another_case_stays_apart_until_a_record_joins_them() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        fs::create_dir(project.root.join("data")).unwrap();
        fs::write(project.root.join("data/A.txt"), b"a").unwrap();
        if !project.root.join("data/a.txt").is_file() {
            return;
        }
        let mut survey = Survey::read(&project, "data", Threads::default());
        let locked = locked("a.txt", b"a");
        survey.retarget(&project.root, &[target(&locked, "data/a.txt")]);
        assert_eq!(survey.disk.keys().collect::<Vec<_>>(), ["data/A.txt"]);
        assert!(survey.listed.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn whatever_stands_in_a_files_way_is_found_from_the_scan_alone() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let data = project.root.join("data");
        for folder in ["real", "empty.csv", "full.csv/inner"] {
            fs::create_dir_all(data.join(folder)).unwrap();
        }
        fs::write(data.join("file"), b"notes").unwrap();
        fs::write(data.join("full.csv/inner/kept"), b"work").unwrap();
        symlink(dir.path(), data.join("linked")).unwrap();
        symlink(dir.path().join("elsewhere"), data.join("real/link.csv")).unwrap();
        let survey = Survey::read(&project, "data", Threads::default());
        let blocker = |path: &str| {
            survey
                .blocker(path)
                .map(|found| (found.path, found.found, found.folder, found.clearable))
        };

        assert_eq!(
            blocker("data/file/a.csv"),
            Some(("data/file".to_owned(), Entry::File, true, true))
        );
        assert_eq!(
            blocker("data/linked/deep/a.csv"),
            Some(("data/linked".to_owned(), Entry::Link, true, true))
        );
        assert_eq!(
            blocker("data/real/link.csv"),
            Some(("data/real/link.csv".to_owned(), Entry::Link, false, true))
        );
        assert_eq!(
            blocker("data/empty.csv"),
            Some(("data/empty.csv".to_owned(), Entry::Folder, false, true))
        );
        assert_eq!(
            blocker("data/full.csv"),
            Some(("data/full.csv".to_owned(), Entry::Folder, false, false))
        );
        assert_eq!(blocker("data/real/free.csv"), None);
        assert_eq!(blocker("data/absent/a.csv"), None);
        assert_eq!(blocker("elsewhere/a.csv"), None);
    }

    #[cfg(unix)]
    #[test]
    fn scanning_skips_links_even_when_their_targets_are_files_or_folders() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("data/nested")).unwrap();
        fs::create_dir(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("outside/private"), b"outside").unwrap();
        fs::write(dir.path().join("data/nested/kept"), b"inside").unwrap();
        symlink(
            dir.path().join("outside"),
            dir.path().join("data/folder-link"),
        )
        .unwrap();
        symlink(
            dir.path().join("outside/private"),
            dir.path().join("data/file-link"),
        )
        .unwrap();
        let Scanned {
            found: files,
            temporaries: leftovers,
            entries,
            ..
        } = scan(dir.path(), "data", Threads::default());
        assert_eq!(files.len(), 1);
        assert_eq!(files["data/nested/kept"].size, 6);
        assert_eq!(leftovers, Vec::<String>::new());
        assert_eq!(
            entries,
            HashMap::from([
                ("data/nested".to_owned(), Entry::Folder),
                ("data/folder-link".to_owned(), Entry::Link),
                ("data/file-link".to_owned(), Entry::Link),
            ])
        );
        assert_eq!(
            scan(dir.path(), "missing", Threads::default()).found.len(),
            0
        );
    }

    /// what a removal leaves in a folder no dataset uses now: work of yours, a link, never a
    /// file placement stages, and never a folder beside it whose name only starts the same.
    #[cfg(unix)]
    #[test]
    fn a_retired_folder_is_the_highest_holding_no_wanted_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let placed = [
            "data/pets/pets.csv",
            "data/pets-extra/keep.csv",
            "data/a/b/old.csv",
            "data/a/c/new.csv",
            "data/old/deep/only.csv",
            "data/gone/only.csv",
        ];
        let own = [
            "data/pets/notes.txt",
            "data/pets/sub/x.txt",
            "data/pets-extra/mine.txt",
            "data/a/b/own.txt",
            "data/a/b/.fetchloom-0123456789abcdef.copy",
            "data/old/mine.txt",
        ];
        for path in placed.iter().chain(&own) {
            let file = project.root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, path).unwrap();
        }
        std::os::unix::fs::symlink(dir.path(), project.root.join("data/pets/link")).unwrap();
        let mut survey = Survey::read(&project, "data", Threads::default());
        for path in placed {
            let record = record(&project.root, path, path.as_bytes(), Method::Copy);
            survey.state.files.insert(path.to_owned(), record);
        }
        let wanted = HashSet::from(["data/pets-extra/keep.csv", "data/a/c/new.csv"]);
        let going = HashSet::from([
            "data/pets/pets.csv",
            "data/a/b/old.csv",
            "data/old/deep/only.csv",
            "data/gone/only.csv",
        ]);

        assert_eq!(
            survey.retired(&wanted, &going),
            [
                Retired {
                    folder: "data/a/b".to_owned(),
                    unplaced: vec!["data/a/b/own.txt".to_owned()],
                },
                Retired {
                    folder: "data/old".to_owned(),
                    unplaced: vec!["data/old/mine.txt".to_owned()],
                },
                Retired {
                    folder: "data/pets".to_owned(),
                    unplaced: vec![
                        "data/pets/link".to_owned(),
                        "data/pets/notes.txt".to_owned(),
                        "data/pets/sub/x.txt".to_owned(),
                    ],
                },
            ]
        );
    }

    /// a file the state records outside the data folder is left behind only while it is a plain
    /// file reached through real folders, and the same file under another spelling of the data
    /// folder, as a disk that ignores case gives, is not.
    #[cfg(unix)]
    #[test]
    fn only_real_files_outside_the_data_folder_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "data/iris/iris.csv",
            "elsewhere/x.csv",
            "inputs/raw/new.csv",
        ] {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"bytes").unwrap();
        }
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
        fs::create_dir(root.join("special")).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere/x.csv"), root.join("special/x.csv"))
            .unwrap();
        let mut state = State::default();
        for path in [
            "data/iris/iris.csv",
            "data/gone.csv",
            "linked/x.csv",
            "special/x.csv",
            "../outside.csv",
            "inputs/raw/new.csv",
        ] {
            state.files.insert(
                path.to_owned(),
                record(root, "elsewhere/x.csv", b"bytes", Method::Copy),
            );
        }

        assert_eq!(
            outside(root, "inputs/raw", &state, &HashMap::new()),
            ["data/iris/iris.csv"]
        );
        let names_alias = root.join("INPUTS").try_exists().unwrap();
        assert_eq!(
            outside(root, "INPUTS/raw", &state, &HashMap::new())
                .contains(&"inputs/raw/new.csv".to_owned()),
            !names_alias
        );
    }

    /// the folder named for files left behind is the data folder they were placed in, found from
    /// where the same files sit now, else the folder they share, and never one that holds the
    /// data folder, which deleting it would delete too.
    #[test]
    fn the_folder_left_behind_never_holds_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let left = |data_dir: &str, outside: &[&str], wanted: &[&str]| {
            let mut survey = Survey::read(&project, data_dir, Threads::default());
            survey.outside = outside.iter().map(|path| (*path).to_owned()).collect();
            let going: HashSet<&str> = outside.iter().copied().collect();
            let wanted: HashSet<&str> = wanted.iter().copied().collect();
            survey.left_behind(&wanted, &going).map(|left| left.folder)
        };

        let moved = ["data/iris/iris.csv", "data/pets/pets.csv"];
        assert_eq!(
            left("inputs", &moved[..1], &["inputs/iris/iris.csv"]),
            Some(Some("data".to_owned())),
            "the data folder, though one dataset alone was in it"
        );
        assert_eq!(
            left("inputs", &moved, &["inputs/iris/iris.csv"]),
            Some(Some("data".to_owned())),
            "the folder they share"
        );
        assert_eq!(
            left("data/new", &moved, &["data/new/iris/iris.csv"]),
            Some(None)
        );
        assert_eq!(
            left("Data", &moved, &[]),
            Some(None),
            "a disk that ignores case"
        );
        assert_eq!(left("inputs", &[], &["inputs/iris/iris.csv"]), None);
    }
}
