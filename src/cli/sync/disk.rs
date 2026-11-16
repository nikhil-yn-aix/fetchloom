//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited. every path here is a path in the project, with `/` between
//! names, unless a doc says otherwise.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use super::Scope;
use super::place::local;
use crate::cache::{self, Cache};
use crate::digest::{Block, Sha256};
use crate::error::Error;
use crate::output::Output;
use crate::place::{self, Method};
use crate::plan::Target;
use crate::project::state::{Look, State};
use crate::project::{Project, native};
use crate::settings::Level;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units::nanos;
use crate::{name, units};

/// the data folder as fetchloom recorded placing it and as it is now.
pub(crate) struct Survey {
    pub(super) threads: Threads,
    /// the scan's start, so marks written while placing files still put them in doubt.
    pub(crate) started: u64,
    pub(crate) state: State,
    /// every file under the data folder and how it looks.
    pub(crate) disk: HashMap<String, Look>,
    /// the listing's spelling of an alias, within the current data folder, for removals.
    pub(crate) listed: HashMap<String, String>,
    /// hardlinks hashing found changed while they share their bytes with the cache or other
    /// projects, or are a copy a change mark records. sync puts them back, with or without --force.
    pub(crate) elsewhere: HashSet<String>,
    /// hardlinks a mark put in doubt whose bytes hashing found intact, which sync keeps, recording
    /// the scan's start.
    pub(crate) vouched: HashSet<String>,
    /// files that still look as recorded but hold other bytes, which hashing found among those
    /// as new as the state itself.
    pub(crate) edited: HashSet<String>,
    /// copies, reflinks and links with no other name that look as placed but hold other bytes, as
    /// `--verify` or a change mark finds. sync puts them back, with or without --force.
    pub(crate) damaged: HashSet<String>,
    /// files the disk lets their owner write. a link to the cache's copy among them leaves the
    /// copy every project shares open to an edit.
    pub(crate) writable: HashSet<String>,
    /// files read-only with no other name, as a copy of the data folder that kept no links leaves
    /// a hardlink fetchloom placed, each path ended by a nul, which no path holds: the scan's on
    /// unix, those of the hardlinks `look_through_links` opens on windows. one buffer for all
    /// costs a scan of thousands of clones or copies no allocation for each.
    alone: String,
    /// `alone` as a set, gathered the first time a hardlink's record asks, and dropped when a path
    /// takes another spelling.
    alone_index: OnceLock<HashSet<String>>,
    /// whether hashing found files as new as the state unchanged, so a state written now settles
    /// them for later runs, or a record moved to another spelling of its path.
    pub(crate) needs_writing: bool,
    /// files under the hidden names placement stages files in.
    temporaries: Vec<String>,
    /// what else the data folder holds: folders, and links and special files, which the survey
    /// never follows or reads.
    pub(crate) entries: HashMap<String, Entry>,
    /// every folder the listing found something in, gathered the first time a blocker asks and
    /// dropped when a path takes another spelling.
    holding: OnceLock<HashSet<String>>,
    /// each listed file by its path folded as [`name::fold`] folds it, gathered the first time a
    /// blocker asks about a folder the listing lacks, which a disk that ignores case may take
    /// for a file of another spelling.
    folded: OnceLock<HashMap<String, String>>,
    /// the project's folder, which paths in it are below.
    root: std::path::PathBuf,
    /// the data folder, below which entries can stand in a file's way.
    data_dir: String,
    /// the files the state records outside the data folder that are still there, in order, as
    /// fetchloom placed them before `data_dir` changed. a sync never deletes them.
    outside: Vec<String>,
}

/// the files fetchloom placed before `data_dir` changed that are still outside the data folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LeftBehind {
    /// the folder they are in, when it can be named without naming the data folder too.
    pub(crate) folder: Option<String>,
    /// the files, in order.
    pub(crate) files: Vec<String>,
    /// what else `folder` holds, such as a person's notes, which deleting the folder takes too:
    /// files, links and special files fetchloom did not place, sorted.
    pub(crate) others: Vec<String>,
}

