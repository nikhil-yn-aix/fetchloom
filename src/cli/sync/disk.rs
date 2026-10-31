//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use super::place::local;
use crate::cache::Cache;
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
}

impl Survey {
    /// reads the state, lists the data folder, and hashes the files the state is too new to
    /// vouch for.
    pub(crate) fn read(project: &Project, data_dir: &str, threads: Threads) -> Survey {
        let started = nanos(SystemTime::now());
        let (state, written) = project.state();
        let mut survey = Survey {
            threads,
            started,
            state,
            disk: scan(&project.root, data_dir),
            listed: HashMap::new(),
            elsewhere: HashSet::new(),
            vouched: HashSet::new(),
            edited: HashSet::new(),
            needs_writing: false,
        };
        survey.hash_racy(&project.root, written);
        survey
    }

    /// reconciles missing recorded names, then hashes files whose recorded time is not older than the
    /// state's own, `written`. a coarse clock, such as FAT's two seconds, gives a file edited in
    /// the tick it was placed the size and time recorded for it, and both times come from that
    /// clock. git treats its racily clean index entries the same way.
    fn hash_racy(&mut self, root: &Path, written: u64) {
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
        let unchanged = place::run_all(&racy, self.threads, |(path, sha256)| {
            Sha256::of_file(&root.join(path)).is_ok_and(|digest| digest == **sha256)
        });
        for ((path, _), unchanged) in racy.into_iter().zip(unchanged) {
            if unchanged {
                self.needs_writing = true;
            } else {
                self.edited.insert(path.clone());
            }
        }
    }

    /// carries a placed record to a wanted alias before decisions or removals can treat the
    /// two spellings as separate files. edits and doubts still belong to that same file.
    pub(crate) fn retarget(&mut self, root: &Path, targets: &[Target]) {
        if self.state.files.is_empty() {
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
            let Some(record) = self.state.files.get(&previous).cloned() else {
                continue;
            };
            let Some(look) = self.disk.remove(&previous) else {
                continue;
            };
            self.disk.insert(path.clone(), look);
            self.state.files.remove(&previous);
            self.state.files.insert(path.clone(), record);
            for paths in [&mut self.edited, &mut self.elsewhere, &mut self.vouched] {
                if paths.remove(&previous) {
                    paths.insert(path.clone());
                }
            }
            let listed = self.listed.remove(&previous).unwrap_or(previous);
            self.listed.insert(path.clone(), listed);
            self.needs_writing = true;
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

    /// files placed as hardlinks whose object the cache marked as changed after fetchloom last
    /// knew them to hold their bytes, while their listing still shows them as placed. on windows a
    /// folder's listing keeps the old size and time of a hardlink changed through another name, so
    /// the mark is the only sign of an edit made through another project. the link may also name
    /// an older copy of the object than the one that changed, so only hashing settles it.
    pub(crate) fn doubted(&self, cache: &Cache) -> Vec<String> {
        let changed = cache.changed();
        if changed.is_empty() {
            return Vec::new();
        }
        self.state
            .files
            .iter()
            .filter(|(path, placed)| {
                placed.method == Method::Hardlink
                    && changed
                        .get(&placed.sha256)
                        .is_some_and(|found| *found > placed.look.modified.max(placed.checked))
                    && self.disk.get(*path) == Some(&placed.look)
                    && !self.edited.contains(*path)
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// hashes the files a mark puts in doubt, separating changed links from intact ones.
    /// an unreadable file stops the check.
    pub(crate) fn check_doubted(&mut self, root: &Path, cache: &Cache) -> Result<(), Error> {
        let doubted = self.doubted(cache);
        if doubted.is_empty() {
            return Ok(());
        }
        let _span = tracing::debug_span!("hash_doubted", files = doubted.len()).entered();
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

    /// hashes what a mark puts in doubt, then reads hardlinks again by name, which windows answers
    /// from the file itself where a listing may be stale: those in doubt, and with `force` every
    /// one, since --force asks for every edit to be undone.
    pub(crate) fn look_again(
        &mut self,
        root: &Path,
        cache: &Cache,
        force: bool,
    ) -> Result<(), Error> {
        self.check_doubted(root, cache)?;
        let paths: Vec<String> = if force && cfg!(windows) {
            self.state
                .files
                .iter()
                .filter(|(_, placed)| placed.method == Method::Hardlink)
                .map(|(path, _)| path.clone())
                .collect()
        } else {
            self.elsewhere
                .iter()
                .chain(&self.vouched)
                .cloned()
                .collect()
        };
        for path in paths {
            if let Ok(metadata) = fs::symlink_metadata(root.join(&path)) {
                self.disk.insert(path, Look::of(&metadata));
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
    if missing.is_empty() {
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

/// every file under the data folder and how it looks, by its path in the project. links are
/// neither followed nor touched.
fn scan(root: &Path, data_dir: &str) -> HashMap<String, Look> {
    let span = tracing::debug_span!("scan", files = tracing::field::Empty).entered();
    let mut found = HashMap::new();
    let mut folders = vec![(root.join(data_dir), data_dir.to_owned())];
    while let Some((folder, relative)) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = format!("{relative}/{}", entry.file_name().to_string_lossy());
            if kind.is_dir() {
                folders.push((entry.path(), path));
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
            {
                found.insert(path, Look::of(&metadata));
            }
        }
    }
    span.record("files", found.len());
    found
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
        survey.hash_racy(&project.root, 0);
        assert_eq!(survey.edited, HashSet::from(["data/changed".to_owned()]));
        assert!(survey.needs_writing);
        survey.smudge_edited();
        assert_eq!(survey.state.files["data/changed"].look.size, u64::MAX);
        assert_eq!(survey.state.files["data/intact"].look.size, 8);
        assert_eq!(fs::read(path).unwrap(), b"modified");
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
        assert_eq!(survey.doubted(&cache).len(), 2);
        survey.look_again(&project.root, &cache, false).unwrap();
        assert_eq!(survey.elsewhere, HashSet::from(["data/changed".to_owned()]));
        assert_eq!(survey.vouched, HashSet::from(["data/intact".to_owned()]));
        assert_eq!(survey.disk["data/changed"].size, 28);
        assert_eq!(survey.disk["data/intact"].size, 6);
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

    #[cfg(windows)]
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
        survey.hash_racy(&project.root, 0);
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
            dataset: "set".to_owned(),
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
        let files = scan(dir.path(), "data");
        assert_eq!(files.len(), 1);
        assert_eq!(files["data/nested/kept"].size, 6);
        assert_eq!(scan(dir.path(), "missing").len(), 0);
    }
}
