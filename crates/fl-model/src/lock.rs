//! The lock file, `data.lock`: exactly what a project resolved, written only by fetchloom.

use std::ops::Range;

use crate::digest::Digest;
use crate::error::ErrorKind;
use crate::name::Name;
use crate::path::DataPath;
use crate::reference::Reference;
use crate::timestamp::Timestamp;
use crate::tree;
use crate::units::count;

mod read;
mod write;

/// The most files a dataset lists inline; a larger listing is stored as a tree object instead.
pub const INLINE_FILES_MAX: usize = 10_000;

const RECORDABLE: u64 = i64::MAX.unsigned_abs();

/// A validated lock: datasets and steps sorted by name, files sorted by path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lock {
    datasets: Vec<LockedDataset>,
    steps: Vec<LockedStep>,
}

/// One `[[dataset]]` table: what a dataset entry resolved to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockedDataset {
    /// The dataset's name in `data.toml`.
    pub name: Name,
    /// The reference as written in `data.toml`.
    pub reference: Reference,
    /// The hash of the dataset's `data.toml` entry when it was locked.
    pub spec: Digest,
    /// The exact version the reference resolved to.
    pub resolved: Reference,
    /// The title the source states.
    pub title: Option<String>,
    /// The license the source states, as SPDX.
    pub license: Option<String>,
    /// The DOI the source states.
    pub doi: Option<String>,
    /// When the listing was retrieved.
    pub retrieved: Timestamp,
    /// The tree hash of the dataset's files, archives left out.
    pub tree: Digest,
    /// The files, when listed inline; `None` when the listing is a tree object in the store.
    pub files: Option<Vec<LockedFile>>,
}

/// One file of a locked dataset.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockedFile {
    /// Where the file is inside the dataset.
    pub path: DataPath,
    /// Its size in bytes.
    pub size: u64,
    /// Its BLAKE3 digest, always present.
    pub blake3: [u8; 32],
    /// The SHA-256 digest the publisher states.
    pub sha256: Option<[u8; 32]>,
    /// The SHA-1 digest the publisher states.
    pub sha1: Option<[u8; 20]>,
    /// The MD5 digest the publisher states.
    pub md5: Option<[u8; 16]>,
    /// Every known location, origin first.
    pub at: Vec<String>,
    /// The archive this file came out of, named by its BLAKE3 digest.
    pub from: Option<Digest>,
    /// What the file is to the dataset, when it is not an ordinary file.
    pub role: Option<Role>,
}

/// What a listed file is to its dataset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// An archive whose members are listed with `from`; it is not part of the tree.
    Archive,
}

/// One `[[step]]` table: the result of a step run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockedStep {
    /// The step's name in `data.toml`.
    pub name: Name,
    /// The step key the result was produced under.
    pub key: Digest,
    /// How many items a `foreach` step ran over.
    pub items: Option<u64>,
    /// The tree hash of the step's output.
    pub tree: Digest,
}

