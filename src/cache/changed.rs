//! objects found changed, remembered after replacement so projects can check their old hardlinks.
//! marked objects stay at their names until a verified copy replaces them. lookup hashes objects
//! written since they were sealed, and objects no newer than their marks, or newer by less than a
//! coarse clock's two second step.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::Cache;
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
    /// object puts it in doubt, as do a writable flag and a time moved since sealing. intact
    /// objects are sealed and dated again; changed ones are marked and left at their names until
    /// a fetched copy replaces them.
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
        if !doubted(&path, &metadata, marks.get(digest).copied()) {
            return Ok(Some(path));
        }
        // the object is hashed, sealed and dated through one handle, so a link swapped in for its
        // name meanwhile is never sealed or dated in its place
        let opened = if writable {
            super::open_unfollowed(&path, false)
        } else {
            super::partial::open_to_redate(&path)
        };
        let hashed = opened.and_then(|file| Ok((Sha256::of_reader(&file)?, file)));
        match hashed {
            Ok((actual, file)) if actual == *digest => {
                if writable {
                    permissions.set_readonly(true);
                    file.set_permissions(permissions).map_err(|error| Doubt {
                        object: path.clone(),
                        changed: false,
                        error,
                    })?;
                }
                if super::object::rewritten(&path, &metadata) {
                    super::object::redate(&file, &metadata);
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

    /// remembers that the object for `digest` no longer held its bytes, as of now, and which copy
    /// it was: the file at its name, which projects that placed it as a link still name after a
    /// repair gives the object a new copy.
    pub(crate) fn mark_changed(&self, digest: &Sha256) -> io::Result<()> {
        let mut copies = self.marked_copies(digest);
        if let Ok(found) = Identity::of(&self.object_path(digest))
            && !copies.contains(&found)
        {
            copies.push(found);
            let excess = copies.len().saturating_sub(COPIES);
            copies.drain(..excess);
        }
        self.write_mark(digest, None, &copies)
    }

    /// remembers that `copy`, a file other projects may link that once was the object for
    /// `digest` and now is not, was found changed, so their links to it are checked. a copy the
    /// mark records already changes nothing. a mark already there keeps what it says of the
    /// object at the name: changed, while it holds no replacement time, or replaced then. with no
    /// mark yet, the object at the name, if the cache holds one, is another file, and its time
    /// keeps it out of the doubt.
    pub(crate) fn mark_copy_changed(&self, digest: &Sha256, copy: Identity) -> io::Result<()> {
        let path = self.changed_folder().join(digest.hex());
        let marked = match fs::symlink_metadata(&path).and_then(|found| Mark::read(&path, &found)) {
            Ok(mark) => Some(mark),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        let mut copies = marked
            .as_ref()
            .map(|mark| mark.copies.clone())
            .unwrap_or_default();
        if copies.contains(&copy) {
            return Ok(());
        }
        copies.push(copy);
        let excess = copies.len().saturating_sub(COPIES);
        copies.drain(..excess);
        let replacement = match marked {
            Some(mark) => mark.replacement,
            None => fs::symlink_metadata(self.object_path(digest))
                .and_then(|found| found.modified())
                .ok()
                .map(nanos),
        };
        self.write_mark(digest, replacement, &copies)
    }

    /// refreshes the mark so projects checked after an earlier repair see this corruption too.
    /// the mark's time stays when the change was found, and it holds `written`, when the copy
    /// that replaced the object was written, which stays out of doubt. only a finished
    /// replacement may record it.
    pub(super) fn mark_replaced(&self, digest: &Sha256, written: SystemTime) -> io::Result<()> {
        let copies = self.marked_copies(digest);
        self.write_mark(digest, Some(nanos(written)), &copies)
    }

    /// writes the mark for `digest`: the replacement's time on its first line, empty before a
    /// replacement, then one line for each copy found changed.
    fn write_mark(
        &self,
        digest: &Sha256,
        replacement: Option<u64>,
        copies: &[Identity],
    ) -> io::Result<()> {
        let folder = self.changed_folder();
        real_folder(&folder, FolderUse::Cache)?;
        let mut text = replacement
            .map(|written| written.to_string())
            .unwrap_or_default();
        for copy in copies {
            text.push('\n');
            text.push_str(&copy.to_string());
        }
        self.write_atomically(&folder.join(digest.hex()), text.as_bytes())
    }

    /// the copies an earlier mark for `digest` names, which a new mark keeps. a mark that cannot
    /// be read names none.
    fn marked_copies(&self, digest: &Sha256) -> Vec<Identity> {
        let path = self.changed_folder().join(digest.hex());
        fs::symlink_metadata(&path)
            .and_then(|metadata| Mark::read(&path, &metadata))
            .map(|mark| mark.copies)
            .unwrap_or_default()
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    /// when the change was found, which projects compare with when they last checked a link.
    pub(crate) found: u64,
    /// when the copy a repair put in place of the object was written. a hardlink to that copy
    /// shares its time.
    pub(crate) replacement: Option<u64>,
    /// the copies found changed, oldest first. a project's link that is still one of them was
    /// changed through some link to the cache's copy, not made anew by that project.
    pub(crate) copies: Vec<Identity>,
}

impl Mark {
    fn read(path: &Path, metadata: &fs::Metadata) -> io::Result<Mark> {
        if !is_plain(metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "it is not a plain file",
            ));
        }
        let text = fs::read_to_string(path)?;
        let mut lines = text.lines();
        let replacement = lines.next().and_then(|line| line.trim().parse().ok());
        Ok(Mark {
            found: nanos(metadata.modified()?),
            replacement,
            copies: lines.filter_map(Identity::parse).collect(),
        })
    }

    /// the time an object of the mark's name must be newer than to be out of doubt. a repaired
    /// object counts from before its copy was written, by three of FAT's two second steps: its
    /// time may round up one step and doubt allows another.
    fn doubt_since(&self) -> u64 {
        self.replacement
            .map_or(self.found, |written| written.saturating_sub(3 * COARSE))
    }
}

/// how many changed copies a mark remembers. each repair leaves one behind at most.
const COPIES: usize = 16;

/// a file apart from its names: its device and inode on unix, its volume and file index on
/// windows, with its size and time. a mark keeps the copies the cache found changed, so a
/// project can tell that its link is that very copy. the size and time keep a file made later
/// under a number a removed copy freed from passing for it, and a copy edited again since the
/// cache found it changed from passing for what the cache found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    device: u64,
    index: u64,
    size: u64,
    modified: u64,
}

impl Identity {
    /// the identity of the plain file at `path`, which is never followed as a link.
    pub(crate) fn of(path: &Path) -> io::Result<Identity> {
        let metadata = fs::symlink_metadata(path)?;
        if !is_plain(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a plain file", path.display()),
            ));
        }
        #[cfg(unix)]
        let (device, index) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(windows)]
        let (device, index) = file_index(path)?;
        Ok(Identity {
            device,
            index,
            size: metadata.len(),
            modified: nanos(metadata.modified()?),
        })
    }

    fn parse(line: &str) -> Option<Identity> {
        let mut numbers = line.split_ascii_whitespace().map(str::parse::<u64>);
        let identity = Identity {
            device: numbers.next()?.ok()?,
            index: numbers.next()?.ok()?,
            size: numbers.next()?.ok()?,
            modified: numbers.next()?.ok()?,
        };
        numbers.next().is_none().then_some(identity)
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {}",
            self.device, self.index, self.size, self.modified
        )
    }
}

