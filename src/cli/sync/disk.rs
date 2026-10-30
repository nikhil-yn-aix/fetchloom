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
use crate::place::{self, Method};
use crate::project::Project;
use crate::project::state::{Look, State, nanos};

/// the data folder as fetchloom recorded placing it and as it is now.
pub(crate) struct Survey {
    /// the scan's start, so marks written while placing files still put them in doubt.
    pub(crate) started: u64,
    pub(crate) state: State,
    /// every file under the data folder and how it looks, by its path in the project.
    pub(crate) disk: HashMap<String, Look>,
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
    /// them for later runs.
    pub(crate) needs_writing: bool,
}

impl Survey {
    /// reads the state, lists the data folder, and hashes the files the state is too new to
    /// vouch for.
    pub(crate) fn read(project: &Project, data_dir: &str) -> Survey {
        let started = nanos(SystemTime::now());
        let (state, written) = project.state();
        let mut survey = Survey {
            started,
            state,
            disk: scan(&project.root, data_dir),
            elsewhere: HashSet::new(),
            vouched: HashSet::new(),
            edited: HashSet::new(),
            needs_writing: false,
        };
        survey.hash_racy(&project.root, written);
        survey
    }

    /// hashes each file that looks as recorded while its recorded time is not older than the
    /// state's own, `written`. a coarse clock, such as FAT's two seconds, gives a file edited in
    /// the tick it was placed the size and time recorded for it, and both times come from that
    /// clock. git treats its racily clean index entries the same way.
    fn hash_racy(&mut self, root: &Path, written: u64) {
        let racy: Vec<(&String, &Sha256)> = self
            .state
            .files
            .iter()
            .filter(|(path, placed)| {
                placed.look.modified >= written && self.disk.get(*path) == Some(&placed.look)
            })
            .map(|(path, placed)| (path, &placed.sha256))
            .collect();
        if racy.is_empty() {
            return;
        }
        let _span = tracing::debug_span!("hash_racy", files = racy.len()).entered();
        let unchanged = place::run_all(&racy, |(path, sha256)| {
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
        let intact = place::run_all(&doubted, |path| {
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
