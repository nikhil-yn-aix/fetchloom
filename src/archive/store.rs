//! the threads that turn members into cache objects while one thread reads the archive. creating,
//! flushing and closing a file costs far more than writing a few kilobytes into it, on windows
//! most of all, so small members travel to the writers whole and are written in parallel. no
//! object is put in place until the whole archive passed every check, so a refused archive leaves
//! nothing in the cache, and an object another process already uses is never taken back.

mod stream;

use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::guard::Guard;
use super::not_written;
#[cfg(target_os = "linux")]
use crate::cache::Batch;
use crate::cache::{Cache, MOST_FLUSHED_ALONE, Member, NewObject, Sealed, Staging};
use crate::digest::Sha256;
use crate::error::{Error, Kind};

/// members up to this size reach a writer as bytes in memory. larger ones stream to a writer
/// or stay on the reader when the handoff would not pay for itself.
const SMALL: usize = 1 << 20;

/// leaves room to repay the handoff above the in-memory spill threshold.
const LARGE: u64 = 4 << 20;

/// creating files stops getting faster past a few threads: a 4 core laptop peaked at 4 to 8.
const MOST_WRITERS: usize = 8;

enum Job {
    #[cfg(test)]
    Pause {
        ready: SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
    Bytes(String, Vec<u8>),
    Written(String, Box<NewObject>),
    Stream(String, Box<stream::Receiving>),
}

/// where the reading thread hands each member.
pub(super) struct Writers<'s> {
    staging: &'s Arc<Staging>,
    jobs: Option<SyncSender<Job>>,
    sealed: &'s Mutex<Vec<(String, Sealed)>>,
    flush_each: bool,
    few: Option<&'s Few>,
    failed: &'s AtomicBool,
    streaming: bool,
}

impl Writers<'_> {
    /// reads a member through the guard; its declared size chooses only the storage handoff.
    pub(super) fn keep(
        &self,
        guard: &mut Guard<'_>,
        path: String,
        size: u64,
        from: &mut dyn Read,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        let mut spill = Spill {
            staging: self.staging,
            jobs: (self.streaming && size > LARGE)
                .then_some(self.jobs.as_ref())
                .flatten(),
            path: &path,
            bytes: Vec::new(),
            object: None,
            stream: None,
        };
        guard.copy(&path, from, &mut spill, buffer)?;
        let Spill {
            bytes,
            object,
            stream,
            ..
        } = spill;
        if let Some(stream) = stream {
            return stream.finish().map_err(|error| not_written(&path, &error));
        }
        let job = match object {
            Some(object) => Job::Written(path, Box::new(object)),
            None => Job::Bytes(path, bytes),
        };
        if let Some(jobs) = &self.jobs {
            jobs.send(job).map_err(|_| stopped())
        } else {
            if let Some(sealed) = seal(self.staging, job, self.flush_each, self.few)? {
                locked(self.sealed).push(sealed);
            }
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
) -> Result<(Vec<Member>, Vec<Sha256>), Error> {
    let threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MOST_WRITERS);
    let threads = cap.map_or(threads, |cap| threads.min(cap));
    store_with(cache, threads, read)
}

