//! the optional ceiling on local worker groups.

use std::ffi::OsString;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ScopedJoinHandle};

use crate::error::{Error, Kind};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Threads(Option<NonZeroUsize>);

impl Threads {
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Threads, Error> {
        let Some(value) = var("FETCHLOOM_THREADS").filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        value
            .to_str()
            .and_then(|text| text.trim().parse::<NonZeroUsize>().ok())
            .filter(|threads| threads.get() <= 256)
            .map(|threads| Self(Some(threads)))
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!(
                        "FETCHLOOM_THREADS is \"{}\", which is not a number from 1 to 256",
                        value.display()
                    ),
                )
                .help("unset FETCHLOOM_THREADS to use the default worker counts, or set it to a number from 1 to 256")
            })
    }

    pub(crate) fn is_limited(self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn limit(self, requested: usize) -> usize {
        self.0.map_or(requested, |cap| requested.min(cap.get()))
    }

    /// derives a local ceiling while preserving a smaller command-wide limit.
    pub(crate) fn at_most(self, requested: usize) -> Threads {
        Threads(NonZeroUsize::new(self.limit(requested).max(1)))
    }

    /// shares the archive budget between readers and their writers. zero writers means the
    /// reader stores members itself, so a cap of one never needs a second thread to drain a queue.
    pub(crate) fn archives(self, requested: usize, pending: usize) -> (usize, Option<usize>) {
        let Some(cap) = self.0 else {
            return (requested, None);
        };
        let readers = requested.max(1).min(pending.max(1)).min(cap.get());
        (readers, Some(cap.get() / readers - 1))
    }
}

/// work on a scoped thread, or retained for the caller when that thread could not start.
#[must_use = "join the worker to run work retained after a spawn failure"]
pub(crate) enum Scoped<'scope, F, T> {
    Thread(ScopedJoinHandle<'scope, T>),
    Caller(Arc<Mutex<F>>),
}

impl<F: FnMut() -> T, T> Scoped<'_, F, T> {
    pub(crate) fn is_threaded(&self) -> bool {
        matches!(self, Self::Thread(_))
    }

    /// finishes this work. unstarted work runs after its producer can close the queue.
    pub(crate) fn join(self) -> T {
        match self {
            Self::Thread(worker) => worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Self::Caller(work) => run_job(&work),
        }
    }
}

/// starts work without losing its owned input when the OS refuses another thread.
pub(crate) fn scoped<'scope, F, T>(
    scope: &'scope thread::Scope<'scope, '_>,
    work: F,
) -> Scoped<'scope, F, T>
where
    F: FnMut() -> T + Send + 'scope,
    T: Send + 'scope,
{
    scoped_with(work, |job| {
        #[cfg(test)]
        check_spawn()?;
        thread::Builder::new().spawn_scoped(scope, move || run_job(&job))
    })
}

fn scoped_with<'scope, F, T>(
    work: F,
    start: impl FnOnce(Arc<Mutex<F>>) -> io::Result<ScopedJoinHandle<'scope, T>>,
) -> Scoped<'scope, F, T> {
    let job = Arc::new(Mutex::new(work));
    match start(Arc::clone(&job)) {
        Ok(worker) => Scoped::Thread(worker),
        Err(error) => {
            tracing::debug!(%error, "keeping an unstarted worker's work on the calling thread");
            Scoped::Caller(job)
        }
    }
}

fn run_job<F: FnMut() -> T, T>(job: &Mutex<F>) -> T {
    (*job.lock().unwrap_or_else(PoisonError::into_inner))()
}