/// a folder that held files fetchloom placed which no dataset needs now, and that holds none a
/// dataset needs, with what else is in it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Retired {
    pub(crate) folder: String,
    /// the files, links and special files in it that fetchloom did not place, sorted, but for
    /// those of `system`.
    pub(crate) unplaced: Vec<String>,
    /// the files in it an operating system made, sorted, which go with the folder when nothing
    /// else keeps it.
    pub(crate) system: Vec<String>,
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
    /// where the entry stands: the file's own path, or a folder on its way.
    pub(crate) path: String,
    pub(crate) found: Entry,
    /// whether data.lock needs a folder at that path, rather than a file.
    pub(crate) folder: bool,
    /// whether `sync --force` may remove it. a folder holding anything stays, since its files
    /// may be someone's work.
    pub(crate) clearable: bool,
    /// the dataset whose file the entry is, or whose files the folder holds, when the run leaves
    /// that dataset alone, as a sync of other datasets does. it stays even with `--force`.
    pub(crate) dataset: Option<String>,
}

/// what removing the files of a run's `going` takes out of the way, from one survey.
pub(crate) struct Freeing<'s> {
    going: &'s HashSet<String>,
    holding_going: HashSet<&'s str>,
    /// the folders holding something removing `going` leaves: another file, a link, a special
    /// file or a folder that holds no file of `going`.
    holding_others: HashSet<&'s str>,
}

impl Freeing<'_> {
    /// whether removing `going` takes `blocker` out of the way: a file among them, or a folder
    /// holding nothing else, since a folder goes with the last file removed from it.
    pub(crate) fn frees(&self, blocker: &Blocker) -> bool {
        match blocker.found {
            Entry::File => self.going.contains(&blocker.path),
            Entry::Folder => {
                self.holding_going.contains(blocker.path.as_str())
                    && !self.holding_others.contains(blocker.path.as_str())
            }
            Entry::Link | Entry::Special => false,
        }
    }
}

/// adds every folder above `path` to `folders`.
fn gather_folders<'p>(folders: &mut HashSet<&'p str>, path: &'p str) {
    let mut inner = path;
    while let Some((above, _)) = inner.rsplit_once('/') {
        // the folders above one already gathered were gathered with it
        if !folders.insert(above) {
            break;
        }
        inner = above;
    }
}

/// the state size from which a survey reads it on its own thread as it walks the data folder: 24.5
/// MB took a settled sync 0.79 of its time for 2 MiB more, while 2.4 to 12.1 MB cost 11 to 18 MiB.
const BESIDE_FROM: u64 = 16 << 20;

/// the run's `resources` level, by which a survey weighs memory against time, set once from the
/// settings before any command runs. [`Survey::surveyed_from`] takes it as a value tests set.
static LEVEL: OnceLock<Level> = OnceLock::new();

/// sets the run's `resources` level once its settings are read. a second set is a bug, which
/// debug builds stop at.
pub(crate) fn set_level(level: Level) {
    let first = LEVEL.set(level).is_ok();
    debug_assert!(first, "the resources level is set once, at start");
}

/// whether a survey reads a whole state of `size` bytes on its own thread as it walks the data
/// folder: from `from` bytes on, unless the run keeps to one worker or is lean on memory.
fn beside(size: u64, from: u64, threads: Threads, level: Level) -> bool {
    size >= from && threads.limit(2) >= 2 && level != Level::Lean
}

impl Survey {
    /// reads the state, lists the data folder, and hashes the files the state is too new to
    /// vouch for.
    ///
    /// # Errors
    ///
    /// interrupted once `stop` is asked, before the listing reaches its next folder.
    pub(crate) fn read(
        project: &Project,
        data_dir: &str,
        threads: Threads,
        stop: &Stop,
    ) -> Result<Survey, Error> {
        Self::surveyed(project, data_dir, None, threads, stop)
    }

    /// [`Survey::read`] of the folders `within` alone, inside `data_dir`, keeping only the state's
    /// records inside them, so that partial state is never written back.
    pub(crate) fn read_within(
        project: &Project,
        data_dir: &str,
        within: &[String],
        threads: Threads,
        stop: &Stop,
    ) -> Result<Survey, Error> {
        Self::surveyed(project, data_dir, Some(within), threads, stop)
    }

    fn surveyed(
        project: &Project,
        data_dir: &str,
        within: Option<&[String]>,
        threads: Threads,
        stop: &Stop,
    ) -> Result<Survey, Error> {
        let level = LEVEL.get().copied().unwrap_or_default();
        Self::surveyed_from(project, data_dir, within, threads, stop, BESIDE_FROM, level)
    }

