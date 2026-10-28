//! what the data folder holds, read by listing its folders, and the objects behind hardlinks
//! someone edited.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::place::local;
use crate::cache::Cache;
use crate::error::Error;
use crate::place::Method;
use crate::project::Project;
use crate::project::manifest::Manifest;
use crate::project::state::{Look, Placed};

/// every file under the data folder and how it looks, by its path in the project. links are
/// neither followed nor touched.
pub(crate) fn scan(root: &Path, data_dir: &str) -> HashMap<String, Look> {
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
    found
}

/// hashes the objects behind hardlinks someone edited, before anything downloads. an edit
/// through a hardlink also changes the cached object, which is then dropped and fetched again,
/// so it never spreads to other projects.
pub(crate) fn audit(project: &Project, manifest: &Manifest, cache: &Cache) -> Result<(), Error> {
    let state = project.state();
    let edited: Vec<&Placed> = state
        .files
        .values()
        .filter(|placed| placed.method == Method::Hardlink)
        .collect();
    if edited.is_empty() {
        return Ok(());
    }
    let disk = scan(&project.root, &manifest.data_dir);
    for (path, placed) in &state.files {
        if placed.method == Method::Hardlink
            && disk.get(path).is_some_and(|seen| *seen != placed.look)
            && !cache
                .verify(&placed.sha256)
                .map_err(|err| local(&project.root.join(path), &err))?
        {
            tracing::debug!(%path, "an edit through a hardlink changed its cached object");
        }
    }
    Ok(())
}
