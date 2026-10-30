//! the threads that turn members into cache objects while one thread reads the archive. creating,
//! flushing and closing a file costs far more than writing a few kilobytes into it, on windows
//! most of all, so small members travel to the writers whole and are written in parallel. no
//! object is put in place until the whole archive passed every check, so a refused archive leaves
//! nothing in the cache, and an object another process already uses is never taken back.

use std::io::{self, Read, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::guard::Guard;
use super::not_written;
use crate::cache::{Cache, Member, NewObject, Sealed, Staging};
use crate::error::{Error, Kind};

/// members up to this size reach a writer as bytes in memory. a larger one is written by the
/// reading thread as it is read, and only flushed and closed by a writer.
const SMALL: usize = 1 << 20;

/// creating files stops getting faster past a few threads: a 4 core laptop peaked at 4 to 8.
const MOST_WRITERS: usize = 8;

enum Job {
    Bytes(String, Vec<u8>),
    Written(String, Box<NewObject>),
}

/// where the reading thread hands each member.
pub(super) struct Writers<'s> {
    staging: &'s Arc<Staging>,
    jobs: Option<SyncSender<Job>>,
    sealed: &'s Mutex<Vec<(String, Sealed)>>,
    flush_each: bool,
    failed: &'s AtomicBool,
}

impl Writers<'_> {
    /// reads one member through the guard and queues it for a writer.
    pub(super) fn keep(
        &self,
        guard: &mut Guard<'_>,
        path: String,
        from: &mut dyn Read,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        let mut spill = Spill {
            staging: self.staging,
            bytes: Vec::new(),
            object: None,
        };
        guard.copy(&path, from, &mut spill, buffer)?;
        let job = match spill.object {
            Some(object) => Job::Written(path, Box::new(object)),
            None => Job::Bytes(path, spill.bytes),
        };
        if let Some(jobs) = &self.jobs {
            jobs.send(job).map_err(|_| stopped())
        } else {
            locked(self.sealed).push(seal(self.staging, job, self.flush_each)?);
            Ok(())
        }
    }
}

/// runs `read` with writers that seal every member it keeps, then puts them all in place and
/// returns them, with how many of them were new to the cache. when anything fails, what was
/// sealed is removed and nothing is put in place.
pub(super) fn store_all(
    cache: &Cache,
    cap: Option<usize>,
    read: impl FnOnce(&Writers<'_>) -> Result<(), Error>,
) -> Result<(Vec<Member>, u64), Error> {
    let staging = cache
        .staging()
        .map_err(|err| Error::io("could not stage the archive's files", &err))?;
    let threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MOST_WRITERS);
    let threads = cap.map_or(threads, |cap| threads.min(cap));
    #[cfg(target_os = "linux")]
    let batch = cache
        .batch()
        .map_err(|err| Error::io("could not open the cache to flush what is unpacked", &err))?;
    #[cfg(target_os = "linux")]
    let flush_each = batch.is_none();
    #[cfg(not(target_os = "linux"))]
    let flush_each = true;
    let (jobs, queue) = sync_channel::<Job>(threads * 2);
    let queue = Mutex::new(queue);
    let failed = AtomicBool::new(false);
    let sealed = Mutex::new(Vec::new());
    let trouble = Mutex::new(None);
    let read = thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let next = locked(&queue).recv();
                    // the queue closes once the reading thread is done and every job is taken
                    let Ok(job) = next else { return };
                    // after a failure, dropping a job removes its temporary file
                    if failed.load(Ordering::Relaxed) {
                        continue;
                    }
                    match seal(&staging, job, flush_each) {
                        Ok(done) => locked(&sealed).push(done),
                        Err(err) => {
                            failed.store(true, Ordering::Relaxed);
                            locked(&trouble).get_or_insert(err);
                        }
                    }
                }
            });
        }
        let writers = Writers {
            staging: &staging,
            jobs: (threads > 0).then_some(jobs),
            sealed: &sealed,
            flush_each,
            failed: &failed,
        };
        read(&writers)
    });
    let sealed = sealed.into_inner().unwrap_or_else(PoisonError::into_inner);
    let trouble = trouble.into_inner().unwrap_or_else(PoisonError::into_inner);
    // a writer's error is the cause, and the reading thread only saw the writers stop
    if let Some(failure) = trouble.or_else(|| read.err()) {
        return Err(failure);
    }
    // nothing is published until every member is on disk
    #[cfg(target_os = "linux")]
    if let Some(batch) = batch {
        batch
            .flush()
            .map_err(|err| Error::io("could not flush the unpacked files to disk", &err))?;
    }
    publish_all(cache, sealed, threads)
}

