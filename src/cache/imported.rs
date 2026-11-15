//! lists of the objects imported from files on some machine. no source can give their bytes
//! back, so `cache gc` counts them apart and `--now` frees them only after a yes.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use super::Cache;
use crate::digest::Sha256;
use crate::place::{FolderUse, real_folder};

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
        real_folder(&folder, FolderUse::Cache)?;
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

    /// each list of imported objects, with its text. one that cannot be read names none, and a
    /// link in place of the folder lists none, since gc removes the lists it reads.
    fn imported_lists(&self) -> Vec<(PathBuf, String)> {
        let folder = self.imported_folder();
        if !fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir()) {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(folder) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry
                        .file_name()
                        .to_str()
                        .and_then(Sha256::from_hex)
                        .is_some()
            })
            .filter_map(|entry| {
                let mut text = String::new();
                crate::volume::read_unfollowed(&entry.path())
                    .ok()?
                    .read_to_string(&mut text)
                    .ok()?;
                Some((entry.path(), text))
            })
            .collect()
    }

    fn imported_folder(&self) -> PathBuf {
        self.root.join("imported")
    }
}
