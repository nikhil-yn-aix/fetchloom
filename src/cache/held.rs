//! the cache-wide lock: runs that use the cache hold it shared and `cache gc` exclusive, so gc
//! never removes an object a run has found and is about to place.

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
    /// this cache, held for `access` until its last clone is dropped, waiting out a conflicting
    /// run: `waiting` is called once, and `stop` gives [`io::ErrorKind::Interrupted`]. shared
    /// access goes on unheld where locking is unsupported; every other lock error fails.
    pub(crate) fn hold(self, access: Access, stop: &Stop, waiting: &dyn Fn()) -> io::Result<Cache> {
        self.hold_with(
            access,
            stop,
            waiting,
            |file, access| match access {
                Access::Shared => file.try_lock_shared(),
                Access::Exclusive => file.try_lock(),
            },
            || std::thread::sleep(RETRY),
            open_lock,
        )
    }

    fn hold_with(
        mut self,
        access: Access,
        stop: &Stop,
        waiting: &dyn Fn(),
        take: impl Fn(&File, Access) -> Result<(), TryLockError>,
        retry: impl Fn(),
        open: impl FnOnce(&Path) -> io::Result<Option<File>>,
    ) -> io::Result<Cache> {
        let Some(file) = open(&self.root.join(LOCK))? else {
            return if access == Access::Shared {
                Ok(self)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::ReadOnlyFilesystem,
                    "cannot collect the cache without an exclusive cache lock on its read-only root",
                ))
            };
        };
        let mut told = false;
        loop {
            let taken = take(&file, access);
            match taken {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(err))
                    if access == Access::Shared && err.kind() == io::ErrorKind::Unsupported =>
                {
                    return Ok(self);
                }
                Err(TryLockError::Error(err)) if access == Access::Exclusive => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("cannot collect the cache without an exclusive cache lock: {err}"),
                    ));
                }
                Err(TryLockError::Error(err)) => return Err(err),
            }
            if stop.is_asked() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if !told {
                waiting();
                told = true;
            }
            retry();
        }
        self.held = Some(Arc::new(file));
        Ok(self)
    }
}

/// the lock file at `path`, made the first time. a read-only root can serve shared readers
/// without creating it, but cannot authorize collection of writable child mounts.
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
        // a forked test helper may still hold the fd until exec, independently of these arcs
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

    #[test]
    fn gc_refuses_every_lock_error_instead_of_running_unlocked() {
        for kind in [
            io::ErrorKind::Unsupported,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
        ] {
            let (_dir, cache) = cache();
            let error = cache
                .hold_with(
                    Access::Exclusive,
                    &Stop::default(),
                    &|| panic!("a lock error is not contention"),
                    |_, access| {
                        assert_eq!(access, Access::Exclusive);
                        Err(TryLockError::Error(io::Error::new(kind, "lock refused")))
                    },
                    || panic!("a lock error must not be retried"),
                    open_lock,
                )
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(error.to_string().contains("exclusive cache lock"));
        }
    }

    #[test]
    fn unexpected_shared_lock_errors_stop_the_run() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let (_dir, cache) = cache();
            let error = cache
                .hold_with(
                    Access::Shared,
                    &Stop::default(),
                    &|| panic!("a lock error is not contention"),
                    |_, _| Err(TryLockError::Error(io::Error::new(kind, "lock refused"))),
                    || panic!("a lock error must not be retried"),
                    open_lock,
                )
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "lock refused");
        }
    }

    #[test]
    fn unsupported_shared_locking_keeps_the_private_writer_fallback() {
        let (_dir, cache) = cache();
        let held = cache
            .hold_with(
                Access::Shared,
                &Stop::default(),
                &|| panic!("unsupported locking is not contention"),
                |_, _| Err(TryLockError::Error(io::ErrorKind::Unsupported.into())),
                || panic!("unsupported locking must not be retried"),
                open_lock,
            )
            .unwrap();
        assert!(held.held.is_none());
    }

    #[test]
    fn contention_is_announced_once_and_retried_until_acquired() {
        let (_dir, cache) = cache();
        let attempts = Cell::new(0);
        let waits = Cell::new(0);
        let retries = Cell::new(0);
        let held = cache
            .hold_with(
                Access::Shared,
                &Stop::default(),
                &|| waits.set(waits.get() + 1),
                |_, _| {
                    attempts.set(attempts.get() + 1);
                    if attempts.get() < 3 {
                        Err(TryLockError::WouldBlock)
                    } else {
                        Ok(())
                    }
                },
                || retries.set(retries.get() + 1),
                open_lock,
            )
            .unwrap();
        assert!(held.held.is_some());
        assert_eq!(attempts.get(), 3);
        assert_eq!(waits.get(), 1);
        assert_eq!(retries.get(), 2);
    }

    #[test]
    fn gc_requires_a_lock_even_when_the_root_is_read_only() {
        for access in [Access::Shared, Access::Exclusive] {
            let (_dir, cache) = cache();
            let held = cache.hold_with(
                access,
                &Stop::default(),
                &|| panic!("no lock is not contention"),
                |_, _| panic!("there is no file to lock"),
                || panic!("a read-only root is not retried"),
                |_| Ok(None),
            );
            match access {
                Access::Shared => assert!(held.unwrap().held.is_none()),
                Access::Exclusive => {
                    let error = held.unwrap_err();
                    assert_eq!(error.kind(), io::ErrorKind::ReadOnlyFilesystem);
                    assert!(error.to_string().contains("exclusive cache lock"));
                }
            }
        }
    }
}
