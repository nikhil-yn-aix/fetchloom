//! the content-addressed cache: one read-only file per object, named by its SHA-256.

mod object;
mod partial;
mod unpacked;
mod upkeep;
mod volume;

#[cfg(test)]
mod link_tests;

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

pub(crate) use object::{NewObject, Sealed};
pub(crate) use partial::{Claim, Partial, PartialWriter};
pub(crate) use unpacked::{Member, tree_hash};
pub(crate) use upkeep::{Checked, Freed, Needed};
use volume::Volume;

use crate::digest::{self, Md5, Sha256};

const LAYOUT: &str = "1\n";
const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache made by fetchloom. backup tools can skip this folder.\n";
const FOLDERS: [&str; 7] = [
    "objects", "partial", "urls", "stated", "unpacked", "projects", "tmp",
];

#[derive(Clone, Debug)]
pub(crate) struct Cache {
    root: PathBuf,
    /// which folders under `objects/`, by the first byte of their hashes, this run has seen to be
    /// real folders.
    checked: Arc<[AtomicBool; 256]>,
    volume: Arc<Volume>,
}

/// an object as it sits in the cache after a write.
#[derive(Debug)]
pub(crate) struct Stored {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
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
        } else if cfg!(target_os = "macos") {
            set("HOME").map(|home| home.join("Library").join("Caches").join("fetchloom"))
        } else {
            set("XDG_CACHE_HOME")
                .or_else(|| set("HOME").map(|home| home.join(".cache")))
                .map(|path| path.join("fetchloom"))
        }
    }

    /// opens the cache at `root`, creating its folders the first time.
    pub(crate) fn open(root: PathBuf) -> io::Result<Cache> {
        fs::create_dir_all(&root)?;
        for folder in FOLDERS {
            real_folder(&root.join(folder))?;
        }
        let cache = Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            root,
            checked: Arc::new(std::array::from_fn(|_| AtomicBool::new(false))),
        };
        for (name, text) in [("layout", LAYOUT), ("CACHEDIR.TAG", TAG)] {
            let path = cache.root.join(name);
            if !path.exists() {
                cache.write_atomically(&path, text.as_bytes())?;
            }
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

    /// the object's path, when the cache holds it. a known size must match, which catches a file
    /// cut short by a crash.
    pub(crate) fn find(&self, digest: &Sha256, size: Option<u64>) -> Option<PathBuf> {
        let path = self.object_path(digest);
        let metadata = fs::metadata(&path).ok()?;
        (metadata.is_file() && size.is_none_or(|size| metadata.len() == size)).then_some(path)
    }

    /// removes an object the cache no longer needs to hold, such as an archive whose files are
    /// all unpacked. files placed from it keep their bytes.
    pub(crate) fn drop_object(&self, digest: &Sha256) -> io::Result<()> {
        match self.volume.remove(&self.object_path(digest)) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    /// hashes an object again. an object whose bytes changed, such as through a hardlink someone
    /// edited, is removed so the next run downloads it again. says whether it was intact.
    pub(crate) fn verify(&self, digest: &Sha256) -> io::Result<bool> {
        let path = self.object_path(digest);
        match Sha256::of_file(&path) {
            Ok(actual) if actual == *digest => Ok(true),
            Ok(_) => {
                fs::remove_file(&path)?;
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
        self.write_atomically(&self.url_record_path(&record.url), &text)
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
        real_folder(&self.root.join("stated").join("md5"))?;
        if let Some(parent) = path.parent() {
            real_folder(parent)?;
        }
        self.write_atomically(&path, format!("{digest} {size}\n").as_bytes())
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
        let written = fill_and_rename(file, bytes, &temporary, path);
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }

    /// moves a sealed file into the cache under its hash. the link never replaces an existing
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
                real_folder(folder)?;
            }
            self.checked[first].store(true, Ordering::Relaxed);
        }
        let new = link_new(&self.volume, source, &path)?;
        Ok(Stored {
            digest,
            size,
            path,
            md5,
            new,
        })
    }
}

/// links `source` to `path` unless something is there already, then removes `source`. says
/// whether the link is new.
fn link_new(volume: &Volume, source: &Path, path: &Path) -> io::Result<bool> {
    let new = match volume.hard_link(source, path) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
        // something removed the object's folder since this run checked it
        Err(err) if err.kind() == io::ErrorKind::NotFound && source.exists() => {
            if let Some(parent) = path.parent() {
                real_folder(parent)?;
            }
            return link_new(volume, source, path);
        }
        // a filesystem without hardlinks gets a plain rename, guarded by a check
        Err(_) if path.exists() => false,
        Err(_) => {
            fs::rename(source, path)?;
            return Ok(true);
        }
    };
    volume.remove(source)?;
    Ok(new)
}

/// writes `bytes` into the new file at `temporary`, flushes them to disk and moves the file to
/// `path`.
fn fill_and_rename(mut file: File, bytes: &[u8], temporary: &Path, path: &Path) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // a rename moves a name: a link at `path` is replaced, never followed
    fs::rename(temporary, path)
}

/// makes `folder` when it is missing, and checks that it is a real folder rather than a link, so
/// that nothing the cache writes there can land outside it.
fn real_folder(folder: &Path) -> io::Result<()> {
    if let Err(err) = fs::create_dir(folder)
        && err.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(err);
    }
    let found = fs::symlink_metadata(folder)?.file_type();
    if found.is_dir() {
        return Ok(());
    }
    let what = if found.is_symlink() {
        "a link"
    } else {
        "a file"
    };
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is {what}, and the cache keeps only real folders",
            folder.display()
        ),
    ))
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
    pub(super) fn seal(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }

    /// keeps `path` from being removed while the result lives.
    #[cfg(windows)]
    pub(super) fn hold(path: &Path) -> impl Sized {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[cfg(unix)]
    pub(super) fn hold(path: &Path) -> impl Sized {
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
        object.seal().unwrap().publish(cache).unwrap()
    }

    #[test]
    fn a_new_cache_marks_itself_for_backup_tools_and_records_its_layout() {
        let (_dir, cache) = cache();
        let tag = fs::read_to_string(cache.root.join("CACHEDIR.TAG")).unwrap();
        assert!(tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55\n"));
        assert_eq!(
            fs::read_to_string(cache.root.join("layout")).unwrap(),
            "1\n"
        );
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
        assert_eq!(fs::read(&stored.path).unwrap(), b"abc");
        assert!(fs::metadata(&stored.path).unwrap().permissions().readonly());
        assert_eq!(
            cache.find(&stored.digest, Some(3)),
            Some(stored.path.clone())
        );
        assert_eq!(cache.find(&stored.digest, None), Some(stored.path.clone()));
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
            assert_eq!(first.path, second.path);
            assert!(fs::metadata(&first.path).unwrap().permissions().readonly());
            let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
            assert_eq!(leftovers, 0, "nothing is left behind in tmp");
        }
    }

    #[test]
    fn a_dropped_object_leaves_the_cache() {
        for (_dir, cache) in [cache(), fat()] {
            let archive = store(&cache, b"an archive whose files are unpacked");
            cache.drop_object(&archive.digest).unwrap();
            assert_eq!(cache.find(&archive.digest, None), None);
        }
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