/// puts every sealed member in place on `threads` threads. what is left after a failure is
/// removed, and what was put in place stays for others to use or for gc to free.
fn publish_all(
    cache: &Cache,
    sealed: Vec<(String, Sealed)>,
    threads: usize,
) -> Result<(Vec<Member>, u64), Error> {
    let count = sealed.len();
    let queue = Mutex::new(sealed.into_iter());
    let members = Mutex::new(Vec::with_capacity(count));
    let new = AtomicU64::new(0);
    let trouble = Mutex::new(None);
    let publish = || {
        loop {
            let Some((path, sealed)) = locked(&queue).next() else {
                return;
            };
            match sealed.publish(cache) {
                Ok(stored) => {
                    if stored.new {
                        new.fetch_add(1, Ordering::Relaxed);
                    }
                    locked(&members).push(Member {
                        path,
                        size: stored.size,
                        sha256: stored.digest,
                    });
                }
                Err(err) => {
                    locked(&trouble).get_or_insert(not_written(&path, &err));
                    return;
                }
            }
        }
    };
    if threads == 0 {
        publish();
    } else {
        thread::scope(|scope| {
            for _ in 0..threads.min(count) {
                scope.spawn(publish);
            }
        });
    }
    match trouble.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(err) => Err(err),
        None => Ok((
            members.into_inner().unwrap_or_else(PoisonError::into_inner),
            new.into_inner(),
        )),
    }
}

