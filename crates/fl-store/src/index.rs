//! The index from a digest a publisher states to the BLAKE3 object it names.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use fl_model::digest::{Algorithm, Digest};

use crate::error::{StoreError, io};
use crate::layout::entries;
use crate::mode;
use crate::platform;
use crate::staged::{Scratch, create_unique};
use crate::tier::Tier;

const STRONGEST_FIRST: [Algorithm; 3] = [Algorithm::Sha256, Algorithm::Sha1, Algorithm::Md5];

impl Tier {
    /// Records that each non BLAKE3 digest in `stated` names the object `blake3`.
    ///
    /// Call it only after the bytes of the object were verified against every digest in
    /// `stated`. Remembering a digest again changes nothing, and of writers racing for one
    /// entry the first places it and the others count as done.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when a directory or an entry cannot be written.
    pub fn remember(&self, stated: &[Digest], blake3: [u8; 32]) -> Result<(), StoreError> {
        let text = Digest::Blake3(blake3).to_string();
        let hex = text.split_once(':').map_or(text.as_str(), |(_, hex)| hex);
        for digest in stated {
            let Some(entry) = self.layout().indexed(digest) else {
                continue;
            };
            if fs::symlink_metadata(&entry).is_ok() {
                continue;
            }
            let dirs: Vec<&Path> = entry.ancestors().skip(1).take(2).collect();
            for dir in dirs.into_iter().rev() {
                mode::create_dir(dir, self.sharing()).map_err(io("create directory", dir))?;
            }
            let (mut file, path) = create_unique(&self.layout().dir("tmp"))?;
            let scratch = Scratch::new(path);
            file.write_all(hex.as_bytes())
                .and_then(|()| platform::seal(&file))
                .map_err(io("write", scratch.path()))?;
            drop(file);
            self.place(scratch, &entry)?;
        }
        Ok(())
    }

    /// The BLAKE3 digest of an object in this tier that one of the `stated` digests names.
    ///
    /// SHA-256 is tried first, then SHA-1, then MD5. An entry counts only when its object is
    /// present and, with a `size`, holds exactly that many bytes. An entry that is missing,
    /// unreadable, malformed or names a missing object is passed over.
    #[must_use]
    pub fn recall(&self, stated: &[Digest], size: Option<u64>) -> Option<[u8; 32]> {
        STRONGEST_FIRST
            .into_iter()
            .flat_map(|algorithm| {
                stated
                    .iter()
                    .filter(move |digest| digest.algorithm() == algorithm)
            })
            .filter_map(|digest| self.layout().indexed(digest))
            .filter_map(|entry| named(&entry))
            .find(|blake3| {
                fs::symlink_metadata(self.layout().object(blake3)).is_ok_and(|metadata| {
                    metadata.is_file() && size.is_none_or(|n| metadata.len() == n)
                })
            })
    }

    /// The index entries that name nothing: malformed, naming an object that is gone, or naming
    /// one of the `removed` paths. Each comes with its size in bytes.
    pub(crate) fn dead_entries(
        &self,
        removed: &HashSet<&Path>,
    ) -> Result<Vec<(PathBuf, u64)>, StoreError> {
        let mut dead = Vec::new();
        for algorithm in entries(&self.layout().dir("index"))? {
            for fan in entries(&algorithm)? {
                for entry in entries(&fan)? {
                    let object = named(&entry).map(|blake3| self.layout().object(&blake3));
                    let live = object.as_deref().is_some_and(|object| {
                        !removed.contains(object) && fs::symlink_metadata(object).is_ok()
                    });
                    if !live && let Ok(metadata) = fs::symlink_metadata(&entry) {
                        dead.push((entry, metadata.len()));
                    }
                }
            }
        }
        Ok(dead)
    }
}