fn store_with(
    cache: &Cache,
    threads: usize,
    read: impl FnOnce(&Writers<'_>) -> Result<(), Error>,
) -> Result<(Vec<Member>, Vec<Sha256>), Error> {
    let staging = cache.staging().map_err(|err| {
        Error::io(
            format_args!(
                "could not make a folder to unpack the archive into, in {}",
                cache.root().join("tmp").display()
            ),
            &err,
        )
    })?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = cache
        .batch()
        .map_err(|err| Error::io("could not open the cache to flush what is unpacked", &err))?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush_each = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush_each = true;
    let few = Few::new();
    let keeps = (cfg!(target_os = "linux") && !flush_each).then_some(&few);
    let (jobs, queue) = sync_channel::<Job>(threads * 2);
    let queue = Mutex::new(queue);
    let failed = AtomicBool::new(false);
    let sealed = Mutex::new(Vec::new());
    let trouble = Mutex::new(None);
    let write = || {
        loop {
            let next = locked(&queue).recv();
            let Ok(job) = next else { return };
            #[cfg(test)]
            let job = match job {
                Job::Pause { ready, release } => {
                    ready.send(()).unwrap();
                    release.recv().unwrap();
                    continue;
                }
                job => job,
            };
            if failed.load(Ordering::Relaxed) {
                continue;
            }
            match seal(&staging, job, flush_each, keeps) {
                Ok(Some(done)) => locked(&sealed).push(done),
                Ok(None) => {}
                Err(err) => {
                    failed.store(true, Ordering::Relaxed);
                    locked(&trouble).get_or_insert(err);
                }
            }
        }
    };
    let read = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| crate::threads::scoped(scope, &write))
            .collect();
        let threaded = workers.iter().any(crate::threads::Scoped::is_threaded);
        let writers = Writers {
            staging: &staging,
            jobs: threaded.then_some(jobs),
            sealed: &sealed,
            flush_each,
            few: keeps,
            failed: &failed,
            streaming: threads > 1,
        };
        // with no live consumer the reader stores inline; joining before closing would hang.
        let result = read(&writers);
        drop(writers);
        for worker in workers {
            worker.join();
        }
        result
    });
    let sealed = sealed.into_inner().unwrap_or_else(PoisonError::into_inner);
    let trouble = trouble.into_inner().unwrap_or_else(PoisonError::into_inner);
    // a writer's error is the cause, and the reading thread only saw the writers stop
    if let Some(failure) = trouble.or_else(|| read.err()) {
        return Err(failure);
    }
    // nothing is published until every member is on disk
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        #[cfg(target_os = "linux")]
        let flushed = few.flush(&batch, threads);
        #[cfg(target_os = "macos")]
        let flushed = batch.flush();
        flushed.map_err(|err| Error::io("could not flush the unpacked files to disk", &err))?;
    }
    publish_all(cache, sealed, threads)
}

