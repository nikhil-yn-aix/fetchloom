//! what recipes made, under `builds/<key>/<tree>`: a result's tree in the encoding of unpacked
//! archives' records, so a record proves itself against the tree data.lock pins. a key may hold
//! several trees, as a step that is not reproducible gives.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Cache, Member, unpacked};
use crate::digest::{self, Sha256};
use crate::place::{FolderUse, real_folder};
use crate::stop::Stop;

/// how often a build waiting for another one of the same key looks again.
const KEY_WAIT: Duration = Duration::from_millis(50);

/// a result of a key this machine holds whole: its record and every object it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Built {
    pub(crate) tree: Sha256,
    /// sorted by path.
    pub(crate) members: Vec<Member>,
    /// when its record was written.
    pub(crate) recorded: SystemTime,
}

/// the lock of one key, held while a build of it runs. dropping it lets the next build in.
#[derive(Debug)]
pub(crate) struct KeyHold {
    _file: Option<File>,
}

/// how a file a step wrote looked when its bytes were hashed, so publishing it can tell that it
/// is still that file with those bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) size: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    /// whether the file has no other name and this user owns it, so it may move into the cache.
    pub(crate) movable: bool,
}

impl Seen {
    pub(crate) fn of(metadata: &fs::Metadata) -> Seen {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Seen {
                size: metadata.len(),
                modified: metadata.modified().ok(),
                identity: (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ),
                movable: metadata.nlink() == 1
                    && metadata.uid() == rustix::process::geteuid().as_raw(),
            }
        }
        #[cfg(not(unix))]
        Seen {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            movable: false,
        }
    }
}

impl Cache {
    /// records `members` as a result of `key`, on disk before this returns, and gives its tree.
    pub(crate) fn record_result(&self, key: &Sha256, members: &[Member]) -> io::Result<Sha256> {
        let text = unpacked::tree(members);
        let tree = digest::sha256(text.as_bytes());
        let folder = self.key_folder(key)?;
        self.write_private(&folder.join(tree.hex()), text.as_bytes())?;
        Ok(tree)
    }

    /// the results of `key` this machine holds whole, newest first: records that hash to their
    /// names, whose objects are all here, and that no other user wrote or may write, since such a
    /// record could pair the key with any files.
    pub(crate) fn results(&self, key: &Sha256) -> Vec<Built> {
        let folder = self.root.join("builds").join(key.hex());
        let Ok(entries) = fs::read_dir(&folder) else {
            return Vec::new();
        };
        let mut built: Vec<Built> = entries
            .flatten()
            .filter_map(|entry| {
                let tree = Sha256::from_hex(entry.file_name().to_str()?)?;
                self.result_in(&entry.path(), tree)
            })
            .collect();
        built.sort_by(|a, b| b.recorded.cmp(&a.recorded).then(a.tree.cmp(&b.tree)));
        built
    }

    /// the result recorded at `path` as `tree`, when it is whole and this user's alone.
    fn result_in(&self, path: &Path, tree: Sha256) -> Option<Built> {
        let file = crate::volume::read_unfollowed(path).ok()?;
        let metadata = file.metadata().ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0
            {
                return None;
            }
        }
        let bytes = fs::read(path).ok()?;
        if digest::sha256(&bytes) != tree {
            return None;
        }
        let members = unpacked::parse(std::str::from_utf8(&bytes).ok()?)?;
        members
            .iter()
            .all(|member| self.find(&member.sha256, Some(member.size)).is_some())
            .then(|| Built {
                tree,
                members,
                recorded: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
    }

    /// takes the lock of `key`, waiting while another build of it runs, after `waiting` says so
    /// once. without locks, builds of one key run at once, costing only the second's time.
    ///
    /// # Errors
    ///
    /// the lock cannot be made, or `Interrupted` when `stop` asks while waiting.
    pub(crate) fn hold_key(
        &self,
        key: &Sha256,
        stop: &Stop,
        waiting: impl FnOnce(),
    ) -> io::Result<KeyHold> {
        let file = crate::volume::open_unfollowed(&self.key_folder(key)?.join("lock"), true)?;
        let mut waiting = Some(waiting);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(KeyHold { _file: Some(file) }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(err)) => {
                    tracing::debug!(%err, "no lock on this disk, so builds of one key may overlap");
                    return Ok(KeyHold { _file: None });
                }
            }
            if let Some(waiting) = waiting.take() {
                waiting();
            }
            if stop.is_asked() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stopped while waiting for another build",
                ));
            }
            std::thread::sleep(KEY_WAIT);
        }
    }

    /// `builds/<key>`, made when missing, never a link.
    fn key_folder(&self, key: &Sha256) -> io::Result<PathBuf> {
        let builds = self.root.join("builds");
        real_folder(&builds, FolderUse::Cache)?;
        let folder = builds.join(key.hex());
        real_folder(&folder, FolderUse::Cache)?;
        Ok(folder)
    }

    /// moves the sealed file at `path` into the cache as the object `digest`, never replacing an
    /// intact one, and drops its name at `path`. a new object must be that very file.
    ///
    /// # Errors
    ///
    /// the cache cannot take it, or `InvalidData` when the object is another file.
    #[cfg(unix)]
    pub(crate) fn publish_captured(
        &self,
        path: &Path,
        digest: Sha256,
        seen: &Seen,
    ) -> io::Result<super::Stored> {
        use std::os::unix::fs::MetadataExt;
        let stored = self.link_object(path, digest, seen.size, None)?;
        if stored.new {
            let object = fs::symlink_metadata(self.object_path(&digest))?;
            if (object.dev(), object.ino()) != (seen.identity.0, seen.identity.1) {
                self.verify(&digest)?;
                return Err(changed(path));
            }
        }
        Ok(stored)
    }
}

