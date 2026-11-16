//! what this cache's downloads proved of git's blobs: for a blob of a repository, the object a
//! download brought whose bytes have the blob's id. a record is kept as a url record is, under a
//! key of the repository and the id, so gc forgets it once the object is gone, and it stands for
//! the blob only while the object's bytes still have that id: a damaged record, or one someone
//! sharing the cache wrote, never stands for other bytes.

use std::io;

use super::{Cache, UrlRecord, Validators};
use crate::digest::{BlobId, Sha256};
use crate::stop::Stop;

/// the key of the record of blob `id` of `repository`, which no url or other record's key takes.
/// a blob of another repository is never this one's, whatever its id, as a fork's or one made to
/// collide with it.
fn key(repository: &str, id: &BlobId) -> String {
    format!("git {repository} blob {id}")
}

impl Cache {
    /// the SHA-256 and size of the object a download proved holds blob `id` of `repository`, as
    /// [`Cache::remember_blob`] records it, which [`Cache::holds_blob`] checks again.
    pub(crate) fn blob_record(&self, repository: &str, id: &BlobId) -> Option<(Sha256, u64)> {
        self.url_record(&key(repository, id))
            .map(|record| (record.sha256, record.size))
    }

    /// whether the cache holds the object for `sha256`, of `size` bytes, and its bytes are the
    /// blob `id`: a git-lfs file's by the pointer git keeps for them, which their SHA-256 and
    /// size make, any other's by hashing them as git does.
    ///
    /// # Errors
    ///
    /// `Interrupted` when `stop` asks, or the cache's error reading the object.
    pub(crate) fn holds_blob(
        &self,
        id: &BlobId,
        sha256: &Sha256,
        size: u64,
        stop: &Stop,
    ) -> io::Result<bool> {
        let Some(path) = self.find(sha256, Some(size)) else {
            return Ok(false);
        };
        if BlobId::of_lfs_pointer(sha256, size) == *id {
            return Ok(true);
        }
        Ok(super::object::blob_id_of(&path, size, stop)? == Some(*id))
    }

    /// records that the object for `sha256`, of `size` bytes, holds blob `id` of `repository`,
    /// once [`Cache::holds_blob`] proves it; says whether it did.
    ///
    /// # Errors
    ///
    /// as [`Cache::holds_blob`], or the record could not be written.
    pub(crate) fn remember_blob(
        &self,
        repository: &str,
        id: &BlobId,
        sha256: Sha256,
        size: u64,
        stop: &Stop,
    ) -> io::Result<bool> {
        if !self.holds_blob(id, &sha256, size, stop)? {
            return Ok(false);
        }
        if self.blob_record(repository, id) != Some((sha256, size)) {
            self.save_url_record(&UrlRecord {
                url: key(repository, id),
                sha256,
                size,
                name: None,
                validators: Validators::default(),
                page: false,
            })?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::cache::tests::{cache, store};
    use crate::cache::{GRACE, Needed, Stored};

    const SURVEY: &str = "https://github.com/lab/survey";
    const ROWS: &[u8] = b"site,count\na,1\n";
    const NOTES: &[u8] = b"collected in 2020\n";

    /// [`Cache::remember_blob`] of blob `id` in lab/survey as the object `stored`.
    fn remember(cache: &Cache, id: &BlobId, stored: &Stored) -> bool {
        let (sha256, size) = (stored.digest, stored.size);
        let stop = Stop::default();
        cache
            .remember_blob(SURVEY, id, sha256, size, &stop)
            .unwrap()
    }

    /// [`Cache::holds_blob`] of blob `id` as the object `stored`.
    fn holds(cache: &Cache, id: &BlobId, stored: &Stored) -> bool {
        let stop = Stop::default();
        cache
            .holds_blob(id, &stored.digest, stored.size, &stop)
            .unwrap()
    }

    /// a record names its repository: the same blob of a fork, or of any other repository,
    /// finds nothing.
    #[test]
    fn a_blob_record_stands_for_its_own_repository_alone() {
        let (_dir, cache) = cache();
        let rows = store(&cache, ROWS);
        let id = BlobId::of_bytes(ROWS);
        assert!(remember(&cache, &id, &rows));
        let found = Some((rows.digest, rows.size));
        assert_eq!(cache.blob_record(SURVEY, &id), found);
        for other in [
            "https://github.com/lab/fork",
            "https://gitlab.com/lab/survey",
        ] {
            assert_eq!(cache.blob_record(other, &id), None, "{other}");
        }
    }

    /// only bytes whose own id is the blob's, or whose git-lfs pointer's is, are recorded as it.
    #[test]
    fn only_bytes_that_are_the_blob_are_recorded_as_it() {
        let (_dir, cache) = cache();
        let (rows, notes) = (store(&cache, ROWS), store(&cache, NOTES));
        let id = BlobId::of_bytes(ROWS);
        assert!(!remember(&cache, &id, &notes));
        assert_eq!(cache.blob_record(SURVEY, &id), None);
        let pointer = BlobId::of_lfs_pointer(&notes.digest, notes.size);
        assert!(remember(&cache, &pointer, &notes));
        let found = Some((notes.digest, notes.size));
        assert_eq!(cache.blob_record(SURVEY, &pointer), found);
        assert!(holds(&cache, &id, &rows));
    }

    /// a record stands for its blob only while the cache holds bytes that are the blob: not when
    /// the object holds other bytes, nor once it is gone, and gc forgets it with its object.
    #[test]
    fn a_blob_record_stands_only_while_its_bytes_do() {
        let (_dir, cache) = cache();
        let (rows, notes) = (store(&cache, ROWS), store(&cache, NOTES));
        let (rows_id, notes_id) = (BlobId::of_bytes(ROWS), BlobId::of_bytes(NOTES));
        assert!(!holds(&cache, &rows_id, &notes));
        assert!(remember(&cache, &rows_id, &rows) && remember(&cache, &notes_id, &notes));
        let keep = Needed {
            objects: HashSet::from([rows.digest]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert!(!holds(&cache, &notes_id, &notes));
        assert_eq!(cache.blob_record(SURVEY, &notes_id), None, "gc forgot it");
        let found = Some((rows.digest, rows.size));
        assert_eq!(cache.blob_record(SURVEY, &rows_id), found);
    }
}
