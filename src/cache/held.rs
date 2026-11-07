//! the cache-wide lock. every run that uses the cache holds it shared, and `cache gc` holds it
//! exclusive, so gc never removes an object a run has found and is about to place, as cargo and
//! uv lock their caches.

use std::fs::{File, TryLockError};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::{Cache, open_unfollowed, read_unfollowed};
use crate::stop::Stop;

/// the file the cache-wide lock is taken on, in the cache's root.
const LOCK: &str = "gc.lock";
/// how often a run that waits for the cache asks for it again.
const RETRY: Duration = Duration::from_millis(100);

/// how a run holds the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    /// a run that finds, adds and places objects, alongside any other such run.
    Shared,
    /// gc, which removes objects, alone.
    Exclusive,
}

impl Cache {
    /// this cache, held for `access` until its last clone is dropped. while another run holds
    /// it in a way that conflicts, `waiting` is called once and the lock is asked for again until
    /// it is free, or until `stop` is asked, which fails with [`io::ErrorKind::Interrupted`]. a
    /// filesystem without locks leaves the cache unheld, as it leaves downloads unclaimed.
    pub(crate) fn hold(
        mut self,
        access: Access,
        stop: &Stop,
        waiting: &dyn Fn(),
    ) -> io::Result<Cache> {
        let Some(file) = open_lock(&self.root.join(LOCK))? else {
            return Ok(self);
        };
        let mut told = false;
        loop {
            let taken = match access {
                Access::Shared => file.try_lock_shared(),
                Access::Exclusive => file.try_lock(),
            };
            match taken {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(_)) => return Ok(self),
            }
            if stop.is_asked() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if !told {
                waiting();
                told = true;
            }
            std::thread::sleep(RETRY);
        }
        self.held = Some(Arc::new(file));
        Ok(self)
    }
}

/// the lock file at `path`, made the first time. none on a read-only disk, where no run can
/// remove an object either.
fn open_lock(path: &Path) -> io::Result<Option<File>> {
    match open_unfollowed(path, true) {
        Ok(file) => Ok(Some(file)),
        // another user's lock file in a shared cache still locks when opened only to read
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            read_unfollowed(path).map(Some)
        }
        Err(err) if err.kind() == io::ErrorKind::ReadOnlyFilesystem => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::super::tests::{cache, store};
    use super::*;

    fn waits_until_stopped(cache: &Cache, access: Access) {
        let stop = Stop::default();
        let waits = Cell::new(0);
        let error = Cache::open(cache.root.clone())
            .unwrap()
            .hold(access, &stop, &|| {
                waits.set(waits.get() + 1);
                stop.ask();
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(waits.get(), 1);
    }

    #[test]
    fn gc_waits_for_every_run_that_holds_the_cache() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"found by a run");
        let run = cache
            .clone()
            .hold(Access::Shared, &Stop::default(), &|| {})
            .unwrap();
        let other = cache
            .clone()
            .hold(Access::Shared, &Stop::default(), &|| {})
            .unwrap();
        waits_until_stopped(&cache, Access::Exclusive);
        drop(run);
        waits_until_stopped(&cache, Access::Exclusive);
        assert!(cache.object_path(&stored.digest).is_file());
        drop(other);
    }

    #[test]
    fn a_run_waits_for_gc_and_a_stop_ends_the_wait() {
        let (_dir, cache) = cache();
        let _gc = cache
            .clone()
            .hold(Access::Exclusive, &Stop::default(), &|| {})
            .unwrap();
        waits_until_stopped(&cache, Access::Shared);
    }

    #[test]
    fn clones_keep_the_cache_held_until_the_last_one_drops() {
        let (_dir, cache) = cache();
        let run = cache
            .clone()
            .hold(Access::Shared, &Stop::default(), &|| {})
            .unwrap();
        let held = Arc::downgrade(run.held.as_ref().unwrap());
        let other = run.clone();
        drop(run);
        assert!(held.upgrade().is_some());
        waits_until_stopped(&cache, Access::Exclusive);
        drop(other);
        // a forked test helper may still hold the fd until exec, independently of these arcs.
        assert!(held.upgrade().is_none());
    }

    #[test]
    fn a_free_cache_accepts_either_access_mode() {
        for access in [Access::Shared, Access::Exclusive] {
            let (_dir, cache) = cache();
            let held = cache
                .hold(access, &Stop::default(), &|| {
                    panic!("a fresh cache is free")
                })
                .unwrap();
            assert!(held.held.is_some());
        }
    }
}