#[cfg(test)]
thread_local! {
    static REFUSE_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn check_spawn() -> io::Result<()> {
    if REFUSE_AFTER.get().is_some_and(|left| {
        REFUSE_AFTER.set(Some(left.saturating_sub(1)));
        left == 0
    }) {
        Err(io::ErrorKind::WouldBlock.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct Refusing(Option<usize>);

#[cfg(test)]
impl Drop for Refusing {
    fn drop(&mut self) {
        REFUSE_AFTER.set(self.0);
    }
}

#[cfg(test)]
pub(crate) fn refuse_after(successful: usize) -> Refusing {
    Refusing(REFUSE_AFTER.replace(Some(successful)))
}

/// the threads [`refuse_after`] still lets this thread start, which counts those it started.
#[cfg(test)]
pub(crate) fn spawns_left() -> Option<usize> {
    REFUSE_AFTER.get()
}

/// probes thread creation before tokio can turn an immediate resource refusal into a panic.
pub(crate) fn start_runtime<T>(start: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    probe_thread()?
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    start()
}

fn probe_thread() -> io::Result<thread::JoinHandle<()>> {
    #[cfg(test)]
    check_spawn()?;
    thread::Builder::new().spawn(|| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_values_keep_defaults_and_accept_the_bounds() {
        for value in [None, Some("")] {
            let threads = Threads::read(&|_| value.map(Into::into)).unwrap();
            assert!(!threads.is_limited());
            assert_eq!(threads.limit(512), 512);
            assert_eq!(threads.archives(4, 4), (4, None));
        }
        for (text, cap) in [("1", 1), (" 8 ", 8), ("256", 256)] {
            let threads = Threads::read(&|_| Some(text.into())).unwrap();
            assert!(threads.is_limited());
            assert_eq!(threads.limit(512), cap);
            assert_eq!(threads.limit(1), 1);
        }
    }

    #[test]
    fn invalid_thread_values_are_settings_errors() {
        for text in [
            " ",
            "0",
            "257",
            "-1",
            "1.5",
            "1K",
            "many",
            "999999999999999999999",
        ] {
            let error = Threads::read(&|_| Some(text.into())).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(error.kind.exit_code(), 2);
            assert!(error.message.contains("FETCHLOOM_THREADS"));
            assert!(error.help.is_some());
        }
    }

    #[test]
    fn archives_share_the_cap_between_readers_and_writers() {
        for cap in 1..=256 {
            let threads = Threads::read(&|_| Some(cap.to_string().into())).unwrap();
            for requested in 1..=4 {
                for pending in 1..=4 {
                    let (readers, writers) = threads.archives(requested, pending);
                    assert!(readers >= 1 && readers <= requested.min(pending));
                    assert!(readers * (writers.unwrap() + 1) <= cap);
                }
            }
        }
        let one = Threads::read(&|_| Some("1".into())).unwrap();
        assert_eq!(one.archives(4, 4), (1, Some(0)));
    }

    #[test]
    fn archive_caps_budget_only_pending_readers() {
        let threads = Threads::read(&|_| Some("8".into())).unwrap();
        for (pending, expected) in [
            (1, (1, Some(7))),
            (2, (2, Some(3))),
            (3, (3, Some(1))),
            (8, (3, Some(1))),
        ] {
            assert_eq!(threads.archives(3, pending), expected, "{pending} archives");
        }
        let one = Threads::read(&|_| Some("1".into())).unwrap();
        assert_eq!(one.archives(3, 8), (1, Some(0)));
        assert_eq!(Threads::default().archives(3, 1), (3, None));
    }

    #[test]
    fn a_refused_worker_keeps_owned_input_until_joined() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let mut input = vec![3, 1, 4];
        let caller = thread::current().id();
        let worker = scoped_with(
            move || {
                assert_eq!(thread::current().id(), caller);
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::mem::take(&mut input)
            },
            |_| Err(io::ErrorKind::WouldBlock.into()),
        );
        assert!(!worker.is_threaded());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(worker.join(), [3, 1, 4]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn refused_workers_preserve_started_results_and_order() {
        let _refusing = refuse_after(1);
        let caller = thread::current().id();
        let results = thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|value| scoped(scope, move || (value, thread::current().id())))
                .collect();
            assert!(workers[0].is_threaded());
            assert!(workers[1..].iter().all(|worker| !worker.is_threaded()));
            workers.into_iter().map(Scoped::join).collect::<Vec<_>>()
        });
        assert_ne!(results[0].1, caller);
        for (index, (value, thread)) in results.into_iter().enumerate() {
            assert_eq!(value, index);
            if index != 0 {
                assert_eq!(thread, caller);
            }
        }
    }

    #[test]
    #[should_panic(expected = "job failed")]
    fn a_refused_worker_does_not_hide_job_panics() {
        let worker = scoped_with(
            || panic!("job failed"),
            |_| Err(io::ErrorKind::WouldBlock.into()),
        );
        worker.join();
    }

    #[test]
    #[should_panic(expected = "job failed")]
    fn a_started_worker_does_not_hide_job_panics() {
        thread::scope(|scope| scoped(scope, || panic!("job failed")).join());
    }

    #[test]
    fn a_refused_startup_probe_does_not_build_the_runtime() {
        let _refusing = refuse_after(0);
        let started = std::cell::Cell::new(false);
        let result = start_runtime(|| {
            started.set(true);
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(!started.get());
    }

    #[test]
    fn a_successful_probe_runs_the_builder_and_keeps_its_io_errors() {
        let _refusing = refuse_after(1);
        let called = std::cell::Cell::new(false);
        let result: io::Result<()> = start_runtime(|| {
            called.set(true);
            assert_eq!(REFUSE_AFTER.get(), Some(0));
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(called.get());
    }

    #[test]
    #[should_panic(expected = "another bug")]
    fn unrelated_runtime_panics_still_propagate() {
        let _: io::Result<()> = start_runtime(|| panic!("another bug"));
    }
}
