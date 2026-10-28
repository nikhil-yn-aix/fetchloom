//! the content-addressed cache: one read-only file per object, named by its SHA-256.

mod object;
mod partial;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use partial::{Claim, Partial, PartialWriter};

use crate::digest::{self, Md5, Sha256};

const LAYOUT: &str = "1\n";
const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache made by fetchloom. backup tools can skip this folder.\n";

#[derive(Clone, Debug)]
pub(crate) struct Cache {
    root: PathBuf,
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
        for folder in ["objects", "partial", "urls", "stated", "tmp"] {
            fs::create_dir_all(root.join(folder))?;
        }
        let cache = Cache { root };
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
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
        let mut file = tempfile::NamedTempFile::new_in(self.root.join("tmp"))?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|err| err.error)?;
        Ok(())
    }

    /// moves a finished file into the cache under its hash. the move never replaces an existing
    /// object, so of two processes racing for the same bytes exactly one places them.
    fn publish(
        &self,
        source: &Path,
        digest: Sha256,
        size: u64,
        md5: Option<Md5>,
    ) -> io::Result<Stored> {
        let path = self.object_path(&digest);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let new = match fs::hard_link(source, &path) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
            // a filesystem without hardlinks gets a plain rename, guarded by a check
            Err(_) if path.exists() => false,
            Err(_) => {
                fs::rename(source, &path)?;
                true
            }
        };
        if source.exists() {
            fs::remove_file(source)?;
        }
        if new {
            let mut permissions = fs::metadata(&path)?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions)?;
        }
        Ok(Stored {
            digest,
            size,
            path,
            md5,
            new,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn cache() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        (dir, cache)
    }

    pub(super) fn store(cache: &Cache, bytes: &[u8]) -> Stored {
        let mut writer = cache.object_writer(false).unwrap();
        writer.write_all(bytes).unwrap();
        writer.close().unwrap().publish().unwrap()
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
        let (_dir, cache) = cache();
        let first = store(&cache, b"same");
        let second = store(&cache, b"same");
        assert!(first.new);
        assert!(!second.new);
        assert_eq!(first.path, second.path);
        let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
        assert_eq!(leftovers, 0, "nothing is left behind in tmp");
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
