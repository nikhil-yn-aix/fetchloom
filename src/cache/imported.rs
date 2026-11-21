//! lists of the objects imported from files on some machine. no source can give their bytes
//! back, so `cache gc` counts them apart and `--now` frees them only after a yes.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::Cache;
use crate::digest::Sha256;

/// the most of one list of imported objects a read takes, some 16 million of them, more than one
/// import lists: one a co-user of a shared cache made larger lists none.
const LIST_MOST: u64 = 1 << 30;

impl Cache {
    /// remembers that the objects for `digests` hold bytes imported from files, in one list
    /// named by its content, so an import of many files writes one record rather than one each.
    pub(crate) fn mark_imported(&self, digests: &[Sha256]) -> io::Result<()> {
        if digests.is_empty() {
            return Ok(());
        }
        let mut text = String::with_capacity(digests.len() * 65);
        for digest in digests {
            text.push_str(&digest.hex());
            text.push('\n');
        }
        let folder = self.imported_folder();
        super::make_folder(&self.root, &folder, self.sharing)?;
        let path = folder.join(crate::digest::sha256(text.as_bytes()).hex());
        if fs::symlink_metadata(&path).is_ok() {
            return Ok(());
        }
        // a list lost to a power cut costs only the question gc asks about it
        self.write_hint(&path, text.as_bytes())
    }

    /// the objects the cache holds that were imported from files, each with its payload size.
    pub(crate) fn imported(&self) -> HashMap<Sha256, u64> {
        let mut found = HashMap::new();
        for (_, text) in self.imported_lists() {
            for digest in text.lines().filter_map(Sha256::from_hex) {
                if found.contains_key(&digest) {
                    continue;
                }
                if let Ok(metadata) = fs::symlink_metadata(self.object_path(&digest))
                    && metadata.is_file()
                {
                    found.insert(digest, metadata.len());
                }
            }
        }
        found
    }

    /// removes each list that names no object the cache holds now, as once gc freed them all.
    pub(crate) fn forget_imported(&self) {
        for (path, text) in self.imported_lists() {
            let held = text
                .lines()
                .filter_map(Sha256::from_hex)
                .any(|digest| fs::symlink_metadata(self.object_path(&digest)).is_ok());
            if !held && let Err(err) = self.remove_inside(&path) {
                tracing::debug!(%err, path = %path.display(), "could not forget a list of imports");
            }
        }
    }

    /// each list of imported objects, with its text, read one at a time as it is wanted. one that
    /// cannot be read, or holds more than [`LIST_MOST`], names none, and a link in place of the
    /// folder lists none, since gc removes the lists it reads.
    fn imported_lists(&self) -> impl Iterator<Item = (PathBuf, String)> {
        let folder = self.imported_folder();
        let lists: Vec<PathBuf> = if fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir())
        {
            fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_file())
                        && entry
                            .file_name()
                            .to_str()
                            .and_then(Sha256::from_hex)
                            .is_some()
                })
                .map(|entry| entry.path())
                .collect()
        } else {
            Vec::new()
        };
        lists.into_iter().filter_map(|path| {
            let file = crate::volume::read_unfollowed(&path).ok()?;
            let text = crate::volume::read_at_most(file, LIST_MOST).ok()?;
            Some((path, String::from_utf8(text).ok()?))
        })
    }

    fn imported_folder(&self) -> PathBuf {
        self.root.join("imported")
    }
}
