//! the threads that turn members into cache objects while one thread reads the archive. creating,
//! flushing and closing a file costs far more than writing a few kilobytes into it, on windows
//! most of all, so small members travel to the writers whole and are written in parallel. no
//! object is put in place until the whole archive passed every check, so a refused archive leaves
//! nothing in the cache, and an object another process already uses is never taken back.

use std::io::{self, Read, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;

use super::guard::Guard;
use super::not_written;
use crate::cache::{Cache, Member, NewObject, Sealed};
use crate::error::{Error, Kind};

/// members up to this size reach a writer as bytes in memory. a larger one is written by the
/// reading thread as it is read, and only flushed and closed by a writer.
const SMALL: usize = 1 << 20;

/// creating files stops getting faster past a few threads: a 4 core laptop peaked at 4 to 8.
const MOST_WRITERS: usize = 8;

/// one member on its way into the cache.
enum Job {
    Bytes(String, Vec<u8>),
    Written(String, Box<NewObject>),
}

/// where the reading thread hands each member.
pub(super) struct Writers<'s> {
    cache: &'s Cache,
    jobs: SyncSender<Job>,
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
            cache: self.cache,
            bytes: Vec::new(),
            object: None,
        };
        guard.copy(&path, from, &mut spill, buffer)?;
        let job = match spill.object {
            Some(object) => Job::Written(path, Box::new(object)),
            None => Job::Bytes(path, spill.bytes),
        };
        self.jobs.send(job).map_err(|_| stopped())
    }
}

/// runs `read` with writers that seal every member it keeps, then puts them all in place and
/// returns them. when anything fails, what was sealed is removed and nothing is put in place.
pub(super) fn store_all(
    cache: &Cache,
    read: impl FnOnce(&Writers<'_>) -> Result<(), Error>,
) -> Result<Vec<Member>, Error> {
    let threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MOST_WRITERS);
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
                    match seal(cache, job) {
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
            cache,
            jobs,
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
    publish_all(cache, sealed, threads)
}

/// puts every sealed member in place on `threads` threads. what is left after a failure is
/// removed, and what was put in place stays for others to use or for gc to free.
fn publish_all(
    cache: &Cache,
    sealed: Vec<(String, Sealed)>,
    threads: usize,
) -> Result<Vec<Member>, Error> {
    let count = sealed.len();
    let queue = Mutex::new(sealed.into_iter());
    let members = Mutex::new(Vec::with_capacity(count));
    let trouble = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..threads.min(count) {
            scope.spawn(|| {
                loop {
                    let Some((path, sealed)) = locked(&queue).next() else {
                        return;
                    };
                    match sealed.publish(cache) {
                        Ok(stored) => locked(&members).push(Member {
                            path,
                            size: stored.size,
                            sha256: stored.digest,
                        }),
                        Err(err) => {
                            locked(&trouble).get_or_insert(not_written(&path, &err));
                            return;
                        }
                    }
                }
            });
        }
    });
    match trouble.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(err) => Err(err),
        None => Ok(members.into_inner().unwrap_or_else(PoisonError::into_inner)),
    }
}

fn seal(cache: &Cache, job: Job) -> Result<(String, Sealed), Error> {
    let (path, object) = match job {
        Job::Bytes(path, bytes) => {
            let object = cache.new_object().and_then(|mut object| {
                object.write_all(&bytes)?;
                Ok(object)
            });
            (path, object)
        }
        Job::Written(path, object) => (path, Ok(*object)),
    };
    match object.and_then(NewObject::seal) {
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
    cache: &'c Cache,
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
        let mut object = self.cache.new_object()?;
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
    use super::*;
    use crate::archive::samples::Scratch;
    use crate::archive::{Limits, guard::Meter};

    fn stored(scratch: &Scratch, members: &[(&str, Vec<u8>)]) -> Result<Vec<Member>, Error> {
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        store_all(&scratch.cache, |writers| {
            let mut buffer = vec![0; 64 << 10];
            for (path, bytes) in members {
                writers.keep(&mut guard, (*path).to_owned(), &mut &bytes[..], &mut buffer)?;
            }
            Ok(())
        })
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
        let err = store_all(&scratch.cache, |writers| {
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
