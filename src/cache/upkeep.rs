//! keeping the cache tidy: which projects use it, how big it is, freeing what no project needs,
//! and hashing every object again.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::Cache;
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::stop::Stop;

/// how long an object no project needs stays, so a download a project has not locked yet, or a
/// `get` from this morning, survives a gc.
pub(super) const GRACE: Duration = Duration::from_hours(24);

pub(crate) struct Object {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    path: PathBuf,
    modified: SystemTime,
}

#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) objects: Vec<Object>,
    pub(crate) unreadable: Vec<PathBuf>,
}

/// what the projects using a cache need from it.
#[derive(Debug, Default)]
pub(crate) struct Needed {
    /// objects a project places, and archives a project has not unpacked on this machine yet.
    pub(crate) objects: HashSet<Sha256>,
    /// archives whose record tells what a project places from them.
    pub(crate) unpacked: HashSet<Sha256>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Freed {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// unfinished downloads and temporary files nobody came back for.
    pub(crate) leftovers: u64,
    pub(crate) not_removed: Vec<PathBuf>,
    /// objects no project needs that stay because they are younger than a day.
    pub(crate) kept_objects: u64,
    pub(crate) kept_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Checked {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// objects whose bytes no longer matched their name.
    pub(crate) changed: Vec<Sha256>,
    /// changed objects whose repair marks could not be written.
    pub(crate) stuck: Vec<Sha256>,
    /// objects or folders that could not be read, left untouched without a new change mark.
    pub(crate) unreadable: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Registration {
    root: PathBuf,
}

impl Cache {
    /// remembers that the project at `root` uses this cache, so gc keeps what it needs.
    pub(crate) fn register(&self, root: &Path) -> io::Result<()> {
        let path = self.registration(root);
        if path.exists() {
            return Ok(());
        }
        let text = serde_json::to_vec(&Registration {
            root: root.to_path_buf(),
        })
        .map_err(io::Error::other)?;
        self.write_atomically(&path, &text)
    }

    /// reads every registration before gc can discard anything it might protect.
    pub(crate) fn registered_projects(&self, stop: &Stop) -> Result<Vec<PathBuf>, Error> {
        let folder = self.root.join("projects");
        stop.check()?;
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(unreadable(&folder, &err)),
        };
        let mut roots = Vec::new();
        for entry in entries {
            stop.check()?;
            let path = entry.map_err(|err| unreadable(&folder, &err))?.path();
            let bytes = fs::read(&path).map_err(|err| unreadable(&path, &err))?;
            let registration: Registration = serde_json::from_slice(&bytes).map_err(|err| {
                unreadable(&path, &io::Error::new(io::ErrorKind::InvalidData, err))
            })?;
            roots.push(registration.root);
        }
        roots.sort();
        Ok(roots)
    }

    pub(crate) fn forget(&self, root: &Path) {
        let _ = fs::remove_file(self.registration(root));
    }

    /// checks an archive member without letting a large writable object delay cancellation.
    pub(crate) fn contains_for_gc(
        &self,
        digest: &Sha256,
        size: u64,
        stop: &Stop,
    ) -> Result<bool, Error> {
        stop.check()?;
        let path = self.object_path(digest);
        let Ok(metadata) = fs::metadata(&path) else {
            return Ok(false);
        };
        if !metadata.is_file() || metadata.len() != size {
            return Ok(false);
        }
        let intact = !super::changed::doubted(&metadata, self.changed_at(digest))
            || hash(&path, stop).is_ok_and(|actual| actual == *digest);
        stop.check()?;
        Ok(intact)
    }

    pub(crate) fn scan_objects(&self, stop: &Stop) -> Result<Scan, Error> {
        let mut scan = Scan::default();
        let folder = self.root.join("objects");
        for fan in scan_entries(&folder, &mut scan.unreadable, stop)? {
            stop.check()?;
            let prefix = fan.file_name().to_string_lossy().into_owned();
            for entry in scan_entries(&fan.path(), &mut scan.unreadable, stop)? {
                stop.check()?;
                let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
                let Some(digest) = Sha256::from_hex(&name) else {
                    continue;
                };
                match entry.metadata().and_then(|metadata| {
                    let modified = metadata.modified()?;
                    Ok((metadata.len(), modified))
                }) {
                    Ok((size, modified)) => scan.objects.push(Object {
                        digest,
                        size,
                        path: entry.path(),
                        modified,
                    }),
                    Err(_) => scan.unreadable.push(entry.path()),
                }
            }
        }
        scan.unreadable.sort();
        scan.unreadable.dedup();
        Ok(scan)
    }

    /// removes old, unneeded objects and abandoned downloads, recording each refused removal.
    pub(crate) fn sweep_with_stop(
        &self,
        needed: &Needed,
        now: SystemTime,
        stop: &Stop,
    ) -> Result<Freed, Error> {
        let old = |modified: SystemTime| now.duration_since(modified).is_ok_and(|age| age > GRACE);
        let scan = self.scan_objects(stop)?;
        if let Some(path) = scan.unreadable.first() {
            return Err(Error::new(
                Kind::LocalIo,
                format!(
                    "cannot inspect {}, so the cache stays as it is",
                    path.display()
                ),
            )
            .help("check the cache folder's permissions, then run the command again"));
        }
        let mut freed = Freed::default();
        for object in scan.objects {
            stop.check()?;
            if needed.objects.contains(&object.digest) {
                continue;
            }
            if !old(object.modified) {
                freed.kept_objects += 1;
                freed.kept_bytes += object.size;
            } else if removed(
                &object.path,
                self.volume.remove(&object.path),
                &mut freed.not_removed,
            ) {
                freed.objects += 1;
                freed.bytes += object.size;
            }
        }
        for entry in scan_entries(&self.root.join("unpacked"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let Some(archive) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let path = entry.path();
            let stale = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(old);
            if !needed.unpacked.contains(&archive) && stale {
                removed(&path, self.volume.remove(&path), &mut freed.not_removed);
            }
        }
        self.sweep_partials(&old, stop, &mut freed)?;
        freed.leftovers += self.sweep_staging(now, stop, &mut freed.not_removed);
        stop.check()?;
        self.forget_changed_reporting(&needed.objects, stop, &mut freed.not_removed)?;
        for entry in scan_entries(&self.root.join("tmp"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let stale = entry
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.modified().is_ok_and(old));
            if stale
                && removed(
                    &entry.path(),
                    self.volume.remove(&entry.path()),
                    &mut freed.not_removed,
                )
            {
                freed.leftovers += 1;
            }
        }
        stop.check()?;
        freed.not_removed.sort();
        freed.not_removed.dedup();
        Ok(freed)
    }

    /// keeps empty lock files because a waiting download may have opened one already.
    fn sweep_partials(
        &self,
        old: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        let folder = self.root.join("partial");
        for entry in scan_entries(&folder, &mut freed.not_removed, stop)? {
            stop.check()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".lock") else {
                continue;
            };
            let Ok(lock) = super::partial::open_unfollowed(&entry.path(), false) else {
                continue;
            };
            if lock.try_lock().is_err() {
                continue;
            }
            let bytes = folder.join(format!("{key}.part"));
            let abandoned = match fs::metadata(&bytes) {
                Ok(metadata) => metadata.modified().is_ok_and(old),
                Err(err) => err.kind() == io::ErrorKind::NotFound,
            };
            if !abandoned {
                continue;
            }
            for leftover in [bytes, folder.join(format!("{key}.json"))] {
                stop.check()?;
                if removed(
                    &leftover,
                    self.volume.remove(&leftover),
                    &mut freed.not_removed,
                ) {
                    freed.leftovers += 1;
                }
            }
        }
        Ok(())
    }

    /// hashes every object on the requested threads, stopping between reads when asked.
    pub(crate) fn verify_with_stop(
        &self,
        threads: usize,
        progress: &(dyn Fn(u64) + Sync),
        stop: &Stop,
    ) -> Result<Checked, Error> {
        let scan = self.scan_objects(stop)?;
        let objects = scan.objects;
        let next = AtomicUsize::new(0);
        let checked = Mutex::new(Checked {
            objects: objects.len() as u64,
            bytes: objects.iter().map(|object| object.size).sum(),
            unreadable: scan.unreadable,
            ..Checked::default()
        });
        std::thread::scope(|scope| {
            for _ in 0..threads.max(1) {
                scope.spawn(|| {
                    while let Some(object) = objects.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if stop.is_asked() {
                            break;
                        }
                        let actual = hash(&object.path, stop);
                        if stop.is_asked() {
                            break;
                        }
                        progress(object.size);
                        if stop.is_asked() {
                            break;
                        }
                        match actual {
                            Ok(actual) if actual == object.digest => continue,
                            Err(_) => {
                                checked
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .unreadable
                                    .push(object.path.clone());
                                continue;
                            }
                            Ok(_) => {}
                        }
                        let marked = self.mark_changed(&object.digest).is_ok();
                        let mut found = checked
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        found.changed.push(object.digest);
                        if !marked {
                            found.stuck.push(object.digest);
                        }
                    }
                });
            }
        });
        stop.check()?;
        let mut checked = checked
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        checked.changed.sort();
        checked.stuck.sort();
        checked.unreadable.sort();
        checked.unreadable.dedup();
        Ok(checked)
    }

    fn registration(&self, root: &Path) -> PathBuf {
        let key = digest::sha256(root.as_os_str().as_encoded_bytes()).hex();
        self.root.join("projects").join(format!("{key}.json"))
    }
}

pub(super) fn scan_entries(
    folder: &Path,
    unreadable: &mut Vec<PathBuf>,
    stop: &Stop,
) -> Result<Vec<fs::DirEntry>, Error> {
    stop.check()?;
    let mut found = Vec::new();
    match fs::read_dir(folder) {
        Ok(entries) => {
            for entry in entries {
                stop.check()?;
                match entry {
                    Ok(entry) => found.push(entry),
                    Err(_) => unreadable.push(folder.to_path_buf()),
                }
            }
        }
        Err(_) => unreadable.push(folder.to_path_buf()),
    }
    Ok(found)
}

pub(super) fn removed(path: &Path, result: io::Result<()>, not_removed: &mut Vec<PathBuf>) -> bool {
    match result {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(_) => {
            not_removed.push(path.to_path_buf());
            false
        }
    }
}

fn unreadable(path: &Path, error: &io::Error) -> Error {
    Error::io(format!("cannot read {}", path.display()), error)
}

fn hash(path: &Path, stop: &Stop) -> io::Result<Sha256> {
    let mut file = fs::File::open(path)?;
    let mut hasher = digest::Hasher::default();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if stop.is_asked() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finish().0);
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::tests::{cache, fat, hold, seal, store};
    use super::*;

