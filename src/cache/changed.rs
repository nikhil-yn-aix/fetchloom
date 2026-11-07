//! objects found changed, remembered after replacement so projects can check their old hardlinks.
//! marked objects stay at their names until a verified copy replaces them. lookup hashes objects
//! no newer than their marks, or newer by less than a coarse clock's two second step.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Cache, hash_object};
use crate::digest::Sha256;
use crate::error::Error;
use crate::place::{FolderUse, found_folder, real_folder};
use crate::volume::is_plain;

/// an object in doubt that could not be hashed, sealed or marked for repair.
#[derive(Debug)]
pub(crate) struct Doubt {
    pub(crate) object: PathBuf,
    /// whether hashing found other bytes but writing the change mark failed.
    pub(crate) changed: bool,
    pub(crate) error: io::Error,
}

impl Cache {
    /// the object for `digest` when the cache holds it, with `size` when one is known, and its
    /// bytes can be trusted. a mark in `marks`, from [`Cache::doubts`], at least as new as the
    /// object puts it in doubt, as does a writable flag. intact writable objects are sealed
    /// again; changed ones are marked and left at their names until a fetched copy replaces them.
    pub(crate) fn trusted(
        &self,
        digest: &Sha256,
        size: Option<u64>,
        marks: &HashMap<Sha256, u64>,
    ) -> Result<Option<PathBuf>, Doubt> {
        let path = self.object_path(digest);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Ok(None);
        };
        if !is_plain(&metadata) || size.is_some_and(|size| metadata.len() != size) {
            return Ok(None);
        }
        let mut permissions = metadata.permissions();
        let writable = !permissions.readonly();
        if !doubted(&metadata, marks.get(digest).copied()) {
            return Ok(Some(path));
        }
        // a writable object is hashed and sealed through one handle, so a link swapped in for
        // its name between the two is never sealed in its place
        let hashed = if writable {
            super::open_unfollowed(&path, false)
                .and_then(|file| Ok((Sha256::of_reader(&file)?, Some(file))))
        } else {
            hash_object(&path).map(|actual| (actual, None))
        };
        match hashed {
            Ok((actual, file)) if actual == *digest => {
                if let Some(file) = file {
                    permissions.set_readonly(true);
                    file.set_permissions(permissions).map_err(|error| Doubt {
                        object: path.clone(),
                        changed: false,
                        error,
                    })?;
                }
                Ok(Some(path))
            }
            Ok(_) => {
                self.mark_changed(digest).map_err(|error| Doubt {
                    object: path,
                    changed: true,
                    error,
                })?;
                Ok(None)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Doubt {
                object: path,
                changed: false,
                error,
            }),
        }
    }

    /// remembers that the object for `digest` no longer held its bytes, as of now.
    pub(crate) fn mark_changed(&self, digest: &Sha256) -> io::Result<()> {
        let folder = self.changed_folder();
        real_folder(&folder, FolderUse::Cache)?;
        self.write_atomically(&folder.join(digest.hex()), b"")
    }

    /// refreshes the mark so projects checked after an earlier repair see this corruption too.
    /// the mark's time stays when the change was found, and it holds `written`, when the copy
    /// that replaced the object was written, which stays out of doubt. only a finished
    /// replacement may record it.
    pub(super) fn mark_replaced(&self, digest: &Sha256, written: SystemTime) -> io::Result<()> {
        let folder = self.changed_folder();
        real_folder(&folder, FolderUse::Cache)?;
        self.write_atomically(
            &folder.join(digest.hex()),
            nanos(written).to_string().as_bytes(),
        )
    }

    /// every object found changed, with when it was found, in nanoseconds since 1970 like the
    /// times a project records for its files.
    #[cfg(test)]
    pub(crate) fn changed(&self) -> HashMap<Sha256, u64> {
        self.marks()
            .unwrap()
            .into_iter()
            .map(|(digest, mark)| (digest, mark.found))
            .collect()
    }

    /// every object found changed, with the time an object of its name must be newer than to
    /// be out of doubt, for [`Cache::trusted`]. unreadable marks stop trusted placement.
    pub(crate) fn doubts(&self) -> Result<HashMap<Sha256, u64>, Error> {
        Ok(self
            .marks()?
            .into_iter()
            .map(|(digest, mark)| (digest, mark.doubt_since()))
            .collect())
    }

    /// every object found changed, with its mark.
    ///
    /// # Errors
    /// returns an error when the marks folder, an entry or a mark cannot be read. only a missing
    /// folder or mark means there is no recorded doubt.
    pub(crate) fn marks(&self) -> Result<HashMap<Sha256, Mark>, Error> {
        let folder = self.changed_folder();
        let metadata = match fs::symlink_metadata(&folder) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(err) => return Err(unreadable_mark(&folder, &err)),
        };
        found_folder(&folder, &metadata, FolderUse::Cache)
            .map_err(|err| unreadable_mark(&folder, &err))?;
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(err) => return Err(unreadable_mark(&folder, &err)),
        };
        let mut marks = HashMap::new();
        for entry in entries {
            let entry = entry.map_err(|err| unreadable_mark(&folder, &err))?;
            let Some(digest) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let path = entry.path();
            let mark = entry
                .metadata()
                .and_then(|metadata| Mark::read(&path, &metadata));
            match mark {
                Ok(mark) => {
                    marks.insert(digest, mark);
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(unreadable_mark(&path, &err)),
            }
        }
        Ok(marks)
    }

    /// forgets marks only after both the object and its projects no longer need them. a retained
    /// damaged object still needs its mark so lookup refuses it.
    #[cfg(test)]
    pub(crate) fn forget_changed(&self, needed: &HashSet<Sha256>) {
        self.forget_changed_reporting(needed, &crate::stop::Stop::default(), &mut Vec::new())
            .unwrap();
    }

    pub(super) fn forget_changed_reporting(
        &self,
        needed: &HashSet<Sha256>,
        stop: &crate::stop::Stop,
        not_removed: &mut Vec<super::Failure>,
    ) -> Result<(), crate::error::Error> {
        let folder = self.changed_folder();
        for entry in super::upkeep::scan_entries(&folder, not_removed, stop)? {
            stop.check()?;
            let Some(digest) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let object = self.object_path(&digest);
            // a pretend sweep keeps the objects it would remove, so their marks go as well
            let gone = self.volume.pretends_removed(&object)
                || object.try_exists().is_ok_and(|exists| !exists);
            if !needed.contains(&digest)
                && gone
                && fs::symlink_metadata(&folder).is_ok_and(|metadata| {
                    found_folder(&folder, &metadata, FolderUse::Cache).is_ok()
                })
            {
                super::upkeep::removed(
                    &entry.path(),
                    self.volume.remove(&entry.path()),
                    not_removed,
                );
            }
        }
        Ok(())
    }

    /// the time the object for `digest` must be newer than to be out of doubt, when it is marked.
    pub(super) fn changed_at(&self, digest: &Sha256) -> Result<Option<u64>, Error> {
        let path = self.changed_folder().join(digest.hex());
        match fs::symlink_metadata(&path).and_then(|metadata| Mark::read(&path, &metadata)) {
            Ok(mark) => Ok(Some(mark.doubt_since())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(unreadable_mark(&path, &err)),
        }
    }

    fn changed_folder(&self) -> PathBuf {
        self.root.join("changed")
    }
}

