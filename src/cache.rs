//! the content-addressed cache: one read-only file per object, named by its SHA-256.

mod changed;
mod object;
mod partial;
mod staging;
mod unpacked;
mod upkeep;

#[cfg(test)]
mod link_tests;

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

pub(crate) use changed::Doubt;
pub(crate) use object::{NewObject, Sealed};
pub(crate) use partial::{Claim, Partial, PartialWriter};
pub(crate) use staging::Staging;
pub(crate) use unpacked::{Member, tree_hash};
pub(crate) use upkeep::{Checked, Freed, Needed};

use crate::digest::{self, Md5, Sha256};
use crate::place::{FolderUse, real_folder};
use crate::volume::Volume;

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache made by fetchloom. backup tools can skip this folder.\n";
const FOLDERS: [&str; 8] = [
    "objects", "partial", "urls", "stated", "unpacked", "projects", "tmp", "changed",
];

#[derive(Clone, Debug)]
pub(crate) struct Cache {
    root: PathBuf,
    /// which folders under `objects/`, by the first byte of their hashes, this run has seen to be
    /// real folders.
    checked: Arc<[AtomicBool; 256]>,
    volume: Arc<Volume>,
}

#[derive(Debug)]
pub(crate) struct Stored {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    /// the MD5 of the bytes, when it was computed to check one a source stated.
    pub(crate) md5: Option<Md5>,
    /// false when the cache already held these bytes.
    pub(crate) new: bool,
}

/// how a server identified the bytes it sent, so a later request can ask whether they changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Validators {
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
}

/// what a URL served the last time it was downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UrlRecord {
    pub(crate) url: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
    /// the file name the server gave, when it gave one.
    pub(crate) name: Option<String>,
    pub(crate) validators: Validators,
}