    impl Cache {
        pub(crate) fn projects(&self) -> Vec<PathBuf> {
            self.registered_projects(&Stop::default()).unwrap()
        }

        pub(crate) fn objects(&self) -> Vec<Object> {
            self.scan_objects(&Stop::default()).unwrap().objects
        }

        pub(crate) fn sweep(&self, needed: &Needed, now: SystemTime) -> Freed {
            self.sweep_with_stop(needed, now, &Stop::default()).unwrap()
        }

        pub(crate) fn verify_all(
            &self,
            threads: usize,
            progress: &(dyn Fn(u64) + Sync),
        ) -> Checked {
            self.verify_with_stop(threads, progress, &Stop::default())
                .unwrap()
        }
    }

    #[test]
    fn stopped_upkeep_does_not_read_or_remove_objects() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"keep");
        let stop = Stop::default();
        stop.ask();
        let checked = cache.verify_with_stop(1, &|_| {}, &stop).unwrap_err();
        assert_eq!(checked.kind, Kind::Interrupted);
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                SystemTime::now() + GRACE + Duration::from_secs(60),
                &stop,
            )
            .unwrap_err();
        assert_eq!(freed.kind, Kind::Interrupted);
        assert!(cache.object_path(&stored.digest).exists());
        assert!(cache.changed().is_empty());
    }

    #[test]
    fn stopping_verification_does_not_mark_the_current_object() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        damage(&cache.object_path(&stored.digest));
        let stop = Stop::default();
        let error = cache
            .verify_with_stop(1, &|_| stop.ask(), &stop)
            .unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(cache.changed().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn verification_reports_directory_and_metadata_permission_errors() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        for (root, mode) in [(true, 0), (false, 0), (false, 0o400)] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"abc");
            let object = cache.object_path(&stored.digest);
            let folder = if root {
                cache.root.join("objects")
            } else {
                object.parent().unwrap().to_path_buf()
            };
            let before = fs::metadata(&folder).unwrap().permissions();
            fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
            let checked = cache.verify_with_stop(1, &|_| {}, &Stop::default());
            fs::set_permissions(&folder, before).unwrap();
            let checked = checked.unwrap();
            let unreadable = if mode == 0o400 {
                object.clone()
            } else {
                folder
            };
            assert_eq!(checked.unreadable, [unreadable]);
            assert_eq!(checked.objects, 0);
            assert_eq!(checked.changed, []);
            assert!(object.exists());
        }
    }

    #[test]
    fn projects_register_once_and_can_be_forgotten() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root.clone()]);
        cache.forget(&root);
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_sweep_reports_refused_removals() {
        let (_dir, mut cache) = cache();
        if !super::super::tests::hardlinks(cache.root()) {
            return;
        }
        let stored = store(&cache, b"cannot remove");
        let object = cache.object_path(&stored.digest);
        let leftover = cache.root.join("tmp").join("left.new");
        fs::write(&leftover, b"leftover").unwrap();
        seal(&leftover);
        cache.volume = std::sync::Arc::new(crate::volume::Volume::refusing(
            cache.root.join("tmp"),
            true,
        ));
        let freed = cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        assert_eq!((freed.objects, freed.bytes, freed.leftovers), (0, 0, 0));
        let report = serde_json::to_value(&freed).unwrap();
        let paths = report["not_removed"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&serde_json::json!(object)));
        assert!(paths.contains(&serde_json::json!(leftover)));
        assert!(object.exists());
        assert!(leftover.exists());
    }

    #[test]
    fn a_sweep_keeps_what_is_needed_and_what_is_new() {
        for (_dir, cache) in [cache(), fat()] {
            let needed = store(&cache, b"needed");
            let unneeded = store(&cache, b"unneeded");
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            let keep = Needed {
                objects: HashSet::from([needed.digest]),
                ..Needed::default()
            };
            assert_eq!(
                cache.sweep(&keep, SystemTime::now()).objects,
                0,
                "nothing is old yet"
            );
            let freed = cache.sweep(&keep, later);
            assert_eq!((freed.objects, freed.bytes), (1, 8));
            assert!(cache.find(&needed.digest, Some(6)).is_some());
            assert!(cache.find(&unneeded.digest, Some(8)).is_none());
        }
    }

    #[test]
    fn a_sweep_removes_what_stopped_runs_left_sealed() {
        for (_dir, cache) in [cache(), fat()] {
            // a download sealed by a run that died while publishing it, and a member sealed by
            // one that died while unpacking
            let partial = cache.root.join("partial");
            fs::write(partial.join("left.lock"), b"").unwrap();
            fs::write(partial.join("left.json"), b"{}").unwrap();
            let sealed = [
                partial.join("left.part"),
                cache.root.join("tmp").join("0123456789abcdef.new"),
            ];
            for path in &sealed {
                fs::write(path, b"abc").unwrap();
                seal(path);
            }
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).leftovers, 3);
            assert!(sealed.iter().all(|path| !path.exists()));
            let left: Vec<String> = fs::read_dir(partial)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(left, ["left.lock"], "a run may hold the lock open");
        }
    }

    #[test]
    fn a_sweep_removes_unlocked_staging_without_waiting() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("lock"), b"").unwrap();
            let member = folder.join("member.new");
            fs::write(&member, b"unfinished").unwrap();
            seal(&member);
            let freed = cache.sweep(&Needed::default(), SystemTime::now());
            assert_eq!(freed.leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn staging_without_locking_keeps_the_age_rule() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("unlocked"), b"").unwrap();
            let member = folder.join("member.new");
            fs::write(&member, b"unfinished").unwrap();
            let now = SystemTime::now();
            let written = now + GRACE;
            fs::OpenOptions::new()
                .write(true)
                .open(&member)
                .unwrap()
                .set_modified(written)
                .unwrap();
            seal(&member);
            assert_eq!(cache.sweep(&Needed::default(), now).leftovers, 0);
            assert_eq!(
                cache
                    .sweep(&Needed::default(), written + Duration::from_secs(60))
                    .leftovers,
                0,
                "a recent write keeps a member in an older folder"
            );
            assert!(member.exists());
            let later = written + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn a_run_that_opened_a_lock_before_a_sweep_owns_the_download_alone() {
        let (_dir, cache) = cache();
        let url = "https://example.org/raced";
        // every download leaves its lock file behind
        drop(cache.claim(url).unwrap());
        let key = digest::sha256(url.as_bytes()).hex();
        let lock = cache.root.join("partial").join(format!("{key}.lock"));
        // a run waiting for the download opens the lock, gc sweeps, then the run takes the lock
        let opened = super::super::partial::open_unfollowed(&lock, true).unwrap();
        cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        opened.try_lock().unwrap();
        assert!(
            matches!(cache.claim(url).unwrap(), super::super::Claim::Busy),
            "a second run owns the same download"
        );
    }

    #[test]
    fn a_sweep_keeps_the_records_of_archives_in_use() {
        let (_dir, cache) = cache();
        let used = digest::sha256(b"used archive");
        let unused = digest::sha256(b"unused archive");
        for archive in [used, unused] {
            cache.record_unpacked(&archive, &[]).unwrap();
        }
        let keep = Needed {
            unpacked: HashSet::from([used]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert_eq!(cache.unpacked_archives(), [used]);
    }

    #[test]
    fn verifying_marks_objects_whose_bytes_changed() {
        let (_dir, cache) = cache();
        let good = store(&cache, b"good");
        let bad = store(&cache, b"bad!");
        let mut permissions = fs::metadata(cache.object_path(&bad.digest))
            .unwrap()
            .permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(cache.object_path(&bad.digest), permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(cache.object_path(&bad.digest))
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
        let seen = AtomicUsize::new(0);
        let checked = cache.verify_all(3, &|bytes| {
            seen.fetch_add(usize::try_from(bytes).unwrap(), Ordering::Relaxed);
        });
        assert_eq!(checked.objects, 2);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(seen.load(Ordering::Relaxed), 8);
        assert!(cache.find(&good.digest, Some(4)).is_some());
        assert!(cache.find(&bad.digest, Some(4)).is_none());
        assert_eq!(
            cache.changed().into_keys().collect::<Vec<_>>(),
            [bad.digest],
            "projects linking to it can learn it changed"
        );
    }

    #[test]
    fn unreadable_objects_are_kept_without_a_change_mark() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"intact");
        let object = cache.object_path(&stored.digest);
        #[cfg(windows)]
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_DELETE: u32 = 4;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_DELETE)
                .open(&object)
                .unwrap()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rustix::process::geteuid().is_root() {
                return;
            }
            fs::set_permissions(&object, fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert!(cache.verify(&stored.digest).is_err());
        assert!(object.exists());
        assert!(cache.changed().is_empty());
        let checked = cache.verify_all(2, &|_| {});
        #[cfg(windows)]
        drop(held);
        assert!(
            checked.changed.is_empty(),
            "a read error is not changed bytes"
        );
        assert_eq!(checked.stuck, vec![]);
        assert!(object.exists(), "unreadable objects must stay in the cache");
        assert!(cache.changed().is_empty());
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 6);
        assert_eq!(
            serde_json::to_value(&checked).unwrap()["unreadable"],
            serde_json::json!([object])
        );
    }

    #[test]
    fn a_changed_object_held_open_is_still_marked() {
        let (_dir, cache) = cache();
        let bad = store(&cache, b"bad!");
        let object = cache.object_path(&bad.digest);
        damage(&object);
        let held = hold(&object);
        let checked = cache.verify_all(2, &|_| {});
        drop(held);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(checked.stuck, []);
        assert_eq!(fs::read(&object).unwrap(), b"BAD!");
    }

    #[test]
    fn objects_whose_bytes_rotted_wait_for_replacement() {
        for (_dir, cache) in [cache(), fat()] {
            let rotten = [store(&cache, b"rot1"), store(&cache, b"rot2")];
            for object in &rotten {
                damage(&cache.object_path(&object.digest));
                // bytes that rot on disk leave the read-only flag as it was
                seal(&cache.object_path(&object.digest));
            }
            assert!(!cache.verify(&rotten[0].digest).unwrap());
            let checked = cache.verify_all(2, &|_| {});
            assert_eq!(checked.changed.len(), 2);
            assert!(
                rotten
                    .iter()
                    .all(|object| checked.changed.contains(&object.digest))
            );
            assert_eq!(checked.stuck, vec![]);
            assert!(
                rotten
                    .iter()
                    .all(|object| cache.object_path(&object.digest).exists())
            );
        }
    }

    fn damage(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
    }
}