/// the record that an object no longer held its bytes, in nanoseconds since 1970.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    /// when the change was found, which projects compare with when they last checked a link.
    pub(crate) found: u64,
    /// when the copy a repair put in place of the object was written. a hardlink to that copy
    /// shares its time.
    pub(crate) replacement: Option<u64>,
}

impl Mark {
    fn read(path: &Path, metadata: &fs::Metadata) -> io::Result<Mark> {
        if !is_plain(metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a change mark must be a plain file",
            ));
        }
        let replacement = fs::read_to_string(path)?.trim().parse().ok();
        Ok(Mark {
            found: nanos(metadata.modified()?),
            replacement,
        })
    }

    /// the time an object of the mark's name must be newer than to be out of doubt. a repaired
    /// object counts from before its copy was written, by three of FAT's two second steps: its
    /// time may round up one step and doubt allows another.
    fn doubt_since(self) -> u64 {
        self.replacement
            .map_or(self.found, |written| written.saturating_sub(3 * COARSE))
    }
}

fn unreadable_mark(path: &Path, err: &io::Error) -> Error {
    Error::io(
        format_args!("could not read cache change marks at {}", path.display()),
        err,
    )
    .help("check the cache storage and permissions, then run `fetchloom cache verify` and retry")
}

pub(super) fn doubted(metadata: &fs::Metadata, marked: Option<u64>) -> bool {
    !metadata.permissions().readonly()
        || marked.is_some_and(|marked| {
            marked.saturating_add(COARSE) >= metadata.modified().map_or(0, nanos)
        })
}

/// whether a mark dated `marked` may have been written after `known`, a time read from the disk
/// or the system clock. hfs+ cuts a file's time down to the second and fat to two, so a mark
/// written just after `known` can carry an earlier time.
pub(crate) fn may_follow(marked: u64, known: u64) -> bool {
    marked.saturating_add(COARSE) > known
}

