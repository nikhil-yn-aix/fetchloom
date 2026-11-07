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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::super::tests::{cache, store};
    use super::*;

    /// waits up to ten seconds for `done`.
    fn soon(done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    #[test]
    fn gc_waits_for_every_run_that_holds_the_cache() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"found by a run");
        let run = Cache::open(cache.root.clone())
            .unwrap()
            .hold(Access::Shared, &Stop::default(), &|| {})
            .unwrap();
        let other = Cache::open(cache.root.clone())
            .unwrap()
            .hold(Access::Shared, &Stop::default(), &|| {})
            .unwrap();
        let waited = AtomicBool::new(false);
        let swept = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let gc = Cache::open(cache.root.clone())
                    .unwrap()
                    .hold(Access::Exclusive, &Stop::default(), &|| {
                        waited.store(true, Ordering::SeqCst);
                    })
                    .unwrap();
                gc.sweep(
                    &super::super::Needed::default(),
                    std::time::SystemTime::now() + super::super::GRACE * 2,
                );
                swept.store(true, Ordering::SeqCst);
            });
            assert!(
                soon(|| waited.load(Ordering::SeqCst) || swept.load(Ordering::SeqCst)),
                "gc neither waited nor swept"
            );
            assert!(
                !swept.load(Ordering::SeqCst),
                "gc swept while runs held the cache"
            );
            assert!(cache.object_path(&stored.digest).is_file());
            drop(run);
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                !swept.load(Ordering::SeqCst),
                "gc swept while a run held the cache"
            );
            drop(other);
            assert!(soon(|| swept.load(Ordering::SeqCst)), "gc went on");
        });
        assert!(!cache.object_path(&stored.digest).exists());
    }

    #[test]
    fn a_run_waits_for_gc_and_a_stop_ends_the_wait() {
        let (_dir, cache) = cache();
        let gc = Cache::open(cache.root.clone())
            .unwrap()
            .hold(Access::Exclusive, &Stop::default(), &|| {})
            .unwrap();
        let stop = Stop::default();
        let error = Cache::open(cache.root.clone())
            .unwrap()
            .hold(Access::Shared, &stop, &|| stop.ask())
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        drop(gc);
        Cache::open(cache.root.clone())
            .unwrap()
            .hold(Access::Shared, &Stop::default(), &|| {
                panic!("nothing holds the cache")
            })
            .unwrap();
    }
}