/// the volume and file index of the file at `path`, which windows keeps apart from metadata.
#[cfg(windows)]
fn file_index(path: &Path) -> io::Result<(u64, u64)> {
    let handle = winapi_util::Handle::from_path_any(path)?;
    let information = winapi_util::file::information(&handle)?;
    Ok((information.volume_serial_number(), information.file_index()))
}

fn unreadable_mark(path: &Path, err: &io::Error) -> Error {
    Error::io(
        format_args!(
            "could not read the cache's list of changed files at {}",
            path.display()
        ),
        err,
    )
    .help("check the cache storage and permissions, then run `fetchloom cache verify` and retry")
}

/// whether the object at `path`, which `seen` describes, holds `digest`'s bytes, by hashing it.
/// an intact object whose time moved since sealing is dated again through the same handle, so
/// the next use need not hash it.
pub(super) fn vouch(path: &Path, seen: &fs::Metadata, digest: &Sha256) -> io::Result<bool> {
    let file = super::partial::open_to_redate(path)?;
    let intact = Sha256::of_reader(&file)? == *digest;
    if intact && super::object::rewritten(path, seen) {
        super::object::redate(&file, seen);
    }
    Ok(intact)
}

/// whether the object at `object`, which `metadata` describes, may not hold the bytes its name
/// says: it is writable, its time moved since sealing, or a mark `marked` is no older than it.
pub(super) fn doubted(object: &Path, metadata: &fs::Metadata, marked: Option<u64>) -> bool {
    !metadata.permissions().readonly()
        || super::object::rewritten(object, metadata)
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
    use super::super::tests::{cache, rewrite, seal, store};
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

    /// an editor that saves in place through a placed hardlink, then puts the read-only flag back,
    /// as vim's `:w!` does, changes the cache's own copy and leaves it looking sealed. its time
    /// moved, so the cache hashes it before trusting it, finds the change and marks it.
    #[test]
    fn an_object_written_through_a_link_and_sealed_again_is_not_trusted() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        if fs::metadata(&object)
            .ok()
            .and_then(|found| super::super::object::birth(&object, &found))
            .is_none()
        {
            return;
        }
        let sealed = fs::metadata(&object).unwrap().modified().unwrap();
        rewrite(&object, b"evil");
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(sealed + std::time::Duration::from_secs(3600))
            .unwrap();
        seal(&object);

        let found = cache
            .trusted(&stored.digest, Some(4), &HashMap::new())
            .unwrap();

        assert_eq!(found, None);
        assert!(cache.changed().contains_key(&stored.digest));
    }

    /// an object whose time moved but whose bytes did not, as after a `touch` through a link, is
    /// trusted once hashed, and dated again so the next use need not hash it.
    #[test]
    fn an_intact_object_whose_time_moved_is_dated_again() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let Some(born) = fs::metadata(&object)
            .ok()
            .and_then(|found| super::super::object::birth(&object, &found))
        else {
            return;
        };
        rewrite(&object, b"good");
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(born + std::time::Duration::from_secs(3600))
            .unwrap();
        seal(&object);

        let found = cache
            .trusted(&stored.digest, Some(4), &HashMap::new())
            .unwrap();

        assert_eq!(found.as_deref(), Some(object.as_path()));
        assert!(!super::super::object::rewritten(
            &object,
            &fs::metadata(&object).unwrap()
        ));
        assert!(cache.changed().is_empty());
    }

    /// an object a project placed as a hardlink shares its time with that project's file, so a
    /// hash vouches for it but leaves its time, which dating it again would make look edited.
    #[test]
    fn an_intact_object_a_project_links_keeps_its_moved_time() {
        let (dir, cache) = cache();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let Some(born) = fs::metadata(&object)
            .ok()
            .and_then(|found| super::super::object::birth(&object, &found))
        else {
            return;
        };
        let placed = dir.path().join("placed");
        if fs::hard_link(&object, &placed).is_err() {
            return;
        }
        rewrite(&object, b"good");
        let moved = born + std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(moved)
            .unwrap();
        seal(&object);

        let found = cache
            .trusted(&stored.digest, Some(4), &HashMap::new())
            .unwrap();

        assert_eq!(found.as_deref(), Some(object.as_path()));
        assert_eq!(fs::metadata(&placed).unwrap().modified().unwrap(), moved);
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

    /// a note of a changed file that cannot be read names the cache's list of changed files and
    /// what is wrong with it, with no internal name for the note.
    #[test]
    fn an_unreadable_note_of_a_change_is_named_plainly() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        let path = cache.changed_folder().join(stored.digest.hex());
        fs::create_dir_all(&path).unwrap();
        let error = cache.doubts().unwrap_err();
        assert_eq!(
            error.message,
            format!(
                "could not read the cache's list of changed files at {}: it is not a plain file",
                path.display()
            )
        );
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
            if mode == Some(0o111) {
                assert!(!kept.unwrap());
            } else {
                assert!(kept.is_err(), "{kept:?}");
            }
        }
    }

    /// a mark remembers which copy the cache found changed, so a project's link that is still
    /// that copy is known after a repair gives the object a new one. a repair keeps the copies.
    #[test]
    fn a_mark_remembers_the_changed_copy_through_its_repair() {
        let (dir, cache) = cache();
        if !super::super::tests::hardlinks(dir.path()) {
            return;
        }
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let placed = dir.path().join("placed");
        fs::hard_link(&object, &placed).unwrap();
        super::super::tests::rewrite(&object, b"evil, and longer");
        super::super::tests::seal(&object);
        let damaged = Identity::of(&placed).unwrap();
        assert!(!cache.verify(&stored.digest).unwrap());
        assert_eq!(cache.marks().unwrap()[&stored.digest].copies, [damaged]);

        assert!(store(&cache, b"good").new);

        let mark = &cache.marks().unwrap()[&stored.digest];
        assert!(mark.replacement.is_some());
        assert_eq!(mark.copies, [damaged]);
        assert_ne!(Identity::of(&object).unwrap(), damaged);
        assert_eq!(Identity::of(&placed).unwrap(), damaged);
    }

    #[test]
    fn a_mark_keeps_its_newest_copies_and_skips_lines_it_cannot_read() {
        let (_dir, cache) = cache();
        let digest = crate::digest::sha256(b"object");
        let copies: Vec<Identity> = (0..20)
            .map(|index| Identity {
                device: 1,
                index,
                size: 4,
                modified: 5,
            })
            .collect();
        cache.write_mark(&digest, Some(42), &copies).unwrap();
        let path = cache.changed_folder().join(digest.hex());
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("\nnot a copy\n1 2 3\n");
        fs::write(&path, text).unwrap();
        let mark = &cache.marks().unwrap()[&digest];
        assert_eq!(mark.replacement, Some(42));
        assert_eq!(mark.copies, copies);
        // the object is gone, so marking it again adds no copy
        cache.mark_changed(&digest).unwrap();
        let mark = &cache.marks().unwrap()[&digest];
        assert_eq!(mark.replacement, None);
        assert_eq!(mark.copies, copies);
        // a new copy pushes the oldest past the cap
        let stored = store(&cache, b"object");
        assert_eq!(stored.digest, digest);
        cache.mark_changed(&digest).unwrap();
        let mark = &cache.marks().unwrap()[&digest];
        assert_eq!(mark.copies.len(), COPIES);
        assert_eq!(mark.copies[..COPIES - 1], copies[20 - (COPIES - 1)..]);
        assert_eq!(
            mark.copies[COPIES - 1],
            Identity::of(&cache.object_path(&digest)).unwrap()
        );
    }

    /// a changed copy found later elsewhere never vouches for an object the mark already holds
    /// changed, as one `cache verify` found rotted with its size and time kept, before a repair.
    #[test]
    fn a_changed_copy_found_later_leaves_a_changed_object_in_doubt() {
        let (dir, cache) = cache();
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let sealed = fs::metadata(&object).unwrap().modified().unwrap();
        rewrite(&object, b"evil");
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(sealed)
            .unwrap();
        seal(&object);
        cache.mark_changed(&stored.digest).unwrap();
        let other = dir.path().join("another copy");
        fs::write(&other, b"evil").unwrap();
        cache
            .mark_copy_changed(&stored.digest, Identity::of(&other).unwrap())
            .unwrap();
        let found = cache.trusted(&stored.digest, Some(4), &cache.doubts().unwrap());
        assert!(matches!(found, Ok(None)), "the rotted object was trusted");
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
