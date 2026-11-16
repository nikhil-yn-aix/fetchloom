//! the objects a key of this machine went with: an empty mark under `keyed/` for each, named as
//! the object is, which push and the web archives read so that a file a key brought is never
//! handed on unasked. a mark is made before its object takes its name, and gc drops it with the
//! object.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::folder::Folder;
use super::{Cache, Failure};
use crate::digest::Sha256;
use crate::error::Error;
use crate::stop::Stop;

/// the folder of the marks, below the cache's root.
const KEYED: &str = "keyed";

impl Cache {
    /// remembers that a credential of this machine went with a request of the download, or of
    /// the archive's download, that brings the object for `digest`. it comes before the object
    /// takes its name, so the object is never there without it.
    pub(crate) fn mark_keyed(&self, digest: &Sha256) -> io::Result<()> {
        let hex = digest.hex();
        let folder = Folder::open(&self.root, &Path::new(KEYED).join(&hex[..2]))?;
        match folder.create_new(&hex[2..]) {
            Err(err) if err.kind() != io::ErrorKind::AlreadyExists => Err(err),
            _ => Ok(()),
        }
    }

    /// whether a key of this machine went with what brought the object for `digest`, as
    /// [`Cache::mark_keyed`] remembers it. a mark that cannot be looked at counts as one.
    pub(crate) fn is_keyed(&self, digest: &Sha256) -> bool {
        !fs::symlink_metadata(self.keyed_path(digest))
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
    }

    /// removes the mark of each object the cache no longer holds, as gc has just freed them, or a
    /// pretend sweep would, recording a refused removal in `not_removed`.
    pub(super) fn forget_keyed_reporting(
        &self,
        stop: &Stop,
        not_removed: &mut Vec<Failure>,
    ) -> Result<(), Error> {
        stop.check()?;
        // listed through folders held from the cache's root, so a link someone put in place of
        // one names no files elsewhere
        let listed =
            |below: &Path| Folder::existing(&self.root, below).and_then(|folder| folder.list());
        let fans = match listed(Path::new(KEYED)) {
            Ok(fans) => fans,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                not_removed.push(Failure::new(&self.root.join(KEYED), &err));
                return Ok(());
            }
        };
        for fan in fans {
            let Some(fan) = fan
                .to_str()
                .filter(|fan| fan.len() == 2 && fan.bytes().all(|byte| byte.is_ascii_hexdigit()))
            else {
                continue;
            };
            let below = Path::new(KEYED).join(fan);
            let marks = match listed(&below) {
                Ok(marks) => marks,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => {
                    not_removed.push(Failure::new(&self.root.join(&below), &err));
                    continue;
                }
            };
            for mark in marks {
                stop.check()?;
                let Some(digest) = mark
                    .to_str()
                    .and_then(|rest| Sha256::from_hex(&format!("{fan}{rest}")))
                else {
                    continue;
                };
                let object = self.object_path(&digest);
                // a pretend sweep keeps the objects it would remove, so their marks go as well
                let gone = self.volume.pretends_removed(&object)
                    || object.try_exists().is_ok_and(|exists| !exists);
                if gone {
                    let path = self.keyed_path(&digest);
                    super::upkeep::removed(&path, self.remove_inside(&path), not_removed);
                }
            }
        }
        Ok(())
    }

    fn keyed_path(&self, digest: &Sha256) -> PathBuf {
        let hex = digest.hex();
        self.root.join(KEYED).join(&hex[..2]).join(&hex[2..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{cache, store};

    /// a mark is made once, whatever asks for it again, and gc drops it with its object alone:
    /// a pretend sweep lists it with the object it would remove, and keeps both.
    #[test]
    fn a_mark_goes_with_its_object() {
        let (_folder, cache) = cache();
        let kept = store(&cache, b"kept");
        let freed = store(&cache, b"freed");
        let never = crate::digest::sha256(b"never stored");
        assert!(!cache.is_keyed(&kept.digest));
        for digest in [kept.digest, freed.digest, freed.digest] {
            cache.mark_keyed(&digest).unwrap();
        }
        assert!(cache.is_keyed(&kept.digest) && cache.is_keyed(&freed.digest));
        assert!(!cache.is_keyed(&never));
        cache.drop_object(&freed.digest).unwrap();
        let pretending = cache.pretending();
        let mut not_removed = Vec::new();
        pretending
            .forget_keyed_reporting(&Stop::default(), &mut not_removed)
            .unwrap();
        assert!(not_removed.is_empty(), "{not_removed:?}");
        assert_eq!(pretending.pretended(), [cache.keyed_path(&freed.digest)]);
        assert!(
            cache.is_keyed(&freed.digest),
            "a pretend sweep removes nothing"
        );
        cache
            .forget_keyed_reporting(&Stop::default(), &mut not_removed)
            .unwrap();
        assert!(not_removed.is_empty(), "{not_removed:?}");
        assert!(cache.is_keyed(&kept.digest));
        assert!(!cache.is_keyed(&freed.digest));
    }

    /// a mark that cannot be looked at counts as one, so doubt never makes a file public.
    #[cfg(unix)]
    #[test]
    fn a_mark_that_cannot_be_read_counts() {
        use std::os::unix::fs::PermissionsExt;
        let (_folder, cache) = cache();
        let digest = crate::digest::sha256(b"abc");
        assert!(!cache.is_keyed(&digest));
        fs::create_dir_all(cache.root().join(KEYED)).unwrap();
        fs::write(cache.root().join(KEYED).join(&digest.hex()[..2]), b"").unwrap();
        assert!(cache.is_keyed(&digest), "a file where its fan should be");
        fs::remove_file(cache.root().join(KEYED).join(&digest.hex()[..2])).unwrap();
        fs::set_permissions(cache.root().join(KEYED), fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = fs::read_dir(cache.root().join(KEYED)).is_err();
        let keyed = cache.is_keyed(&digest);
        fs::set_permissions(cache.root().join(KEYED), fs::Permissions::from_mode(0o755)).unwrap();
        // root reads every folder, so only another user sees the refusal
        if unreadable {
            assert!(keyed, "a folder that refuses a look");
        }
    }
}
