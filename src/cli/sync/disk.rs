//! what the data folder holds, read once per command by listing its folders, and the objects
//! behind hardlinks someone edited.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::place::local;
use crate::cache::Cache;
use crate::error::Error;
use crate::place::Method;
use crate::project::Project;
use crate::project::state::{Look, State};

/// the data folder as fetchloom recorded placing it and as it is now.
pub(crate) struct Survey {
    pub(crate) state: State,
    /// every file under the data folder and how it looks, by its path in the project.
    pub(crate) disk: HashMap<String, Look>,
    /// files placed as hardlinks whose bytes were changed through another project's link, which
    /// sync puts back without --force.
    pub(crate) elsewhere: HashSet<String>,
}

impl Survey {
    /// reads the state and lists the data folder.
    pub(crate) fn read(project: &Project, data_dir: &str) -> Survey {
        Survey {
            state: project.state(),
            disk: scan(&project.root, data_dir),
            elsewhere: HashSet::new(),
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
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// reads hardlinks again by name, which windows answers from the file itself where a listing
    /// may be stale: those edited elsewhere, and with `force` every one, since --force asks for
    /// every edit to be undone. what changed is remembered, for audit and for placing.
    pub(crate) fn look_again(&mut self, root: &Path, cache: &Cache, force: bool) {
        let paths = if force && cfg!(windows) {
            self.state
                .files
                .iter()
                .filter(|(_, placed)| placed.method == Method::Hardlink)
                .map(|(path, _)| path.clone())
                .collect()
        } else {
            self.edited_elsewhere(cache)
        };
        for path in paths {
            let Some(placed) = self.state.files.get(&path) else {
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(root.join(&path)) else {
                continue;
            };
            let look = Look::of(&metadata);
            if look != placed.look && !force {
                self.elsewhere.insert(path.clone());
            }
            self.disk.insert(path, look);
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
            && survey
                .disk
                .get(path)
                .is_some_and(|seen| *seen != placed.look)
            && !cache
                .verify(&placed.sha256)
                .map_err(|err| local(&project.root.join(path), &err))?
        {
            tracing::debug!(%path, "an edit through a hardlink changed its cached object");
        }
    }
    Ok(())
}