    /// [`Survey::surveyed`], reading a whole state of `beside_from` bytes or more on a thread of
    /// its own while it walks the data folder, as [`beside`] decides for `threads` and `level`.
    fn surveyed_from(
        project: &Project,
        data_dir: &str,
        within: Option<&[String]>,
        threads: Threads,
        stop: &Stop,
        beside_from: u64,
        level: Level,
    ) -> Result<Survey, Error> {
        project.refuse_linked_data_dir(data_dir)?;
        let started = nanos(SystemTime::now());
        let within = within.map(|folders| outermost(data_dir, folders));
        let walk = || scan(&project.root, data_dir, within.as_deref(), threads, stop);
        let ((state, written), scanned) = if let Some(folders) = &within {
            (
                project.state_within(folders, &|path| inside_any(path, folders)),
                walk(),
            )
        } else {
            let opened = project.open_state();
            let size = opened.as_ref().map_or(0, |(_, found)| found.size);
            if beside(size, beside_from, threads, level) {
                let mut opened = Some(opened);
                std::thread::scope(|scope| {
                    let reading = crate::threads::scoped(scope, move || {
                        opened.take().map_or_else(Default::default, |opened| {
                            project.read_opened(opened, State::read_whole)
                        })
                    });
                    let scanned = walk();
                    (reading.join(), scanned)
                })
            } else {
                (project.read_opened(opened, State::read_whole), walk())
            }
        };
        let Scanned {
            found: disk,
            temporaries,
            entries,
            writable,
            alone,
        } = scanned?;
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
            alone,
            alone_index: OnceLock::new(),
            needs_writing: false,
            temporaries,
            entries,
            holding: OnceLock::new(),
            folded: OnceLock::new(),
            root: project.root.clone(),
            data_dir: data_dir.to_owned(),
            outside: Vec::new(),
        };
        let other_device = !survey.state.files.is_empty()
            && apart(&native(&project.root, data_dir), &project.own_folder());
        survey.hash_racy(&project.root, written, other_device);
        // after hash_racy, so a record found under another spelling is looked through too
        survey.look_through_links(&project.root);
        if within.is_none() {
            survey.outside = outside(&project.root, data_dir, &survey.state, &survey.disk);
        }
        Ok(survey)
    }

    /// the files among `going`, the records a run drops, still outside the data folder, and their
    /// folder: the old data folder, found from where a file of `wanted` sits below the new one,
    /// else the folder they share. none when no file was left behind.
    pub(crate) fn left_behind(
        &self,
        root: &Path,
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
        let folder = Some(folder).filter(|folder| !folder.is_empty() && !holds_data(folder));
        let others = folder
            .as_deref()
            .map(|folder| self.not_placed(root, folder))
            .unwrap_or_default();
        Some(LeftBehind {
            folder,
            files,
            others,
        })
    }

    /// what `folder` holds that no record says fetchloom placed, besides copies a placement
    /// staged and files an operating system made: files, links and special files, sorted. links
    /// are neither followed nor read.
    fn not_placed(&self, root: &Path, folder: &str) -> Vec<String> {
        let mut entries = HashMap::new();
        let mut others = Vec::new();
        let start = vec![(native(root, folder), folder.to_owned())];
        let made_by_system =
            |path: &str| name::system_file(path.rsplit('/').next().unwrap_or(path));
        // one folder outside the data folder, listed while placing, which looks at the stop itself
        visit_files(
            start,
            &mut entries,
            || false,
            |_, path, temporary, _| {
                if !temporary && !self.state.files.contains_key(&path) && !made_by_system(&path) {
                    others.push(path);
                }
            },
        );
        others.extend(
            entries
                .into_iter()
                .filter(|(_, entry)| *entry != Entry::Folder)
                .map(|(path, _)| path),
        );
        others.sort();
        others
    }

    /// each highest folder below the data folder holding a file of `going` and none of `wanted`,
    /// with what else the survey listed in it. one holding nothing else goes with its last file.
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
        let mut retired: BTreeMap<&str, Retired> = BTreeMap::new();
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
                let found = retired.entry(folder).or_insert_with(|| Retired {
                    folder: folder.to_owned(),
                    unplaced: Vec::new(),
                    system: Vec::new(),
                });
                if self.made_by_system(path) {
                    found.system.push(path.clone());
                } else {
                    found.unplaced.push(path.clone());
                }
            }
        }
        retired
            .into_values()
            .map(|mut found| {
                found.unplaced.sort();
                found.system.sort();
                found
            })
            .collect()
    }

    /// whether the file at `path` is one an operating system made rather than fetchloom or a
    /// person: unplaced, with a name [`name::system_file`] knows, as `.DS_Store` or
    /// `__MACOSX/._a.csv`. none is a difference. what a system's folder holds otherwise, as a
    /// disk's trash, may be a person's.
    pub(crate) fn made_by_system(&self, path: &str) -> bool {
        self.disk.contains_key(path)
            && !self.state.files.contains_key(path)
            && name::system_file(path.rsplit('/').next().unwrap_or(path))
    }

    /// windows listings keep a hardlink's size, time and read-only flag as this name last saw
    /// them, hiding an edit through another link, so each file placed as a hardlink is opened,
    /// once, which also tells whether it still has another name.
    #[cfg(windows)]
    fn look_through_links(&mut self, root: &Path) {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        for (path, placed) in &self.state.files {
            if placed.method != Method::Hardlink {
                continue;
            }
            let Some(seen) = self.disk.get_mut(path) else {
                continue;
            };
            // opened as `fs::metadata` opens it, asking for no access, so a program holding the
            // file open keeps no look from this survey
            let Ok(file) = fs::OpenOptions::new()
                .access_mode(0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(native(root, path))
            else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            *seen = Look::of(&metadata);
            if metadata.permissions().readonly() {
                self.writable.remove(path);
            } else {
                self.writable.insert(path.clone());
            }
            let handle = winapi_util::Handle::from_file(file);
            if winapi_util::file::information(&handle)
                .is_ok_and(|information| information.number_of_links() == 1)
            {
                push_alone(&mut self.alone, path);
            }
        }
    }

    #[cfg(not(windows))]
    #[expect(
        clippy::unused_self,
        reason = "unix listings report a hardlinked file's size and time as they are"
    )]
    fn look_through_links(&mut self, _root: &Path) {}

    /// carries listed aliases to missing recorded names, then hashes files that look as recorded
    /// but are not older than the state, `written`: a coarse clock gives a file edited in the tick
    /// it was placed the size and time recorded for it.
    fn hash_racy(&mut self, root: &Path, written: u64, other_device: bool) {
        let written = vouched_before(written, other_device);
        let mut racy = Vec::new();
        let mut missing = Vec::new();
        for (path, placed) in &self.state.files {
            match self.disk.get(path) {
                None => missing.push(path),
                Some(seen) if placed.look.modified >= written && *seen == placed.look => {
                    racy.push((path, placed));
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
            self.holding.take();
            self.folded.take();
            self.listed.insert(path.clone(), listed);
            if let Some(placed) = self.state.files.get(path)
                && placed.look.modified >= written
                && look == placed.look
            {
                racy.push((path, placed));
            }
        }
        if racy.is_empty() {
            return;
        }
        let _span = tracing::debug_span!("hash_racy", files = racy.len()).entered();
        tracing::debug!(
            files = racy.len(),
            "hashing files placed too recently to trust their time"
        );
        let unchanged = place::run_all(&racy, self.threads, |(path, placed)| {
            let block = Block::sized(placed.look.size.saturating_add(1));
            hash_plain(&native(root, path), block).is_ok_and(|digest| digest == placed.sha256)
        });
        for ((path, placed), unchanged) in racy.into_iter().zip(unchanged) {
            if unchanged {
                self.needs_writing = true;
                let _ = placed.racy_intact.set(());
            } else {
                tracing::debug!(%path, "hashing found an edit");
                self.edited.insert(path.clone());
            }
        }
    }

    /// carries a record, with its edits and doubts, to a wanted alias, so decisions and removals
    /// see one file. an unrecorded file moves only when the names differ in unicode composition.
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
            self.holding.take();
            self.folded.take();
            if self.writable.remove(&previous) {
                self.writable.insert(path.clone());
            }
            self.moved_alone(&previous, path);
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

    /// gives each edited file a recorded size no file has, as git smudges racily clean entries,
    /// since a state written now would vouch for the size and time the edit kept.
    pub(crate) fn smudge_edited(&mut self) {
        for path in &self.edited {
            if let Some(placed) = self.state.files.get_mut(path) {
                placed.look.size = u64::MAX;
            }
        }
    }

    /// the entry in the way of a file data.lock places at `path`, where no plain file stands: a
    /// non-folder where a folder on its way belongs, or a folder, link or special file at `path`.
    pub(crate) fn blocker(&self, path: &str) -> Option<Blocker> {
        let start = self.data_dir.len() + 1;
        let below = path
            .strip_prefix(self.data_dir.as_str())?
            .strip_prefix('/')?;
        for (end, _) in below.match_indices('/') {
            let folder = &path[..start + end];
            let (folder, found) = if self.disk.contains_key(folder) {
                (folder, Entry::File)
            } else {
                match self.entries.get(folder) {
                    Some(Entry::Folder) => continue,
                    Some(entry) => (folder, *entry),
                    None => (self.file_taking(folder)?, Entry::File),
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

    /// the listed file of another spelling that this disk takes for the folder `folder`, which the
    /// listing lacks, as a disk that ignores case takes `data/Iris` for `data/iris`.
    fn file_taking(&self, folder: &str) -> Option<&str> {
        let by_fold = self.folded.get_or_init(|| {
            self.disk
                .keys()
                .map(|path| (name::fold(path), path.clone()))
                .collect()
        });
        let listed = by_fold.get(&name::fold(folder))?;
        let look = self.disk.get(listed)?;
        let found = fs::symlink_metadata(native(&self.root, folder)).ok()?;
        (listed != folder && found.is_file() && Look::of(&found) == *look)
            .then_some(listed.as_str())
    }

    /// takes what the listing found at `from` for `to`, once it was renamed there.
    pub(crate) fn renamed(&mut self, from: &str, to: &str) {
        if let Some(look) = self.disk.remove(from) {
            self.disk.insert(to.to_owned(), look);
        }
        if self.writable.remove(from) {
            self.writable.insert(to.to_owned());
        }
        self.moved_alone(from, to);
        self.folded.take();
    }

    /// whether the file at `path` is read-only with no other name, as a hardlink fetchloom placed
    /// is not, though a copy of it is.
    pub(crate) fn is_alone(&self, path: &str) -> bool {
        self.alone_index
            .get_or_init(|| {
                self.alone
                    .split_terminator('\0')
                    .map(str::to_owned)
                    .collect()
            })
            .contains(path)
    }

    /// carries what the scan found of the file at `from` alone to `to`, its other spelling.
    fn moved_alone(&mut self, from: &str, to: &str) {
        if self.is_alone(from) {
            let mut moved = String::with_capacity(self.alone.len() + to.len());
            for path in self.alone.split_terminator('\0') {
                push_alone(&mut moved, if path == from { to } else { path });
            }
            self.alone = moved;
            self.alone_index.take();
        }
    }

    /// whether nothing stands at `path` or on its way, so a file can take that name.
    pub(crate) fn is_free(&self, path: &str) -> bool {
        !self.disk.contains_key(path)
            && !self.entries.contains_key(path)
            && self.blocker(path).is_none()
    }

    /// what removing the files in `going`, with the folders that leaves empty, takes out of the
    /// way, worked out once for every blocker a run asks about. files a system made go with the
    /// folder their last other file leaves, so they keep none.
    pub(crate) fn freeing<'s>(&'s self, going: &'s HashSet<String>) -> Freeing<'s> {
        let mut holding_going = HashSet::new();
        for path in going.iter().filter(|path| self.disk.contains_key(*path)) {
            gather_folders(&mut holding_going, path);
        }
        let mut holding_others = HashSet::new();
        for path in self
            .disk
            .keys()
            .filter(|path| !going.contains(*path) && !self.made_by_system(path))
        {
            gather_folders(&mut holding_others, path);
        }
        for (path, entry) in &self.entries {
            if *entry != Entry::Folder || !holding_going.contains(path.as_str()) {
                gather_folders(&mut holding_others, path);
            }
        }
        Freeing {
            going,
            holding_going,
            holding_others,
        }
    }

    /// whether the folder at `path` held nothing but files a system made when the survey listed
    /// it, as a person sees an empty folder.
    fn holds_nothing_under(&self, path: &str) -> bool {
        let holding = self.holding.get_or_init(|| {
            let mut holding = HashSet::new();
            let kept = self.disk.keys().filter(|inner| !self.made_by_system(inner));
            for inner in kept.chain(self.entries.keys()) {
                gather_folders(&mut holding, inner);
            }
            holding.into_iter().map(str::to_owned).collect()
        });
        !holding.contains(path)
    }

    /// the files a system made that the survey listed right inside the folder at `path`.
    pub(crate) fn made_by_system_in<'s>(
        &'s self,
        path: &'s str,
    ) -> impl Iterator<Item = &'s String> {
        self.disk.keys().filter(move |inner| {
            inner
                .strip_prefix(path)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|name| !name.contains('/'))
                && self.made_by_system(inner)
        })
    }

    /// the files an interrupted placement left under the hidden names it stages files in, which
    /// no record claims. only placement makes such names.
    pub(crate) fn leftovers(&self) -> impl Iterator<Item = &String> {
        self.temporaries
            .iter()
            .filter(|path| !self.state.files.contains_key(*path))
    }

    /// hardlinks only hashing can tell intact from changed, with the cache's copy in doubt.
    ///
    /// one looks as placed, but its object was marked changed after, or within a coarse clock's
    /// step of, the time the link was last known intact; the very file a repair put in place is
    /// trusted, as a shared coarse time alone proves nothing.
    ///
    /// the other changed while it is still the cache's copy, or a copy a mark records, so its edit
    /// is damage every sharing project reads; a link replaced by a file of its own stays an edit.
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
                    same_file::is_same_file(native(root, path), cache.object_path(&placed.sha256))
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
                        cache::Identity::of(&native(root, path))
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
            let block = Block::sized(files[path].look.size.saturating_add(1));
            hash_plain(&native(root, path), block).map(|digest| digest == files[path].sha256)
        });
        let marks = cache.marks()?;
        for (path, intact) in doubted.into_iter().zip(intact) {
            match intact {
                Ok(true) => {
                    self.vouched.insert(path);
                }
                Ok(false) => {
                    let file = native(root, &path);
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

    /// hashes for `--verify` each placed file in `scope` that looks as placed and that this survey
    /// has not hashed or put in doubt, and returns those changed, which join `elsewhere` or
    /// `damaged`. `output` shows a bar of the bytes; an unreadable file stops the check.
    pub(crate) fn verify(
        &mut self,
        root: &Path,
        scope: &Scope,
        output: Option<&Output>,
    ) -> Result<HashSet<String>, Error> {
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
                    && (placed.racy_intact.get().is_none() || self.damaged.contains(*path))
            })
            .collect();
        if files.is_empty() {
            return Ok(HashSet::new());
        }
        let total = files.iter().fold(0_u64, |sum, (_, placed)| {
            sum.saturating_add(placed.look.size)
        });
        let _span = tracing::debug_span!("verify", files = files.len()).entered();
        let shown = output.map(|output| {
            let what = units::count(files.len() as u64, "placed file", "placed files");
            let bar = output.bar("Checking", what.clone());
            bar.start(Some(total), 0);
            (bar, output.ticker("Checking", what, [Some(total)]))
        });
        let read = |bytes| {
            if let Some((bar, ticker)) = &shown {
                bar.advance(bytes);
                ticker.advance(bytes);
            }
        };
        let path: place::FilePath<'_, (&String, &crate::project::state::Placed)> =
            &|(path, _)| std::borrow::Cow::Owned(native(root, path));
        let ahead = place::ReadAhead::when_cold(&files, path);
        let intact = place::run_hashes(
            &files,
            total,
            self.threads,
            ahead.as_ref(),
            |(path, placed)| {
                let file = crate::volume::read_unfollowed(&native(root, path))?;
                let block = Block::sized(placed.look.size.saturating_add(1));
                Sha256::of_reader_telling(file, block, &read).map(|digest| digest == placed.sha256)
            },
        );
        drop(shown);
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
            if linked && place::has_other_names(&native(root, &path)) {
                self.elsewhere.insert(path);
            } else {
                self.damaged.insert(path);
            }
        }
        Ok(found)
    }
}

