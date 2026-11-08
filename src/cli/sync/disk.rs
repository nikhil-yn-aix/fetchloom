//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

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
    /// hardlinks a mark put in doubt whose bytes hashing found changed: edited through another
    /// project's link. sync puts them back with or without --force.
    pub(crate) elsewhere: HashSet<String>,
    /// hardlinks a mark put in doubt whose bytes hashing found intact, which sync keeps, recording
    /// the scan's start.
    pub(crate) vouched: HashSet<String>,
    /// files that still look as recorded but hold other bytes, which hashing found among those
    /// as new as the state itself.
    pub(crate) edited: HashSet<String>,
    /// whether hashing found files as new as the state unchanged, so a state written now settles
    /// them for later runs, or a record moved to another spelling of its path.
    pub(crate) needs_writing: bool,
    /// files under the hidden names placement stages files in, by their paths in the project.
    temporaries: Vec<String>,
}

impl Survey {
    /// reads the state, lists the data folder, and hashes the files the state is too new to
    /// vouch for.
    pub(crate) fn read(project: &Project, data_dir: &str, threads: Threads) -> Survey {
        let started = nanos(SystemTime::now());
        let (state, written) = project.state();
        let (disk, temporaries) = scan(&project.root, data_dir, threads);
        let mut survey = Survey {
            threads,
            started,
            state,
            disk,
            listed: HashMap::new(),
            elsewhere: HashSet::new(),
            vouched: HashSet::new(),
            edited: HashSet::new(),
            needs_writing: false,
            temporaries,
        };
        let other_device = !survey.state.files.is_empty()
            && apart(&project.root.join(data_dir), &project.own_folder());
        survey.hash_racy(&project.root, written, other_device);
        // after hash_racy, so a record found under another spelling is looked through too
        survey.look_through_links(&project.root);
        survey
    }

    /// on windows, a folder's listing keeps a hardlinked file's size and time as they were when
    /// this link last opened it, so an edit made through another project's link does not show.
    /// opening the file reads them as they are, for the files placed as hardlinks only.
    #[cfg(windows)]
    fn look_through_links(&mut self, root: &Path) {
        for (path, placed) in &self.state.files {
            if placed.method == Method::Hardlink
                && let Some(seen) = self.disk.get_mut(path)
                && let Ok(metadata) = fs::metadata(root.join(path))
            {
                *seen = Look::of(&metadata);
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
        let written = if other_device {
            written.saturating_sub(COARSE)
        } else {
            written
        };
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
            if let Some(record) = record {
                self.state.files.remove(&previous);
                self.state.files.insert(path.clone(), record);
                for paths in [&mut self.edited, &mut self.elsewhere, &mut self.vouched] {
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
        for (path, intact) in doubted.into_iter().zip(intact) {
            match intact {
                Ok(true) => {
                    self.vouched.insert(path);
                }
                Ok(false) => {
                    self.elsewhere.insert(path);
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

/// whether `a` and `b` sit on different devices, whose filesystems may keep times to different
/// steps. folders that cannot be read count as apart.
fn apart(a: &Path, b: &Path) -> bool {
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

/// every file under the data folder and how it looks, by its path in the project, and the paths
/// of those under the hidden names placement stages files in. links are neither followed nor
/// touched.
fn scan(root: &Path, data_dir: &str, threads: Threads) -> (HashMap<String, Look>, Vec<String>) {
    let span = tracing::debug_span!("scan", files = tracing::field::Empty).entered();
    let mut found = HashMap::new();
    let mut temporaries = Vec::new();
    let mut pending = Vec::new();
    let mut last_parent = None;
    let mut parents = 0;
    visit_files(root, data_dir, |entry, path, temporary, parent| {
        if last_parent != Some(parent) {
            // DirEntry retains its parent handle on unix; bound distinct parents too.
            if parents == 8 {
                scan_pending(&mut pending, threads, &mut found, &mut temporaries);
                parents = 0;
            }
            parents += 1;
            last_parent = Some(parent);
        }
        pending.push((entry, path, temporary));
        if pending.len() == 4096 {
            scan_pending(&mut pending, threads, &mut found, &mut temporaries);
            parents = 0;
            last_parent = None;
        }
    });
    scan_pending(&mut pending, threads, &mut found, &mut temporaries);
    span.record("files", found.len());
    (found, temporaries)
}

fn scan_pending(
    pending: &mut Vec<(fs::DirEntry, String, bool)>,
    threads: Threads,
    found: &mut HashMap<String, Look>,
    temporaries: &mut Vec<String>,
) {
    let metadata =
        |(entry, _, _): &(fs::DirEntry, String, bool)| entry.metadata().ok().as_ref().map(Look::of);
    let looks = if pending.len() < 512 {
        pending.iter().map(metadata).collect()
    } else {
        place::run_all(pending, threads, metadata)
    };
    for ((_, path, temporary), look) in pending.drain(..).zip(looks) {
        if let Some(look) = look {
            if temporary {
                temporaries.push(path.clone());
            }
            found.insert(path, look);
        }
    }
}

fn visit_files(
    root: &Path,
    data_dir: &str,
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
                folders.push((entry.path(), path));
            } else if kind.is_file() {
                let temporary = place::is_copy_temporary(&name);
                visit(entry, path, temporary, parent);
            }
        }
    }
}

/// hashes the objects behind hardlinks someone edited, before anything downloads. an edit
/// through a hardlink also changes the cached object, which is then dropped and fetched again,
/// so it never spreads to other projects.
pub(crate) fn audit(project: &Project, survey: &Survey, cache: &Cache) -> Result<(), Error> {
    let _span = tracing::debug_span!("audit").entered();
    for (path, placed) in &survey.state.files {
        if placed.method == Method::Hardlink
            && (survey.edited.contains(path)
                || survey
                    .disk
                    .get(path)
                    .is_some_and(|seen| *seen != placed.look))
            && !cache
                .verify(&placed.sha256)
                .map_err(|err| local(&project.root.join(path), &err))?
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
        let (found, _) = scan(dir.path(), "data", Threads::default());
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
        assert_eq!(expected.0.len(), 513);
        assert_eq!(expected.1, ["data/.fetchloom-0123456789abcdef.copy"]);
        for count in ["1", "8"] {
            let threads = Threads::read(&|_| Some(count.into())).unwrap();
            let actual = scan(root, "data", threads);
            assert_eq!(actual, expected);
            assert_eq!(scan(root, "absent", threads), (HashMap::new(), Vec::new()));
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
            locked: &locked,
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
            locked,
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
        let (files, leftovers) = scan(dir.path(), "data", Threads::default());
        assert_eq!(files.len(), 1);
        assert_eq!(files["data/nested/kept"].size, 6);
        assert_eq!(leftovers, Vec::<String>::new());
        assert_eq!(scan(dir.path(), "missing", Threads::default()).0.len(), 0);
    }
}