impl Lock {
    /// Validates and sorts a lock: datasets and steps by name, files by path.
    ///
    /// # Errors
    ///
    /// Returns [`LockError`] for a name used twice or by both a dataset and a step, a path listed
    /// twice in one dataset, more than [`INLINE_FILES_MAX`] inline files, a size or item count
    /// above what a TOML integer holds, a `from` naming no archive of its dataset, or a stored
    /// tree that differs from the hash of the inline files.
    pub fn new(
        mut datasets: Vec<LockedDataset>,
        mut steps: Vec<LockedStep>,
    ) -> Result<Self, LockError> {
        sort_by_key(&mut datasets, |dataset| &dataset.name);
        sort_by_key(&mut steps, |step| &step.name);
        if let Some(pair) = datasets
            .windows(2)
            .find(|pair| pair[0].name == pair[1].name)
        {
            return Err(LockError::plain(format!(
                "dataset `{}` appears twice",
                pair[0].name
            )));
        }
        if let Some(pair) = steps.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(LockError::plain(format!(
                "step `{}` appears twice",
                pair[0].name
            )));
        }
        for step in &steps {
            if datasets
                .binary_search_by(|dataset| dataset.name.cmp(&step.name))
                .is_ok()
            {
                return Err(LockError::plain(format!(
                    "`{}` names both a dataset and a step",
                    step.name
                )));
            }
            if step.items.is_some_and(|items| items > RECORDABLE) {
                return Err(LockError::plain(format!(
                    "step `{}` has more items than a lock can record",
                    step.name
                )));
            }
        }
        for dataset in &mut datasets {
            if let Some(files) = &mut dataset.files {
                check_files(&dataset.name, &dataset.tree, files)?;
            }
        }
        Ok(Self { datasets, steps })
    }

    /// Reads and validates `data.lock` text. A leading byte order mark, CRLF line endings and
    /// entries in any order are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`LockError`] for invalid TOML, an unknown key, a `version` other than 1, a value
    /// of the wrong form, or anything [`Lock::new`] refuses.
    pub fn from_toml(text: &str) -> Result<Self, LockError> {
        let (datasets, steps) = read::read(text)?;
        Self::new(datasets, steps)
    }

    /// The lock as `data.lock` text: the same lock always gives the same bytes.
    #[must_use]
    pub fn to_toml(&self) -> String {
        write::to_toml(self)
    }

    /// The datasets, sorted by name.
    #[must_use]
    pub fn datasets(&self) -> &[LockedDataset] {
        &self.datasets
    }

    /// The steps, sorted by name.
    #[must_use]
    pub fn steps(&self) -> &[LockedStep] {
        &self.steps
    }
}

fn sort_by_key<T, K: Ord>(items: &mut [T], key: impl Fn(&T) -> &K) {
    if !items.is_sorted_by(|a, b| key(a) <= key(b)) {
        items.sort_unstable_by(|a, b| key(a).cmp(key(b)));
    }
}

fn check_files(name: &Name, stored: &Digest, files: &mut [LockedFile]) -> Result<(), LockError> {
    if files.len() > INLINE_FILES_MAX {
        return Err(LockError::plain(format!(
            "dataset `{name}` lists {} inline, a lock holds at most {}",
            count(files.len() as u64, "file", "files"),
            count(INLINE_FILES_MAX as u64, "file", "files"),
        )));
    }
    sort_by_key(files, |file| &file.path);
    if let Some(pair) = files.windows(2).find(|pair| pair[0].path == pair[1].path) {
        return Err(LockError::plain(format!(
            "path `{}` appears twice in dataset `{name}`",
            pair[0].path
        )));
    }
    let mut archives: Vec<Digest> = files
        .iter()
        .filter(|file| file.role == Some(Role::Archive))
        .map(|file| Digest::Blake3(file.blake3))
        .collect();
    archives.sort_unstable();
    for file in files.iter() {
        if file.size > RECORDABLE {
            return Err(LockError::plain(format!(
                "file `{}` of dataset `{name}` is larger than a lock can record",
                file.path
            )));
        }
        if let Some(from) = &file.from
            && archives.binary_search(from).is_err()
        {
            return Err(LockError::plain(format!(
                "file `{}` of dataset `{name}` comes from {from}, which is no archive of the dataset",
                file.path
            )));
        }
    }
    let computed = tree::hash_sorted(
        files
            .iter()
            .filter(|file| file.role.is_none())
            .map(|file| (&file.path, file.size, &file.blake3)),
    );
    if computed != *stored {
        return Err(LockError::plain(format!(
            "dataset `{name}` records tree {stored} but its files hash to {computed}"
        )));
    }
    Ok(())
}

/// Why `data.lock` could not be read or a lock could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LockError {
    /// What is wrong, in one sentence.
    pub message: String,
    /// The byte range of `data.lock` it points at, when known.
    pub span: Option<Range<usize>>,
    /// What may fix it, such as the closest valid key.
    pub help: Option<String>,
}

impl LockError {
    /// The kind this error is reported as.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::ProjectInvalidLock
    }

    fn plain(message: String) -> Self {
        Self {
            message,
            span: None,
            help: None,
        }
    }
}

#[cfg(test)]
mod tests;