/// each path of `missing` with the one listed spelling it folds to, when both name the same file.
/// nothing is folded or read again when no recorded spelling is missing.
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

/// the names in each folder read so far, by the folder's path on disk.
type Listings = HashMap<std::path::PathBuf, HashSet<std::ffi::OsString>>;

fn same_entry(root: &Path, path: &str, listed: &str, look: Look, listings: &mut Listings) -> bool {
    let Ok(metadata) = fs::symlink_metadata(native(root, path)) else {
        return false;
    };
    if !metadata.is_file() || Look::of(&metadata) != look {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let _ = listings;
        let Ok(other) = fs::symlink_metadata(native(root, listed)) else {
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

/// hashes the plain file at `path` through `block`, never following a link in its place nor
/// waiting on a fifo: what the walk saw as a file may have been swapped since.
pub(crate) fn hash_plain(path: &Path, block: Block) -> std::io::Result<Sha256> {
    Sha256::of_reader(crate::volume::read_unfollowed(path)?, block)
}

/// the coarsest clock step a data folder may keep: fat cuts a file's time down to two seconds.
const COARSE: u64 = 2_000_000_000;

/// the time before which a state written at `written` vouches for files that look as recorded.
/// a data folder on an `other_device` may keep a coarser clock, so there it is `COARSE` earlier.
pub(crate) fn vouched_before(written: u64, other_device: bool) -> u64 {
    if other_device {
        written.saturating_sub(COARSE)
    } else {
        written
    }
}

/// the files `state` records outside `data_dir`, placed before it changed, that are still plain
/// files in real folders, in order, unless they are a data folder file under another spelling.
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

/// the deepest folder holding all of `paths`, empty when that is the project itself.
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

/// whether `path` is a plain file in real folders, never links; `real` caches each folder's answer.
fn plain_in_real_folders(root: &Path, path: &str, real: &mut HashMap<String, bool>) -> bool {
    let folders_real = path.match_indices('/').all(|(end, _)| {
        let folder = &path[..end];
        *real.entry(folder.to_owned()).or_insert_with(|| {
            fs::symlink_metadata(native(root, folder))
                .is_ok_and(|found| found.is_dir() && !found.file_type().is_symlink())
        })
    });
    folders_real
        && fs::symlink_metadata(native(root, path))
            .is_ok_and(|found| crate::volume::is_plain(&found))
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
        && same_file::is_same_file(
            native(root, path),
            native(root, &format!("{data_dir}/{rest}")),
        )
        .unwrap_or(false)
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

#[derive(Debug, Default, PartialEq)]
struct Scanned {
    found: HashMap<String, Look>,
    /// the files under the hidden names placement stages files in.
    temporaries: Vec<String>,
    /// every other entry: folders, links and special files.
    entries: HashMap<String, Entry>,
    writable: HashSet<String>,
    /// the read-only files with no other name, as [`Survey`] keeps them. a hardlink that lost its
    /// other name stays read-only, while a dataset that asks for copies has writable ones, so
    /// those cost nothing to list.
    alone: String,
}

/// adds `path` to `alone`, a list of paths each ended by a nul.
fn push_alone(alone: &mut String, path: &str) {
    alone.push_str(path);
    alone.push('\0');
}

/// whether a file `metadata` describes is read-only with no other name. unix counts names in
/// every listing; a windows listing does not, so there the survey asks each hardlink.
fn read_only_alone(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() == 1 && metadata.permissions().readonly()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// lists the data folder, or with `within` only those folders and what stands on the way down to
/// them, never following links. interrupted once `stop` is asked, before the next folder.
fn scan(
    root: &Path,
    data_dir: &str,
    within: Option<&[String]>,
    threads: Threads,
    stop: &Stop,
) -> Result<Scanned, Error> {
    let span = tracing::debug_span!("scan", files = tracing::field::Empty).entered();
    let mut scanned = Scanned::default();
    let mut pending = Vec::new();
    let mut last_parent = None;
    let mut parents = 0;
    let mut entries = HashMap::new();
    let starts = match within {
        None => vec![(native(root, data_dir), data_dir.to_owned())],
        Some(folders) => folders
            .iter()
            .filter(|folder| way_down(root, data_dir, folder, &mut entries, &mut scanned))
            .map(|folder| (native(root, folder), folder.clone()))
            .collect(),
    };
    let walked = visit_files(
        starts,
        &mut entries,
        || stop.is_asked(),
        |entry, path, temporary, parent| {
            if last_parent != Some(parent) {
                // a DirEntry holds its parent's handle on unix, so distinct parents are bounded too
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
    if !walked {
        return Err(Stop::stopped());
    }
    scan_pending(&mut pending, threads, &mut scanned);
    scanned.entries = entries;
    span.record("files", scanned.found.len());
    Ok(scanned)
}

fn scan_pending(
    pending: &mut Vec<(fs::DirEntry, String, bool)>,
    threads: Threads,
    scanned: &mut Scanned,
) {
    let metadata = |(entry, _, _): &(fs::DirEntry, String, bool)| {
        entry.metadata().ok().map(|found| {
            (
                Look::of(&found),
                !found.permissions().readonly(),
                read_only_alone(&found),
            )
        })
    };
    let looks = if pending.len() < 512 {
        pending.iter().map(metadata).collect()
    } else {
        place::run_all(pending, threads, metadata)
    };
    for ((_, path, temporary), look) in pending.drain(..).zip(looks) {
        if let Some((look, writable, alone)) = look {
            if temporary {
                scanned.temporaries.push(path.clone());
            }
            if writable {
                scanned.writable.insert(path.clone());
            }
            if alone {
                push_alone(&mut scanned.alone, &path);
            }
            scanned.found.insert(path, look);
        }
    }
}

/// whether `folder`, inside `data_dir`, is reached through real folders as a whole scan would
/// reach it, recording what stands on the way as that scan would.
fn way_down(
    root: &Path,
    data_dir: &str,
    folder: &str,
    entries: &mut HashMap<String, Entry>,
    scanned: &mut Scanned,
) -> bool {
    let Some(below) = folder.strip_prefix(data_dir).and_then(|rest| {
        if rest.is_empty() {
            Some(None)
        } else {
            rest.strip_prefix('/').map(Some)
        }
    }) else {
        return false;
    };
    let Some(below) = below else {
        return true;
    };
    let mut path = data_dir.to_owned();
    for part in below.split('/') {
        path.push('/');
        path.push_str(part);
        let Ok(found) = fs::symlink_metadata(native(root, &path)) else {
            return false;
        };
        let kind = found.file_type();
        if kind.is_dir() {
            entries.insert(path.clone(), Entry::Folder);
            continue;
        }
        if kind.is_file() {
            if place::is_copy_temporary(part) {
                scanned.temporaries.push(path.clone());
            }
            if !found.permissions().readonly() {
                scanned.writable.insert(path.clone());
            }
            if read_only_alone(&found) {
                push_alone(&mut scanned.alone, &path);
            }
            scanned.found.insert(path, Look::of(&found));
        } else if kind.is_symlink() {
            entries.insert(path, Entry::Link);
        } else {
            entries.insert(path, Entry::Special);
        }
        return false;
    }
    true
}

/// compares paths with an implicit final `/`, so `a-` cannot separate `a` from `a/x`.
fn folder_order(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let end = left.len().min(right.len());
    left[..end]
        .cmp(&right[..end])
        .then_with(|| match (left.get(end), right.get(end)) {
            (Some(left), None) => left.cmp(&b'/').then(Ordering::Greater),
            (None, Some(right)) => b'/'.cmp(right).then(Ordering::Less),
            _ => Ordering::Equal,
        })
}

/// `folders` inside `data_dir` without those inside another, which listing that one covers.
fn outermost(data_dir: &str, folders: &[String]) -> Vec<String> {
    if folders.iter().any(|folder| folder == data_dir) {
        return vec![data_dir.to_owned()];
    }
    let mut sorted: Vec<&String> = folders.iter().collect();
    sorted.sort_unstable_by(|left, right| folder_order(left, right));
    sorted.dedup_by(|folder, parent| is_inside(folder, parent));
    sorted.into_iter().cloned().collect()
}

/// whether `path` is in the disjoint, ordered `folders` returned by `outermost`.
fn inside_any(path: &str, folders: &[String]) -> bool {
    match folders {
        [] => false,
        [folder] => is_inside(path, folder),
        _ => {
            let end = folders.partition_point(|folder| folder_order(folder, path).is_le());
            end.checked_sub(1)
                .is_some_and(|at| is_inside(path, &folders[at]))
        }
    }
}

/// whether `path` is `folder` or inside it.
pub(crate) fn is_inside(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// lists `folders`, each by its path on disk and in the project, and every folder below, handing
/// each file to `visit` and keeping the other entries in `others`. false when `stopped` ended it.
fn visit_files(
    mut folders: Vec<(std::path::PathBuf, String)>,
    others: &mut HashMap<String, Entry>,
    stopped: impl Fn() -> bool,
    mut visit: impl FnMut(fs::DirEntry, String, bool, usize),
) -> bool {
    let mut parent = 0;
    while let Some((folder, relative)) = folders.pop() {
        if stopped() {
            return false;
        }
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
    true
}

/// hashes the objects behind hardlinks someone edited, before anything downloads, so a changed
/// object is fetched again rather than spread. a link in `elsewhere` marks its object, or, when
/// it is no longer the object, its own copy, so other projects linking that copy check it too.
pub(crate) fn audit(project: &Project, survey: &Survey, cache: &Cache) -> Result<(), Error> {
    let _span = tracing::debug_span!("audit").entered();
    for (path, placed) in &survey.state.files {
        if placed.method != Method::Hardlink {
            continue;
        }
        let file = native(&project.root, path);
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
mod tests;