impl Cache {
    /// the cache folder: `FETCHLOOM_CACHE` when set, else the platform's cache folder.
    pub(crate) fn locate(var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        let set = |name: &str| {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        if let Some(path) = set("FETCHLOOM_CACHE") {
            return Some(path);
        }
        if cfg!(windows) {
            set("LOCALAPPDATA").map(|path| path.join("fetchloom"))
        } else {
            set("XDG_CACHE_HOME")
                .or_else(|| set("HOME").map(|home| home.join(".cache")))
                .map(|path| path.join("fetchloom"))
        }
    }

    /// opens the cache at `root`, creating its folders the first time.
    pub(crate) fn open(root: PathBuf) -> io::Result<Cache> {
        let _span = tracing::debug_span!("open_cache").entered();
        fs::create_dir_all(&root)?;
        for folder in FOLDERS {
            real_folder(&root.join(folder), FolderUse::Cache)?;
        }
        let cache = Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            root,
            checked: Arc::new(std::array::from_fn(|_| AtomicBool::new(false))),
        };
        let tag = cache.root.join("CACHEDIR.TAG");
        if !tag.exists() {
            cache.write_atomically(&tag, TAG.as_bytes())?;
        }
        Ok(cache)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn object_path(&self, digest: &Sha256) -> PathBuf {
        let hex = digest.hex();
        self.root.join("objects").join(&hex[..2]).join(&hex[2..])
    }

    /// the object's path, when the cache holds it. a known size must match, and writable or
    /// marked objects must still hash to their names.
    pub(crate) fn find(&self, digest: &Sha256, size: Option<u64>) -> Option<PathBuf> {
        let path = self.object_path(digest);
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_file() || size.is_some_and(|size| metadata.len() != size) {
            return None;
        }
        if changed::doubted(&metadata, self.changed_at(digest))
            && Sha256::of_file(&path).ok()? != *digest
        {
            return None;
        }
        Some(path)
    }

    /// removes an object the cache no longer needs to hold, such as an archive whose files are
    /// all unpacked. files placed from it keep their bytes.
    pub(crate) fn drop_object(&self, digest: &Sha256) -> io::Result<()> {
        match self.volume.remove(&self.object_path(digest)) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    /// hashes an object again. changed bytes are marked for replacement and kept until a good
    /// copy arrives. the mark also tells projects to check their placed hardlinks. a read error
    /// leaves the object and its change mark untouched.
    pub(crate) fn verify(&self, digest: &Sha256) -> io::Result<bool> {
        let path = self.object_path(digest);
        match Sha256::of_file(&path) {
            Ok(actual) if actual == *digest => Ok(true),
            Ok(_) => {
                self.mark_changed(digest)?;
                Ok(false)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn url_record(&self, url: &str) -> Option<UrlRecord> {
        let text = fs::read(self.url_record_path(url)).ok()?;
        let record: UrlRecord = serde_json::from_slice(&text).ok()?;
        (record.url == url).then_some(record)
    }

    pub(crate) fn save_url_record(&self, record: &UrlRecord) -> io::Result<()> {
        let text = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        self.write_hint(&self.url_record_path(&record.url), &text)
    }

    /// the object holding the bytes a source stated this MD5 for, when an earlier download
    /// checked it.
    pub(crate) fn stated_md5(&self, md5: &Md5) -> Option<(Sha256, u64)> {
        let text = fs::read_to_string(self.stated_path(md5)).ok()?;
        let (digest, size) = text.trim().split_once(' ')?;
        Some((Sha256::from_hex(digest)?, size.parse().ok()?))
    }

    pub(crate) fn save_stated_md5(&self, md5: &Md5, digest: &Sha256, size: u64) -> io::Result<()> {
        let path = self.stated_path(md5);
        real_folder(&self.root.join("stated").join("md5"), FolderUse::Cache)?;
        if let Some(parent) = path.parent() {
            real_folder(parent, FolderUse::Cache)?;
        }
        self.write_hint(&path, format!("{digest} {size}\n").as_bytes())
    }

    fn url_record_path(&self, url: &str) -> PathBuf {
        self.root
            .join("urls")
            .join(format!("{}.json", digest::sha256(url.as_bytes()).hex()))
    }

    fn stated_path(&self, md5: &Md5) -> PathBuf {
        let hex = md5.hex();
        self.root
            .join("stated")
            .join("md5")
            .join(&hex[..2])
            .join(&hex[2..])
    }

    /// replaces `path` in one step, so a reader sees the old file or the new one, never half of one.
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_file(path, bytes, true)
    }

    /// replaces `path` in one step without flushing it first. for records whose loss after a power
    /// cut costs only a download or a resume, and which read as absent when torn. dropping their
    /// flushes halved fetching 1,000 files of 100 KiB from a server on the same machine.
    pub(crate) fn write_hint(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_file(path, bytes, false)
    }

    fn replace_file(&self, path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
        let (temporary, file) = loop {
            let temporary = self
                .root
                .join("tmp")
                .join(format!("{:016x}.tmp", fastrand::u64(..)));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => break (temporary, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        };
        let written = fill_and_rename(file, bytes, &temporary, path, durable);
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }

    /// moves a sealed file into the cache under its hash. the link never replaces an intact
    /// object, so of two processes racing for the same bytes exactly one places them.
    fn link_object(
        &self,
        source: &Path,
        digest: Sha256,
        size: u64,
        md5: Option<Md5>,
    ) -> io::Result<Stored> {
        let path = self.object_path(&digest);
        let first = usize::from(digest.bytes()[0]);
        if !self.checked[first].load(Ordering::Relaxed) {
            if let Some(folder) = path.parent() {
                real_folder(folder, FolderUse::Cache)?;
            }
            self.checked[first].store(true, Ordering::Relaxed);
        }
        let new = self.link_new(source, &path, &digest, size)?;
        Ok(Stored {
            digest,
            size,
            md5,
            new,
        })
    }

    /// links `source` to `path` unless the object is there already, then removes `source`. says
    /// whether the link is new. a file at `path` that does not hold these bytes, such as one an
    /// edit through a writable hardlink changed, is marked as changed and replaced, since keeping
    /// it would place the edit in every project instead of these bytes. one that cannot be read
    /// stays: nothing shows it changed.
    fn link_new(&self, source: &Path, path: &Path, digest: &Sha256, size: u64) -> io::Result<bool> {
        let volume = &self.volume;
        let new = match volume.hard_link(source, path) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
            // something removed the object's folder since this run checked it
            Err(err) if err.kind() == io::ErrorKind::NotFound && source.exists() => {
                if let Some(parent) = path.parent() {
                    real_folder(parent, FolderUse::Cache)?;
                }
                return self.link_new(source, path, digest, size);
            }
            // a filesystem without hardlinks gets a plain rename, guarded by a check
            Err(_) if path.exists() => false,
            Err(_) => match volume.rename(source, path) {
                Ok(()) => return Ok(true),
                // another writer published the object since the check, and the rename could not
                // replace it
                Err(_) if path.exists() => false,
                Err(err) => return Err(err),
            },
        };
        if !new {
            let found = fs::metadata(path);
            match existing(&found, size, || Sha256::of_file(path), digest) {
                Existing::Keep => {}
                Existing::Gone => return self.link_new(source, path, digest, size),
                Existing::Changed => {
                    if found.is_ok() {
                        self.mark_replaced(digest, source)?;
                    }
                    match volume.replace(source, path) {
                        Ok(()) => return Ok(true),
                        // another repair may have replaced the read-only destination while ours
                        // was being opened to clear its flag
                        Err(_)
                            if source.exists()
                                && Sha256::of_file(path).is_ok_and(|actual| actual == *digest) => {}
                        Err(err) => {
                            return Err(io::Error::new(
                                err.kind(),
                                format!("could not replace {}: {err}", path.display()),
                            ));
                        }
                    }
                }
            }
        }
        volume.remove(source)?;
        Ok(new)
    }
}

/// what is at an object's path when a new copy of its bytes arrives.
enum Existing {
    /// these bytes, or a file that cannot be read, which is no proof that it changed
    Keep,
    /// something else: other bytes, another size or not a file
    Changed,
    /// nothing, since another process took it away
    Gone,
}

/// judges what `found` at an object's path holds, hashing it with `hash` only when its size
/// matches.
fn existing(
    found: &io::Result<fs::Metadata>,
    size: u64,
    hash: impl FnOnce() -> io::Result<Sha256>,
    digest: &Sha256,
) -> Existing {
    match found {
        Ok(found) if found.is_file() && found.len() == size => match hash() {
            Ok(actual) if actual == *digest => Existing::Keep,
            Ok(_) => Existing::Changed,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Existing::Gone,
            Err(_) => Existing::Keep,
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Existing::Gone,
        _ => Existing::Changed,
    }
}

/// writes `bytes` into the new file at `temporary`, flushes them to disk when `durable` and moves
/// the file to `path`.
fn fill_and_rename(
    mut file: File,
    bytes: &[u8],
    temporary: &Path,
    path: &Path,
    durable: bool,
) -> io::Result<()> {
    file.write_all(bytes)?;
    if durable {
        file.sync_all()?;
    }
    drop(file);
    // a rename moves a name: a link at `path` is replaced, never followed
    fs::rename(temporary, path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(super) fn cache() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        (dir, cache)
    }

    /// a cache that acts as if it sat on FAT32 or exFAT: it cannot hardlink, and it refuses to
    /// remove a read-only file, as windows does there.
    pub(crate) fn fat() -> (tempfile::TempDir, Cache) {
        let (dir, mut cache) = cache();
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), false));
        (dir, cache)
    }

    /// whether the volume under `dir` can hardlink. FAT32 and exFAT cannot.
    pub(crate) fn hardlinks(dir: &Path) -> bool {
        let probe = dir.join("probe");
        let link = dir.join("probe link");
        fs::write(&probe, b"").unwrap();
        let linked = fs::hard_link(&probe, &link).is_ok();
        for path in [probe, link] {
            let _ = fs::remove_file(path);
        }
        linked
    }

    /// makes the file at `path` read-only, as every object is.
    pub(crate) fn seal(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }

    /// keeps `path` from being removed while the result lives.
    #[cfg(windows)]
    pub(crate) fn hold(path: &Path) -> impl Sized {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[cfg(unix)]
    pub(crate) fn hold(path: &Path) -> impl Sized {
        use std::os::unix::fs::PermissionsExt;
        struct Locked(PathBuf);
        impl Drop for Locked {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        let folder = path.parent().unwrap().to_path_buf();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        Locked(folder)
    }

    pub(crate) fn store(cache: &Cache, bytes: &[u8]) -> Stored {
        let mut object = cache.new_object().unwrap();
        object.write_all(bytes).unwrap();
        object.seal(true).unwrap().publish(cache).unwrap()
    }

    #[test]
    fn a_new_cache_marks_itself_for_backup_tools_without_a_layout_marker() {
        let (_dir, cache) = cache();
        let tag = fs::read_to_string(cache.root.join("CACHEDIR.TAG")).unwrap();
        assert!(tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55\n"));
        assert!(!cache.root.join("layout").exists());
    }

    #[test]
    fn old_layout_markers_and_unknown_files_do_not_affect_upkeep() {
        let (_dir, cache) = cache();
        for name in ["layout", "unknown"] {
            fs::write(cache.root.join(name), b"leave this alone").unwrap();
        }
        let cache = Cache::open(cache.root.clone()).unwrap();
        store(&cache, b"abc");

        let checked = cache.verify_all(1, &|_| {});
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 3);
        assert_eq!(checked.changed, []);
        assert_eq!(checked.stuck, []);
        assert_eq!(checked.unreadable, []);
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());

        let later = std::time::SystemTime::now() + std::time::Duration::from_hours(48);
        let freed = cache.sweep(&Needed::default(), later);
        assert_eq!(freed.objects, 1);
        assert_eq!(freed.bytes, 3);
        assert_eq!(freed.leftovers, 0);
        for name in ["layout", "unknown"] {
            assert_eq!(
                fs::read(cache.root.join(name)).unwrap(),
                b"leave this alone"
            );
        }
    }

    #[test]
    fn cache_folders_cannot_be_files() {
        for name in FOLDERS {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(name), b"keep these bytes").unwrap();

            let error = Cache::open(dir.path().to_path_buf()).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                fs::read(dir.path().join(name)).unwrap(),
                b"keep these bytes"
            );
        }
    }

    #[test]
    fn an_object_lands_read_only_under_its_hash() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"abc");
        assert!(stored.new);
        assert_eq!(
            stored.digest.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(fs::read(cache.object_path(&stored.digest)).unwrap(), b"abc");
        assert!(
            fs::metadata(cache.object_path(&stored.digest))
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(
            cache.find(&stored.digest, Some(3)),
            Some(cache.object_path(&stored.digest))
        );
        assert_eq!(
            cache.find(&stored.digest, None),
            Some(cache.object_path(&stored.digest))
        );
        assert_eq!(
            cache.find(&stored.digest, Some(4)),
            None,
            "a size mismatch is not a hit"
        );
    }

    #[test]
    fn the_same_bytes_twice_are_stored_once() {
        for (_dir, cache) in [cache(), fat()] {
            let first = store(&cache, b"same");
            let second = store(&cache, b"same");
            assert!(first.new);
            assert!(!second.new);
            assert_eq!(first.digest, second.digest);
            assert!(
                fs::metadata(cache.object_path(&first.digest))
                    .unwrap()
                    .permissions()
                    .readonly()
            );
            let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
            assert_eq!(leftovers, 0, "nothing is left behind in tmp");
        }
    }

    #[test]
    fn bytes_another_writer_published_since_the_check_are_not_new() {
        let (_dir, mut cache) = cache();
        // FAT refuses the link, and another writer publishes the same bytes before the rename
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), false).losing_renames());
        assert_eq!(cache.find(&digest::sha256(b"same"), None), None);
        let stored = store(&cache, b"same");
        assert!(!stored.new);
        assert_eq!(
            fs::read(cache.object_path(&stored.digest)).unwrap(),
            b"same"
        );
        assert!(
            fs::metadata(cache.object_path(&stored.digest))
                .unwrap()
                .permissions()
                .readonly()
        );
        let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
        assert_eq!(leftovers, 0, "nothing is left behind in tmp");
    }

    #[test]
    fn a_dropped_object_leaves_the_cache() {
        for (_dir, cache) in [cache(), fat()] {
            let archive = store(&cache, b"an archive whose files are unpacked");
            cache.drop_object(&archive.digest).unwrap();
            assert_eq!(cache.find(&archive.digest, None), None);
        }
    }

    /// writes `bytes` over the object at `path`, as an edit through a hardlink someone made
    /// writable does.
    fn rewrite(path: &Path, bytes: &[u8]) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn bytes_stored_again_replace_an_object_whose_size_changed() {
        for (_dir, cache) in [cache(), fat()] {
            let first = store(&cache, b"abc");
            let object = cache.object_path(&first.digest);
            rewrite(&object, b"abcX");
            let again = store(&cache, b"abc");
            assert_eq!(
                String::from_utf8(fs::read(&object).unwrap()).unwrap(),
                "abc"
            );
            assert!(again.new);
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
            assert_eq!(
                cache.changed().into_keys().collect::<Vec<_>>(),
                [first.digest],
                "projects linking to the changed bytes can learn it"
            );
        }
    }

    #[test]
    fn bytes_stored_again_replace_an_object_whose_bytes_changed_in_place() {
        for (_dir, cache) in [cache(), fat()] {
            let first = store(&cache, b"abc");
            let object = cache.object_path(&first.digest);
            rewrite(&object, b"abX");
            let again = store(&cache, b"abc");
            assert_eq!(
                String::from_utf8(fs::read(&object).unwrap()).unwrap(),
                "abc"
            );
            assert!(again.new);
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
            assert_eq!(
                cache.changed().into_keys().collect::<Vec<_>>(),
                [first.digest],
                "projects linking to the changed bytes can learn it"
            );
        }
    }

    #[test]
    fn a_doubted_object_keeps_its_name_until_repaired() {
        for (_dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            rewrite(&object, b"evil");
            seal(&object);
            cache.mark_changed(&stored.digest).unwrap();
            let other = Cache::open(cache.root().to_path_buf()).unwrap();
            for run in [&cache, &other] {
                assert!(
                    run.trusted(&stored.digest, Some(4), &run.changed())
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fs::read(&object).unwrap(), b"evil");
                assert!(run.find(&stored.digest, Some(4)).is_none());
            }
            store(&cache, b"good");
            // the other repair has not published yet, but placement can use the first copy
            let placed = cache.root().join("placed");
            fs::copy(&object, &placed).unwrap();
            assert_eq!(fs::read(&placed).unwrap(), b"good");
            assert!(!store(&other, b"good").new);
            assert_eq!(fs::read(&object).unwrap(), b"good");
        }
    }

    #[test]
    fn repairing_an_object_again_refreshes_its_change_mark() {
        use std::time::{Duration, UNIX_EPOCH};

        for (dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            let repair = |written| {
                rewrite(&object, b"evil");
                seal(&object);
                let source = dir.path().join("repair");
                fs::write(&source, b"good").unwrap();
                File::options()
                    .write(true)
                    .open(&source)
                    .unwrap()
                    .set_modified(written)
                    .unwrap();
                seal(&source);
                assert!(
                    cache
                        .link_object(&source, stored.digest, 4, None)
                        .unwrap()
                        .new
                );
                assert_eq!(fs::read(&object).unwrap(), b"good");
                let marked = UNIX_EPOCH + Duration::from_nanos(cache.changed()[&stored.digest]);
                assert!(marked < fs::metadata(&object).unwrap().modified().unwrap());
                marked
            };
            // fixed write times keep the two repairs distinct even on coarse clocks.
            let first = repair(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
            let second = repair(UNIX_EPOCH + Duration::from_secs(1_700_000_010));
            assert!(second > first, "another corruption needs a fresh mark");
        }
    }

    #[test]
    fn verification_keeps_a_copy_published_by_another_repair() {
        for (_dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            rewrite(&object, b"evil");
            seal(&object);
            let checked = cache.verify_all(1, &|_| {
                // verification has read the damaged bytes, then another repair publishes
                assert!(store(&cache, b"good").new);
            });
            assert_eq!(checked.changed, [stored.digest]);
            assert_eq!(fs::read(&object).unwrap(), b"good");
            assert!(cache.find(&stored.digest, Some(4)).is_some());
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
        }
    }

    #[test]
    fn repairing_a_read_only_object_preserves_placed_links() {
        let (dir, cache) = cache();
        if !hardlinks(dir.path()) {
            return;
        }
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let placed = dir.path().join("placed");
        fs::hard_link(&object, &placed).unwrap();
        rewrite(&object, b"evil");
        seal(&object);
        assert!(store(&cache, b"good").new);
        assert_eq!(fs::read(&object).unwrap(), b"good");
        assert_eq!(fs::read(&placed).unwrap(), b"evil");
        for path in [&object, &placed] {
            assert!(fs::metadata(path).unwrap().permissions().readonly());
        }
    }

    #[test]
    fn an_object_found_in_place_is_kept_only_while_it_holds_these_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        fs::write(&path, b"abc").unwrap();
        let found = fs::metadata(&path);
        let digest = crate::digest::sha256(b"abc");
        let hashed = |result: io::Result<Sha256>| move || result;
        let failed = |kind: io::ErrorKind| hashed(Err(io::Error::from(kind)));
        assert!(matches!(
            existing(&found, 3, hashed(Ok(digest)), &digest),
            Existing::Keep
        ));
        assert!(matches!(
            existing(
                &found,
                3,
                hashed(Ok(crate::digest::sha256(b"abX"))),
                &digest
            ),
            Existing::Changed
        ));
        assert!(matches!(
            existing(&found, 4, hashed(Ok(digest)), &digest),
            Existing::Changed
        ));
        assert!(
            matches!(
                existing(&found, 3, failed(io::ErrorKind::PermissionDenied), &digest),
                Existing::Keep
            ),
            "bytes that cannot be read are no proof of change"
        );
        assert!(
            matches!(
                existing(&found, 3, failed(io::ErrorKind::NotFound), &digest),
                Existing::Gone
            ),
            "another process took it away while it was hashed"
        );
        let missing = fs::metadata(dir.path().join("missing"));
        assert!(matches!(
            existing(&missing, 3, hashed(Ok(digest)), &digest),
            Existing::Gone
        ));
    }

    #[test]
    fn bytes_are_stored_only_once_they_hold_their_name() {
        let (_dir, cache) = cache();
        let object = cache.object_path(&store(&cache, b"abc").digest);
        rewrite(&object, b"abcX");
        seal(&object);
        let held = hold(&object);
        let mut again = cache.new_object().unwrap();
        again.write_all(b"abc").unwrap();
        let published = again.seal(true).unwrap().publish(&cache);
        drop(held);
        // root on unix can replace the changed object despite its folder's permissions
        let holds = fs::read(&object).unwrap() == b"abc";
        assert_eq!(published.is_ok(), holds, "{published:?}");
        assert!(fs::metadata(&object).unwrap().permissions().readonly());
    }

    #[test]
    fn url_records_round_trip() {
        let (_dir, cache) = cache();
        let record = UrlRecord {
            url: "https://example.org/iris.csv".to_owned(),
            sha256: store(&cache, b"x").digest,
            size: 1,
            name: Some("iris.csv".to_owned()),
            validators: Validators {
                etag: Some("\"x\"".to_owned()),
                last_modified: Some("Tue, 01 Oct 2024 10:00:00 GMT".to_owned()),
            },
        };
        cache.save_url_record(&record).unwrap();
        assert_eq!(cache.url_record(&record.url), Some(record));
        assert_eq!(cache.url_record("https://example.org/other"), None);
    }

    #[test]
    fn a_stated_md5_finds_its_object() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"abc");
        let md5 = Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap();
        assert_eq!(cache.stated_md5(&md5), None);
        cache.save_stated_md5(&md5, &stored.digest, 3).unwrap();
        assert_eq!(cache.stated_md5(&md5), Some((stored.digest, 3)));
    }

    #[test]
    fn locate_prefers_the_variable_then_the_platform_folder() {
        let var = |name: &str| (name == "FETCHLOOM_CACHE").then(|| OsString::from("/x/cache"));
        assert_eq!(Cache::locate(&var), Some(PathBuf::from("/x/cache")));
        let none = |_: &str| None;
        assert_eq!(Cache::locate(&none), None);
    }
}
