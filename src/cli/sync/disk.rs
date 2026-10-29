//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::place::local;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::Error;
use crate::place::{self, Method};
use crate::project::Project;
use crate::project::state::{Look, State};

/// the data folder as fetchloom recorded placing it and as it is now.
pub(crate) struct Survey {
    pub(crate) state: State,
    /// every file under the data folder and how it looks, by its path in the project.
    pub(crate) disk: HashMap<String, Look>,
    /// files placed as hardlinks whose object the cache marked as changed after they were placed,
    /// which sync puts back with or without --force.
    pub(crate) elsewhere: HashSet<String>,
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
        let (state, written) = project.state();
        let mut survey = Survey {
            state,
            disk: scan(&project.root, data_dir),
            elsewhere: HashSet::new(),
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

    /// files placed as hardlinks whose object the cache found changed after they were placed,
    /// while their listing here still shows them as placed. on windows a folder's listing keeps
    /// the old size and time of a hardlink changed through another name, so this is the only
    /// sign of an edit made through another project.
    pub(crate) fn edited_elsewhere(&self, cache: &Cache) -> Vec<String> {
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
                        .is_some_and(|found| *found > placed.look.modified)
                    && self.disk.get(*path) == Some(&placed.look)
                    && !self.edited.contains(*path)
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// remembers the files edited elsewhere, then reads hardlinks again by name, which windows
    /// answers from the file itself where a listing may be stale: those edited elsewhere, and with
    /// `force` every one, since --force asks for every edit to be undone.
    pub(crate) fn look_again(&mut self, root: &Path, cache: &Cache, force: bool) {
        self.elsewhere = self.edited_elsewhere(cache).into_iter().collect();
        let paths: Vec<String> = if force && cfg!(windows) {
            self.state
                .files
                .iter()
                .filter(|(_, placed)| placed.method == Method::Hardlink)
                .map(|(path, _)| path.clone())
                .collect()
        } else {
            self.elsewhere.iter().cloned().collect()
        };
        for path in paths {
            if let Ok(metadata) = fs::symlink_metadata(root.join(&path)) {
                self.disk.insert(path, Look::of(&metadata));
            }
        }
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