fn named(entry: &Path) -> Option<[u8; 32]> {
    let text = fs::read_to_string(entry).ok()?;
    match Digest::from_hex(Algorithm::Blake3, &text) {
        Ok(Digest::Blake3(blake3)) => Some(blake3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::tier::Sharing;

    fn tier() -> (tempfile::TempDir, Tier) {
        let temp = tempfile::tempdir().unwrap();
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap();
        (temp, tier)
    }

    fn ingest(tier: &Tier, bytes: &[u8]) -> [u8; 32] {
        let mut staged = tier.stage(None).unwrap();
        staged.write_all(bytes).unwrap();
        tier.ingest(staged).unwrap();
        *blake3::hash(bytes).as_bytes()
    }

    fn md5(byte: u8) -> Digest {
        Digest::Md5([byte; 16])
    }

    fn remove(tier: &Tier, blake3: [u8; 32]) {
        let path = tier.layout().object(&blake3);
        crate::files::remove(&path).unwrap();
    }

    #[test]
    fn a_remembered_md5_finds_the_object() {
        let (_temp, tier) = tier();
        let blake3 = ingest(&tier, b"hello");
        tier.remember(&[md5(1)], blake3).unwrap();
        assert_eq!(tier.recall(&[md5(1)], Some(5)), Some(blake3));
        assert_eq!(tier.recall(&[md5(1)], None), Some(blake3));
        assert_eq!(tier.recall(&[md5(2)], None), None);
    }

    #[test]
    fn remembering_twice_changes_nothing() {
        let (_temp, tier) = tier();
        let blake3 = ingest(&tier, b"hello");
        tier.remember(&[md5(1), Digest::Blake3(blake3)], blake3)
            .unwrap();
        tier.remember(&[md5(1)], blake3).unwrap();
        assert_eq!(tier.recall(&[md5(1)], None), Some(blake3));
        assert_eq!(fs::read_dir(tier.layout().dir("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn a_size_that_differs_refuses_the_entry() {
        let (_temp, tier) = tier();
        let blake3 = ingest(&tier, b"hello");
        tier.remember(&[md5(1)], blake3).unwrap();
        assert_eq!(tier.recall(&[md5(1)], Some(6)), None);
    }

    #[test]
    fn an_entry_whose_object_was_removed_is_ignored() {
        let (_temp, tier) = tier();
        let blake3 = ingest(&tier, b"hello");
        tier.remember(&[md5(1)], blake3).unwrap();
        remove(&tier, blake3);
        assert_eq!(tier.recall(&[md5(1)], None), None);
    }

    #[test]
    fn a_malformed_entry_is_ignored() {
        let (_temp, tier) = tier();
        let blake3 = ingest(&tier, b"hello");
        tier.remember(&[md5(1)], blake3).unwrap();
        let entry = tier.layout().indexed(&md5(1)).unwrap();
        fs::remove_file(&entry).unwrap();
        fs::write(&entry, b"not hex").unwrap();
        assert_eq!(tier.recall(&[md5(1)], None), None);
    }

    #[test]
    fn the_strongest_stated_digest_wins() {
        let (_temp, tier) = tier();
        let (weak, strong) = (ingest(&tier, b"weak"), ingest(&tier, b"strong"));
        let sha256 = Digest::Sha256([3; 32]);
        tier.remember(&[md5(1)], weak).unwrap();
        tier.remember(&[sha256], strong).unwrap();
        assert_eq!(tier.recall(&[md5(1), sha256], None), Some(strong));
    }

    #[test]
    fn dead_entries_are_the_ones_naming_nothing_or_a_removed_path() {
        let (_temp, tier) = tier();
        let (gone, kept, doomed) = (
            ingest(&tier, b"gone"),
            ingest(&tier, b"kept"),
            ingest(&tier, b"doomed"),
        );
        for (byte, blake3) in [(1, gone), (2, kept), (3, doomed)] {
            tier.remember(&[md5(byte)], blake3).unwrap();
        }
        remove(&tier, gone);
        let object = tier.layout().object(&doomed);
        let removed = HashSet::from([object.as_path()]);
        let mut dead: Vec<PathBuf> = tier
            .dead_entries(&removed)
            .unwrap()
            .into_iter()
            .map(|(path, size)| {
                assert_eq!(size, 64);
                path
            })
            .collect();
        dead.sort_unstable();
        let expected: Vec<PathBuf> = [1, 3]
            .into_iter()
            .map(|byte| tier.layout().indexed(&md5(byte)).unwrap())
            .collect();
        assert_eq!(dead, expected);
    }
}