/// puts every sealed member in place on `threads` threads, and returns the members with the
/// digests of those new to the cache. what is left after a failure is removed, and what was put
/// in place stays for others to use or for gc to free.
fn publish_all(
    cache: &Cache,
    sealed: Vec<(String, Sealed)>,
    threads: usize,
) -> Result<(Vec<Member>, Vec<Sha256>), Error> {
    let count = sealed.len();
    let queue = Mutex::new(sealed.into_iter());
    let members = Mutex::new(Vec::with_capacity(count));
    let added = Mutex::new(Vec::new());
    let trouble = Mutex::new(None);
    let publish = || {
        loop {
            let Some((path, sealed)) = locked(&queue).next() else {
                return;
            };
            match sealed.publish(cache) {
                Ok(stored) => {
                    if stored.new {
                        locked(&added).push(stored.digest);
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
            let workers: Vec<_> = (0..threads.min(count))
                .map(|_| crate::threads::scoped(scope, &publish))
                .collect();
            for worker in workers {
                worker.join();
            }
        });
    }
    match trouble.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(err) => Err(err),
        None => Ok((
            members.into_inner().unwrap_or_else(PoisonError::into_inner),
            added.into_inner().unwrap_or_else(PoisonError::into_inner),
        )),
    }
}

fn seal(
    staging: &Arc<Staging>,
    job: Job,
    flush: bool,
    few: Option<&Few>,
) -> Result<Option<(String, Sealed)>, Error> {
    let (path, object) = match job {
        #[cfg(test)]
        Job::Pause { .. } => return Err(stopped()),
        Job::Bytes(path, bytes) => {
            let object = staging.new_object().and_then(|mut object| {
                object.write_all(&bytes)?;
                Ok(Some(object))
            });
            (path, object)
        }
        Job::Written(path, object) => (path, Ok(Some(*object))),
        Job::Stream(path, stream) => (path, stream.store(staging)),
    };
    let Some(object) = object.map_err(|error| not_written(&path, &error))? else {
        return Ok(None);
    };
    // in a batch the members are flushed together before any is published
    let sealed = (|| {
        if let Some(few) = few {
            few.keep(&object)?;
        }
        object.seal(flush)
    })();
    match sealed {
        Ok(sealed) => Ok(Some((path, sealed))),
        Err(err) => Err(not_written(&path, &err)),
    }
}

/// the handles that wrote an archive's first members. on linux an archive of a few members
/// flushes each through its own handle, since one flush of the filesystem also waits for
/// everything other programs wrote there. none are kept once the archive has more.
struct Few(Mutex<Option<Vec<File>>>);

impl Few {
    fn new() -> Few {
        Few(Mutex::new(Some(Vec::new())))
    }

    fn keep(&self, object: &NewObject) -> io::Result<()> {
        let mut few = locked(&self.0);
        if let Some(handles) = few
            .as_mut()
            .filter(|handles| handles.len() < MOST_FLUSHED_ALONE)
        {
            handles.push(object.handle()?);
        } else {
            *few = None;
        }
        Ok(())
    }

    /// flushes every member on `threads` threads: through the kept handles while they cover
    /// every member, else once through `batch` for the whole filesystem.
    #[cfg(target_os = "linux")]
    fn flush(self, batch: &Batch, threads: usize) -> io::Result<()> {
        let Some(handles) = self.0.into_inner().unwrap_or_else(PoisonError::into_inner) else {
            return batch.flush();
        };
        let count = handles.len();
        let queue = Mutex::new(handles.into_iter());
        let failed = Mutex::new(None);
        let flush = || {
            loop {
                let Some(file) = locked(&queue).next() else {
                    return;
                };
                if let Err(err) = file.sync_all() {
                    locked(&failed).get_or_insert(err);
                    return;
                }
            }
        };
        if threads == 0 {
            flush();
        } else {
            thread::scope(|scope| {
                let workers: Vec<_> = (0..threads.min(count))
                    .map(|_| crate::threads::scoped(scope, &flush))
                    .collect();
                for worker in workers {
                    worker.join();
                }
            });
        }
        failed
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or(Ok(()), Err)
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// what the reading thread sees once a writer failed. the writer's own error is the one reported.
fn stopped() -> Error {
    Error::new(Kind::LocalIo, "storing the archive's files stopped")
}

/// a member's bytes: buffered while small, then streamed to a writer or stored inline.
struct Spill<'c> {
    staging: &'c Arc<Staging>,
    jobs: Option<&'c SyncSender<Job>>,
    path: &'c str,
    bytes: Vec<u8>,
    object: Option<NewObject>,
    stream: Option<stream::Sending>,
}

impl Write for Spill<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(stream) = &mut self.stream {
            return stream.write(buffer);
        }
        if let Some(object) = &mut self.object {
            return object.write(buffer);
        }
        if self.bytes.len() + buffer.len() <= SMALL {
            self.bytes.extend_from_slice(buffer);
            return Ok(buffer.len());
        }
        if let Some(jobs) = self.jobs {
            let (mut stream, receiving) = stream::channel(std::mem::take(&mut self.bytes));
            jobs.send(Job::Stream(self.path.to_owned(), Box::new(receiving)))
                .map_err(|_| io::Error::other("the archive member's writer stopped"))?;
            let written = stream.write(buffer)?;
            self.stream = Some(stream);
            return Ok(written);
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
                writers.keep(
                    &mut guard,
                    (*path).to_owned(),
                    bytes.len() as u64,
                    &mut &bytes[..],
                    &mut buffer,
                )?;
            }
            Ok(())
        })
        .map(|(members, _)| members)
    }

    /// an archive of a few members flushes each through its own handle, never its whole
    /// filesystem; only a larger one flushes the filesystem once.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_an_archive_of_many_members_waits_for_one_flush() {
        use rustix::fs::{Mode, OFlags};

        let scratch = Scratch::new();
        let staging = scratch.cache.staging().unwrap();
        // syncfs rejects an O_PATH handle, so an archive that waits for one flush fails
        let failing = || {
            let flags = OFlags::PATH | OFlags::CLOEXEC;
            let handle = rustix::fs::open(scratch.cache.root(), flags, Mode::empty());
            Batch::with_file_for_test(File::from(handle.unwrap()))
        };
        for members in [MOST_FLUSHED_ALONE, MOST_FLUSHED_ALONE + 1] {
            let few = Few::new();
            for index in 0..members {
                let job = Job::Bytes(format!("{index}.csv"), index.to_string().into_bytes());
                seal(&staging, job, false, Some(&few)).unwrap();
            }

            let flushed = few.flush(&failing(), 2);

            assert_eq!(
                flushed.is_ok(),
                members <= MOST_FLUSHED_ALONE,
                "{flushed:?}"
            );
        }
    }

    #[test]
    fn no_writer_threads_still_store_every_member() {
        let scratch = Scratch::new();
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let bytes = vec![7; SMALL + 1];
        let (mut members, added) = store_all(&scratch.cache, Some(0), |writers| {
            assert!(writers.jobs.is_none());
            for (name, mut bytes) in [("large", bytes.as_slice()), ("small", b"1,2\n".as_slice())] {
                writers.keep(
                    &mut guard,
                    name.to_owned(),
                    bytes.len() as u64,
                    &mut bytes,
                    &mut [0; 1024],
                )?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(added.len(), 2);
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
                4,
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
        fs::create_dir_all(folder.join("03")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        let member = folder.join("03/member.new");
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
        assert_eq!(
            fs::read(folder.join("03/member.new")).unwrap(),
            b"unfinished"
        );
        drop(lock);
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
                bytes.len() as u64,
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
                    .removed_leftovers,
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
                    3,
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

    #[test]
    fn staging_shards_preserve_members_and_failed_cleanup() {
        let scratch = Scratch::new();
        let large = vec![7; SMALL + 1];
        let mut members = stored(
            &scratch,
            &[("small", b"1,2\n".to_vec()), ("large", large.clone())],
        )
        .unwrap();
        members.sort_by(|a, b| a.path.cmp(&b.path));
        scratch.assert_stored(&members[0], &large);
        scratch.assert_stored(&members[1], b"1,2\n");
        let before = scratch.cache.objects().len();
        let meter = Meter::default();
        let limits = Limits::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        let error = store_all(&scratch.cache, Some(0), |writers| {
            writers.keep(
                &mut guard,
                "discard".to_owned(),
                8,
                &mut &b"not kept"[..],
                &mut [0; 32],
            )?;
            Err(Error::new(Kind::SourceInvalid, "refused"))
        })
        .unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert_eq!(scratch.cache.objects().len(), before);
        assert_eq!(
            fs::read_dir(scratch.cache.root().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }

    fn pending_files(scratch: &Scratch) -> usize {
        let mut folders = vec![scratch.cache.root().join("tmp")];
        let mut count = 0;
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    folders.push(entry.path());
                } else if entry.path().extension().is_some_and(|ext| ext == "new") {
                    count += 1;
                }
            }
        }
        count
    }

    fn paused_writers(writers: &Writers<'_>, count: usize) -> Vec<SyncSender<()>> {
        let (ready, paused) = sync_channel(count);
        let mut releases = Vec::new();
        for _ in 0..count {
            let (release, waiting) = sync_channel(1);
            writers
                .jobs
                .as_ref()
                .unwrap()
                .send(Job::Pause {
                    ready: ready.clone(),
                    release: waiting,
                })
                .unwrap();
            releases.push(release);
        }
        for _ in 0..count {
            paused.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        releases
    }

    #[test]
    fn large_members_leave_writes_and_hashing_on_the_writer() {
        struct Ahead {
            read: usize,
            notified: bool,
            ready: SyncSender<()>,
        }
        impl Read for Ahead {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.notified && self.read >= SMALL + 2 * (64 << 10) {
                    self.notified = true;
                    self.ready.send(()).unwrap();
                }
                let size = buffer.len().min(8 * SMALL - self.read);
                buffer[..size].fill(b'a');
                self.read += size;
                Ok(size)
            }
        }
        let scratch = Scratch::new();
        let (members, _) = store_with(&scratch.cache, 2, |writers| {
            let releases = paused_writers(writers, 2);
            let (ahead, reached) = sync_channel(1);
            let files = thread::scope(|scope| {
                let reading = scope.spawn(|| {
                    let limits = Limits::default();
                    let meter = Meter::default();
                    meter.count(8 * SMALL);
                    let mut guard = Guard::new(&limits, &|_| false, &meter);
                    let mut input = Ahead {
                        read: 0,
                        notified: false,
                        ready: ahead,
                    };
                    writers.keep(
                        &mut guard,
                        "large.bin".to_owned(),
                        (8 * SMALL) as u64,
                        &mut input,
                        &mut vec![0; 64 << 10],
                    )
                });
                let reached = reached.recv_timeout(Duration::from_secs(5));
                let files = pending_files(&scratch);
                for release in releases {
                    release.send(()).unwrap();
                }
                reading.join().unwrap().unwrap();
                reached.unwrap();
                files
            });
            assert_eq!(
                files, 0,
                "a paused writer has not stored the reader's large member"
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].size, (8 * SMALL) as u64);
        assert_eq!(
            members[0].sha256.hex(),
            "ad97f87076920684e2ca66fc44e5d322797dc9d64706b174e51b5d0828937043"
        );
    }

    #[test]
    fn stopping_after_a_large_member_started_preserves_interruption() {
        use std::cell::Cell;
        struct Counted<'a> {
            read: &'a Cell<usize>,
        }
        impl Read for Counted<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let size = buffer.len().min(8 * SMALL - self.read.get());
                buffer[..size].fill(b'a');
                self.read.set(self.read.get() + size);
                Ok(size)
            }
        }
        let scratch = Scratch::new();
        let limits = Limits::default();
        let meter = Meter::default();
        meter.count(8 * SMALL);
        let read = Cell::new(0);
        let stopped = |_| read.get() > SMALL + (64 << 10);
        let mut guard = Guard::new(&limits, &stopped, &meter);
        let error = store_with(&scratch.cache, 2, |writers| {
            writers.keep(
                &mut guard,
                "large.bin".to_owned(),
                (8 * SMALL) as u64,
                &mut Counted { read: &read },
                &mut vec![0; 64 << 10],
            )
        })
        .unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        scratch.assert_empty();
        assert_eq!(pending_files(&scratch), 0);
    }

    #[test]
    fn short_members_or_one_writer_keep_storage_inline() {
        let scratch = Scratch::new();
        let bytes = vec![b'a'; SMALL + (64 << 10)];
        for (size, threads) in [(bytes.len() as u64, 2), (8 << 20, 1)] {
            let (members, _) = store_with(&scratch.cache, threads, |writers| {
                let releases = paused_writers(writers, threads);
                let limits = Limits::default();
                let meter = Meter::default();
                let mut guard = Guard::new(&limits, &|_| false, &meter);
                let written = writers.keep(
                    &mut guard,
                    "medium.bin".to_owned(),
                    size,
                    &mut &bytes[..],
                    &mut vec![0; 64 << 10],
                );
                let files = pending_files(&scratch);
                for release in releases {
                    release.send(()).unwrap();
                }
                written?;
                assert_eq!(
                    files, 1,
                    "short members and one writer avoid the streaming handoff"
                );
                Ok(())
            })
            .unwrap();
            assert_eq!(members[0].size, bytes.len() as u64);
            assert_eq!(
                members[0].sha256.hex(),
                "cd2d309f2ece5dc637ce39628379d5412e600e04628e753e8a9287a554716349"
            );
        }
    }

    #[test]
    fn a_refused_archive_writer_keeps_large_members_inline() {
        let scratch = Scratch::new();
        let refused = crate::threads::refuse_after(0);
        let limits = Limits::default();
        let meter = Meter::default();
        meter.count(8 * SMALL);
        let mut guard = Guard::new(&limits, &|_| false, &meter);
        let bytes = vec![b'a'; 8 * SMALL];
        let (members, _) = store_all(&scratch.cache, Some(2), |writers| {
            assert!(writers.jobs.is_none());
            writers.keep(
                &mut guard,
                "large.bin".to_owned(),
                bytes.len() as u64,
                &mut &bytes[..],
                &mut vec![0; 64 << 10],
            )
        })
        .unwrap();
        drop(refused);
        assert_eq!(
            members[0].sha256.hex(),
            "ad97f87076920684e2ca66fc44e5d322797dc9d64706b174e51b5d0828937043"
        );
        assert_eq!(members[0].size, (8 * SMALL) as u64);
    }
}
