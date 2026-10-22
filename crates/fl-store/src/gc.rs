//! Garbage collection: keep what registered locks need, remove the rest once it is old enough.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fl_model::digest::Digest;
use fl_model::lock::Lock;
use fl_model::tree::Tree;

use crate::error::{StoreError, io};
use crate::files;
use crate::layout::{entries, object_digest};
use crate::tier::Tier;

/// How a collection runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Collect {
    /// The present moment, against which ages are measured.
    pub now: SystemTime,
    /// Nothing younger than this is removed, so a sync running at the same moment is safe.
    pub grace: Duration,
    /// List what would be removed and remove nothing.
    pub dry_run: bool,
}

/// What a collection removed, or would remove in a dry run.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Collected {
    /// Lock files that are gone, whose roots were dropped.
    pub dropped_roots: Vec<PathBuf>,
    /// Files removed from the tier: objects, trees, step records, index entries, leftovers in
    /// `tmp/` and unfinished downloads.
    pub removed: Vec<PathBuf>,
    /// The bytes those files held.
    pub bytes: u64,
}

#[derive(Default)]
struct Marked {
    objects: HashSet<Digest>,
    names: HashSet<PathBuf>,
}

impl Tier {
    /// Drops the roots whose lock file is gone, marks everything the other locks need, and
    /// removes every object, tree, step record, `tmp/` file and unfinished download that is
    /// unmarked and older than the grace period.
    ///
    /// Anything that cannot be read stops the collection before a file is removed: a lock, or a
    /// tree object a dataset lists its files in.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidLock`] for a lock that does not parse,
    /// [`StoreError::Tree`] for a tree object that is missing or damaged, and
    /// [`StoreError::Io`] when a file cannot be read or removed.
    pub fn collect(&self, options: &Collect) -> Result<Collected, StoreError> {
        let span = tracing::debug_span!(
            "gc",
            tier = %self.layout().root().display(),
            dry_run = options.dry_run,
            removed = tracing::field::Empty,
            bytes = tracing::field::Empty,
            dropped_roots = tracing::field::Empty,
        )
        .entered();
        let mut collected = Collected::default();
        let mut marked = Marked::default();
        let mut dead = Vec::new();
        for root in self.roots()? {
            if root.is_dead() {
                collected.dropped_roots.push(root.lock.clone());
                dead.push(root);
            } else {
                self.mark(&root.lock, &mut marked)?;
            }
        }
        let mut doomed = Vec::new();
        for fan in entries(&self.layout().dir("objects"))? {
            for object in entries(&fan)? {
                if !marked.objects.contains(&object_digest(&fan, &object)?) {
                    doomed.push(object);
                }
            }
        }
        for dir in ["trees", "steps"] {
            let found = entries(&self.layout().dir(dir))?;
            doomed.extend(
                found
                    .into_iter()
                    .filter(|path| !marked.names.contains(path)),
            );
        }
        doomed.extend(entries(&self.layout().dir("tmp"))?);
        let mut judged: Vec<(PathBuf, PathBuf)> = doomed
            .into_iter()
            .map(|path| (path.clone(), path))
            .collect();
        for path in entries(&self.layout().dir("partial"))? {
            let part = path.with_extension("part");
            let by = if fs::symlink_metadata(&part).is_ok() {
                part
            } else {
                path.clone()
            };
            judged.push((path, by));
        }
        for (path, by) in judged {
            if let Some(bytes) = old(&path, &by, options)? {
                collected.bytes += bytes;
                collected.removed.push(path);
            }
        }
        let dead_entries = {
            let removed: HashSet<&Path> = collected.removed.iter().map(PathBuf::as_path).collect();
            self.dead_entries(&removed)?
        };
        for (path, bytes) in dead_entries {
            collected.bytes += bytes;
            collected.removed.push(path);
        }
        collected.removed.sort_unstable();
        span.record("removed", collected.removed.len());
        span.record("bytes", collected.bytes);
        span.record("dropped_roots", collected.dropped_roots.len());
        if !options.dry_run {
            for root in &dead {
                self.drop_root(root)?;
            }
            for path in &collected.removed {
                files::remove(path)?;
            }
        }
        Ok(collected)
    }

    fn mark(&self, lock: &Path, marked: &mut Marked) -> Result<(), StoreError> {
        let text = fs::read_to_string(lock).map_err(io("read", lock))?;
        let parsed = Lock::from_toml(&text).map_err(|source| StoreError::InvalidLock {
            lock: lock.to_owned(),
            source,
        })?;
        for dataset in parsed.datasets() {
            if let Some(files) = &dataset.files {
                marked
                    .objects
                    .extend(files.iter().map(|file| Digest::Blake3(file.blake3)));
            } else {
                let tree = self
                    .read_tree(&dataset.tree)?
                    .ok_or_else(|| StoreError::Tree {
                        tree: dataset.tree,
                        reason: format!("is missing, and `{}` needs it", lock.display()),
                    })?;
                marked.mark_tree(self.layout().named("trees", &dataset.tree), &tree);
            }
        }
        for step in parsed.steps() {
            marked.names.insert(self.layout().named("steps", &step.key));
            if let Some(tree) = self.read_tree(&step.tree)? {
                marked.mark_tree(self.layout().named("trees", &step.tree), &tree);
            }
        }
        Ok(())
    }

    fn read_tree(&self, digest: &Digest) -> Result<Option<Tree>, StoreError> {
        let path = self.layout().named("trees", digest);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io("read", &path)(error)),
        };
        let damaged = |reason: String| StoreError::Tree {
            tree: *digest,
            reason,
        };
        let tree = Tree::decode(&bytes).map_err(|error| damaged(error.to_string()))?;
        if tree.hash() != *digest {
            return Err(damaged(format!("hashes to `{}`", tree.hash())));
        }
        Ok(Some(tree))
    }
}

/// The size of the file at `path` when the file at `by`, itself or the bytes a download record
/// belongs to, is older than the grace period. A file that is gone already, taken by a writer
/// or another collection, is not old.
fn old(path: &Path, by: &Path, options: &Collect) -> Result<Option<u64>, StoreError> {
    let Some(judged) = metadata(by)? else {
        return Ok(None);
    };
    let modified = judged.modified().map_err(io("read the time of", by))?;
    let age = options.now.duration_since(modified).unwrap_or_default();
    if age <= options.grace {
        return Ok(None);
    }
    if path == by {
        return Ok(Some(judged.len()));
    }
    Ok(metadata(path)?.map(|metadata| metadata.len()))
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io("read metadata of", path)(error)),
    }
}

impl Marked {
    fn mark_tree(&mut self, path: PathBuf, tree: &Tree) {
        self.names.insert(path);
        self.objects.extend(
            tree.entries()
                .iter()
                .map(|entry| Digest::Blake3(entry.blake3)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATER: Collect = Collect {
        now: SystemTime::UNIX_EPOCH,
        grace: Duration::ZERO,
        dry_run: false,
    };

    #[test]
    fn a_file_already_gone_is_not_old_and_one_that_cannot_be_read_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let options = Collect {
            now: SystemTime::now() + Duration::from_secs(60),
            ..LATER
        };
        let file = temp.path().join("file");
        fs::write(&file, b"four").unwrap();
        assert_eq!(old(&file, &file, &options).unwrap(), Some(4));
        assert_eq!(
            old(&file, &temp.path().join("gone"), &options).unwrap(),
            None
        );
        assert_eq!(
            old(&temp.path().join("gone"), &file, &options).unwrap(),
            None
        );
        if cfg!(unix) {
            assert!(old(&file.join("below"), &file.join("below"), &options).is_err());
        }
    }
}