/// the step of the coarsest clock a cache may sit on: FAT and exFAT round a file's time up to
/// two seconds, so an object edited just before its mark can look newer than the mark.
const COARSE: u64 = 2_000_000_000;

/// `time` in nanoseconds since 1970, as projects record the times of their files.
fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{cache, store};
    use super::*;

    #[test]
    fn marking_a_changed_object_never_writes_through_a_link() {
        let (dir, cache) = cache();
        let outside = dir.path().join("outside");
        fs::write(&outside, b"keep these bytes").unwrap();
        let digest = crate::digest::sha256(b"object");
        fs::create_dir_all(cache.changed_folder()).unwrap();
        let mark = cache.changed_folder().join(digest.hex());
        // a filesystem without hardlinks, such as FAT, cannot hold this trap
        if fs::hard_link(&outside, &mark).is_err() {
            return;
        }

        cache.mark_changed(&digest).unwrap();

        assert_eq!(fs::read(outside).unwrap(), b"keep these bytes");
        assert_eq!(fs::read(mark).unwrap(), b"");
    }

    #[test]
    fn writable_objects_are_hashed_and_resealed_before_use() {
        for damaged in [false, true] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            let mut permissions = fs::metadata(&object).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the test models a user clearing a hardlink's flag"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(&object, permissions).unwrap();
            if damaged {
                fs::write(&object, b"evil").unwrap();
            }

            let found = cache
                .trusted(&stored.digest, Some(4), &HashMap::new())
                .unwrap();

            if damaged {
                assert!(found.is_none());
                assert_eq!(fs::read(&object).unwrap(), b"evil");
                assert!(cache.changed().contains_key(&stored.digest));
            } else {
                assert_eq!(found.as_deref(), Some(object.as_path()));
                assert!(
                    fs::metadata(&object).unwrap().permissions().readonly(),
                    "a trusted object must be read-only"
                );
                assert_eq!(fs::read(&object).unwrap(), b"good");
                assert!(cache.changed().is_empty());
            }
        }
    }

    #[test]
    fn a_changed_object_is_remembered_until_no_project_needs_it() {
        let (_dir, cache) = cache();
        assert!(cache.changed().is_empty(), "nothing marked");
        let kept = store(&cache, b"kept").digest;
        let dropped = store(&cache, b"dropped").digest;
        for digest in [kept, dropped] {
            cache.mark_changed(&digest).unwrap();
        }
        let changed = cache.changed();
        assert_eq!(changed.len(), 2);
        assert!(changed.values().all(|found| *found > 0));
        cache.forget_changed(&HashSet::from([kept]));
        assert_eq!(cache.changed().len(), 2, "objects still need their marks");
        cache.drop_object(&dropped).unwrap();
        cache.forget_changed(&HashSet::from([kept]));
        assert_eq!(cache.changed().into_keys().collect::<Vec<_>>(), [kept]);
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_marks_never_make_an_object_trusted() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        for mode in [Some(0o111), Some(0o000), None] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&object, b"evil").unwrap();
            super::super::tests::seal(&object);
            assert!(!cache.verify(&stored.digest).unwrap());
            let folder = cache.changed_folder();
            let path = if mode.is_some() {
                folder
            } else {
                folder.join(stored.digest.hex())
            };
            let before = fs::metadata(&path).unwrap().permissions();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode.unwrap_or(0))).unwrap();
            let marks = cache.doubts();
            let found = cache.find(&stored.digest, Some(4));
            let kept = cache.contains_for_gc(&stored.digest, 4, &crate::stop::Stop::default());
            fs::set_permissions(&path, before).unwrap();
            let error = marks.unwrap_err();
            assert!(
                error.message.contains(&path.display().to_string()),
                "{error}"
            );
            assert_eq!(found, None);
            if mode != Some(0o111) {
                assert!(kept.is_err(), "{kept:?}");
            } else {
                assert!(!kept.unwrap());
            }
        }
    }

    #[test]
    fn missing_marks_are_empty_but_non_files_are_errors() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        fs::remove_dir(cache.changed_folder()).unwrap();
        assert!(cache.marks().unwrap().is_empty());
        assert_eq!(cache.changed_at(&stored.digest).unwrap(), None);
        assert!(cache.find(&stored.digest, Some(4)).is_some());
        fs::create_dir(cache.changed_folder()).unwrap();
        fs::create_dir(cache.changed_folder().join(stored.digest.hex())).unwrap();
        assert!(cache.marks().is_err());
        assert!(cache.changed_at(&stored.digest).is_err());
        assert!(cache.find(&stored.digest, Some(4)).is_none());
    }
}