/// readies the file a step wrote at `path` to become its bytes' object where it is: read-only,
/// dated by its birth and, with `flush`, on disk. one that changed since `seen` is refused.
///
/// # Errors
///
/// the file changed, or cannot be sealed, which its caller answers by copying it instead.
#[cfg(unix)]
pub(crate) fn seal_captured(path: &Path, seen: &Seen, flush: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = crate::volume::read_unfollowed(path)?;
    if Seen::of(&file.metadata()?) != *seen {
        return Err(changed(path));
    }
    file.set_permissions(fs::Permissions::from_mode(0o444))?;
    super::object::date_by_birth(&file, None)?;
    if flush {
        file.sync_all()?;
    }
    // a batched object still needs its plain fsync on macos for the batch's `F_FULLFSYNC`
    #[cfg(target_os = "macos")]
    if !flush {
        rustix::fs::fsync(&file)?;
    }
    Ok(())
}

/// the error for a file a step wrote that changed while fetchloom took it in.
#[cfg(unix)]
fn changed(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} changed while fetchloom read it", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{cache, store};

    fn member(cache: &Cache, path: &str, bytes: &[u8]) -> Member {
        let stored = store(cache, bytes);
        Member {
            path: path.to_owned(),
            size: stored.size,
            sha256: stored.digest,
        }
    }

    /// a record is found by its key once every object it names is here, and is the tree data.lock
    /// would pin for those files.
    #[test]
    fn a_whole_result_is_found_by_its_key() {
        let (_dir, cache) = cache();
        let key = digest::sha256(b"key");
        let members = vec![member(&cache, "b.csv", b"b"), member(&cache, "a.csv", b"a")];
        let tree = cache.record_result(&key, &members).unwrap();
        assert_eq!(tree, crate::cache::tree_hash(&members));
        let found = cache.results(&key);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tree, tree);
        assert_eq!(
            found[0]
                .members
                .iter()
                .map(|member| member.path.as_str())
                .collect::<Vec<_>>(),
            ["a.csv", "b.csv"]
        );
        assert_eq!(
            cache.results(&digest::sha256(b"another key")),
            Vec::<Built>::new()
        );
    }

    /// a result whose object is gone, or a record whose bytes are not its tree, is no result.
    #[test]
    fn a_partial_or_edited_result_is_not_found() {
        let (_dir, cache) = cache();
        let key = digest::sha256(b"key");
        let gone = Member {
            path: "gone.csv".to_owned(),
            size: 4,
            sha256: digest::sha256(b"gone"),
        };
        cache.record_result(&key, &[gone]).unwrap();
        assert_eq!(cache.results(&key), Vec::<Built>::new());
        let other = digest::sha256(b"other key");
        let tree = cache
            .record_result(&other, &[member(&cache, "a.csv", b"a")])
            .unwrap();
        let record = cache
            .root()
            .join("builds")
            .join(other.hex())
            .join(tree.hex());
        crate::cache::tests::rewrite(&record, b"a.csv\t1\t00\n");
        assert_eq!(cache.results(&other), Vec::<Built>::new());
    }

    /// a record others may write is never used: in a shared cache, someone could change its bytes
    /// and rename it to their hash, pairing the key with files of their choosing.
    #[cfg(unix)]
    #[test]
    fn a_record_others_may_write_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        let key = digest::sha256(b"key");
        let tree = cache
            .record_result(&key, &[member(&cache, "a.csv", b"a")])
            .unwrap();
        let record = cache.root().join("builds").join(key.hex()).join(tree.hex());
        for mode in [0o664, 0o646] {
            fs::set_permissions(&record, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(cache.results(&key), Vec::<Built>::new(), "{mode:o}");
        }
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(cache.results(&key).len(), 1);
    }

    /// a second build of a key waits for the first and says so once, and the hold lets go when
    /// dropped.
    #[test]
    fn one_build_of_a_key_runs_at_a_time() {
        let (_dir, cache) = cache();
        let key = digest::sha256(b"key");
        let stop = Stop::default();
        let first = cache.hold_key(&key, &stop, || {}).unwrap();
        let (told, waiting) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let second =
                scope.spawn(|| cache.hold_key(&key, &stop, move || told.send(()).unwrap()));
            waiting
                .recv_timeout(Duration::from_secs(30))
                .expect("the second build never waited");
            drop(first);
            assert!(second.join().unwrap().is_ok());
        });
        assert!(waiting.try_recv().is_err(), "it says so once");
    }
}
