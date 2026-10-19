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
    /// Files removed from the tier: objects, trees, step records and leftovers in `tmp/`.
    pub removed: Vec<PathBuf>,
    /// The bytes those files held.
    pub bytes: u64,
}

#[derive(Default)]
struct Marked {
    objects: HashSet<[u8; 32]>,
    names: HashSet<PathBuf>,
}

impl Tier {
    /// Drops the roots whose lock file is gone, marks everything the other locks need, and
    /// removes every object, tree, step record and `tmp/` file that is unmarked and older than
    /// the grace period.
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
                let Digest::Blake3(blake3) = object_digest(&fan, &object)? else {
                    continue;
                };
                if !marked.objects.contains(&blake3) {
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
        for path in doomed {
            if let Some(bytes) = old(&path, options)? {
                collected.bytes += bytes;
                collected.removed.push(path);
            }
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
                marked.objects.extend(files.iter().map(|file| file.blake3));
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

/// The size of the file at `path` when it is older than the grace period. A file that is gone
/// already, taken by a writer or another collection, is not old.
fn old(path: &Path, options: &Collect) -> Result<Option<u64>, StoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io("read metadata of", path)(error)),
    };
    let modified = metadata.modified().map_err(io("read the time of", path))?;
    let age = options.now.duration_since(modified).unwrap_or_default();
    Ok((age > options.grace).then_some(metadata.len()))
}

impl Marked {
    fn mark_tree(&mut self, path: PathBuf, tree: &Tree) {
        self.names.insert(path);
        self.objects
            .extend(tree.entries().iter().map(|entry| entry.blake3));
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
        assert_eq!(old(&file, &options).unwrap(), Some(4));
        assert_eq!(old(&temp.path().join("gone"), &options).unwrap(), None);
        if cfg!(unix) {
            assert!(old(&file.join("below"), &options).is_err());
        }
    }
}