fn seal(staging: &Arc<Staging>, job: Job, flush: bool) -> Result<(String, Sealed), Error> {
    let (path, object) = match job {
        Job::Bytes(path, bytes) => {
            let object = staging.new_object().and_then(|mut object| {
                object.write_all(&bytes)?;
                Ok(object)
            });
            (path, object)
        }
        Job::Written(path, object) => (path, Ok(*object)),
    };
    // in a batch the members are flushed together before any is published
    match object.and_then(|object| object.seal(flush)) {
        Ok(sealed) => Ok((path, sealed)),
        Err(err) => Err(not_written(&path, &err)),
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// what the reading thread sees once a writer failed. the writer's own error is the one reported.
fn stopped() -> Error {
    Error::new(Kind::LocalIo, "storing the archive's files stopped")
}

/// a member's bytes: in memory while small, in a new object once they outgrow that.
struct Spill<'c> {
    staging: &'c Arc<Staging>,
    bytes: Vec<u8>,
    object: Option<NewObject>,
}

impl Write for Spill<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(object) = &mut self.object {
            return object.write(buffer);
        }
        if self.bytes.len() + buffer.len() <= SMALL {
            self.bytes.extend_from_slice(buffer);
            return Ok(buffer.len());
        }
        let mut object = self.staging.new_object()?;
        object.write_all(&std::mem::take(&mut self.bytes))?;
        let written = object.write(buffer)?;
        self.object = Some(object);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::archive::samples::Scratch;
    use crate::archive::{Limits, guard::Meter};

    fn stored(scratch: &Scratch, members: &[(&str, Vec<u8>)]) -> Result<Vec<Member>, Error> {
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        store_all(&scratch.cache, None, |writers| {
            let mut buffer = vec![0; 64 << 10];
            for (path, bytes) in members {
                writers.keep(&mut guard, (*path).to_owned(), &mut &bytes[..], &mut buffer)?;
            }
            Ok(())
        })
        .map(|(members, _)| members)
    }

    #[test]
    fn no_writer_threads_still_store_every_member() {
        let scratch = Scratch::new();
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let bytes = vec![7; SMALL + 1];
        let (mut members, written) = store_all(&scratch.cache, Some(0), |writers| {
            assert!(writers.jobs.is_none());
            for (name, mut bytes) in [("large", bytes.as_slice()), ("small", b"1,2\n".as_slice())] {
                writers.keep(&mut guard, name.to_owned(), &mut bytes, &mut [0; 1024])?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(written, 2);
        members.sort_by(|a, b| a.path.cmp(&b.path));
        scratch.assert_stored(&members[0], &bytes);
        scratch.assert_stored(&members[1], b"1,2\n");
    }

    #[test]
    fn a_failed_inline_read_publishes_nothing() {
        let scratch = Scratch::new();
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let error = store_all(&scratch.cache, Some(0), |writers| {
            writers.keep(
                &mut guard,
                "small".to_owned(),
                &mut &b"1,2\n"[..],
                &mut [0; 1024],
            )?;
            Err(Error::new(Kind::SourceInvalid, "refused at the end"))
        })
        .unwrap_err();
        assert_eq!(error.message, "refused at the end");
        scratch.assert_empty();
    }

    fn staged(scratch: &Scratch) -> PathBuf {
        let folder = scratch
            .cache
            .root()
            .join("tmp")
            .join("0123456789abcdef.stage");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        let member = folder.join("member.new");
        fs::write(&member, b"unfinished").unwrap();
        crate::cache::tests::seal(&member);
        folder
    }

    #[test]
    fn the_next_unpack_removes_abandoned_staging() {
        let scratch = Scratch::new();
        let folder = staged(&scratch);
        stored(&scratch, &[]).unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn the_next_unpack_leaves_locked_staging() {
        let scratch = Scratch::new();
        let folder = staged(&scratch);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(folder.join("lock"))
            .unwrap();
        lock.try_lock().unwrap();
        stored(&scratch, &[]).unwrap();
        let later = SystemTime::now() + Duration::from_hours(48);
        scratch.cache.sweep(&crate::cache::Needed::default(), later);
        assert_eq!(fs::read(folder.join("member.new")).unwrap(), b"unfinished");
        drop(lock);
        stored(&scratch, &[]).unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn an_unpack_keeps_its_staging_locked_until_cleanup() {
        let scratch = Scratch::new();
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let bytes = vec![7; SMALL + 1];
        let (members, _) = store_all(&scratch.cache, None, |writers| {
            writers.keep(
                &mut guard,
                "large.bin".to_owned(),
                &mut &bytes[..],
                &mut [0; 1024],
            )?;
            let folder = fs::read_dir(scratch.cache.root().join("tmp"))
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .find(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "stage")
                })
                .unwrap();
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(folder.join("lock"))
                .unwrap();
            assert!(matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)));
            let later = SystemTime::now() + Duration::from_hours(48);
            assert_eq!(
                scratch
                    .cache
                    .sweep(&crate::cache::Needed::default(), later)
                    .leftovers,
                0
            );
            assert!(folder.is_dir());
            Ok(())
        })
        .unwrap();
        scratch.assert_stored(&members[0], &bytes);
        assert_eq!(
            fs::read_dir(scratch.cache.root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn small_and_large_members_are_stored_alike() {
        let scratch = Scratch::new();
        let large = vec![7; SMALL + 1];
        let mut members = stored(
            &scratch,
            &[
                ("small.csv", b"1,2\n".to_vec()),
                ("large.bin", large.clone()),
            ],
        )
        .unwrap();
        members.sort_by(|a, b| a.path.cmp(&b.path));
        scratch.assert_stored(&members[0], &large);
        scratch.assert_stored(&members[1], b"1,2\n");
        assert_eq!(
            std::fs::read_dir(scratch.cache.root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn a_failed_read_puts_nothing_in_place() {
        let scratch = Scratch::new();
        let kept = crate::cache::tests::store(&scratch.cache, b"0\n");
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let err = store_all(&scratch.cache, None, |writers| {
            let mut buffer = vec![0; 1024];
            for index in 0..50 {
                writers.keep(
                    &mut guard,
                    format!("{index}.csv"),
                    &mut format!("{index}\n").as_bytes(),
                    &mut buffer,
                )?;
            }
            Err(Error::new(Kind::SourceInvalid, "refused at the end"))
        })
        .unwrap_err();
        assert_eq!(err.message, "refused at the end");
        assert_eq!(
            scratch.cache.objects().len(),
            1,
            "only the object stored before"
        );
        assert!(scratch.cache.find(&kept.digest, Some(2)).is_some());
        assert_eq!(
            std::fs::read_dir(scratch.cache.root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }
}
